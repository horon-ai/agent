use super::client_actions::{
    AgentTaskContext, NotifySessionContext, handle_agent_task, handle_archive_session, handle_compact,
    handle_input_shell, handle_notify_session, handle_rename_session, handle_run_subagent, handle_set_feature,
    handle_set_session_saved, handle_set_subagent_model, handle_split, handle_stdin_response, handle_transfer,
    handle_trigger_memory_extraction,
};
use super::client_ask::{
    handle_ask_response, handle_ask_response_with_receipt, handle_permission_decision, spawn_ask_forwarder,
};
use super::client_comm::{
    handle_comm_channel_members, handle_comm_list, handle_comm_list_channels, handle_comm_message, handle_comm_read,
    handle_comm_share, handle_comm_subscribe_channel, handle_comm_unsubscribe_channel,
};
use super::client_comm_swarms::{handle_comm_list_swarms, handle_comm_set_swarm_label};
use super::client_disconnect_cleanup::{cleanup_client_connection, detach_client_attachment};
use super::client_lifecycle_logging::{
    ServerRequestLifecycleFields, interrupt_request_log_fields, request_payload_summary, request_type_from_line,
    request_type_is_read_only, server_request_lifecycle_fields,
};
use super::client_lightweight_control::{
    LightweightControlContext, handle_lightweight_control_request, parse_swarm_spawn_mode,
};
use super::client_session::{handle_clear_session, handle_reload, handle_resume_session, handle_subscribe};
use super::client_state::{
    handle_get_compacted_history, handle_get_history, handle_get_model_catalog, handle_get_state,
};
use super::client_writer::write_direct_event;
use super::comm_await::{CommAwaitMembersContext, handle_comm_await_members};
use super::comm_control::{
    handle_comm_assign_next, handle_comm_assign_role, handle_comm_assign_task, handle_comm_task_control,
};
use super::comm_plan::{handle_comm_approve_plan, handle_comm_propose_plan, handle_comm_reject_plan};
use super::comm_session::{handle_comm_spawn, handle_comm_stop};
use super::comm_sync::{
    CommResyncPlanContext, handle_comm_plan_status, handle_comm_read_context, handle_comm_resync_plan,
    handle_comm_status, handle_comm_summary,
};
use super::provider_control::{
    handle_cycle_model, handle_invalidate_openai_usage, handle_notify_auth_changed, handle_refresh_models,
    handle_set_compaction_mode, handle_set_model, handle_set_premium_mode, handle_set_reasoning_effort,
    handle_set_route, handle_set_service_tier, handle_set_transport, handle_switch_openai_account,
    try_available_models_catalog,
};
use super::{
    AwaitMembersRuntime, ClientConnectionInfo, FileTouchService, SessionControlHandle, SessionInterruptQueues,
    SharedContext, SwarmEvent, SwarmMember, SwarmMutationRuntime, VersionedPlan, format_structured_completion_report,
    register_session_interrupt_queue, send_swarm_plan_to_session, truncate_detail, update_member_status,
    update_member_status_with_report, update_member_status_with_report_tldr,
};
use crate::agent::Agent;
use crate::bus::{Bus, BusEvent};
use crate::id;
use crate::protocol::{Request, ServerEvent, decode_request, encode_event, encode_event_stamped};
use crate::provider::Provider;
use crate::tool::Registry;
use crate::transport::Stream;
use anyhow::Result;
use futures::FutureExt;
use horon_agent_agent_runtime::{InterruptSignal, SoftInterruptSource, StreamError};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};

type SessionAgents = Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>>;
type ChannelSubscriptions = Arc<RwLock<HashMap<String, HashMap<String, HashSet<String>>>>>;
const RELOAD_STARTING_GUARD_MAX_AGE: Duration = Duration::from_secs(30);
const REQUEST_HANDLER_STALL_THRESHOLDS_MS: [u64; 3] = [2_000, 10_000, 60_000];

#[path = "client_subscribe_context.rs"]
mod subscribe_context;
use subscribe_context::*;

struct ProcessingMessage {
    id: u64,
    content: String,
    images: Vec<(String, String)>,
    system_reminder: Option<String>,
    active_skill: Option<String>,
    /// Present when the sending connection negotiated ACP v2.
    v2: Option<AcpV2Message>,
}

const V2_MESSAGE_QUEUED: u8 = 0;
const V2_MESSAGE_CLAIMED: u8 = 1;
const V2_MESSAGE_WITHDRAWN: u8 = 2;

/// Who got to a v2 `message` request first: its turn, which claims it once it
/// holds the agent and then stores it, or a withdrawal, which drops it while it
/// still waits. Exactly one of the two wins.
#[derive(Clone)]
struct AcpV2Message {
    request_id: u64,
    state: Arc<std::sync::atomic::AtomicU8>,
    /// The idle report of the turn this message starts, set when its task is
    /// spawned, so a cancel can hold it before the turn even runs.
    idle: Arc<std::sync::OnceLock<super::foreground_state::IdleHandle>>,
}

impl AcpV2Message {
    fn new(request_id: u64) -> Self {
        Self {
            request_id,
            state: Arc::new(std::sync::atomic::AtomicU8::new(V2_MESSAGE_QUEUED)),
            idle: Arc::new(std::sync::OnceLock::new()),
        }
    }

    fn defer_idle(&self) -> Option<super::foreground_state::DeferredIdle> {
        self.idle.get().and_then(super::foreground_state::IdleHandle::defer)
    }

    fn claim(&self) -> bool {
        self.state
            .compare_exchange(
                V2_MESSAGE_QUEUED,
                V2_MESSAGE_CLAIMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn withdraw(&self) -> bool {
        self.state
            .compare_exchange(
                V2_MESSAGE_QUEUED,
                V2_MESSAGE_WITHDRAWN,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

/// How a spawned processing task got on with its message.
enum TurnBegin {
    Ran {
        result: Result<()>,
        start_message_index: usize,
        pending_queue: horon_agent_agent_runtime::SoftInterruptQueue,
    },
    /// A withdrawal won; the withdrawing request cleans up.
    Withdrawn,
    /// Refused before anything was stored.
    Rejected(String),
}

struct ProcessingState<'a> {
    client_is_processing: &'a mut bool,
    message_id: &'a mut Option<u64>,
    session_id: &'a mut Option<String>,
    task: &'a mut Option<tokio::task::JoinHandle<()>>,
    /// Set by this connection's turn task once it holds the agent lock. A
    /// cancel reads it to tell a turn it can stop from one still waiting for
    /// the lock, which has nothing to stop cooperatively.
    turn_acquired: &'a Arc<AtomicBool>,
}

struct SwarmStatusRefs<'a> {
    members: &'a Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: &'a Arc<RwLock<HashMap<String, HashSet<String>>>>,
    event_history: &'a Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    event_counter: &'a Arc<std::sync::atomic::AtomicU64>,
    event_tx: &'a broadcast::Sender<SwarmEvent>,
}

fn should_start_idle_soft_interrupt(
    client_is_processing: bool,
    active_turn_registered: bool,
    session_connection_busy: bool,
) -> bool {
    !client_is_processing && !active_turn_registered && !session_connection_busy
}

struct RequestHandlerWatchdog {
    done: Arc<AtomicBool>,
}

struct RequestHandlerWatchdogContext {
    request_id: u64,
    request_kind: String,
    client_session_id: String,
    client_connection_id: String,
    client_instance_id: Option<String>,
    client_is_processing: bool,
    message_id: Option<u64>,
    processing_session_id: Option<String>,
    line_bytes: usize,
    lifecycle_logged: bool,
}

impl RequestHandlerWatchdog {
    fn spawn(ctx: RequestHandlerWatchdogContext) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let done_for_task = Arc::clone(&done);
        tokio::spawn(async move {
            let started = Instant::now();
            let mut previous_threshold = Duration::ZERO;
            for threshold_ms in REQUEST_HANDLER_STALL_THRESHOLDS_MS {
                let threshold = Duration::from_millis(threshold_ms);
                tokio::time::sleep(threshold.saturating_sub(previous_threshold)).await;
                previous_threshold = threshold;
                if done_for_task.load(Ordering::Acquire) {
                    return;
                }
                crate::logging::event_warn(
                    "SERVER_REQUEST_HANDLER_STALLED",
                    vec![
                        ("request_id", ctx.request_id.to_string()),
                        ("request_kind", ctx.request_kind.clone()),
                        ("session_id", ctx.client_session_id.clone()),
                        ("client_connection_id", ctx.client_connection_id.clone()),
                        (
                            "client_instance_id",
                            ctx.client_instance_id.clone().unwrap_or_else(|| "none".to_string()),
                        ),
                        ("client_processing", ctx.client_is_processing.to_string()),
                        (
                            "message_id",
                            ctx.message_id
                                .map(|id| id.to_string())
                                .unwrap_or_else(|| "none".to_string()),
                        ),
                        (
                            "processing_session_id",
                            ctx.processing_session_id.clone().unwrap_or_else(|| "none".to_string()),
                        ),
                        ("line_bytes", ctx.line_bytes.to_string()),
                        ("lifecycle_logged", ctx.lifecycle_logged.to_string()),
                        ("threshold_ms", threshold_ms.to_string()),
                        ("elapsed_ms", started.elapsed().as_millis().to_string()),
                    ],
                );
            }
        });
        Self { done }
    }
}

impl Drop for RequestHandlerWatchdog {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
    }
}

fn log_request_lifecycle_handled(
    fields: ServerRequestLifecycleFields<'_>,
    request_lifecycle_start: Instant,
    request_decoded_at: Instant,
) {
    let mut fields = server_request_lifecycle_fields(fields);
    fields.push((
        "handler_total_ms".to_string(),
        request_lifecycle_start.elapsed().as_millis().to_string(),
    ));
    fields.push((
        "since_decode_ms".to_string(),
        request_decoded_at.elapsed().as_millis().to_string(),
    ));
    crate::logging::event_info("SERVER_REQUEST_LIFECYCLE", fields);
}

/// How long a request may wait for the agent lock when no turn is registered.
///
/// Background housekeeping holds the lock for microseconds - the resume-time
/// model prefetch takes it once to snapshot the catalog and once to apply the
/// refresh - so a request that arrives inside that window must not be failed.
/// The bound stays below the first request-handler stall warning (2 s).
const AGENT_LOCK_BACKGROUND_WAIT: Duration = Duration::from_millis(500);

/// Reject a request arm that mutates session state while the session has a
/// running turn, answering with the "Cannot handle ... busy" error clients
/// already understand.
///
/// The decision reads real turn state: `client_is_processing` for this
/// connection plus the process-wide active-turn registry, which also covers
/// server-initiated and resumed turns. It deliberately does not read
/// `agent.try_lock()`, because a transient hold from background work made
/// archive, rename and save answer "busy" while nothing was running - ACP
/// `session/delete` hit that right after the subscribe `Done`, since the resume
/// path spawns the model prefetch.
///
/// A lock held while no turn is registered may still be guarding a turn:
/// another connection's turn, or a daemon-initiated one, holds it before it
/// registers and again in the tail after it unregisters. So a hold only passes
/// when it frees inside AGENT_LOCK_BACKGROUND_WAIT; a longer one gets the same
/// busy answer, because parking the sequential request loop on the agent mutex
/// would also stop that loop from reading Cancel.
async fn reject_if_agent_busy_for_request(
    request_id: u64,
    request_kind: &'static str,
    client_session_id: &str,
    client_is_processing: bool,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) -> bool {
    if client_is_processing || crate::turn_cancel_registry::has_active_turn(client_session_id) {
        send_agent_busy_error(
            request_id,
            request_kind,
            client_session_id,
            client_is_processing,
            client_event_tx,
        );
        return true;
    }

    if agent.try_lock().is_ok() {
        return false;
    }

    let waited = Instant::now();
    match tokio::time::timeout(AGENT_LOCK_BACKGROUND_WAIT, agent.lock()).await {
        Ok(_) => false,
        Err(_) => {
            let still_processing =
                client_is_processing || crate::turn_cancel_registry::has_active_turn(client_session_id);
            crate::logging::event_warn(
                "SERVER_REQUEST_AGENT_LOCK_OUTLIVED_WAIT",
                vec![
                    ("request_id", request_id.to_string()),
                    ("request_kind", request_kind.to_string()),
                    ("session_id", client_session_id.to_string()),
                    ("client_processing", still_processing.to_string()),
                    ("waited_ms", waited.elapsed().as_millis().to_string()),
                ],
            );
            send_agent_busy_error(
                request_id,
                request_kind,
                client_session_id,
                still_processing,
                client_event_tx,
            );
            true
        }
    }
}

fn send_agent_busy_error(
    request_id: u64,
    request_kind: &'static str,
    client_session_id: &str,
    client_is_processing: bool,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    crate::logging::event_warn(
        "SERVER_REQUEST_BUSY_AGENT_REJECTED",
        vec![
            ("request_id", request_id.to_string()),
            ("request_kind", request_kind.to_string()),
            ("session_id", client_session_id.to_string()),
            ("client_processing", client_is_processing.to_string()),
            ("reason", "agent_busy".to_string()),
        ],
    );
    let _ = client_event_tx.send(ServerEvent::Error {
        id: request_id,
        message: format!(
            "Cannot handle {request_kind} while the session is busy. Try again after the current turn finishes."
        ),
        retry_after_secs: Some(1),
    });
}

fn server_reload_starting() -> bool {
    matches!(
        crate::server::recent_reload_state(RELOAD_STARTING_GUARD_MAX_AGE),
        Some(state) if state.phase == crate::server::ReloadPhase::Starting
    )
}

fn compaction_server_event(event: crate::compaction::CompactionEvent) -> ServerEvent {
    ServerEvent::Compaction {
        trigger: event.trigger,
        pre_tokens: event.pre_tokens,
        post_tokens: event.post_tokens,
        tokens_saved: event.tokens_saved,
        duration_ms: event.duration_ms,
        messages_dropped: event.messages_dropped,
        messages_compacted: event.messages_compacted,
        summary_chars: event.summary_chars,
        active_messages: event.active_messages,
    }
}

async fn poll_agent_compaction_completion(agent: Arc<Mutex<Agent>>) -> Option<ServerEvent> {
    let mut agent_guard = agent.lock().await;
    agent_guard
        .poll_compaction_completion_event()
        .map(compaction_server_event)
}

async fn refresh_session_control_handle(
    session_id: &str,
    agent: &Arc<Mutex<Agent>>,
    shutdown_signals: &Arc<RwLock<HashMap<String, InterruptSignal>>>,
    soft_interrupt_queues: &SessionInterruptQueues,
) -> SessionControlHandle {
    let started = Instant::now();
    let agent_guard = match agent.try_lock() {
        Ok(agent_guard) => agent_guard,
        Err(_) => {
            crate::logging::warn(&format!(
                "refresh_session_control_handle: waiting for busy agent lock for session {}; cancel/control requests on this connection may be delayed",
                session_id
            ));
            let fallback_stop_signal = shutdown_signals.read().await.get(session_id).cloned();
            let fallback_soft_interrupt_queue = soft_interrupt_queues.read().await.get(session_id).cloned();
            if let Some(soft_interrupt_queue) = fallback_soft_interrupt_queue {
                // A missing shutdown-signal registration (e.g. a session created
                // through the headless spawn path) must not force this connection
                // to block on the busy agent mutex: cancels fired through the
                // handle fan out to the running turn's own signal via the
                // turn-cancel registry (issue #428), so a detached signal is a
                // safe stand-in.
                let stop_signal = fallback_stop_signal.unwrap_or_else(|| {
                    crate::logging::warn(&format!(
                        "refresh_session_control_handle: no registered shutdown signal for busy session {}; using detached signal (cancels reach the running turn via the turn cancel registry)",
                        session_id
                    ));
                    InterruptSignal::new()
                });
                crate::logging::warn(&format!(
                    "refresh_session_control_handle: using lock-free cancel-only control handle for busy session {} after {}ms",
                    session_id,
                    started.elapsed().as_millis()
                ));
                return SessionControlHandle::cancel_only(session_id, soft_interrupt_queue, stop_signal);
            }
            let agent_guard = agent.lock().await;
            crate::logging::warn(&format!(
                "refresh_session_control_handle: acquired agent lock for session {} after {}ms",
                session_id,
                started.elapsed().as_millis()
            ));
            agent_guard
        }
    };
    SessionControlHandle::new(
        session_id,
        agent_guard.soft_interrupt_queue(),
        agent_guard.background_tool_signal(),
        agent_guard.graceful_shutdown_signal(),
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "client lifecycle wiring spans sessions, swarm state, file state, channels, debug, and runtime coordination"
)]
pub(super) async fn handle_client(
    stream: Stream,
    sessions: SessionAgents,
    _global_event_tx: broadcast::Sender<ServerEvent>,
    provider_template: Arc<dyn Provider>,
    _global_is_processing: Arc<RwLock<bool>>,
    global_session_id: Arc<RwLock<String>>,
    client_count: Arc<RwLock<usize>>,
    client_connections: Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    swarm_members: Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    shared_context: Arc<RwLock<HashMap<String, HashMap<String, SharedContext>>>>,
    swarm_plans: Arc<RwLock<HashMap<String, VersionedPlan>>>,
    swarm_coordinators: Arc<RwLock<HashMap<String, String>>>,
    file_touch: FileTouchService,
    channel_subscriptions: ChannelSubscriptions,
    channel_subscriptions_by_session: ChannelSubscriptions,
    event_history: Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    event_counter: Arc<std::sync::atomic::AtomicU64>,
    swarm_event_tx: broadcast::Sender<SwarmEvent>,
    server_name: String,
    server_icon: String,
    mcp_pool: Arc<crate::mcp::SharedMcpPool>,
    shutdown_signals: Arc<RwLock<HashMap<String, InterruptSignal>>>,
    soft_interrupt_queues: SessionInterruptQueues,
    await_members_runtime: AwaitMembersRuntime,
    swarm_mutation_runtime: SwarmMutationRuntime,
    idle_reconnect_grace: Duration,
) -> Result<()> {
    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let writer = Arc::new(Mutex::new(writer));
    let mut line = String::new();

    let mut initial_request = loop {
        line.clear();
        let n = match reader.read_line(&mut line).await {
            Ok(n) => n,
            Err(error) => {
                crate::logging::error(&format!("Client read error before initialization: {}", error));
                return Ok(());
            }
        };
        if n == 0 {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }

        match decode_request(&line) {
            Ok(request) => {
                if request.is_lightweight_control_request() {
                    let keep_connection_open = matches!(request, Request::Ping { .. });
                    handle_lightweight_control_request(
                        request,
                        Arc::clone(&writer),
                        LightweightControlContext {
                            sessions: &sessions,
                            global_session_id: &global_session_id,
                            provider_template: &provider_template,
                            swarm_members: &swarm_members,
                            swarms_by_id: &swarms_by_id,
                            shared_context: &shared_context,
                            swarm_plans: &swarm_plans,
                            swarm_coordinators: &swarm_coordinators,
                            file_touch: &file_touch,
                            channel_subscriptions: &channel_subscriptions,
                            channel_subscriptions_by_session: &channel_subscriptions_by_session,
                            client_connections: &client_connections,
                            event_history: &event_history,
                            event_counter: &event_counter,
                            swarm_event_tx: &swarm_event_tx,
                            mcp_pool: &mcp_pool,
                            soft_interrupt_queues: &soft_interrupt_queues,
                            await_members_runtime: &await_members_runtime,
                            swarm_mutation_runtime: &swarm_mutation_runtime,
                        },
                    )
                    .await?;
                    // Native SSH probes daemon capability before sending its
                    // Subscribe on this same stream. Ping must not consume the
                    // connection, unlike the other one-shot control requests.
                    if keep_connection_open {
                        continue;
                    }
                    return Ok(());
                }
                break request;
            }
            Err(error) => {
                write_direct_event(
                    &writer,
                    &ServerEvent::Error {
                        id: 0,
                        message: format!("Invalid request: {}", error),
                        retry_after_secs: None,
                    },
                )
                .await?;
            }
        }
    };

    let initial_working_dir =
        match resolve_target_subscribe_working_dir(&mut initial_request, &sessions, &swarm_members)
            .await
            .and_then(|()| initial_subscribe_working_dir(&initial_request))
        {
            Ok(working_dir) => working_dir,
            Err(message) => {
                write_direct_event(
                    &writer,
                    &ServerEvent::Error {
                        id: initial_request.id(),
                        message,
                        retry_after_secs: None,
                    },
                )
                .await?;
                return Ok(());
            }
        };
    let mut active_terminal_env = initial_subscribe_terminal_env(&initial_request);

    // Per-client state
    let mut client_is_processing = false;
    let (processing_done_tx, mut processing_done_rx) = mpsc::unbounded_channel::<(u64, Result<()>, Option<String>)>();
    let mut processing_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut processing_message_id: Option<u64> = None;
    let mut processing_session_id: Option<String> = None;
    // Set by a turn task once it holds the agent; reset whenever one is
    // spawned. A cancel of a task that never got there aborts it at once.
    let turn_acquired = Arc::new(AtomicBool::new(false));
    let mut current_client_instance_id: Option<String> = None;
    let mut continue_on_disconnect = false;
    let mut model_usage_updates_enabled = false;
    let mut supports_user_response = false;
    let mut supports_ask_requests = false;
    let mut ask_installation: Option<(String, u64)> = None;
    let mut acp_installation: Option<(String, u64)> = None;
    // Set by a Subscribe whose ACP options negotiated protocol v2. The event
    // forwarder reads it for every event, so a re-subscribe that drops v2 stops
    // the v2-only events at once.
    let acp_v2 = Arc::new(AtomicBool::new(false));
    // The latest v2 `message` request, kept after its turn ends so a late
    // withdrawal is answered "already inserted" rather than "unknown".
    let mut last_v2_message: Option<AcpV2Message> = None;
    let mut client_bridge_installation: Option<(String, u64)> = None;
    let mut permission_bridge_installation: Option<(String, u64)> = None;
    let mut approval_override_installation: Option<(String, u64)> = None;
    // The ACP editor's own MCP servers for the attached session. Owned by this
    // connection; shut down on disconnect, re-subscribe or a new server list.
    let mut session_mcp: Option<crate::tool::session_mcp::SessionMcp> = None;
    // Working directory of the last accepted Subscribe; editor servers fall
    // back to it when the session agent is busy at configure time.
    let mut subscribed_working_dir: Option<std::path::PathBuf> = None;
    let client_start = std::time::Instant::now();

    let provider = provider_template.fork_for_new_session();
    let provider_fork_ms = client_start.elapsed().as_millis();
    let t0 = std::time::Instant::now();
    let registry = Registry::new(provider.clone()).await;
    let registry_ms = t0.elapsed().as_millis();

    let mut swarm_enabled = crate::config::config().features.swarm;
    let mut last_available_models_snapshot: Option<Arc<str>> = None;

    // Create a new session for this client
    let t0 = std::time::Instant::now();
    let mut new_agent = crate::hooks::with_client_terminal_env(active_terminal_env.clone(), async {
        Agent::new_provisional_with_initial_working_dir(
            Arc::clone(&provider),
            // Deliberately the plain clone: this builds a NEW session, whose
            // constructor seeds the clone's own `CompactionManager` from that
            // session and this provider's window.
            registry.clone(),
            Some(&initial_working_dir),
        )
    })
    .await;
    let agent_new_ms = t0.elapsed().as_millis();

    new_agent.set_memory_enabled(crate::config::config().features.memory);
    new_agent.set_persistent_runtime(true);
    // The provider warmup is NOT awaited here. Attaching must not wait on
    // speculative network work; it is started below as one owned background
    // task once the session exists, and cancelled by the first real request,
    // by a route/model change, or by the connection ending.
    crate::logging::info(&format!(
        "[TIMING] handle_client setup: provider_fork={provider_fork_ms}ms, registry={registry_ms}ms, agent_new={agent_new_ms}ms, prewarm=deferred, total={}ms",
        client_start.elapsed().as_millis()
    ));
    let mut client_session_id = new_agent.session_id().to_string();
    let friendly_name = new_agent.session_short_name().map(|s| s.to_string());
    let client_connection_id = id::new_id("conn");
    let connected_at = Instant::now();
    let (disconnect_tx, mut disconnect_rx) = mpsc::unbounded_channel::<()>();

    {
        let mut connections = client_connections.write().await;
        connections.insert(
            client_connection_id.clone(),
            ClientConnectionInfo {
                client_id: client_connection_id.clone(),
                session_id: client_session_id.clone(),
                client_instance_id: None,
                connected_at,
                last_seen: connected_at,
                is_processing: false,
                current_tool_name: None,
                terminal_env: active_terminal_env.clone(),
                disconnect_tx: disconnect_tx.clone(),
            },
        );
    }

    {
        let mut current = global_session_id.write().await;
        if current.is_empty() || *current != client_session_id {
            *current = client_session_id.clone();
        }
    }

    // Get lock-free control-plane handles BEFORE wrapping in Mutex.
    // This allows cancel/soft-interrupt/background-tool requests while the agent is processing.
    let mut session_control = SessionControlHandle::new(
        client_session_id.clone(),
        new_agent.soft_interrupt_queue(),
        new_agent.background_tool_signal(),
        new_agent.graceful_shutdown_signal(),
    );

    // Register the shutdown signal in the server-level map so
    // graceful_shutdown_sessions can signal it without locking the agent mutex
    {
        let mut signals = shutdown_signals.write().await;
        signals.insert(client_session_id.clone(), session_control.stop_current_turn_signal());
    }
    register_session_interrupt_queue(
        &soft_interrupt_queues,
        &client_session_id,
        new_agent.soft_interrupt_queue(),
    )
    .await;

    let mut agent = Arc::new(Mutex::new(new_agent));
    {
        let mut sessions_guard = sessions.write().await;
        sessions_guard.insert(client_session_id.clone(), Arc::clone(&agent));
    }

    // One owned background warmup, started only now that the session exists and
    // its route/model are settled. The session lock is taken just long enough to
    // snapshot the tools and the static prompt, then released BEFORE the provider
    // is awaited: a request that arrives during the warmup must never queue behind
    // it. The handle is aborted by the first real request, by a route/model change,
    // or when this connection ends.
    let mut prewarm_task = initial_prewarm_task(&initial_request, &sessions, &agent).await;
    crate::runtime_memory_log::emit_event(
        crate::runtime_memory_log::RuntimeMemoryLogEvent::new("session_created", "new_live_session_attached")
            .with_session_id(client_session_id.clone())
            .force_attribution(),
    );

    // Per-client event channel (not shared with other clients)
    let _sdk_connection_guard = crate::tool::sdk::ConnectionGuard(client_connection_id.clone());
    let (client_event_tx, mut client_event_rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
    // A flush request is answered once every event queued before it was written
    // (see `flush_client_events`).
    let (client_event_flush_tx, mut client_event_flush_rx) =
        tokio::sync::mpsc::unbounded_channel::<tokio::sync::oneshot::Sender<()>>();

    // Spawn event forwarder for this client only
    let writer_clone = Arc::clone(&writer);
    let client_connection_id_for_events = client_connection_id.clone();
    let client_connections_for_events = Arc::clone(&client_connections);
    let client_session_id_for_events = client_session_id.clone();
    let acp_v2_for_events = Arc::clone(&acp_v2);
    let event_handle = tokio::spawn(async move {
        let mut flushed = Vec::new();
        loop {
            let event = match client_event_rx.try_recv() {
                Ok(event) => event,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    // The queue is drained: every event sent before a flush
                    // request we hold has been written.
                    for written in flushed.drain(..) {
                        let _ = tokio::sync::oneshot::Sender::send(written, ());
                    }
                    tokio::select! {
                        biased;
                        event = client_event_rx.recv() => match event {
                            Some(event) => event,
                            None => break,
                        },
                        Some(written) = client_event_flush_rx.recv() => {
                            // Events sent before this request are visible now;
                            // the next pass writes them before answering it.
                            flushed.push(written);
                            continue;
                        }
                    }
                }
            };
            // Turn state, insertion and tool exits reach every attachment through
            // the session fanout; only a connection that negotiated ACP v2 has
            // asked for them.
            if super::foreground_state::is_v2_only(&event) && !acp_v2_for_events.load(Ordering::Acquire) {
                continue;
            }
            {
                let mut connections = client_connections_for_events.write().await;
                if let Some(info) = connections.get_mut(&client_connection_id_for_events) {
                    match &event {
                        ServerEvent::ToolStart { name, .. } => {
                            info.is_processing = true;
                            info.current_tool_name = Some(name.clone());
                        }
                        ServerEvent::ToolDone { .. } => {
                            info.current_tool_name = None;
                        }
                        ServerEvent::Done { .. } | ServerEvent::Error { .. } | ServerEvent::Interrupted => {
                            info.is_processing = false;
                            info.current_tool_name = None;
                        }
                        _ => {}
                    }
                }
            }
            let json = {
                // The delivery boundary is what a client observes, so every
                // event written to this connection carries the owner's next
                // sequence number. A client can then prove that everything
                // after the `seq` in its snapshot is new, without inferring
                // order from arrival.
                let seq = super::session_observation::stamp_event_delivery(&client_session_id_for_events);
                encode_event_stamped(&event, seq)
            };
            let mut w = writer_clone.lock().await;
            if let Err(error) = w.write_all(json.as_bytes()).await {
                // A broken pipe here is routine (client reload/disconnect mid
                // broadcast), so keep the line short: a full Debug dump of e.g.
                // a SwarmStatus event prints every member and floods the log.
                let event_desc = crate::logging::truncate_for_log(&format!("{:?}", event), 200);
                crate::logging::warn(&format!(
                    "event_forwarder write failed for connection {} while sending {}: {}",
                    client_connection_id_for_events, event_desc, error
                ));
                break;
            }
        }
    });

    // Note: Don't send initial SessionId here - it's sent by the Subscribe handler
    // Sending it via the channel causes race conditions where it can arrive after
    // other events (like History) that are written directly to the socket.

    let stdin_responses: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<String>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Subscribe to bus events so we can forward ModelsUpdated to this client
    // (e.g. when Copilot finishes async init after the initial History was sent)
    let mut bus_rx = Bus::global().subscribe();

    // Set up stdin request forwarding: tools send StdinInputRequest, we forward to TUI
    let (stdin_req_tx, mut stdin_req_rx) = tokio::sync::mpsc::unbounded_channel::<crate::tool::StdinInputRequest>();
    {
        let mut agent_guard = agent.lock().await;
        agent_guard.set_stdin_request_tx(stdin_req_tx.clone());
    }
    let stdin_forwarder = {
        let client_event_tx = client_event_tx.clone();
        let stdin_responses = stdin_responses.clone();
        let tool_call_id = String::new();
        tokio::spawn(async move {
            while let Some(req) = stdin_req_rx.recv().await {
                let request_id = req.request_id.clone();
                stdin_responses.lock().await.insert(request_id.clone(), req.response_tx);
                let _ = client_event_tx.send(ServerEvent::StdinRequest {
                    request_id,
                    prompt: req.prompt,
                    is_password: req.is_password,
                    tool_call_id: tool_call_id.clone(),
                });
            }
        })
    };

    let (pending_asks, ask_req_tx, ask_forwarder) = spawn_ask_forwarder(client_event_tx.clone());

    // Do not drain global bus traffic until the client has completed its first
    // subscribe. Under heavy swarm file-activity load, ignored bus frames can
    // otherwise monopolize the select loop before the initial subscribe/read.
    let mut client_subscribed = false;
    let mut provisional_session = true;
    let mut pending_request = Some(initial_request);

    let connection_result: Result<()> = async {
    loop {
        let mut request = if let Some(request) = pending_request.take() {
            request
        } else {
            line.clear();
            tokio::select! {
            biased;
            // Prioritize direct client I/O so subscribe/ping/message requests do not get
            // starved behind noisy background bus traffic.
            n = reader.read_line(&mut line) => {
                let n = match n {
                    Ok(n) => n,
                    Err(e) => {
                        crate::logging::error(&format!("Client read error: {}", e));
                        break;
                    }
                };
                if n == 0 {
                    break; // Client disconnected
                }
                let mut connections = client_connections.write().await;
                if let Some(info) = connections.get_mut(&client_connection_id) {
                    info.last_seen = Instant::now();
                }
            }
            done = processing_done_rx.recv() => {
                if let Some((done_id, result, completion_report)) = done {
                    if Some(done_id) != processing_message_id {
                        crate::logging::warn(&format!(
                            "Done event id={} doesn't match processing_message_id={:?}, dropping",
                            done_id, processing_message_id
                        ));
                        continue;
                    }
                    crate::logging::info(&format!(
                        "Processing done for message id={}, result={}",
                        done_id,
                        if result.is_ok() { "ok" } else { "err" }
                    ));
                    processing_message_id = None;
                    processing_task = None;
                    client_is_processing = false;
                    {
                        let mut connections = client_connections.write().await;
                        if let Some(info) = connections.get_mut(&client_connection_id) {
                            info.is_processing = false;
                            info.current_tool_name = None;
                        }
                    }

                    let done_session = processing_session_id.take();
                    record_processing_completion(
                        done_session.as_deref(), result, completion_report,
                        &SwarmStatusRefs {
                            members: &swarm_members,
                            swarms_by_id: &swarms_by_id,
                            event_history: &event_history,
                            event_counter: &event_counter,
                            event_tx: &swarm_event_tx,
                        },
                    ).await;
                } else {
                    break;
                }
                continue;
            }
            disconnect_signal = disconnect_rx.recv() => {
                if disconnect_signal.is_some() {
                    crate::logging::info(&format!(
                        "Client connection {} was superseded; disconnecting old owner of session {}",
                        client_connection_id, client_session_id
                    ));
                    break;
                }
                continue;
            }
            // Forward bus events to this client
            bus_event = bus_rx.recv(), if client_subscribed => {
                match bus_event {
                    Ok(BusEvent::ModelUsageUpdated(route)) => {
                        if model_usage_updates_enabled {
                            let _ = client_event_tx.send(ServerEvent::ModelUsageUpdated { route });
                        }
                    }
                    Ok(BusEvent::ModelsUpdated) => {
                        let Some(catalog) = try_available_models_catalog(&agent) else {
                            continue;
                        };
                        // Equivalent clients share the snapshot, normalized key,
                        // and size/downgrade decision. Pointer equality makes an
                        // unchanged shared catalog a constant-time fast path.
                        if last_available_models_snapshot.as_ref().is_some_and(|previous| {
                            Arc::ptr_eq(previous, &catalog.dedup_key)
                                || previous.as_ref() == catalog.dedup_key.as_ref()
                        }) {
                            continue;
                        }
                        if let Some(event) = catalog.live_event() {
                            let _ = client_event_tx.send(event.clone());
                        } else {
                            crate::logging::warn(&format!(
                                "Skipping oversized catalog for connection {} ({} bytes)",
                                client_connection_id, catalog.encoded_len
                            ));
                        }
                        last_available_models_snapshot = Some(Arc::clone(&catalog.dedup_key));
                    }
                    Ok(BusEvent::BatchProgress(progress)) => {
                        if progress.session_id == client_session_id {
                            let _ = client_event_tx.send(ServerEvent::BatchProgress { progress });
                        }
                    }
                    Ok(BusEvent::CompactionFinished) => {
                        let agent = Arc::clone(&agent);
                        let tx = client_event_tx.clone();
                        tokio::spawn(async move {
                            if let Some(event) = poll_agent_compaction_completion(agent).await {
                                let _ = tx.send(event);
                            }
                        });
                    }
                    _ => {}
                }
                continue;
            }
            }

            match decode_request(&line) {
                Ok(r) => r,
                Err(e) => {
                    let event = ServerEvent::Error {
                        id: 0,
                        message: format!("Invalid request: {}", e),
                        retry_after_secs: None,
                    };
                    let json = encode_event(&event);
                    let mut w = writer.lock().await;
                    if w.write_all(json.as_bytes()).await.is_err() {
                        break;
                    }
                    continue;
                }
            }
        };
        let request_decoded_at = Instant::now();
        let request_id = request.id();
        let request_kind = request_type_from_line(&line);
        let request_lifecycle_logged = !request_type_is_read_only(&request_kind);
        let request_lifecycle_start = Instant::now();
        let _request_watchdog = RequestHandlerWatchdog::spawn(RequestHandlerWatchdogContext {
            request_id,
            request_kind: request_kind.clone(),
            client_session_id: client_session_id.clone(),
            client_connection_id: client_connection_id.clone(),
            client_instance_id: current_client_instance_id.clone(),
            client_is_processing,
            message_id: processing_message_id,
            processing_session_id: processing_session_id.clone(),
            line_bytes: line.len(),
            lifecycle_logged: request_lifecycle_logged,
        });
        if request_lifecycle_logged {
            let mut fields = server_request_lifecycle_fields(ServerRequestLifecycleFields {
                phase: "received",
                request_id,
                request_kind: &request_kind,
                client_session_id: &client_session_id,
                client_connection_id: &client_connection_id,
                client_instance_id: current_client_instance_id.as_deref(),
                client_is_processing,
                message_id: processing_message_id,
                processing_session_id: processing_session_id.as_deref(),
                line_bytes: line.len(),
            });
            fields.extend(request_payload_summary(&request_kind, &line));
            crate::logging::event_info("SERVER_REQUEST_LIFECYCLE", fields);
        }
        if let Some(fields) = interrupt_request_log_fields(
            &request,
            &client_session_id,
            client_is_processing,
            processing_message_id,
            processing_task.is_some(),
            line.len(),
        ) {
            crate::logging::info(&format!("SERVER_INTERRUPT_REQUEST_DECODED {}", fields));
        }

        // A cancellation request must never be gated on writing an Ack to the client.
        // The normal Ack path takes the shared outbound writer before dispatching the
        // request. During heavy streaming, history replay, or client-side backpressure,
        // that writer can be busy long enough that an already-decoded cancel would sit
        // behind outbound bytes instead of signalling the agent's lock-free cancel
        // handle. Queue the Ack through the event channel and signal cancellation first.
        if let Request::Cancel { id } = request {
            let ack_queued = client_event_tx.send(ServerEvent::Ack { id }).is_ok();
            crate::logging::info(&format!(
                "SERVER_INTERRUPT_CANCEL_PRE_ACK_DISPATCH id={} session={} ack_queued={} decoded_to_dispatch_ms={}",
                id,
                client_session_id,
                ack_queued,
                request_decoded_at.elapsed().as_millis()
            ));
            let cancel_dispatch_start = Instant::now();
            let queued_v2 = last_v2_message
                .clone()
                .filter(|message| processing_message_id == Some(message.request_id));
            cancel_processing_message(
                &mut ProcessingState {
                    client_is_processing: &mut client_is_processing,
                    message_id: &mut processing_message_id,
                    session_id: &mut processing_session_id,
                    task: &mut processing_task,
                    turn_acquired: &turn_acquired,
                },
                &session_control,
                &client_event_tx,
                &SwarmStatusRefs {
                    members: &swarm_members,
                    swarms_by_id: &swarms_by_id,
                    event_history: &event_history,
                    event_counter: &event_counter,
                    event_tx: &swarm_event_tx,
                },
                Some(id),
                Some(request_decoded_at),
                queued_v2.as_ref(),
            )
            .await;
            crate::logging::info(&format!(
                "SERVER_INTERRUPT_CANCEL_PRE_ACK_DONE id={} session={} dispatch_ms={} total_since_decode_ms={}",
                id,
                client_session_id,
                cancel_dispatch_start.elapsed().as_millis(),
                request_decoded_at.elapsed().as_millis()
            ));
            if !client_is_processing {
                let mut connections = client_connections.write().await;
                if let Some(info) = connections.get_mut(&client_connection_id) {
                    info.is_processing = false;
                    info.current_tool_name = None;
                }
            }
            if request_lifecycle_logged {
                log_request_lifecycle_handled(
                    ServerRequestLifecycleFields {
                        phase: "handled",
                        request_id,
                        request_kind: &request_kind,
                        client_session_id: &client_session_id,
                        client_connection_id: &client_connection_id,
                        client_instance_id: current_client_instance_id.as_deref(),
                        client_is_processing,
                        message_id: processing_message_id,
                        processing_session_id: processing_session_id.as_deref(),
                        line_bytes: line.len(),
                    },
                    request_lifecycle_start,
                    request_decoded_at,
                );
            }
            continue;
        }

        // SDK controls reply only after validation. Callback results must never
        // wait for the Agent mutex held by the turn awaiting that callback.
        if matches!(
            &request,
            Request::ConfigureTools { .. } | Request::ListTools { .. } | Request::ToolResult { .. } | Request::ClientResponse { .. }
                | Request::SetSessionApprovalMode { .. }
        ) {
            let id = request.id();
            let response: anyhow::Result<ServerEvent> = match request {
                // A refused mode must reach the client as the only reply, not
                // after an acknowledgment that reads as acceptance.
                Request::SetSessionApprovalMode { mode, .. } => {
                    let acp_attached = acp_installation
                        .as_ref()
                        .is_some_and(|(session, _)| session == &client_session_id);
                    let _ = client_event_tx.send(crate::tool::approval_override::handle_request(
                        id,
                        mode,
                        &client_session_id,
                        &client_connection_id,
                        acp_attached,
                        &mut approval_override_installation,
                    ));
                    continue;
                }
                Request::ToolResult { call_id, output, error, .. } => {
                    crate::tool::sdk::complete(&client_connection_id, &client_session_id, &call_id, output, error)
                        .map(|()| ServerEvent::Ack { id })
                }
                Request::ClientResponse {
                    request_id,
                    result,
                    error,
                    ..
                } => {
                    let answer = match (result, error) {
                        (_, Some(error)) => Err(error),
                        (Some(value), None) => Ok(value),
                        (None, None) => {
                            let _ = client_event_tx.send(ServerEvent::Error {
                                id,
                                message: "client response carried neither result nor error".to_string(),
                                retry_after_secs: None,
                            });
                            continue;
                        }
                    };
                    match crate::tool::client_bridge::bridge_for_connection(&client_session_id, &client_connection_id) {
                        Some(bridge) => bridge.complete(&request_id, answer).map(|()| ServerEvent::Ack { id }),
                        None => Err(anyhow::anyhow!(
                            "client request `{request_id}` is not served by this connection"
                        )),
                    }
                }
                Request::ConfigureTools { tools, .. } => {
                    if client_is_processing || crate::turn_cancel_registry::has_active_turn(&client_session_id) { Err(anyhow::anyhow!("Session is busy")) }
                    else if let Ok(mut locked) = agent.try_lock() {
                        let result = crate::tool::sdk::configure(locked.session_id(), &client_connection_id, tools, client_event_tx.clone());
                        if result.is_ok() {
                            locked.invalidate_sdk_tools();
                            // Queue the acknowledgment before releasing the session lock,
                            // so a second client cannot start a new-policy call first.
                            let _ = client_event_tx.send(ServerEvent::Ack { id });
                            continue;
                        }
                        result.map(|()| ServerEvent::Ack { id })
                    } else { Err(anyhow::anyhow!("Session is busy")) }
                }
                Request::ListTools { .. } => {
                    if let Ok(locked) = agent.try_lock() {
                        Ok(ServerEvent::Tools { id, tools: crate::tool::sdk::wire_definitions(locked.tool_definitions_for_debug().await) })
                    } else { Err(anyhow::anyhow!("Session is busy")) }
                }
                _ => unreachable!(),
            };
            let event = response.unwrap_or_else(|error| ServerEvent::Error { id, message: error.to_string(), retry_after_secs: None });
            let _ = client_event_tx.send(event);
            continue;
        }

        // Send ack
        let ack = ServerEvent::Ack { id: request.id() };
        let json = encode_event(&ack);
        {
            let ack_start = Instant::now();
            let mut w = writer.lock().await;
            if w.write_all(json.as_bytes()).await.is_err() {
                if request_lifecycle_logged {
                    let mut fields =
                        server_request_lifecycle_fields(ServerRequestLifecycleFields {
                            phase: "ack_write_failed",
                            request_id,
                            request_kind: &request_kind,
                            client_session_id: &client_session_id,
                            client_connection_id: &client_connection_id,
                            client_instance_id: current_client_instance_id.as_deref(),
                            client_is_processing,
                            message_id: processing_message_id,
                            processing_session_id: processing_session_id.as_deref(),
                            line_bytes: line.len(),
                        });
                    fields.push((
                        "ack_write_ms".to_string(),
                        ack_start.elapsed().as_millis().to_string(),
                    ));
                    crate::logging::event_warn("SERVER_REQUEST_LIFECYCLE", fields);
                }
                break;
            }
            if request_lifecycle_logged {
                let mut fields = server_request_lifecycle_fields(ServerRequestLifecycleFields {
                    phase: "acked",
                    request_id,
                    request_kind: &request_kind,
                    client_session_id: &client_session_id,
                    client_connection_id: &client_connection_id,
                    client_instance_id: current_client_instance_id.as_deref(),
                    client_is_processing,
                    message_id: processing_message_id,
                    processing_session_id: processing_session_id.as_deref(),
                    line_bytes: line.len(),
                });
                fields.push((
                    "ack_write_ms".to_string(),
                    ack_start.elapsed().as_millis().to_string(),
                ));
                fields.push((
                    "since_decode_ms".to_string(),
                    request_decoded_at.elapsed().as_millis().to_string(),
                ));
                crate::logging::event_info("SERVER_REQUEST_LIFECYCLE", fields);
            }
        }

        // Legacy/direct clients can send a prompt without Subscribe. Their
        // first session action commits ownership, but inspection/attach does not.
        if provisional_session
            && matches!(
                &request,
                Request::Message { .. }
                    | Request::SoftInterrupt { .. }
                    | Request::RunSubagent { .. }
            )
        {
            agent.lock().await.materialize_provisional_session();
            provisional_session = false;
        }

        if let Err(message) = resolve_target_subscribe_working_dir(
            &mut request, &sessions, &swarm_members,
        ).await {
            let _ = client_event_tx.send(ServerEvent::Error {
                id: request.id(), message, retry_after_secs: None,
            });
            continue;
        }
        let sdk_session_before_request = client_session_id.clone();
        match request {
            Request::ConfigureTools { .. } | Request::ListTools { .. } | Request::ToolResult { .. } => unreachable!("SDK controls dispatched before acknowledgment"),
            Request::ClientResponse { .. } => unreachable!("client responses dispatched before acknowledgment"),
            Request::Message {
                id,
                content,
                images,
                system_reminder,
                active_skill,
                no_reply,
            } => {
                if no_reply {
                    append_context_message(
                        id,
                        &content,
                        images,
                        &client_session_id,
                        client_is_processing,
                        &agent,
                        &client_event_tx,
                    )
                    .await;
                    continue;
                }
                if !client_is_processing {
                    // A live resume cannot replace stdin routing while the old
                    // turn owns the agent. Restore it when this client starts a
                    // later turn, without reviving any disconnected prompt.
                    if continue_on_disconnect && let Ok(mut agent) = agent.try_lock() {
                        agent.set_stdin_request_tx(stdin_req_tx.clone());
                    }
                    let mut connections = client_connections.write().await;
                    if let Some(info) = connections.get_mut(&client_connection_id) {
                        info.is_processing = true;
                        info.current_tool_name = None;
                    }
                }
                let peer_content = content.clone();
                // A real turn wins over the speculative warmup: abort it rather
                // than letting it race the request for the provider connection.
                if let Some(handle) = prewarm_task.take() {
                    handle.abort();
                }
                let v2_message = acp_v2.load(Ordering::Acquire).then(|| AcpV2Message::new(id));
                start_processing_message(
                    ProcessingMessage {
                        id,
                        content,
                        images,
                        system_reminder,
                        active_skill,
                        v2: v2_message.clone(),
                    },
                    &client_session_id,
                    Some(&client_connection_id),
                    &mut ProcessingState {
                        client_is_processing: &mut client_is_processing,
                        message_id: &mut processing_message_id,
                        session_id: &mut processing_session_id,
                        task: &mut processing_task,
                        turn_acquired: &turn_acquired,
                    },
                    &agent,
                    &client_event_tx,
                    &processing_done_tx,
                    active_terminal_env.clone(),
                    &SwarmStatusRefs {
                        members: &swarm_members,
                        swarms_by_id: &swarms_by_id,
                        event_history: &event_history,
                        event_counter: &event_counter,
                        event_tx: &swarm_event_tx,
                    },
                )
                .await;
                if processing_message_id == Some(id) && v2_message.is_some() {
                    last_v2_message = v2_message;
                }
                // Other windows attached to this session only see the turn's
                // stream events, never the message that started it. Tell them
                // so the prompt (or auto-poke notice) shows up everywhere.
                if processing_message_id == Some(id) && !peer_content.trim().is_empty() {
                    super::state::fanout_peer_client_event(
                        &swarm_members,
                        &client_session_id,
                        &client_connection_id,
                        ServerEvent::PeerUserMessage {
                            content: peer_content,
                        },
                    )
                    .await;
                }
            }

            Request::Cancel { id } => {
                let queued_v2 = last_v2_message
                    .clone()
                    .filter(|message| processing_message_id == Some(message.request_id));
                cancel_processing_message(
                    &mut ProcessingState {
                        client_is_processing: &mut client_is_processing,
                        message_id: &mut processing_message_id,
                        session_id: &mut processing_session_id,
                        task: &mut processing_task,
                        turn_acquired: &turn_acquired,
                    },
                    &session_control,
                    &client_event_tx,
                    &SwarmStatusRefs {
                        members: &swarm_members,
                        swarms_by_id: &swarms_by_id,
                        event_history: &event_history,
                        event_counter: &event_counter,
                        event_tx: &swarm_event_tx,
                    },
                    Some(id),
                    Some(request_decoded_at),
                    queued_v2.as_ref(),
                )
                .await;
                if !client_is_processing {
                    let mut connections = client_connections.write().await;
                    if let Some(info) = connections.get_mut(&client_connection_id) {
                        info.is_processing = false;
                        info.current_tool_name = None;
                    }
                }
            }

            Request::SoftInterrupt {
                id,
                content,
                images,
                urgent,
                client_id,
            } => {
                // A soft interrupt has somewhere to go only while a turn is
                // active. When the session is idle, queueing it would strand
                // the user's prompt until an unrelated future message starts
                // a turn. Claim the idle session and process it as the next
                // user message instead. The connection-map claim is atomic
                // with the cross-client busy check, so two attachments cannot
                // both decide that the same session is idle.
                let start_idle_turn = {
                    let mut connections = client_connections.write().await;
                    let active_turn_registered =
                        !crate::turn_cancel_registry::active_turn_signals(&client_session_id)
                            .is_empty();
                    let session_connection_busy = connections
                        .values()
                        .any(|info| info.session_id == client_session_id && info.is_processing);
                    let start = should_start_idle_soft_interrupt(
                        client_is_processing,
                        active_turn_registered,
                        session_connection_busy,
                    );
                    if start && let Some(info) = connections.get_mut(&client_connection_id) {
                        info.is_processing = true;
                    }
                    start
                };
                if start_idle_turn {
                    // The interrupt becomes the new turn's prompt, so the
                    // model sees it before anything else. A caller tracking it
                    // by id must still learn that it was picked up.
                    if let Some(client_id) = client_id {
                        let _ = client_event_tx.send(ServerEvent::SoftInterruptInjected {
                            content: content.clone(),
                            display_role: None,
                            point: "turn_start".to_string(),
                            tools_skipped: None,
                            client_ids: vec![client_id],
                        });
                    }
                    if let Some(handle) = prewarm_task.take() {
                        handle.abort();
                    }
                    start_processing_message(
                        ProcessingMessage {
                            id,
                            content,
                            images,
                            system_reminder: None,
                            active_skill: None,
                            v2: None,
                        },
                        &client_session_id,
                        Some(&client_connection_id),
                        &mut ProcessingState {
                            client_is_processing: &mut client_is_processing,
                            message_id: &mut processing_message_id,
                            session_id: &mut processing_session_id,
                            task: &mut processing_task,
                            turn_acquired: &turn_acquired,
                        },
                        &agent,
                        &client_event_tx,
                        &processing_done_tx,
                        active_terminal_env.clone(),
                        &SwarmStatusRefs {
                            members: &swarm_members,
                            swarms_by_id: &swarms_by_id,
                            event_history: &event_history,
                            event_counter: &event_counter,
                            event_tx: &swarm_event_tx,
                        },
                    )
                    .await;
                    if !client_is_processing {
                        let mut connections = client_connections.write().await;
                        if let Some(info) = connections.get_mut(&client_connection_id) {
                            info.is_processing = false;
                        }
                    }
                } else {
                    queue_soft_interrupt(
                        id,
                        content,
                        images,
                        urgent,
                        client_id,
                        &session_control,
                        &client_event_tx,
                    )
                    .await;
                }
            }

            Request::CancelSoftInterrupts { id, client_ids } => match client_ids {
                Some(client_ids) => {
                    cancel_soft_interrupts_by_id(
                        id,
                        &client_session_id,
                        &client_ids,
                        &session_control,
                        &client_event_tx,
                    )
                    .await
                }
                None => {
                    clear_soft_interrupts(id, &client_session_id, &session_control, &client_event_tx).await
                }
            },

            Request::BackgroundTool { id } => {
                move_tool_to_background(id, &session_control, &client_event_tx);
            }

            Request::Clear { id } => {
                if reject_if_agent_busy_for_request(
                    id,
                    "clear",
                    &client_session_id,
                    client_is_processing,
                    &agent,
                    &client_event_tx,
                ).await {
                    continue;
                }
                crate::hooks::with_client_terminal_env(
                    active_terminal_env.clone(),
                    handle_clear_session(
                        id,
                        &mut client_session_id,
                        &client_connection_id,
                        &agent,
                        &provider,
                        &registry,
                        &sessions,
                        &shutdown_signals,
                        &soft_interrupt_queues,
                        &client_connections,
                        &swarm_members,
                        &swarms_by_id,
                        &file_touch,
                        &channel_subscriptions,
                        &channel_subscriptions_by_session,
                        &swarm_plans,
                        &event_history,
                        &event_counter,
                        &swarm_event_tx,
                        &client_event_tx,
                    ),
                )
                .await;
                session_control = refresh_session_control_handle(
                    &client_session_id,
                    &agent,
                    &shutdown_signals,
                    &soft_interrupt_queues,
                )
                .await;
            }

            Request::Rewind { id, message_index } => {
                if client_is_processing {
                    let _ = client_event_tx.send(ServerEvent::Error {
                        id,
                        message: "Cannot rewind while a turn is processing.".to_string(),
                        retry_after_secs: None,
                    });
                    continue;
                }

                let rewind_result = {
                    let mut agent_guard = agent.lock().await;
                    agent_guard.rewind_to_message(message_index)
                };

                match rewind_result {
                    Ok(removed) => {
                        crate::logging::info(&format!(
                            "Rewound session {} to message {} (removed {})",
                            client_session_id, message_index, removed
                        ));
                        if handle_get_history(
                            id,
                            &client_session_id,
                            client_is_processing,
                            &agent,
                            &provider,
                            &sessions,
                            &client_connections,
                            &client_count,
                            &writer,
                            &server_name,
                            &server_icon,
                            None,
                            acp_v2.load(Ordering::Acquire),
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                        // The truncated History replaces the client transcript
                        // (dropping the inline plan graph); re-send the plan so
                        // the diagram comes back.
                        send_swarm_plan_to_session(
                            &client_session_id,
                            &swarm_members,
                            &swarm_plans,
                        )
                        .await;
                    }
                    Err(message) => {
                        let _ = client_event_tx.send(ServerEvent::Error {
                            id,
                            message,
                            retry_after_secs: None,
                        });
                    }
                }
            }

            Request::RewindUndo { id } => {
                if client_is_processing {
                    let _ = client_event_tx.send(ServerEvent::Error {
                        id,
                        message: "Cannot undo rewind while a turn is processing.".to_string(),
                        retry_after_secs: None,
                    });
                    continue;
                }

                let undo_result = {
                    let mut agent_guard = agent.lock().await;
                    agent_guard.undo_rewind()
                };

                match undo_result {
                    Ok(restored) => {
                        crate::logging::info(&format!(
                            "Undid rewind for session {} (restored {})",
                            client_session_id, restored
                        ));
                        if handle_get_history(
                            id,
                            &client_session_id,
                            client_is_processing,
                            &agent,
                            &provider,
                            &sessions,
                            &client_connections,
                            &client_count,
                            &writer,
                            &server_name,
                            &server_icon,
                            None,
                            acp_v2.load(Ordering::Acquire),
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                        // Same as rewind: restore the inline plan graph after
                        // the transcript replacement.
                        send_swarm_plan_to_session(
                            &client_session_id,
                            &swarm_members,
                            &swarm_plans,
                        )
                        .await;
                    }
                    Err(message) => {
                        let _ = client_event_tx.send(ServerEvent::Error {
                            id,
                            message,
                            retry_after_secs: None,
                        });
                    }
                }
            }

            Request::Ping { id } => {
                let event = super::session_connections::connection_presence_pong(id, &client_connections, &swarm_members).await;
                let json = encode_event(&event);
                let mut w = writer.lock().await;
                if w.write_all(json.as_bytes()).await.is_err() {
                    break;
                }
            }

            Request::PrepareDisconnect { id } => {
                drop(crate::tool::sdk::ConnectionGuard(client_connection_id.clone()));
                let json = encode_event(&ServerEvent::Done { id });
                let mut w = writer.lock().await;
                if w.write_all(json.as_bytes()).await.is_err() {
                    break;
                }
            }

            Request::GetState { id } => {
                if handle_get_state(
                    id,
                    &client_session_id,
                    client_is_processing,
                    session_control.pending_soft_interrupt_count(),
                    &sessions,
                    &writer,
                )
                .await
                .is_err()
                {
                    break;
                }
            }

            Request::Subscribe {
                id,
                system_prompt,
                supports_user_response: requested_user_response,
                supports_ask_requests: requested_ask_requests,
                working_dir: subscribe_working_dir,
                target_session_id,
                client_instance_id,
                client_has_local_history,
                allow_session_takeover,
                crash_on_disconnect: _,
                continue_on_disconnect: requested_continuation,
                terminal_env,
                acp: acp_options,
            } => {
                if let Err(message) =
                    validated_subscribe_working_dir(
                        subscribe_working_dir.as_deref(), requested_continuation,
                    )
                {
                    let _ = client_event_tx.send(ServerEvent::Error {
                        id,
                        message,
                        retry_after_secs: None,
                    });
                    continue;
                }
                // The editor's servers belong to the previous attachment: stop
                // them before the Subscribe reply, so a client that saw the
                // reply never races their processes.
                if let Some(previous) = session_mcp.take() {
                    previous.shutdown().await;
                }
                subscribed_working_dir = subscribe_working_dir.as_deref().map(std::path::PathBuf::from);
                // Overrides are creation-only. In particular, never apply one to
                // a target attachment or a repeated Subscribe on this connection.
                if let Some(prompt) = new_session_system_prompt(
                    provisional_session,
                    target_session_id.as_deref(),
                    system_prompt.as_deref(),
                ) {
                    agent.lock().await.set_system_prompt(prompt);
                }
                // Every Subscribe carries an authoritative snapshot. An empty
                // snapshot must clear terminal vars inherited by the daemon
                // rather than retaining a prior pane's values.
                continue_on_disconnect = requested_continuation;
                supports_user_response = requested_user_response;
                supports_ask_requests = requested_ask_requests;
                // Read before the attach below writes History, which carries
                // stored message ids only for a v2 connection.
                acp_v2.store(
                    acp_options.as_ref().is_some_and(|options| options.protocol_version == Some(2)),
                    Ordering::Release,
                );
                active_terminal_env = terminal_env;
                current_client_instance_id = client_instance_id.clone();
                {
                    let mut connections = client_connections.write().await;
                    if let Some(info) = connections.get_mut(&client_connection_id) {
                        info.client_instance_id = client_instance_id.clone();
                        info.terminal_env = active_terminal_env.clone();
                    }
                }
                if let Some(target_session_id) = target_session_id {
                    // A brand-new desktop panel has no transcript on disk until
                    // its first prompt. Its creator connection can detach before
                    // the panel connection arrives, while the live agent is
                    // already registered in memory. Treat that as an existing
                    // session or the target-aware subscribe silently creates a
                    // different session and every subsequent command reports a
                    // wrong-session attachment.
                    if crate::session::session_exists(&target_session_id)
                        || sessions.read().await.contains_key(&target_session_id)
                    {
                        let pre_resume_session_id = client_session_id.clone();
                        let was_warm = settle_prewarm(&mut prewarm_task).await;
                        crate::hooks::with_client_terminal_env(
                            active_terminal_env.clone(),
                            handle_resume_session(
                                id,
                                target_session_id.clone(),
                                subscribe_working_dir.as_deref(),
                                client_instance_id.as_deref(),
                                client_has_local_history,
                                allow_session_takeover,
                                &mut client_session_id,
                                &client_connection_id,
                                &mut agent,
                                &provider,
                                &registry,
                                &sessions,
                                &shutdown_signals,
                                &soft_interrupt_queues,
                                &client_connections,
                                &swarm_members,
                                &swarms_by_id,
                                &file_touch,
                                &channel_subscriptions,
                                &channel_subscriptions_by_session,
                                &swarm_plans,
                                &swarm_coordinators,
                                &client_count,
                                &writer,
                                &server_name,
                                &server_icon,
                                &client_event_tx,
                                &mcp_pool,
                                &event_history,
                                &event_counter,
                                &swarm_event_tx,
                                acp_v2.load(Ordering::Acquire),
                            ),
                        )
                        .await?;
                        if was_warm && client_session_id == pre_resume_session_id {
                            prewarm_task = Some(spawn_deferred_prewarm(&agent));
                        }
                        session_control = refresh_session_control_handle(
                            &client_session_id,
                            &agent,
                            &shutdown_signals,
                            &soft_interrupt_queues,
                        )
                        .await;
                        if client_session_id == target_session_id {
                            handle_subscribe(
                                id,
                                subscribe_working_dir,
                                false,
                                &client_session_id,
                                &client_connection_id,
                                &friendly_name,
                                &agent,
                                &registry,
                                swarm_enabled,
                                &swarm_members,
                                &swarms_by_id,
                                &channel_subscriptions,
                                &channel_subscriptions_by_session,
                                &swarm_plans,
                                &swarm_coordinators,
                                &client_event_tx,
                                &mcp_pool,
                                &event_history,
                                &event_counter,
                                &swarm_event_tx,
                            )
                            .await;
                            if let Some(snapshot) = try_available_models_snapshot(&agent) {
                                last_available_models_snapshot = Some(snapshot);
                            }
                        } else {
                            // handle_resume_session already queued this
                            // Subscribe's refusal (the store's error, or the
                            // live-attach conflict). The cleanup after the loop
                            // aborts the forwarder, which dropped that frame:
                            // the client saw only the Ack and then nothing until
                            // the reconnect grace closed the socket.
                            flush_client_events(&client_event_flush_tx).await;
                            crate::logging::warn(&format!(
                                "Target-aware subscribe failed to bind {} from temporary {}; closing temporary client connection {}",
                                target_session_id, pre_resume_session_id, client_connection_id
                            ));
                            break;
                        }
                    } else {
                        if provisional_session {
                            agent.lock().await.materialize_provisional_session();
                        }
                        handle_subscribe(
                            id,
                            subscribe_working_dir,
                            true,
                            &client_session_id,
                            &client_connection_id,
                            &friendly_name,
                            &agent,
                            &registry,
                            swarm_enabled,
                            &swarm_members,
                            &swarms_by_id,
                            &channel_subscriptions,
                            &channel_subscriptions_by_session,
                            &swarm_plans,
                            &swarm_coordinators,
                            &client_event_tx,
                            &mcp_pool,
                            &event_history,
                            &event_counter,
                            &swarm_event_tx,
                        )
                        .await;
                    }
                } else {
                    if provisional_session {
                        agent.lock().await.materialize_provisional_session();
                    }
                    handle_subscribe(
                        id,
                        subscribe_working_dir,
                        true,
                        &client_session_id,
                        &client_connection_id,
                        &friendly_name,
                        &agent,
                        &registry,
                        swarm_enabled,
                        &swarm_members,
                        &swarms_by_id,
                        &channel_subscriptions,
                        &channel_subscriptions_by_session,
                        &swarm_plans,
                        &swarm_coordinators,
                        &client_event_tx,
                        &mcp_pool,
                        &event_history,
                        &event_counter,
                        &swarm_event_tx,
                    )
                    .await;
                    // No catalog dedup baseline here. A fresh subscribe sends
                    // no catalog to the client (History does, and GetHistory
                    // records the baseline right after). Building the snapshot
                    // here cost ~200ms on a cold server and sat directly in
                    // front of the client's GetHistory, delaying first paint.
                    last_available_models_snapshot = None;
                }
                // Queued after the attach's History and Done, on this
                // connection's ordered event path, and before anything below
                // that may wait for a busy agent: the snapshot reports that
                // turn, so it must not wait for the turn to end.
                if acp_v2.load(Ordering::Acquire) {
                    let _ = client_event_tx.send(super::foreground_state::snapshot(&client_session_id));
                }
                super::client_session::apply_or_defer_desktop_response_capability(
                    &agent,
                    supports_user_response,
                )
                .await;
                // The installation follows the session this connection is
                // attached to now, not the one it had before a re-subscribe.
                if let Some((session, token)) = ask_installation.take() {
                    crate::tool::ask_bridge::clear(&session, token);
                }
                if supports_ask_requests {
                    let token = crate::tool::ask_bridge::install(&client_session_id, ask_req_tx.clone());
                    ask_installation = Some((client_session_id.clone(), token));
                }
                // ACP connections own their bridges by token, so a re-subscribe
                // and a disconnect both withdraw exactly what they installed.
                if let Some((session, token)) = acp_installation.take() {
                    crate::tool::acp_session::clear(&session, token);
                }
                if let Some((session, token)) = client_bridge_installation.take() {
                    crate::tool::client_bridge::clear(&session, token);
                }
                if let Some((session, token)) = permission_bridge_installation.take() {
                    crate::tool::permission_bridge::clear(&session, token);
                }
                if let Some((session, token)) = approval_override_installation.take() {
                    crate::tool::approval_override::clear(&session, token);
                }
                if let Some(options) = acp_options {
                    let token = crate::tool::acp_session::install_with_version(
                        &client_session_id,
                        &client_connection_id,
                        client_event_tx.clone(),
                        options.protocol_version,
                    );
                    acp_installation = Some((client_session_id.clone(), token));
                    if crate::tool::client_bridge::serves_client_methods(&options.client_capabilities) {
                        let token = crate::tool::client_bridge::install(
                            &client_session_id,
                            &client_connection_id,
                            options.client_capabilities,
                            client_event_tx.clone(),
                        );
                        client_bridge_installation = Some((client_session_id.clone(), token));
                    }
                    if options.permission_requests {
                        let token = crate::tool::permission_bridge::install(
                            &client_session_id,
                            &client_connection_id,
                            client_event_tx.clone(),
                        );
                        permission_bridge_installation = Some((client_session_id.clone(), token));
                    }
                    // The directory list belongs to the session, not to this
                    // attachment: the context is captured with it once, and a
                    // later attachment that changes it queues one reminder.
                    agent
                        .lock()
                        .await
                        .set_additional_dirs(options.additional_dirs.clone());
                }
                client_subscribed = true;
                provisional_session = false;
            }

            Request::GetHistory { id } => {
                if handle_get_history(
                    id,
                    &client_session_id,
                    client_is_processing,
                    &agent,
                    &provider,
                    &sessions,
                    &client_connections,
                    &client_count,
                    &writer,
                    &server_name,
                    &server_icon,
                    None,
                    acp_v2.load(Ordering::Acquire),
                )
                .await
                .is_err()
                {
                    break;
                }
                // Follow the History payload with the current swarm plan: a
                // session-changing History clears the client's plan snapshot
                // (and the inline plan graph), so re-send it afterwards
                // instead of leaving the graph blank until the next plan
                // mutation broadcast.
                send_swarm_plan_to_session(&client_session_id, &swarm_members, &swarm_plans).await;
                if let Some(snapshot) = try_available_models_snapshot(&agent) {
                    last_available_models_snapshot = Some(snapshot);
                }
            }

            Request::GetModelCatalog { id, subscribe_usage_updates } => {
                model_usage_updates_enabled = subscribe_usage_updates;
                if handle_get_model_catalog(id, &client_session_id, &agent, &provider, &writer)
                    .await
                    .is_err()
                {
                    break;
                }
                if let Some(snapshot) = try_available_models_snapshot(&agent) {
                    last_available_models_snapshot = Some(snapshot);
                }
            }

            Request::GetCompactedHistory {
                id,
                visible_messages,
            } => {
                if handle_get_compacted_history(
                    id,
                    &client_session_id,
                    &agent,
                    &writer,
                    visible_messages,
                    acp_v2.load(Ordering::Acquire),
                )
                .await
                .is_err()
                {
                    break;
                }
            }

            Request::DebugCommand { id, .. } => {
                let _ = client_event_tx.send(ServerEvent::Error {
                    id,
                    message: "debug_command is only supported on the debug socket".to_string(),
                    retry_after_secs: None,
                });
            }

            Request::Reload { id, force } => {
                handle_reload(
                    id,
                    force,
                    &client_session_id,
                    &agent,
                    &swarm_members,
                    &client_event_tx,
                )
                .await;
            }

            Request::ResumeSession {
                id,
                session_id,
                client_instance_id,
                client_has_local_history,
                allow_session_takeover,
            } => {
                let pre_resume_session_id = client_session_id.clone();
                let resume_working_dir = {
                    let agent_guard = agent.lock().await;
                    agent_guard.working_dir().map(str::to_string)
                };
                current_client_instance_id = client_instance_id.clone();
                {
                    let mut connections = client_connections.write().await;
                    if let Some(info) = connections.get_mut(&client_connection_id) {
                        info.client_instance_id = client_instance_id.clone();
                    }
                }
                // Settle the warmup before the handler detaches this session.
                        let was_warm = settle_prewarm(&mut prewarm_task).await;
                crate::hooks::with_client_terminal_env(
                    active_terminal_env.clone(),
                    handle_resume_session(
                        id,
                        session_id,
                        resume_working_dir.as_deref(),
                        client_instance_id.as_deref(),
                        client_has_local_history,
                        allow_session_takeover,
                        &mut client_session_id,
                        &client_connection_id,
                        &mut agent,
                        &provider,
                        &registry,
                        &sessions,
                        &shutdown_signals,
                        &soft_interrupt_queues,
                        &client_connections,
                        &swarm_members,
                        &swarms_by_id,
                        &file_touch,
                        &channel_subscriptions,
                        &channel_subscriptions_by_session,
                        &swarm_plans,
                        &swarm_coordinators,
                        &client_count,
                        &writer,
                        &server_name,
                        &server_icon,
                        &client_event_tx,
                        &mcp_pool,
                        &event_history,
                        &event_counter,
                        &swarm_event_tx,
                        acp_v2.load(Ordering::Acquire),
                    ),
                )
                .await?;
                if client_session_id != pre_resume_session_id {
                    provisional_session = false;
                }
                if acp_v2.load(Ordering::Acquire) {
                    let _ = client_event_tx.send(super::foreground_state::snapshot(&client_session_id));
                }
                if was_warm && client_session_id == pre_resume_session_id {
                    prewarm_task = Some(spawn_deferred_prewarm(&agent));
                }
                super::client_session::apply_or_defer_desktop_response_capability(
                    &agent,
                    supports_user_response,
                )
                .await;
                session_control = refresh_session_control_handle(
                    &client_session_id,
                    &agent,
                    &shutdown_signals,
                    &soft_interrupt_queues,
                )
                .await;
                if let Some(snapshot) = try_available_models_snapshot(&agent) {
                    last_available_models_snapshot = Some(snapshot);
                }
            }

            Request::ResumeAllSessions { id } => {
                super::client_actions::handle_resume_all_sessions(
                    id,
                    &sessions,
                    &swarm_members,
                    &swarms_by_id,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &client_event_tx,
                )
                .await;
            }

            Request::CycleModel { id, direction } => {
                if let Some(handle) = prewarm_task.take() {
                    handle.abort();
                }
                handle_cycle_model(id, direction, &agent, &client_event_tx).await;
                prewarm_task = Some(spawn_deferred_prewarm(&agent));
            }

            Request::RefreshModels { id } => {
                handle_refresh_models(id, &provider, &agent, &client_event_tx).await;
            }

            Request::SetPremiumMode { id, mode } => {
                handle_set_premium_mode(id, mode, &agent, &client_event_tx).await;
            }

            Request::SetModel { id, model } => {
                if let Some(handle) = prewarm_task.take() {
                    handle.abort();
                }
                handle_set_model(id, model, &agent, &client_event_tx).await;
                prewarm_task = Some(spawn_deferred_prewarm(&agent));
            }

            Request::SetRoute { id, selection, automatic_failover } => {
                if let Some(handle) = prewarm_task.take() {
                    handle.abort();
                }
                handle_set_route(id, selection, automatic_failover, &agent, &client_event_tx).await;
                prewarm_task = Some(spawn_deferred_prewarm(&agent));
            }

            // Acks for events the daemon keeps re-delivering until a client
            // confirms it saw them. Only this path clears the durable copy, so
            // a client that never acks keeps receiving the event.
            Request::ApprovalAck { id, request_id } => {
                handle_approval_ack(id, request_id, &client_event_tx);
            }

            Request::FailoverAck {
                id,
                chain_id,
                from,
                to,
                attempt,
            } => {
                handle_failover_ack(
                    id,
                    &client_session_id,
                    chain_id,
                    from,
                    to,
                    attempt,
                    &client_event_tx,
                );
            }

            Request::SetAgentModel { id, target, model, global } => {
                let result = if global {
                    super::client_actions::set_global_agent_model(
                        &agent, &client_session_id, &target, model,
                    ).await
                } else {
                    super::client_actions::set_session_agent_model(
                        &agent, &client_session_id, &target, model,
                    ).await
                };
                let event = match result {
                    Ok(overrides) => ServerEvent::AgentModelsChanged { id, session_id: client_session_id.clone(), overrides },
                    Err(error) => ServerEvent::Error { id, message: error.to_string(), retry_after_secs: None },
                };
                let _ = client_event_tx.send(event);
            }

            Request::SetSubagentModel { id, model } => {
                if reject_if_agent_busy_for_request(
                    id,
                    "set_subagent_model",
                    &client_session_id,
                    client_is_processing,
                    &agent,
                    &client_event_tx,
                ).await {
                    continue;
                }
                handle_set_subagent_model(id, model, &agent, &client_event_tx).await;
            }

            Request::RunSubagent {
                id,
                prompt,
                subagent_type,
                model,
                session_id,
            } => {
                handle_run_subagent(
                    id,
                    prompt,
                    subagent_type,
                    model,
                    session_id,
                    &agent,
                    &client_event_tx,
                );
            }

            Request::SetReasoningEffort {
                id,
                effort,
                target_session_id,
            } => {
                if let Some(target_session_id) = target_session_id {
                    let target_agent = { sessions.read().await.get(&target_session_id).cloned() };
                    if let Some(target_agent) = target_agent {
                        handle_set_reasoning_effort(id, effort, &target_agent, &client_event_tx)
                            .await;
                    } else {
                        let _ = client_event_tx.send(ServerEvent::ReasoningEffortChanged {
                            id,
                            effort: None,
                            error: Some(format!("target session not found: {target_session_id}")),
                        });
                    }
                } else {
                    handle_set_reasoning_effort(id, effort, &agent, &client_event_tx).await;
                }
            }

            Request::SetServiceTier { id, service_tier } => {
                handle_set_service_tier(id, service_tier, &agent, &client_event_tx).await;
            }

            Request::SetTransport { id, transport } => {
                handle_set_transport(id, transport, &agent, &client_event_tx).await;
            }

            Request::SetCompactionMode { id, mode } => {
                handle_set_compaction_mode(id, mode, &agent, &client_event_tx).await;
            }

            Request::SetSessionSaved { id, saved, label } => {
                if reject_if_agent_busy_for_request(
                    id,
                    "set_session_saved",
                    &client_session_id,
                    client_is_processing,
                    &agent,
                    &client_event_tx,
                ).await {
                    continue;
                }
                handle_set_session_saved(
                    id,
                    saved,
                    label,
                    &agent,
                    &client_session_id,
                    &swarm_members,
                    &client_event_tx,
                )
                .await;
            }

            Request::ArchiveSession {
                id,
                archived,
                reason,
            } => {
                if reject_if_agent_busy_for_request(
                    id,
                    "archive_session",
                    &client_session_id,
                    client_is_processing,
                    &agent,
                    &client_event_tx,
                ).await {
                    continue;
                }
                handle_archive_session(
                    id,
                    archived,
                    reason,
                    &agent,
                    &client_session_id,
                    &client_event_tx,
                )
                .await;
            }

            Request::RenameSession { id, title } => {
                if reject_if_agent_busy_for_request(
                    id,
                    "rename_session",
                    &client_session_id,
                    client_is_processing,
                    &agent,
                    &client_event_tx,
                ).await {
                    continue;
                }
                handle_rename_session(
                    id,
                    title,
                    &agent,
                    &client_session_id,
                    &swarm_members,
                    &client_event_tx,
                )
                .await;
            }

            Request::NotifyAuthChanged {
                id,
                provider: provider_hint,
                auth,
                prefer_strongest,
            } => {
                handle_notify_auth_changed(
                    id,
                    provider_hint,
                    auth,
                    prefer_strongest,
                    &provider,
                    &provider_template,
                    &sessions,
                    &client_session_id,
                    &agent,
                    &client_event_tx,
                )
                .await;
            }

            Request::SwitchOpenAiAccount { id, label } => {
                handle_switch_openai_account(id, label, &agent, &client_event_tx).await;
            }

            Request::InvalidateOpenAiUsage { id, account_label } => {
                handle_invalidate_openai_usage(id, account_label, &client_event_tx).await;
            }

            Request::SetFeature {
                id,
                feature,
                enabled,
            } => {
                if reject_if_agent_busy_for_request(
                    id,
                    "set_feature",
                    &client_session_id,
                    client_is_processing,
                    &agent,
                    &client_event_tx,
                ).await {
                    continue;
                }
                handle_set_feature(
                    id,
                    feature,
                    enabled,
                    &agent,
                    &client_session_id,
                    &friendly_name,
                    &mut swarm_enabled,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_coordinators,
                    &channel_subscriptions,
                    &channel_subscriptions_by_session,
                    &swarm_plans,
                    &client_event_tx,
                )
                .await;
            }

            Request::Split { id } => {
                handle_split(id, &client_session_id, &agent, &client_event_tx).await;
            }

            Request::Transfer { id } => {
                if reject_if_agent_busy_for_request(
                    id,
                    "transfer",
                    &client_session_id,
                    client_is_processing,
                    &agent,
                    &client_event_tx,
                ).await {
                    continue;
                }
                handle_transfer(id, &client_session_id, &agent, &client_event_tx).await;
            }

            Request::Compact { id } => {
                handle_compact(id, &agent, &client_event_tx);
            }

            Request::TriggerMemoryExtraction { id } => {
                if reject_if_agent_busy_for_request(
                    id,
                    "trigger_memory_extraction",
                    &client_session_id,
                    client_is_processing,
                    &agent,
                    &client_event_tx,
                ).await {
                    continue;
                }
                handle_trigger_memory_extraction(id, &agent, &client_event_tx).await;
            }

            // Agent-to-agent communication
            Request::AgentRegister { id, .. } => {
                let _ = client_event_tx.send(ServerEvent::Done { id });
            }

            Request::StdinResponse {
                id,
                request_id,
                input,
            } => {
                handle_stdin_response(id, request_id, input, &stdin_responses, &client_event_tx)
                    .await;
            }

            Request::AskResponse {
                id,
                request_id,
                answers,
                answer_id,
            } => {
                match answer_id {
                    // A client that negotiated `ask_receipts_v1` sent an answer
                    // identity, so this answer resolves through the session's
                    // ledger: a retry of the same id replays one resolution
                    // instead of resolving the question twice.
                    Some(answer_id) => {
                        handle_ask_response_with_receipt(
                            id,
                            request_id,
                            answers,
                            answer_id,
                            &client_session_id,
                            &pending_asks,
                            &client_event_tx,
                        )
                        .await;
                    }
                    // The legacy request carries no identity and behaves
                    // exactly as it always has.
                    None => {
                        handle_ask_response(id, request_id, answers, &pending_asks, &client_event_tx).await;
                    }
                }
            }

            Request::GetRuntimeState { id } => {
                let reply = match super::runtime_state_api::runtime_state_summary(&client_session_id).await {
                    Ok(state) => ServerEvent::RuntimeState {
                        id,
                        state: serde_json::to_value(state).unwrap_or(serde_json::Value::Null),
                    },
                    Err(message) => ServerEvent::Error {
                        id,
                        message,
                        retry_after_secs: None,
                    },
                };
                let _ = client_event_tx.send(reply);
            }

            Request::ApplyControl {
                id,
                operation_id,
                action,
                args,
            } => {
                let reply = match super::runtime_state_api::parse_control_action(&action, args.as_ref()) {
                    Ok(parsed) => {
                        let outcome =
                            super::runtime_state_api::apply_control(&session_control, &operation_id, parsed).await;
                        super::runtime_state_api::control_receipt_event(id, operation_id, action, &outcome)
                    }
                    // A request this daemon cannot apply is refused rather
                    // than answered with a success it did not earn.
                    Err(message) => ServerEvent::Error {
                        id,
                        message,
                        retry_after_secs: None,
                    },
                };
                let _ = client_event_tx.send(reply);
            }

            Request::GetControlReceipt { id, operation_id } => {
                let reply = match super::runtime_state_api::control_receipt(&client_session_id, &operation_id).await {
                    Ok((action, outcome)) => {
                        super::runtime_state_api::control_receipt_event(id, operation_id, action, &outcome)
                    }
                    Err(message) => ServerEvent::Error {
                        id,
                        message,
                        retry_after_secs: None,
                    },
                };
                let _ = client_event_tx.send(reply);
            }

            Request::GetSnapshot {
                id,
                cursor,
                tail,
                max_bytes,
            } => {
                let reply = match super::runtime_state_api::snapshot(&client_session_id, cursor, tail, max_bytes).await
                {
                    Ok(snapshot) => ServerEvent::SnapshotResult {
                        id,
                        snapshot: serde_json::to_value(snapshot).unwrap_or(serde_json::Value::Null),
                    },
                    Err(message) => ServerEvent::Error {
                        id,
                        message,
                        retry_after_secs: None,
                    },
                };
                let _ = client_event_tx.send(reply);
            }

            Request::PermissionDecision {
                id,
                request_id,
                decision,
            } => {
                let bridge =
                    crate::tool::permission_bridge::bridge_for_connection(&client_session_id, &client_connection_id);
                handle_permission_decision(id, request_id, decision, &client_event_tx, bridge.as_deref());
            }

            Request::AgentTask { id, task, .. } => {
                handle_agent_task(
                    id,
                    task,
                    &client_session_id,
                    &agent,
                    &AgentTaskContext {
                        client_event_tx: &client_event_tx,
                        swarm_members: &swarm_members,
                        swarms_by_id: &swarms_by_id,
                        event_history: &event_history,
                        event_counter: &event_counter,
                        swarm_event_tx: &swarm_event_tx,
                    },
                )
                .await;
            }

            Request::AgentCapabilities { id } => {
                let _ = client_event_tx.send(ServerEvent::Done { id });
            }

            Request::RuntimeCommand { id, command, args } => {
                let result: anyhow::Result<(serde_json::Value, String)> = async {
                    let parsed: horon_agent_harness_api::RuntimeCommand = serde_json::from_value(serde_json::json!(command))?;
                    if parsed == horon_agent_harness_api::RuntimeCommand::ServerReload {
                        if !args.is_empty() { anyhow::bail!("Server reload accepts no arguments. Forced reload is intentionally not exposed."); }
                        let _ = client_event_tx.send(ServerEvent::RuntimeCommandResult { id, command: command.clone(),
                            data: serde_json::json!({"requested": true, "force": false}),
                            message: "Requested a state-preserving server upgrade if a newer binary is available. Watch reload progress for completion.".into() });
                        handle_reload(id, false, &client_session_id, &agent, &swarm_members, &client_event_tx).await;
                        return Ok((serde_json::Value::Null, String::new()));
                    }
                    if matches!(parsed, horon_agent_harness_api::RuntimeCommand::Continue | horon_agent_harness_api::RuntimeCommand::ResumeAll) {
                        if !args.is_empty() { anyhow::bail!("This command accepts no arguments"); }
                        let (tx, mut rx) = mpsc::unbounded_channel();
                        super::client_actions::handle_resume_all_sessions(id, &sessions, &swarm_members, &swarms_by_id,
                            &event_history, &event_counter, &swarm_event_tx, &tx).await;
                        while let Ok(event) = rx.try_recv() {
                            if let ServerEvent::ResumeAllResult { resumed, skipped, message, resumed_sessions, .. } = event {
                                return Ok((serde_json::json!({"resumed": resumed, "skipped": skipped, "resumed_sessions": resumed_sessions}), message));
                            }
                        }
                        anyhow::bail!("Runtime did not report continuation outcome");
                    }
                    if parsed == horon_agent_harness_api::RuntimeCommand::RefreshModelList {
                        if !args.is_empty() { anyhow::bail!("This command accepts no arguments"); }
                        provider.refresh_model_catalog().await?;
                        crate::bus::Bus::global().publish_models_updated();
                        let guard = agent.try_lock().map_err(|_| anyhow::anyhow!("Catalog refreshed, but session is busy. Open the model picker after the turn."))?;
                        let data = serde_json::json!({"refreshed": true, "models": guard.available_models_for_switching(), "routes": guard.model_routes()});
                        return Ok((data, "Provider model catalog refreshed.".into()));
                    }
                    if parsed == horon_agent_harness_api::RuntimeCommand::Swarm {
                        if let Some(enabled) = super::runtime_commands::boolean_action(&args, swarm_enabled, false)? {
                            if reject_if_agent_busy_for_request(id, "runtime_command", &client_session_id, client_is_processing, &agent, &client_event_tx).await {
                                return Ok((serde_json::Value::Null, String::new()));
                            }
                            let (tx, mut rx) = mpsc::unbounded_channel();
                            handle_set_feature(id, crate::protocol::FeatureToggle::Swarm, enabled, &agent, &client_session_id, &friendly_name,
                                &mut swarm_enabled, &swarm_members, &swarms_by_id, &swarm_coordinators,
                                &channel_subscriptions, &channel_subscriptions_by_session, &swarm_plans, &tx).await;
                            while let Ok(event) = rx.try_recv() {
                                if let ServerEvent::Error { message, .. } = event { anyhow::bail!("{message}"); }
                            }
                        }
                        let members = swarm_members.read().await;
                        let member = members.get(&client_session_id);
                        let data = serde_json::json!({"enabled": swarm_enabled, "config_default": crate::config::config().features.swarm,
                            "swarm_id": member.and_then(|m| m.swarm_id.as_deref()), "role": member.map(|m| &m.role)});
                        return Ok((data.clone(), serde_json::to_string_pretty(&data)?));
                    }
                    if parsed == horon_agent_harness_api::RuntimeCommand::Subscription {
                        if !args.is_empty() && args != ["status".to_string()] { anyhow::bail!("Usage: subscription [status]"); }
                        let data = if crate::subscription_catalog::has_credentials() {
                            serde_json::to_value(crate::subscription_api::fetch_subscription_me().await?)?
                        } else { serde_json::json!({"signed_in": false, "status": "not_signed_in"}) };
                        return Ok((data.clone(), serde_json::to_string_pretty(&data)?));
                    }
                    let mut guard = agent.try_lock().map_err(|_| anyhow::anyhow!("Session is busy. Retry this runtime command after the current turn."))?;
                    super::runtime_commands::execute(&mut guard, parsed, &args, swarm_enabled, serde_json::Value::Null).await
                }.await;
                match result {
                    Ok((data, message)) if !data.is_null() => { let _ = client_event_tx.send(ServerEvent::RuntimeCommandResult { id, command, data, message }); },
                    Ok(_) => {},
                    Err(error) => { let _ = client_event_tx.send(ServerEvent::Error { id, message: error.to_string(), retry_after_secs: None }); },
                }
            }
            Request::AgentContext { id } => {
                let _ = client_event_tx.send(ServerEvent::Done { id });
            }

            Request::NotifySession {
                id,
                session_id,
                message,
            } => {
                handle_notify_session(
                    id,
                    session_id,
                    message,
                    NotifySessionContext {
                        sessions: &sessions,
                        soft_interrupt_queues: &soft_interrupt_queues,
                        client_connections: &client_connections,
                        swarm_members: &swarm_members,
                        swarms_by_id: &swarms_by_id,
                        event_history: &event_history,
                        event_counter: &event_counter,
                        swarm_event_tx: &swarm_event_tx,
                        client_event_tx: &client_event_tx,
                    },
                )
                .await;
            }

            Request::InputShell { id, command } => {
                handle_input_shell(id, command, &agent, &client_event_tx);
            }

            // === Agent communication ===
            Request::CommShare {
                id,
                session_id: req_session_id,
                key,
                value,
                append,
            } => {
                handle_comm_share(
                    id,
                    req_session_id,
                    key,
                    value,
                    append,
                    &client_event_tx,
                    &swarm_members,
                    &swarms_by_id,
                    &shared_context,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                )
                .await;
            }

            Request::CommRead {
                id,
                session_id: req_session_id,
                key,
            } => {
                handle_comm_read(
                    id,
                    req_session_id,
                    key,
                    &client_event_tx,
                    &swarm_members,
                    &shared_context,
                )
                .await;
            }

            Request::CommMessage {
                id,
                from_session,
                message,
                to_session,
                channel,
                delivery,
                wake,
                tldr,
                to_swarm,
            } => {
                handle_comm_message(
                    id,
                    from_session,
                    message,
                    to_session,
                    channel,
                    delivery,
                    wake,
                    tldr,
                    to_swarm,
                    &client_event_tx,
                    &sessions,
                    &soft_interrupt_queues,
                    &swarm_members,
                    &swarms_by_id,
                    &channel_subscriptions,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &client_connections,
                )
                .await;
            }

            Request::CommList {
                id,
                session_id: req_session_id,
            } => {
                handle_comm_list(
                    id,
                    req_session_id,
                    &client_event_tx,
                    &swarm_members,
                    &swarms_by_id,
                    &file_touch,
                    &sessions,
                    &client_connections,
                )
                .await;
            }

            Request::CommListSwarms {
                id,
                session_id: req_session_id,
            } => {
                handle_comm_list_swarms(
                    id,
                    req_session_id,
                    &client_event_tx,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_coordinators,
                )
                .await;
            }

            Request::CommSetSwarmLabel {
                id,
                session_id: req_session_id,
                label,
            } => {
                handle_comm_set_swarm_label(
                    id,
                    req_session_id,
                    label,
                    &client_event_tx,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_coordinators,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                )
                .await;
            }

            Request::CommListChannels {
                id,
                session_id: req_session_id,
            } => {
                handle_comm_list_channels(
                    id,
                    req_session_id,
                    &client_event_tx,
                    &swarm_members,
                    &channel_subscriptions,
                )
                .await;
            }

            Request::CommChannelMembers {
                id,
                session_id: req_session_id,
                channel,
            } => {
                handle_comm_channel_members(
                    id,
                    req_session_id,
                    channel,
                    &client_event_tx,
                    &swarm_members,
                    &channel_subscriptions,
                )
                .await;
            }

            Request::CommProposePlan {
                id,
                session_id: req_session_id,
                items,
            } => {
                handle_comm_propose_plan(
                    id,
                    req_session_id,
                    items,
                    &client_event_tx,
                    &swarm_members,
                    &swarms_by_id,
                    &shared_context,
                    &swarm_plans,
                    &swarm_coordinators,
                    &sessions,
                    &soft_interrupt_queues,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &swarm_mutation_runtime,
                )
                .await;
            }

            Request::CommApprovePlan {
                id,
                session_id: req_session_id,
                proposer_session,
            } => {
                handle_comm_approve_plan(
                    id,
                    req_session_id,
                    proposer_session,
                    &client_event_tx,
                    &swarm_members,
                    &swarms_by_id,
                    &shared_context,
                    &swarm_plans,
                    &swarm_coordinators,
                    &sessions,
                    &soft_interrupt_queues,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &swarm_mutation_runtime,
                )
                .await;
            }

            Request::CommRejectPlan {
                id,
                session_id: req_session_id,
                proposer_session,
                reason,
            } => {
                handle_comm_reject_plan(
                    id,
                    req_session_id,
                    proposer_session,
                    reason,
                    &client_event_tx,
                    &swarm_members,
                    &shared_context,
                    &swarm_coordinators,
                    &sessions,
                    &soft_interrupt_queues,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &swarm_mutation_runtime,
                )
                .await;
            }

            Request::CommSeedGraph {
                id,
                session_id: req_session_id,
                mode,
                nodes,
            } => {
                super::comm_graph::handle_comm_seed_graph(
                    id,
                    req_session_id,
                    mode,
                    nodes,
                    &client_event_tx,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_plans,
                    &swarm_coordinators,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                )
                .await;
            }

            Request::CommExpandNode {
                id,
                session_id: req_session_id,
                node_id,
                children,
            } => {
                super::comm_graph::handle_comm_expand_node(
                    id,
                    req_session_id,
                    node_id,
                    children,
                    &client_event_tx,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_plans,
                    &swarm_coordinators,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                )
                .await;
            }

            Request::CommCompleteNode {
                id,
                session_id: req_session_id,
                node_id,
                artifact_json,
            } => {
                super::comm_graph::handle_comm_complete_node(
                    id,
                    req_session_id,
                    node_id,
                    artifact_json,
                    &client_event_tx,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_plans,
                    &swarm_coordinators,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                )
                .await;
            }

            Request::CommInjectGap {
                id,
                session_id: req_session_id,
                gate_id,
                nodes,
            } => {
                super::comm_graph::handle_comm_inject_gap(
                    id,
                    req_session_id,
                    gate_id,
                    nodes,
                    &client_event_tx,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_plans,
                    &swarm_coordinators,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                )
                .await;
            }

            Request::CommSpawn {
                id,
                session_id: req_session_id,
                working_dir,
                initial_message,
                request_nonce,
                spawn_mode,
                model,
                effort,
                label,
                isolation,
                iso_apply,
                output_schema,
            } => {
                let spawn_mode = match parse_swarm_spawn_mode(id, spawn_mode, &client_event_tx) {
                    Some(spawn_mode) => spawn_mode,
                    None => return Ok(()),
                };
                handle_comm_spawn(
                    id,
                    req_session_id,
                    working_dir,
                    initial_message,
                    request_nonce,
                    spawn_mode,
                    model,
                    effort,
                    label,
                    isolation,
                    iso_apply,
                    output_schema,
                    &client_event_tx,
                    &sessions,
                    &global_session_id,
                    &provider_template,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_coordinators,
                    &swarm_plans,
                    &channel_subscriptions,
                    &channel_subscriptions_by_session,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &mcp_pool,
                    &soft_interrupt_queues,
                    &swarm_mutation_runtime,
                    &client_connections,
                )
                .await;
            }

            Request::CommListModels {
                id,
                session_id: req_session_id,
            } => {
                super::comm_session::handle_comm_list_models(
                    id,
                    &req_session_id,
                    &sessions,
                    &provider_template,
                    |event| {
                        let _ = client_event_tx.send(event);
                    },
                )
                .await;
            }

            Request::CommStop {
                id,
                session_id: req_session_id,
                target_session,
                force,
            } => {
                handle_comm_stop(
                    id,
                    req_session_id,
                    target_session,
                    force.unwrap_or(false),
                    &client_event_tx,
                    &sessions,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_coordinators,
                    &swarm_plans,
                    &channel_subscriptions,
                    &channel_subscriptions_by_session,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &soft_interrupt_queues,
                    &swarm_mutation_runtime,
                )
                .await;
            }

            Request::CommAssignRole {
                id,
                session_id: req_session_id,
                target_session,
                role,
            } => {
                handle_comm_assign_role(
                    id,
                    req_session_id,
                    target_session,
                    role,
                    &client_event_tx,
                    &sessions,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_coordinators,
                    &swarm_plans,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &swarm_mutation_runtime,
                )
                .await;
            }

            Request::CommSummary {
                id,
                session_id: req_session_id,
                target_session,
                limit,
            } => {
                handle_comm_summary(
                    id,
                    req_session_id,
                    target_session,
                    limit,
                    &sessions,
                    &swarm_members,
                    &client_event_tx,
                )
                .await;
            }

            Request::CommStatus {
                id,
                session_id: req_session_id,
                target_session,
            } => {
                handle_comm_status(
                    id,
                    req_session_id,
                    target_session,
                    &sessions,
                    &swarm_members,
                    &client_connections,
                    &file_touch,
                    &client_event_tx,
                )
                .await;
            }

            Request::CommReport {
                id,
                session_id: req_session_id,
                status,
                message,
                validation,
                follow_up,
                tldr,
            } => {
                let status = status.unwrap_or_else(|| "ready".to_string());
                let report = format_structured_completion_report(
                    &message,
                    validation.as_deref(),
                    follow_up.as_deref(),
                );
                let detail = Some(truncate_detail(&message, 160));
                update_member_status_with_report_tldr(
                    &req_session_id,
                    &status,
                    detail,
                    Some(report),
                    tldr,
                    &swarm_members,
                    &swarms_by_id,
                    Some(&event_history),
                    Some(&event_counter),
                    Some(&swarm_event_tx),
                )
                .await;
                let _ = client_event_tx.send(ServerEvent::CommReportResponse {
                    id,
                    status,
                    message: "Report recorded and delivered to the coordinator when applicable."
                        .to_string(),
                });
            }

            Request::CommPlanStatus {
                id,
                session_id: req_session_id,
            } => {
                handle_comm_plan_status(
                    id,
                    req_session_id,
                    &swarm_members,
                    &swarm_plans,
                    &client_event_tx,
                )
                .await;
            }

            Request::CommReadContext {
                id,
                session_id: req_session_id,
                target_session,
            } => {
                handle_comm_read_context(
                    id,
                    req_session_id,
                    target_session,
                    &sessions,
                    &swarm_members,
                    &client_event_tx,
                )
                .await;
            }

            Request::CommResyncPlan {
                id,
                session_id: req_session_id,
            } => {
                handle_comm_resync_plan(
                    id,
                    req_session_id,
                    &CommResyncPlanContext {
                        client_event_tx: &client_event_tx,
                        swarm_members: &swarm_members,
                        swarms_by_id: &swarms_by_id,
                        swarm_plans: &swarm_plans,
                        swarm_coordinators: &swarm_coordinators,
                        event_history: &event_history,
                        event_counter: &event_counter,
                        swarm_event_tx: &swarm_event_tx,
                    },
                )
                .await;
            }

            Request::CommAssignTask {
                id,
                session_id: req_session_id,
                target_session,
                task_id,
                message,
            } => {
                handle_comm_assign_task(
                    id,
                    req_session_id,
                    target_session,
                    task_id,
                    message,
                    &client_event_tx,
                    &sessions,
                    &soft_interrupt_queues,
                    &client_connections,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_plans,
                    &swarm_coordinators,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &swarm_mutation_runtime,
                )
                .await;
            }

            Request::CommAssignNext {
                id,
                session_id: req_session_id,
                target_session,
                working_dir,
                prefer_spawn,
                spawn_if_needed,
                message,
                model,
                effort,
            } => {
                handle_comm_assign_next(
                    id,
                    req_session_id,
                    target_session,
                    working_dir,
                    prefer_spawn,
                    spawn_if_needed,
                    message,
                    model,
                    effort,
                    &client_event_tx,
                    &sessions,
                    &global_session_id,
                    &provider_template,
                    &soft_interrupt_queues,
                    &client_connections,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_plans,
                    &swarm_coordinators,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &mcp_pool,
                    &swarm_mutation_runtime,
                )
                .await;
            }

            Request::CommTaskControl {
                id,
                session_id: req_session_id,
                action,
                task_id,
                target_session,
                message,
            } => {
                handle_comm_task_control(
                    id,
                    req_session_id,
                    action,
                    task_id,
                    target_session,
                    message,
                    &client_event_tx,
                    &sessions,
                    &soft_interrupt_queues,
                    &client_connections,
                    &swarm_members,
                    &swarms_by_id,
                    &swarm_plans,
                    &swarm_coordinators,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                    &swarm_mutation_runtime,
                )
                .await;
            }

            Request::CommSubscribeChannel {
                id,
                session_id: req_session_id,
                channel,
            } => {
                handle_comm_subscribe_channel(
                    id,
                    req_session_id,
                    channel,
                    &client_event_tx,
                    &swarm_members,
                    &channel_subscriptions,
                    &channel_subscriptions_by_session,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                )
                .await;
            }

            Request::CommUnsubscribeChannel {
                id,
                session_id: req_session_id,
                channel,
            } => {
                handle_comm_unsubscribe_channel(
                    id,
                    req_session_id,
                    channel,
                    &client_event_tx,
                    &swarm_members,
                    &channel_subscriptions,
                    &channel_subscriptions_by_session,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                )
                .await;
            }

            Request::CommAwaitMembers {
                id,
                session_id: req_session_id,
                target_status,
                session_ids: requested_ids,
                mode,
                timeout_secs,
                background,
                notify,
                wake,
            } => {
                handle_comm_await_members(
                    id,
                    req_session_id,
                    target_status,
                    requested_ids,
                    mode,
                    timeout_secs,
                    background,
                    notify,
                    wake,
                    CommAwaitMembersContext {
                        client_event_tx: &client_event_tx,
                        swarm_members: &swarm_members,
                        swarms_by_id: &swarms_by_id,
                        swarm_event_tx: &swarm_event_tx,
                        await_members_runtime: &await_members_runtime,
                    },
                )
                .await;
            }

            // Only the ACP connection attached to this session may give it MCP
            // servers; a plain client has no editor to own their lifetime.
            Request::ConfigureSessionMcp { id, servers } => {
                let acp_attached = acp_installation
                    .as_ref()
                    .is_some_and(|(session, _)| session == &client_session_id);
                let configs = if acp_attached {
                    crate::tool::session_mcp::server_configs(servers)
                } else {
                    Err(crate::tool::approval_override::NOT_ACP_MESSAGE.to_string())
                };
                match configs {
                    Err(message) => {
                        let _ = client_event_tx.send(ServerEvent::Error {
                            id,
                            message,
                            retry_after_secs: None,
                        });
                    }
                    Ok(configs) => {
                        if let Some(previous) = session_mcp.take() {
                            previous.shutdown().await;
                        }
                        if !configs.is_empty() {
                            let working_dir = agent
                                .try_lock()
                                .ok()
                                .and_then(|guard| guard.working_dir().map(std::path::PathBuf::from))
                                .or_else(|| subscribed_working_dir.clone());
                            let session_agent = Arc::clone(&agent);
                            session_mcp = Some(crate::tool::session_mcp::SessionMcp::start(
                                &client_session_id,
                                &client_connection_id,
                                configs,
                                working_dir,
                                Box::pin(async move { session_agent.lock().await.registry() }),
                                client_event_tx.clone(),
                            ));
                        }
                        let _ = client_event_tx.send(ServerEvent::Done { id });
                    }
                }
            }

            Request::SetSessionApprovalMode { .. } => unreachable!("approval mode replies only after validation"),

            // These are handled via channels, not direct requests from TUI
            Request::WithdrawMessage { id, message_request_id } => {
                let queued = last_v2_message
                    .as_ref()
                    .filter(|message| message.request_id == message_request_id);
                let reply = match queued {
                    Some(message) if processing_message_id == Some(message_request_id) && message.withdraw() => {
                        if let Some(handle) = processing_task.take() {
                            handle.abort();
                        }
                        processing_message_id = None;
                        client_is_processing = false;
                        {
                            let mut connections = client_connections.write().await;
                            if let Some(info) = connections.get_mut(&client_connection_id) {
                                info.is_processing = false;
                                info.current_tool_name = None;
                            }
                        }
                        // A withdrawal wins only while this prompt still waits
                        // for the agent, which some other surface's turn holds:
                        // that turn is running the session, so its member must
                        // stay running rather than be reported ready.
                        if let Some(session_id) = processing_session_id.take()
                            && !super::foreground_state::is_running(&session_id)
                        {
                            update_member_status(
                                &session_id,
                                "ready",
                                None,
                                &swarm_members,
                                &swarms_by_id,
                                Some(&event_history),
                                Some(&event_counter),
                                Some(&swarm_event_tx),
                            )
                            .await;
                        }
                        ServerEvent::Done { id }
                    }
                    Some(_) => ServerEvent::Error {
                        id,
                        message: "already inserted".to_string(),
                        retry_after_secs: None,
                    },
                    None => ServerEvent::Error {
                        id,
                        message: format!("message request {message_request_id} is not queued on this connection"),
                        retry_after_secs: None,
                    },
                };
                let _ = client_event_tx.send(reply);
            }
        }
        if sdk_session_before_request != client_session_id {
            drop(crate::tool::sdk::ConnectionGuard(client_connection_id.clone()));
        }
        if request_lifecycle_logged {
            log_request_lifecycle_handled(
                ServerRequestLifecycleFields {
                    phase: "handled",
                    request_id,
                    request_kind: &request_kind,
                    client_session_id: &client_session_id,
                    client_connection_id: &client_connection_id,
                    client_instance_id: current_client_instance_id.as_deref(),
                    client_is_processing,
                    message_id: processing_message_id,
                    processing_session_id: processing_session_id.as_deref(),
                    line_bytes: line.len(),
                },
                request_lifecycle_start,
                request_decoded_at,
            );
        }
    }

    Ok(())
    }.await;

    drop(_sdk_connection_guard);

    // Everything this connection installed for its session goes with the
    // socket, on every exit path: the idle path below returns before the
    // retained-turn cleanup, and an installation left behind would keep routing
    // prompts, file calls and an approval mode to a connection that is gone.
    if let Some((session, token)) = ask_installation.take() {
        crate::tool::ask_bridge::clear(&session, token);
    }
    if let Some((session, token)) = acp_installation.take() {
        crate::tool::acp_session::clear(&session, token);
    }
    if let Some((session, token)) = client_bridge_installation.take() {
        crate::tool::client_bridge::clear(&session, token);
    }
    if let Some((session, token)) = permission_bridge_installation.take() {
        crate::tool::permission_bridge::clear(&session, token);
    }
    if let Some((session, token)) = approval_override_installation.take() {
        crate::tool::approval_override::clear(&session, token);
    }
    if let Some(session_mcp) = session_mcp.take() {
        session_mcp.shutdown().await;
    }
    // Settle the warmup and this reference before the barrier may open.
    settle_prewarm(&mut prewarm_task).await;
    drop(agent);

    // Cleanup serializes the successor decision against live attachment claims.
    // A successor must inherit the active owner, including its completion receiver,
    // rather than just an Agent whose processing task was already aborted.
    let event_handle = if !continue_on_disconnect {
        let retained = crate::hooks::with_client_terminal_env(
            active_terminal_env.clone(),
            cleanup_client_connection(
                &sessions,
                &client_session_id,
                client_is_processing,
                &mut processing_task,
                event_handle,
                &swarm_members,
                &swarms_by_id,
                &swarm_coordinators,
                &swarm_plans,
                &file_touch,
                &channel_subscriptions,
                &channel_subscriptions_by_session,
                &client_connections,
                &client_connection_id,
                &shutdown_signals,
                &soft_interrupt_queues,
                &event_history,
                &event_counter,
                &swarm_event_tx,
                &client_event_tx,
                idle_reconnect_grace,
            ),
        )
        .await?;
        match retained {
            Some(handle) => handle,
            None => return connection_result,
        }
    } else {
        event_handle
    };

    {
        // Retain the existing turn owner, not the socket. Its JoinHandle and
        // completion receiver stay alive so normal finalization still runs and
        // the daemon cannot idle-shutdown midway through remote work. New
        // attachments receive future events through the existing session fanout.
        detach_client_attachment(
            &client_session_id,
            &client_connection_id,
            &client_connections,
            &swarm_members,
        )
        .await;
        event_handle.abort();
        drop(reader);
        drop(writer);
        // Input prompts belong to this transport and cannot safely be replayed
        // to a new client. Close response channels instead of waiting forever.
        stdin_forwarder.abort();
        let _ = stdin_forwarder.await;
        stdin_responses.lock().await.clear();
        // Dropping the pending answer channels ends a tool's wait instead of
        // leaving the turn parked on a client that is gone. The forwarder is
        // aborted without being awaited: it only relays prompts from a
        // connection that has already closed.
        ask_forwarder.abort();
        pending_asks.lock().await.clear();
        if let Some(handle) = processing_task.take() {
            crate::logging::info(&format!(
                "Retaining disconnected turn for session {}",
                client_session_id
            ));
            let _ = handle.await;
            while let Ok((done_id, result, report)) = processing_done_rx.try_recv() {
                if Some(done_id) == processing_message_id {
                    record_processing_completion(
                        processing_session_id.as_deref(),
                        result,
                        report,
                        &SwarmStatusRefs {
                            members: &swarm_members,
                            swarms_by_id: &swarms_by_id,
                            event_history: &event_history,
                            event_counter: &event_counter,
                            event_tx: &swarm_event_tx,
                        },
                    )
                    .await;
                }
            }
            client_is_processing = false;
        } else {
            // A reattached remote connection may disconnect again while the
            // original lifecycle owns the task. Wait for its active-turn lease
            // before attempting cleanup. Returning early here would leak the
            // session if the original owner had just skipped cleanup for this
            // successor. All finishers serialize cleanup against live attach.
            while crate::turn_cancel_registry::has_active_turn(&client_session_id) {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            client_is_processing = false;
        }
    }

    crate::hooks::with_client_terminal_env(
        active_terminal_env,
        cleanup_client_connection(
            &sessions,
            &client_session_id,
            client_is_processing,
            &mut processing_task,
            event_handle,
            &swarm_members,
            &swarms_by_id,
            &swarm_coordinators,
            &swarm_plans,
            &file_touch,
            &channel_subscriptions,
            &channel_subscriptions_by_session,
            &client_connections,
            &client_connection_id,
            &shutdown_signals,
            &soft_interrupt_queues,
            &event_history,
            &event_counter,
            &swarm_event_tx,
            &client_event_tx,
            idle_reconnect_grace,
        ),
    )
    .await?;
    connection_result
}

/// Longest wait for the forwarder to write what the request loop queued, so a
/// client that stopped reading cannot hold the connection's cleanup.
const CLIENT_EVENT_FLUSH_BOUND: Duration = Duration::from_secs(5);

/// Wait until the connection's forwarder has written every event queued on its
/// channel before this call, or it stopped, or [`CLIENT_EVENT_FLUSH_BOUND`].
async fn flush_client_events(flush: &tokio::sync::mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>) {
    let (written, flushed) = tokio::sync::oneshot::channel();
    if flush.send(written).is_ok() {
        let _ = tokio::time::timeout(CLIENT_EVENT_FLUSH_BOUND, flushed).await;
    }
}

async fn record_processing_completion(
    done_session: Option<&str>,
    result: Result<()>,
    completion_report: Option<String>,
    swarm: &SwarmStatusRefs<'_>,
) {
    match result {
        Ok(()) => {
            if let Some(session_id) = done_session {
                update_member_status_with_report(
                    session_id,
                    "ready",
                    None,
                    completion_report,
                    swarm.members,
                    swarm.swarms_by_id,
                    Some(swarm.event_history),
                    Some(swarm.event_counter),
                    Some(swarm.event_tx),
                )
                .await;
            }
        }
        Err(e) => {
            if let Some(session_id) = done_session {
                update_member_status(
                    session_id,
                    "failed",
                    Some(truncate_detail(&e.to_string(), 120)),
                    swarm.members,
                    swarm.swarms_by_id,
                    Some(swarm.event_history),
                    Some(swarm.event_counter),
                    Some(swarm.event_tx),
                )
                .await;
            }
        }
    }
}

/// The retry hint a failed turn or message entry reports to its client.
///
/// Provider back-off wins; an owed resume commit that failed to land is retried
/// by the next entry, so the client is told to come back shortly.
fn retry_after_for_turn_error(error: &anyhow::Error) -> Option<u64> {
    if let Some(stream_error) = error.downcast_ref::<StreamError>() {
        return stream_error.retry_after_secs;
    }
    error.downcast_ref::<crate::agent::ResumeCommitFailed>().map(|_| 1)
}

async fn append_context_message(
    id: u64,
    content: &str,
    images: Vec<(String, String)>,
    client_session_id: &str,
    client_is_processing: bool,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let Ok(mut agent) = agent.try_lock() else {
        send_agent_busy_error(
            id,
            "context_message",
            client_session_id,
            client_is_processing,
            client_event_tx,
        );
        return;
    };
    let result = agent.append_user_context_message(content, images);
    let event = match result {
        Ok(()) => ServerEvent::ContextMessageAdded { id },
        Err(error) => ServerEvent::Error {
            id,
            message: crate::util::format_error_chain(&error),
            retry_after_secs: retry_after_for_turn_error(&error),
        },
    };
    let _ = client_event_tx.send(event);
}

#[allow(clippy::too_many_arguments)]
async fn start_processing_message(
    message: ProcessingMessage,
    client_session_id: &str,
    turn_owner: Option<&str>,
    state: &mut ProcessingState<'_>,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
    processing_done_tx: &mpsc::UnboundedSender<(u64, Result<()>, Option<String>)>,
    client_terminal_env: Vec<(String, String)>,
    swarm: &SwarmStatusRefs<'_>,
) {
    let ProcessingMessage {
        id,
        content,
        images,
        system_reminder,
        active_skill,
        v2,
    } = message;
    if server_reload_starting() {
        crate::logging::info(&format!(
            "Rejecting new message for session {} because server reload is starting",
            client_session_id
        ));
        let _ = client_event_tx.send(ServerEvent::Reloading { new_socket: None });
        return;
    }

    if *state.client_is_processing {
        let _ = client_event_tx.send(ServerEvent::Error {
            id,
            message: "Already processing a message".to_string(),
            retry_after_secs: None,
        });
        return;
    }

    // A v2 prompt never waits for the agent here: its turn task does, so this
    // connection keeps reading and can still withdraw the prompt while another
    // surface's turn holds the agent. The skill check moves into that task.
    if v2.is_none() && !agent.lock().await.set_remote_active_skill(active_skill.clone()) {
        let skill_name = active_skill.as_deref().unwrap_or_default();
        let _ = client_event_tx.send(ServerEvent::Error {
            id,
            message: format!("Skill '{skill_name}' is not installed on the server"),
            retry_after_secs: None,
        });
        return;
    }

    let claim_session_id = client_session_id.to_string();
    let turn_owner = turn_owner.map(str::to_string);

    *state.client_is_processing = true;
    *state.message_id = Some(id);
    *state.session_id = Some(client_session_id.to_string());

    if let Some(reminder) = system_reminder.as_deref()
        && let Err(error) = super::reload_recovery::mark_delivered_if_matching_continuation(
            client_session_id,
            reminder,
            "client_message_accepted",
        )
    {
        crate::logging::warn(&format!(
            "Failed to mark reload recovery intent delivered for accepted message session={} id={}: {}",
            client_session_id, id, error
        ));
    }

    update_member_status(
        client_session_id,
        "running",
        Some(truncate_detail(&content, 120)),
        swarm.members,
        swarm.swarms_by_id,
        Some(swarm.event_history),
        Some(swarm.event_counter),
        Some(swarm.event_tx),
    )
    .await;

    let prepared = if v2.is_none() {
        let agent_guard = agent.lock().await;
        Some((agent_guard.message_count(), agent_guard.soft_interrupt_queue()))
    } else {
        None
    };
    let fallback_queue = prepared.as_ref().map(|(_, queue)| queue.clone());
    let on_inserted = v2.as_ref().map(|_| {
        let inserted_tx = client_event_tx.clone();
        Box::new(move |message_id: &str| {
            let _ = inserted_tx.send(ServerEvent::UserMessageInserted {
                id,
                message_id: message_id.to_string(),
            });
        }) as super::foreground_state::InsertionCallback
    });
    let agent = Arc::clone(agent);
    let tx = super::state::session_event_fanout_sender_with_fallback(
        client_session_id.to_string(),
        Arc::clone(swarm.members),
        client_event_tx.clone(),
    );
    let rejection_tx = client_event_tx.clone();
    let done_tx = processing_done_tx.clone();
    // Moved into the task and closed after its terminal event: the idle
    // foreground state is the turn's last event, and an aborted task still
    // reports it.
    let mut foreground = super::foreground_state::ForegroundGuard::new(&tx);
    let report = foreground.report(on_inserted);
    if let Some(message) = v2.as_ref() {
        let _ = message.idle.set(foreground.idle_handle());
    }
    crate::logging::info(&format!("Processing message id={} spawning task", id));
    let turn_acquired = Arc::clone(state.turn_acquired);
    turn_acquired.store(false, Ordering::Release);
    *state.task = Some(tokio::spawn(async move {
        let event_tx = tx.clone();
        let mut stop_reason = crate::protocol::TurnStopReason::Failure;
        // Held until this turn's terminal event and idle state reached every
        // attachment, also after a panic, a cancel that holds the idle state, or
        // an abort: the agent passes to the next queued turn (another surface's,
        // a v2 prompt's) only then, so no connection sees that turn start before
        // this one ended.
        let mut agent = foreground.keep_until_forwarded({
            let lock = agent.lock_owned().await;
            turn_acquired.store(true, Ordering::Release);
            lock
        });
        let locked = &mut agent;
        let turn = async move {
            let agent = &mut ***locked;
            let (start_message_index, pending_queue) = match prepared {
                Some(prepared) => prepared,
                None => {
                    if v2.as_ref().is_some_and(|message| !message.claim()) {
                        return TurnBegin::Withdrawn;
                    }
                    if !agent.set_remote_active_skill(active_skill.clone()) {
                        let skill_name = active_skill.as_deref().unwrap_or_default();
                        return TurnBegin::Rejected(format!("Skill '{skill_name}' is not installed on the server"));
                    }
                    (agent.message_count(), agent.soft_interrupt_queue())
                }
            };
            // Ownership is claimed only once this turn holds the agent, and the
            // guard drops before that lock is released. Claiming earlier left a
            // window in which a daemon wake turn could take the idle agent and
            // run its tools under this connection's claim (bridges, approval
            // override). The guard drops when the turn ends, fails, or is
            // aborted, so no later turn inherits it.
            let _turn_ownership = crate::tool::acp_session::claim_turn(&claim_session_id, turn_owner.as_deref());
            let result =
                process_locked_message_streaming_mpsc(agent, &content, images, system_reminder, event_tx, Some(report))
                    .await;
            TurnBegin::Ran {
                result,
                start_message_index,
                pending_queue,
            }
        };
        let outcome = std::panic::AssertUnwindSafe(crate::hooks::with_client_terminal_env(client_terminal_env, turn))
            .catch_unwind()
            .await;
        let (result, start_message_index, pending_queue) = match outcome {
            Ok(TurnBegin::Ran {
                result,
                start_message_index,
                pending_queue,
            }) => (result, Some(start_message_index), Some(pending_queue)),
            Ok(TurnBegin::Withdrawn) => return,
            Ok(TurnBegin::Rejected(message)) => {
                let _ = rejection_tx.send(ServerEvent::Error {
                    id,
                    message,
                    retry_after_secs: None,
                });
                let _ = done_tx.send((id, Ok(()), None));
                return;
            }
            Err(panic_payload) => {
                stop_reason = crate::protocol::TurnStopReason::Crash;
                let msg = if let Some(text) = panic_payload.downcast_ref::<&str>() {
                    text.to_string()
                } else if let Some(text) = panic_payload.downcast_ref::<String>() {
                    text.clone()
                } else {
                    "unknown panic".to_string()
                };
                crate::logging::error(&format!("Processing task PANICKED for message id={}: {}", id, msg));
                (
                    Err(anyhow::anyhow!("Processing task panicked: {}", msg)),
                    None,
                    fallback_queue,
                )
            }
        };
        foreground.finish(result.is_ok());
        match &result {
            Ok(()) => crate::logging::info(&format!("Processing task completed OK for message id={}", id)),
            Err(error) => crate::logging::warn(&format!(
                "Processing task completed with error for message id={}: {}",
                id, error
            )),
        }
        let completion_report = match (result.is_ok(), start_message_index) {
            (true, Some(start_message_index)) => agent.latest_assistant_text_after(start_message_index),
            _ => None,
        };
        // Keep the terminal event on the same ordered fanout channel as the
        // stream. Sending it later from the owning client's event loop could
        // race ahead of the final MessageEnd for newly attached clients.
        if let Err(error) = &result {
            let _ = tx.send(ServerEvent::TurnStopped {
                reason: stop_reason,
                message: crate::util::format_error_chain(error),
                provider_stop_reason: None,
            });
        }
        let terminal_event = match &result {
            Ok(()) => ServerEvent::Done { id },
            Err(error) => ServerEvent::Error {
                id,
                message: crate::util::format_error_chain(error),
                retry_after_secs: retry_after_for_turn_error(error),
            },
        };
        // Interrupts still queued here were never seen by this turn. Report
        // them on the ordered stream so a client can reconcile its own view.
        // Usually zero (the turn injects everything it can before ending), so
        // skip the event then and let clients default to zero.
        let pending = pending_queue
            .and_then(|queue| queue.lock().map(|queue| queue.len()).ok())
            .unwrap_or(0);
        if pending > 0 {
            let _ = tx.send(ServerEvent::SoftInterruptQueue { pending });
        }
        let _ = tx.send(terminal_event);
        // Tell the request loop the turn finished before this task waits for
        // its idle state to be forwarded: the client already has its terminal
        // event, and a prompt it sends next must not meet
        // "Already processing a message". The next turn's task still waits for
        // the agent this task keeps until the idle state is out (close), so its
        // `running` cannot precede this turn's idle.
        let _ = done_tx.send((id, result, completion_report));
        foreground.close(agent).await;
    }));
}

async fn cancel_processing_message(
    state: &mut ProcessingState<'_>,
    session_control: &SessionControlHandle,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
    swarm: &SwarmStatusRefs<'_>,
    request_id: Option<u64>,
    request_decoded_at: Option<Instant>,
    queued_v2: Option<&AcpV2Message>,
) {
    let cancel_start = Instant::now();
    let session_label = state
        .session_id
        .as_deref()
        .unwrap_or(session_control.session_id.as_str())
        .to_string();
    // A v2 prompt still waiting for the agent is taken back first, through the
    // same claim race a withdraw_message uses: it is then never stored or run,
    // and the cancel reply below is its only reply.
    let withdrew_queued_v2 = queued_v2.is_some_and(AcpV2Message::withdraw);
    // Held until this function returns, after the cancel reply (Interrupted,
    // Done) is queued below: a v2 editor's idle state for the running turn, and
    // for this connection's own turn if it already claimed the agent, must come
    // after that reply. Marked cancelled only where a turn is actually stopped.
    let idle_holds: Vec<super::foreground_state::DeferredIdle> =
        super::foreground_state::defer_idle_for_cancel(&session_label)
            .into_iter()
            .chain(
                queued_v2
                    .filter(|_| !withdrew_queued_v2)
                    .and_then(AcpV2Message::defer_idle),
            )
            .collect();
    let mark_stopped = || {
        for hold in &idle_holds {
            hold.mark_cancelled();
        }
    };
    // Stopping a session also stops the workers it spawned. This runs first
    // and on every path, including the idle one: a coordinator waiting on
    // `await_members` has no running turn of its own while its workers do.
    super::swarm::cancel_spawned_descendant_turns(
        &session_control.session_id,
        swarm.members,
        swarm.swarms_by_id,
        Some(swarm.event_history),
        Some(swarm.event_counter),
        Some(swarm.event_tx),
    )
    .await;
    crate::logging::info(&format!(
        "SERVER_INTERRUPT_CANCEL_RECEIVED request_id={:?} session={} control_session={} client_processing={} message_id={:?} has_task={} decoded_age_ms={:?}",
        request_id,
        session_label,
        session_control.session_id,
        *state.client_is_processing,
        *state.message_id,
        state.task.is_some(),
        request_decoded_at.map(|instant| instant.elapsed().as_millis())
    ));
    if let Some(mut handle) = state.task.take() {
        if handle.is_finished() {
            crate::logging::info(&format!(
                "SERVER_INTERRUPT_CANCEL_IGNORED_FINISHED request_id={:?} session={} message_id={:?} total_ms={}",
                request_id,
                session_label,
                *state.message_id,
                cancel_start.elapsed().as_millis()
            ));
            *state.task = Some(handle);
            return;
        }
        let stopped = ServerEvent::TurnStopped {
            reason: crate::protocol::TurnStopReason::Interrupted,
            message: "The turn was interrupted by a cancellation request.".into(),
            provider_stop_reason: None,
        };
        // Publish before signalling: the worker can finish cooperatively and
        // emit Done immediately after request_cancel.
        if super::state::fanout_session_event(swarm.members, &session_label, stopped.clone()).await == 0 {
            let _ = client_event_tx.send(stopped);
        }
        mark_stopped();
        // A turn that never took the agent has nothing to stop cooperatively:
        // it is either still waiting for the lock some other turn holds, or its
        // v2 prompt was just withdrawn. Waiting for it would only burn the
        // cooperative window, because the cancel holds that other turn's idle
        // state, which parks its lock until this cancel returns. Abort it at
        // once, like a withdraw: it reports nothing (it never stored a
        // message), so the reply below is still its only reply.
        let abort_queued_turn = withdrew_queued_v2 || !state.turn_acquired.load(Ordering::Acquire);
        let cancel_epoch = session_control.request_cancel();
        crate::logging::info(&format!(
            "SERVER_INTERRUPT_CANCEL_SIGNALLED request_id={:?} session={} message_id={:?} wait_ms={} acquired={}",
            request_id,
            session_label,
            *state.message_id,
            if abort_queued_turn { 0 } else { 500 },
            !abort_queued_turn
        ));
        if abort_queued_turn {
            crate::logging::info(&format!(
                "SERVER_INTERRUPT_CANCEL_ABORT_QUEUED request_id={:?} session={} message_id={:?} withdrew_queued={} elapsed_ms={}",
                request_id,
                session_label,
                *state.message_id,
                withdrew_queued_v2,
                cancel_start.elapsed().as_millis()
            ));
            handle.abort();
            match tokio::time::timeout(std::time::Duration::from_millis(2000), handle).await {
                Ok(_) => crate::logging::info(&format!(
                    "SERVER_INTERRUPT_CANCEL_ABORT_RELEASED request_id={:?} session={} elapsed_ms={}",
                    request_id,
                    session_label,
                    cancel_start.elapsed().as_millis()
                )),
                Err(_) => crate::logging::warn(&format!(
                    "SERVER_INTERRUPT_CANCEL_ABORT_RELEASE_TIMEOUT request_id={:?} session={} elapsed_ms={} wait_ms=2000",
                    request_id,
                    session_label,
                    cancel_start.elapsed().as_millis()
                )),
            }
            // The cancel still targets the turn that holds the agent: clear the
            // signal later, like the no-local-task path, so that turn can
            // observe it and a prompt sent right after this reply does not start
            // with a stale flag.
            let reset_control = session_control.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                reset_control.reset_cancel_if_epoch(cancel_epoch);
            });
        } else {
            match tokio::time::timeout(std::time::Duration::from_millis(500), &mut handle).await {
                Ok(_) => {
                    crate::logging::info(&format!(
                        "SERVER_INTERRUPT_CANCEL_COOPERATIVE_DONE request_id={:?} session={} message_id={:?} elapsed_ms={}",
                        request_id,
                        session_label,
                        *state.message_id,
                        cancel_start.elapsed().as_millis()
                    ));
                }
                Err(_) => {
                    crate::logging::warn(&format!(
                        "SERVER_INTERRUPT_CANCEL_COOPERATIVE_TIMEOUT request_id={:?} session={} message_id={:?} elapsed_ms={} action=abort_task",
                        request_id,
                        session_label,
                        *state.message_id,
                        cancel_start.elapsed().as_millis()
                    ));
                    handle.abort();
                    match tokio::time::timeout(std::time::Duration::from_millis(2000), handle).await {
                        Ok(_) => crate::logging::info(&format!(
                            "SERVER_INTERRUPT_CANCEL_ABORT_RELEASED request_id={:?} session={} elapsed_ms={}",
                            request_id,
                            session_label,
                            cancel_start.elapsed().as_millis()
                        )),
                        Err(_) => crate::logging::warn(&format!(
                            "SERVER_INTERRUPT_CANCEL_ABORT_RELEASE_TIMEOUT request_id={:?} session={} elapsed_ms={} wait_ms=2000",
                            request_id,
                            session_label,
                            cancel_start.elapsed().as_millis()
                        )),
                    }
                }
            }
            // Only clear the cancel we fired: a newer cancel (repeated Esc, jade
            // relay, another connection) must not be erased before its target
            // observes it (issue #428).
            session_control.reset_cancel_if_epoch(cancel_epoch);
        }
        *state.task = None;
        *state.client_is_processing = false;
        if let Some(session_id) = state.session_id.take() {
            update_member_status(
                &session_id,
                "stopped",
                Some("cancelled".to_string()),
                swarm.members,
                swarm.swarms_by_id,
                Some(swarm.event_history),
                Some(swarm.event_counter),
                Some(swarm.event_tx),
            )
            .await;
        }
        if let Some(message_id) = state.message_id.take() {
            let _ = client_event_tx.send(ServerEvent::Interrupted);
            let _ = client_event_tx.send(ServerEvent::Done { id: message_id });
            crate::logging::info(&format!(
                "SERVER_INTERRUPT_CANCEL_EVENTS_EMITTED request_id={:?} session={} interrupted=true done_id={} total_ms={}",
                request_id,
                session_label,
                message_id,
                cancel_start.elapsed().as_millis()
            ));
        }
    } else {
        crate::logging::warn(&format!(
            "SERVER_INTERRUPT_CANCEL_NO_LOCAL_TASK request_id={:?} session={} control_session={} client_processing={} message_id={:?}; signalling session cancel handle anyway",
            request_id, session_label, session_control.session_id, *state.client_is_processing, *state.message_id
        ));
        // Nothing is running anywhere for this session, so there is no turn to
        // interrupt and arming the signal can only harm the *next* one: the
        // deferred reset below runs 500ms later, and a message sent inside
        // that window starts with the cancel flag already set and dies
        // immediately, with no reply and no error. Report the interrupt and
        // stop. Sessions whose turn is owned by another connection still take
        // the signalling path, since the registry sees those turns.
        if !crate::turn_cancel_registry::has_active_turn(&session_control.session_id) {
            crate::logging::info(&format!(
                "SERVER_INTERRUPT_CANCEL_IDLE_NOOP request_id={:?} session={}",
                request_id, session_label
            ));
            *state.client_is_processing = false;
            let _ = client_event_tx.send(ServerEvent::Interrupted);
            if let Some(message_id) = state.message_id.take() {
                let _ = client_event_tx.send(ServerEvent::Done { id: message_id });
            }
            return;
        }
        let stopped = ServerEvent::TurnStopped {
            reason: crate::protocol::TurnStopReason::Interrupted,
            message: "The turn was interrupted by a cancellation request.".into(),
            provider_stop_reason: None,
        };
        // Publish before signalling: the worker can finish cooperatively and
        // emit Done immediately after request_cancel.
        if super::state::fanout_session_event(swarm.members, &session_label, stopped.clone()).await == 0 {
            let _ = client_event_tx.send(stopped);
        }
        mark_stopped();
        let cancel_epoch = session_control.request_cancel();
        let reset_control = session_control.clone();
        tokio::spawn(async move {
            // The running turn is not owned by this connection (post-reload
            // recovery, server-initiated turn, or attach), so we cannot await
            // it. Clear the flag later so the *next* turn is not aborted by a
            // stale cancel, but only if no newer cancel fired in the meantime:
            // an unconditional reset here used to erase rapid repeated Esc
            // cancels before the busy turn observed them (issue #428).
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            reset_control.reset_cancel_if_epoch(cancel_epoch);
        });
        *state.client_is_processing = false;
        let status_session_id = state
            .session_id
            .take()
            .unwrap_or_else(|| session_control.session_id.clone());
        update_member_status(
            &status_session_id,
            "stopped",
            Some("cancelled".to_string()),
            swarm.members,
            swarm.swarms_by_id,
            Some(swarm.event_history),
            Some(swarm.event_counter),
            Some(swarm.event_tx),
        )
        .await;
        let _ = client_event_tx.send(ServerEvent::Interrupted);
        if let Some(message_id) = state.message_id.take() {
            let _ = client_event_tx.send(ServerEvent::Done { id: message_id });
            crate::logging::info(&format!(
                "SERVER_INTERRUPT_CANCEL_EVENTS_EMITTED request_id={:?} session={} interrupted=true done_id={} total_ms={}",
                request_id,
                session_label,
                message_id,
                cancel_start.elapsed().as_millis()
            ));
        } else {
            crate::logging::info(&format!(
                "SERVER_INTERRUPT_CANCEL_EVENTS_EMITTED request_id={:?} session={} interrupted=true done_id=None total_ms={}",
                request_id,
                session_label,
                cancel_start.elapsed().as_millis()
            ));
        }
    }
}

fn try_available_models_snapshot(agent: &Arc<Mutex<Agent>>) -> Option<Arc<str>> {
    Some(Arc::clone(&try_available_models_catalog(agent)?.dedup_key))
}

async fn queue_soft_interrupt(
    id: u64,
    content: String,
    images: Vec<(String, String)>,
    urgent: bool,
    client_id: Option<String>,
    session_control: &SessionControlHandle,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let source = SoftInterruptSource::User;
    let content_bytes = content.len();
    let content_chars = content.chars().count();
    crate::logging::info(&format!(
        "SERVER_SOFT_INTERRUPT_QUEUE_REQUEST id={} session={} source={:?} urgent={} content_bytes={} content_chars={} client_id={}",
        id,
        session_control.session_id,
        source,
        urgent,
        content_bytes,
        content_chars,
        client_id.is_some()
    ));
    let message = horon_agent_agent_runtime::SoftInterruptMessage {
        content,
        images,
        urgent,
        source,
        client_id,
        input_id: None,
    };
    let accepted = match crate::durable_input_runtime::accept_message(&session_control.session_id, message).await {
        Ok(accepted) => accepted,
        Err(err) => {
            crate::logging::warn(&format!(
                "SERVER_SOFT_INTERRUPT_QUEUE_ACCEPT_FAILED id={} session={} error={}",
                id, session_control.session_id, err
            ));
            let _ = client_event_tx.send(ServerEvent::Error {
                id,
                message: format!("the interrupt was not accepted durably: {err}"),
                retry_after_secs: Some(1),
            });
            return;
        }
    };
    let queued = session_control.enqueue_message(accepted);
    let ack_queued = if queued {
        client_event_tx.send(ServerEvent::Ack { id }).is_ok()
    } else {
        let _ = client_event_tx.send(ServerEvent::Error {
            id,
            message: "the interrupt queue is unavailable".to_string(),
            retry_after_secs: Some(1),
        });
        false
    };
    crate::logging::info(&format!(
        "SERVER_SOFT_INTERRUPT_QUEUE_RESULT id={} session={} queued={} ack_queued={}",
        id, session_control.session_id, queued, ack_queued
    ));
}

async fn cancel_soft_interrupts_by_id(
    id: u64,
    session_id: &str,
    client_ids: &[String],
    session_control: &SessionControlHandle,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    // The journal records the cancellation before the queue forgets the
    // message, so a restart cannot resurrect an interrupt the user cancelled.
    let (durable_cancelled, durable_not_live) =
        crate::durable_input_runtime::cancel_by_client_ids(session_id, client_ids.to_vec()).await;
    let (memory_cancelled, not_queued) = session_control.cancel_soft_interrupts_by_id(&durable_not_live);
    let mut cancelled: Vec<String> = durable_cancelled;
    for client_id in memory_cancelled {
        if !cancelled.contains(&client_id) {
            cancelled.push(client_id);
        }
    }
    let mut not_queued = not_queued;
    // An interrupt sent while no live queue was reachable is parked on disk
    // until the session restores it. It is just as cancellable.
    if !not_queued.is_empty() {
        match crate::soft_interrupt_store::remove_by_client_ids(session_id, &not_queued) {
            Ok(removed) if !removed.is_empty() => {
                not_queued.retain(|id| !removed.contains(id));
                for client_id in removed {
                    if !cancelled.contains(&client_id) {
                        cancelled.push(client_id);
                    }
                }
            }
            Ok(_) => {}
            Err(err) => crate::logging::warn(&format!(
                "SERVER_SOFT_INTERRUPT_CANCEL_BY_ID_PERSISTED_FAILED id={} session={} error={}",
                id, session_id, err
            )),
        }
    }
    cancelled.sort();
    let _ = client_event_tx.send(ServerEvent::SoftInterruptsCancelled {
        id,
        cancelled,
        not_queued,
    });
}

async fn clear_soft_interrupts(
    id: u64,
    session_id: &str,
    session_control: &SessionControlHandle,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    crate::logging::info(&format!(
        "SERVER_SOFT_INTERRUPT_CLEAR_REQUEST id={} session={} control_session={}",
        id, session_id, session_control.session_id
    ));
    crate::durable_input_runtime::clear_pending(session_id).await;
    session_control.clear_soft_interrupts();
    let persisted_clear = match crate::soft_interrupt_store::clear(session_id) {
        Ok(()) => true,
        Err(err) => {
            crate::logging::warn(&format!(
                "SERVER_SOFT_INTERRUPT_CLEAR_PERSISTED_FAILED id={} session={} error={}",
                id, session_id, err
            ));
            false
        }
    };
    let ack_queued = client_event_tx.send(ServerEvent::Ack { id }).is_ok();
    crate::logging::info(&format!(
        "SERVER_SOFT_INTERRUPT_CLEAR_RESULT id={} session={} persisted_clear={} ack_queued={}",
        id, session_id, persisted_clear, ack_queued
    ));
}

pub(crate) fn move_tool_to_background(
    id: u64,
    session_control: &SessionControlHandle,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    crate::logging::info(&format!(
        "SERVER_BACKGROUND_TOOL_REQUEST id={} session={}",
        id, session_control.session_id
    ));
    let running = crate::running_tool_registry::current(&session_control.session_id);
    // Fire regardless: the agent resets the signal before every tool wait, so
    // an idle fire is harmless, and this keeps the pre-#1778 behavior.
    let signalled = session_control.request_background_current_tool();
    let ack_queued = client_event_tx.send(ServerEvent::Ack { id }).is_ok();
    let running = running.filter(|_| signalled);
    let moved = running.is_some();
    let (tool_call_id, tool_name) = match running {
        Some(tool) => (Some(tool.call_id), Some(tool.name)),
        None => (None, None),
    };
    let _ = client_event_tx.send(ServerEvent::BackgroundToolResult {
        id,
        moved,
        tool_call_id,
        tool_name,
    });
    crate::logging::info(&format!(
        "SERVER_BACKGROUND_TOOL_RESULT id={} session={} signalled={} ack_queued={}",
        id, session_control.session_id, signalled, ack_queued
    ));
}

/// Process a message and stream events (mpsc channel - per-client)
pub(super) async fn process_message_streaming_mpsc(
    agent: Arc<Mutex<Agent>>,
    content: &str,
    images: Vec<(String, String)>,
    system_reminder: Option<String>,
    event_tx: tokio::sync::mpsc::UnboundedSender<ServerEvent>,
) -> Result<()> {
    // These callers send no terminal event of their own, so the idle state the
    // guard reports when this returns (or is dropped) is the turn's last event.
    let mut foreground = super::foreground_state::ForegroundGuard::new(&event_tx);
    let report = foreground.report(None);
    // The agent stays this turn's until the idle state reached the session, so
    // the next turn (a client prompt, another surface's, a wake) starts only
    // after every attachment has this turn's end: its `running` cannot precede
    // that idle state, and a cancel-held idle keeps its `cancelled` reason
    // instead of being overtaken. The sender must be one that reports
    // forwarding (a session fanout sender); the wait is bounded otherwise.
    let mut agent = foreground.keep_until_forwarded(agent.lock_owned().await);
    let result =
        process_locked_message_streaming_mpsc(&mut agent, content, images, system_reminder, event_tx, Some(report))
            .await;
    foreground.finish(result.is_ok());
    foreground.close(agent).await;
    result
}

/// Same as [`process_message_streaming_mpsc`] for a caller that already holds
/// the agent lock (e.g. a wake turn that reserved the idle agent up front, see
/// #1152).
pub(super) async fn process_locked_message_streaming_mpsc(
    agent: &mut Agent,
    content: &str,
    images: Vec<(String, String)>,
    system_reminder: Option<String>,
    event_tx: tokio::sync::mpsc::UnboundedSender<ServerEvent>,
    report: Option<super::foreground_state::TurnReport>,
) -> Result<()> {
    let session_id = agent.session_id().to_string();
    let result = agent
        .run_once_streaming_mpsc_with_display_role(content, images, system_reminder, event_tx, None, report)
        .await;
    if result.is_ok() {
        crate::runtime_memory_log::emit_event(
            crate::runtime_memory_log::RuntimeMemoryLogEvent::new("turn_completed", "message_turn_finished")
                .with_session_id(session_id)
                .force_attribution(),
        );
        crate::process_memory::release_retained_heap_debounced(
            "server_turn_completed",
            std::time::Duration::from_secs(30),
        );
    }
    result
}

/// Clear the durable `pending_event` of the approval a client just acked.
///
/// A record that is already gone is success, not an error: the ack can arrive
/// after the runner cleaned the record up, and failing the client would only
/// make it retry an ack that has nothing left to clear. Only `pending_event`
/// changes; the decision fields the resume classifier reads stay untouched.
fn handle_approval_ack(id: u64, request_id: String, client_event_tx: &mpsc::UnboundedSender<ServerEvent>) {
    let cleared = crate::ambient::BlockedApprovalStore::open_default()
        .and_then(|store| store.update_with(&request_id, |record| record.pending_event = None));
    match cleared {
        Ok(record_found) => {
            crate::logging::event_info(
                "server_approval_ack",
                vec![
                    ("id", id.to_string()),
                    ("request_id", request_id),
                    ("record_found", record_found.to_string()),
                ],
            );
            let _ = client_event_tx.send(ServerEvent::Ack { id });
        }
        Err(error) => {
            crate::logging::warn(&format!(
                "approval ack for request {request_id} could not be applied: {error}"
            ));
            let _ = client_event_tx.send(ServerEvent::Error {
                id,
                message: format!("approval ack could not be applied: {error}"),
                retry_after_secs: None,
            });
        }
    }
}

/// Drop the durable failover announcement a client just acked.
///
/// The announcement is scoped to the attached session, and the whole switch key
/// has to match: an ack naming a different chain, route or attempt leaves every
/// entry in place. A clear that genuinely could not run (the lock stayed held,
/// or the rewrite failed) is answered with `Error`, never with `Ack`: an `Ack`
/// tells the client to stop retrying, which would leave the entry on disk and
/// re-deliver it on every attach and restart. `Ok(0)` — nothing matched — is
/// still success, because retrying an ack with nothing left to clear cannot help.
fn handle_failover_ack(
    id: u64,
    session_id: &str,
    chain_id: String,
    from: String,
    to: String,
    attempt: u32,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let cleared = super::state::try_clear_acked_failover_event(session_id, &chain_id, &from, &to, attempt);
    match cleared {
        Ok(removed) => {
            crate::logging::event_info(
                "server_failover_ack",
                vec![
                    ("id", id.to_string()),
                    ("session_id", session_id.to_string()),
                    ("chain_id", chain_id),
                    ("from", from),
                    ("to", to),
                    ("attempt", attempt.to_string()),
                    ("removed", removed.to_string()),
                ],
            );
            let _ = client_event_tx.send(ServerEvent::Ack { id });
        }
        Err(error) => {
            crate::logging::warn(&format!(
                "failover ack for chain {chain_id} attempt {attempt} could not be applied: {error}"
            ));
            let _ = client_event_tx.send(ServerEvent::Error {
                id,
                message: format!("failover ack could not be applied: {error}"),
                retry_after_secs: None,
            });
        }
    }
}

#[cfg(test)]
#[path = "client_lifecycle_ack_tests.rs"]
mod ack_transport_tests;

/// Failure transport of the failover ack: a clear that could not run must not be
/// reported as an ack, or the client stops retrying and the announcement is
/// replayed on every attach and restart.
#[cfg(test)]
mod ack_transport_failure_tests {
    use super::*;
    use crate::auth::test_sandbox::AuthTestSandbox;
    use crate::server::state::{EmitOutcome, PendingFailoverEvent, emit_session_event};

    fn switch_event(chain_id: &str, from: &str, to: &str, attempt: u32) -> ServerEvent {
        ServerEvent::FailoverSwitched {
            chain_id: chain_id.to_string(),
            from: from.to_string(),
            to: to.to_string(),
            reason: "429 Too Many Requests".to_string(),
            attempt,
            max: 3,
        }
    }

    fn stored_switches(sandbox: &AuthTestSandbox) -> Vec<PendingFailoverEvent> {
        let path = sandbox.root().join("failover-events.json");
        if !path.exists() {
            return Vec::new();
        }
        crate::storage::read_json::<Vec<PendingFailoverEvent>>(&path).expect("the announcement file is valid JSON")
    }

    /// A second ack for the same key finds nothing left to clear. That is still
    /// success: the entry is already gone and retrying cannot help, so the
    /// client must not be pushed into an ack loop over it.
    #[test]
    fn a_duplicate_ack_still_succeeds_and_removes_nothing() {
        let sandbox = AuthTestSandbox::new().expect("isolated home");
        let session_id = "sess-ack-duplicate";
        assert_eq!(
            emit_session_event(session_id, switch_event("chain-1", "model-a", "model-b", 1)),
            EmitOutcome::NoSink
        );

        let (tx, mut rx) = mpsc::unbounded_channel();
        for id in [500, 501] {
            handle_failover_ack(
                id,
                session_id,
                "chain-1".to_string(),
                "model-a".to_string(),
                "model-b".to_string(),
                1,
                &tx,
            );
            assert!(
                matches!(rx.try_recv(), Ok(ServerEvent::Ack { id: replied }) if replied == id),
                "the first ack clears the entry and the duplicate ack is not an error"
            );
        }

        assert!(
            stored_switches(&sandbox).is_empty(),
            "the acknowledged entry is gone and the duplicate changed nothing"
        );
    }

    /// The lock is held for the whole retry budget, so the clear cannot run. The
    /// handler must say so and the entry must survive; the lock is taken by this
    /// process on a separate file descriptor, which is exactly what the
    /// cross-process lock sees, so the contention is not a timing race.
    #[test]
    fn a_held_announcement_lock_is_an_error_not_an_ack() {
        let sandbox = AuthTestSandbox::new().expect("isolated home");
        let session_id = "sess-ack-lock-held";
        assert_eq!(
            emit_session_event(session_id, switch_event("chain-1", "model-a", "model-b", 1)),
            EmitOutcome::NoSink
        );
        assert_eq!(
            stored_switches(&sandbox).len(),
            1,
            "precondition: one pending announcement"
        );

        let lock_path = sandbox.root().join("failover-events.lock");
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .expect("open the announcement lock");
        lock.try_lock().expect("this test holds the announcement lock");

        let (tx, mut rx) = mpsc::unbounded_channel();
        handle_failover_ack(
            400,
            session_id,
            "chain-1".to_string(),
            "model-a".to_string(),
            "model-b".to_string(),
            1,
            &tx,
        );

        assert!(
            matches!(rx.try_recv(), Ok(ServerEvent::Error { id: 400, .. })),
            "a clear that never ran must be reported as an error, not an ack"
        );
        assert_eq!(
            stored_switches(&sandbox).len(),
            1,
            "the entry a client was told was cleared is still on disk"
        );

        lock.unlock().expect("release the announcement lock");
    }

    /// The rewrite cannot land because the storage directory is read-only. The
    /// entry survives, so the reply must be an error.
    #[test]
    #[cfg(unix)]
    fn a_failed_announcement_rewrite_is_an_error_not_an_ack() {
        use std::os::unix::fs::PermissionsExt;

        struct RestorePermissions(std::path::PathBuf);
        impl Drop for RestorePermissions {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
            }
        }

        let sandbox = AuthTestSandbox::new().expect("isolated home");
        let session_id = "sess-ack-read-only";
        assert_eq!(
            emit_session_event(session_id, switch_event("chain-1", "model-a", "model-b", 1)),
            EmitOutcome::NoSink
        );
        assert_eq!(
            stored_switches(&sandbox).len(),
            1,
            "precondition: one pending announcement"
        );

        let _restore = RestorePermissions(sandbox.root().to_path_buf());
        std::fs::set_permissions(sandbox.root(), std::fs::Permissions::from_mode(0o500))
            .expect("make the storage directory read-only");

        let (tx, mut rx) = mpsc::unbounded_channel();
        handle_failover_ack(
            600,
            session_id,
            "chain-1".to_string(),
            "model-a".to_string(),
            "model-b".to_string(),
            1,
            &tx,
        );

        assert!(
            matches!(rx.try_recv(), Ok(ServerEvent::Error { id: 600, .. })),
            "a rewrite that failed must be reported as an error, not an ack"
        );
        assert_eq!(
            stored_switches(&sandbox).len(),
            1,
            "the entry a client was told was cleared is still on disk"
        );
    }
}

#[cfg(test)]
#[path = "client_lifecycle_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "client_target_attach_tests.rs"]
mod target_attach_tests;
