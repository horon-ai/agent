#![cfg_attr(test, allow(clippy::await_holding_lock))]

mod advisor;
#[cfg(test)]
#[path = "agent/advisor_tests.rs"]
mod advisor_tests;
mod compaction;
mod conversation_reset;
#[cfg(test)]
#[path = "agent/durable_input_tests.rs"]
mod durable_input_tests;
mod environment;
mod failover_budget;
mod final_commit;
mod goal_continuation;
#[cfg(test)]
#[path = "agent/goal_continuation_tests.rs"]
mod goal_continuation_tests;
mod idle_hibernation;
mod inline_tail;
mod interrupts;
mod loop_guard;
#[cfg(test)]
mod loop_guard_tests;
mod messages;
#[cfg(test)]
mod model_usage_tests;
#[cfg(test)]
#[path = "agent/post_tool_feedback_replace_tests.rs"]
mod post_tool_feedback_replace_tests;
mod prompting;
mod provider;
mod provisional_init;
#[cfg(test)]
#[path = "agent/pruning_tests.rs"]
mod pruning_tests;
mod request_messages;
#[cfg(test)]
pub(crate) use request_messages::request_extra_clone_counters;
pub(crate) use request_messages::{join_request_messages, record_request_extra_clone};
mod response_recovery;
mod resume_commit;
mod round_budget;
mod session_replication;
#[cfg(test)]
#[path = "agent/soft_interrupt_commit_tests.rs"]
mod soft_interrupt_commit_tests;
mod status;
mod stream_rules;
mod streaming;
#[cfg(test)]
mod streaming_clone_compaction_tests;
mod tool_markup_recovery;
mod tool_result_aging;
mod tool_round;
#[cfg(test)]
mod tool_round_tests;
mod tools;
mod turn_execution;
mod turn_failover;
mod turn_loops;
#[cfg(test)]
mod turn_recovery_tests;
mod turn_recovery_wiring;
#[cfg(test)]
mod turn_start_tests;
#[cfg(test)]
#[path = "agent/turn_state_transition_tests.rs"]
mod turn_state_transition_tests;
mod turn_streaming_mpsc;
mod utils;

pub(crate) use self::resume_commit::{ResumeCommitFailed, resume_commit_owed};
pub(crate) use self::turn_recovery_wiring::AutoContinuation;

use self::streaming::{send_stream_keepalive_mpsc, stream_keepalive_ticker};
#[cfg(test)]
use self::tools::tool_output_to_content_blocks;
use self::tools::{
    cap_sdk_tool_content_for_history, cap_tool_output_for_history, print_tool_summary, tool_output_side_pane_images,
    tool_output_to_history_blocks,
};
use self::utils::trace_enabled;
use crate::build;
use crate::bus::{Bus, BusEvent, SubagentStatus, ToolEvent, ToolStatus};
use crate::cache_tracker::CacheTracker;
use crate::compaction::CompactionEvent;
use crate::id;
use crate::logging;
use crate::message::{ContentBlock, Message, Role, StreamEvent, TOOL_OUTPUT_MISSING_TEXT, ToolCall, ToolDefinition};
use crate::protocol::{HistoryMessage, ServerEvent};
use crate::provider::{NativeToolResult, Provider, ProviderRuntimeState};
use crate::session::{GitState, Session, SessionStatus, StoredDisplayRole, StoredMessage};
use crate::skill::SkillRegistry;
use crate::tool::{Registry, ToolContext, ToolExecutionMode};
use anyhow::Result;
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub use horon_agent_agent_runtime::{
    BackgroundToolSignal, GracefulShutdownSignal, InterruptSignal, SoftInterruptMessage, SoftInterruptQueue,
    SoftInterruptSource, StreamError, TurnOutcome, TurnState, outcome_for_stop_reason,
};
use interrupts::{InFlightGuard, NoToolCallOutcome, PostToolInterruptOutcome};

const HORON_AGENT_NATIVE_TOOLS: &[&str] = &["communicate"];
static RECOVERED_TEXT_WRAPPED_TOOL_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static HORON_AGENT_REPO_SOURCE_STATE: LazyLock<(Option<String>, Option<bool>)> = LazyLock::new(|| {
    crate::build::get_repo_dir()
        .map(|repo_dir| {
            (
                build::current_git_hash(&repo_dir).ok(),
                build::is_working_tree_dirty(&repo_dir).ok(),
            )
        })
        .unwrap_or((None, None))
});
static WORKING_GIT_STATE_CACHE: LazyLock<StdMutex<HashMap<PathBuf, Option<GitState>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));
const STREAM_KEEPALIVE_PONG_ID: u64 = 0;

fn stable_hash_str(value: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn stable_hash_json<T: serde::Serialize + ?Sized>(value: &T) -> u64 {
    let encoded = serde_json::to_string(value).unwrap_or_default();
    stable_hash_str(&encoded)
}

fn stable_json_len<T: serde::Serialize + ?Sized>(value: &T) -> usize {
    serde_json::to_string(value)
        .map(|encoded| encoded.len())
        .unwrap_or_default()
}

/// Hash the cache-relevant projection, not the raw `Message`.
///
/// Raw hashing keys off non-transmitted metadata (timestamp,
/// `tool_duration_ms`, `ReasoningTrace` blocks, `cache_control` markers), which
/// triggers spurious `harness:_prefix_changed` KV-cache miss reports when the
/// same message is re-serialized with backfilled metadata on the next turn.
fn projected_message_hash(projected: &serde_json::Value) -> u64 {
    let encoded = serde_json::to_string(projected).unwrap_or_default();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&encoded, &mut hasher);
    std::hash::Hasher::finish(&hasher)
}

/// Slice-based form of [`kv_cache_request_event_ref`], kept as the reference
/// implementation the request-preparation tests compare against.
#[cfg(test)]
fn kv_cache_request_event(
    messages: &[Message],
    tools: &[ToolDefinition],
    system_static: &str,
    ephemeral_messages: &[Message],
) -> ServerEvent {
    kv_cache_request_event_ref(
        messages.iter(),
        messages.len(),
        tools,
        system_static,
        ephemeral_messages,
    )
}

/// Build the KV-cache signature event from borrowed messages.
///
/// The turn already owns one request snapshot, so the signature is derived from
/// references into it plus whatever small suffix was appended. Taking an
/// iterator keeps the canonical serialization (`cache_relevant_message_value`,
/// per-message hashes over that projection, and the raw array length) byte for
/// byte identical to the slice-based form, which `kv_cache_monitor` keeps
/// consuming unchanged.
fn kv_cache_request_event_ref<'a, I>(
    messages: I,
    message_count: usize,
    tools: &[ToolDefinition],
    system_static: &str,
    ephemeral_messages: &[Message],
) -> ServerEvent
where
    I: IntoIterator<Item = &'a Message>,
{
    // Provider-native deferred definitions are outside the cached prefix.
    // Late MCP discovery must not look like a harness-induced tools change
    // when the provider reports a cache miss for some unrelated reason.
    // Borrow definitions rather than cloning potentially large MCP schemas.
    let eager_tools: Vec<_> = tools.iter().filter(|tool| !tool.defer_loading).collect();
    let ephemeral_hash = if ephemeral_messages.is_empty() {
        None
    } else {
        Some(stable_hash_json(ephemeral_messages))
    };
    let mut projected: Vec<serde_json::Value> = Vec::with_capacity(message_count);
    let mut hashes: Vec<u64> = Vec::with_capacity(message_count);
    // `serde_json` renders an array as `[a,b]`, so the array's length is the
    // sum of its item lengths plus the brackets and separators. Accumulating it
    // here keeps `messages_json_chars` identical to serializing the whole array
    // without allocating that array string a second time.
    let mut messages_json_chars = 2usize;
    let mut counted = 0usize;
    for message in messages {
        let value = crate::message::cache_relevant_message_value(message);
        hashes.push(projected_message_hash(&value));
        projected.push(value);
        if counted > 0 {
            messages_json_chars += 1;
        }
        messages_json_chars += stable_json_len(message);
        counted += 1;
    }
    debug_assert_eq!(counted, message_count, "cache signature message count drifted");
    ServerEvent::KvCacheRequest {
        system_static_hash: stable_hash_str(system_static),
        tools_hash: stable_hash_json(&eager_tools),
        messages_hash: stable_hash_json(&projected),
        message_hashes: hashes,
        message_count: counted,
        tool_count: eager_tools.len(),
        system_static_chars: system_static.chars().count(),
        tools_json_chars: stable_json_len(&eager_tools),
        messages_json_chars,
        ephemeral_hash,
        ephemeral_chars: stable_json_len(ephemeral_messages),
        ephemeral_message_count: ephemeral_messages.len(),
    }
}

impl Agent {
    /// Provider identity used for cache retention lookups. Generic OpenAI is
    /// only refined when the credential mode is explicitly pinned.
    fn kv_cache_provider_identity(&self) -> String {
        let name = self.provider.name().to_string();
        if !name.eq_ignore_ascii_case("openai") {
            return name;
        }
        match self.provider.active_explicit_credential() {
            Some(horon_agent_provider_core::ResolvedCredential::ApiKey) => "openai-api".into(),
            Some(horon_agent_provider_core::ResolvedCredential::Oauth) | None => name,
        }
    }

    fn begin_kv_cache_monitor_request(&mut self, event: &ServerEvent, model: &str) {
        let ServerEvent::KvCacheRequest {
            system_static_hash,
            tools_hash,
            messages_hash,
            message_hashes,
            message_count,
            tool_count,
            ..
        } = event
        else {
            return;
        };
        let provider = self.kv_cache_provider_identity();
        let route = crate::kv_cache_monitor::RequestRoute {
            cache_ttl_secs: crate::provider::cache_ttl_for_provider_model(&provider, Some(model)),
            ttl_is_estimate: crate::provider::cache_ttl_is_estimate(&provider),
            provider,
            model: model.to_string(),
            upstream_provider: self.last_upstream_provider.clone(),
        };
        let signature = crate::kv_cache_monitor::RequestSignature {
            system_static_hash: *system_static_hash,
            tools_hash: *tools_hash,
            tool_count: *tool_count,
            messages_hash: *messages_hash,
            message_hashes: message_hashes.clone(),
            message_count: *message_count,
        };
        self.kv_cache_monitor.begin_request(route, signature);
    }

    /// Classify the completed request's usage. Returns the event to send when
    /// the request missed the KV cache.
    fn finish_kv_cache_monitor_request(
        &mut self,
        input: u64,
        cache_read: Option<u64>,
        cache_creation: Option<u64>,
    ) -> Option<ServerEvent> {
        let effective = self.effective_context_tokens_from_usage(input, cache_read, cache_creation);
        let miss = self.kv_cache_monitor.finish_request(effective, cache_read)?;
        logging::warn(&format!(
            "KV_CACHE_MISS session={} reason={} harness_caused={} missed={} expected={} read={} documented={:?}",
            self.session.id,
            miss.reason.id(),
            miss.reason.harness_caused(),
            miss.missed_tokens,
            miss.expected_tokens,
            miss.read_tokens,
            miss.documented_cause,
        ));
        Some(ServerEvent::KvCacheMiss {
            reason: miss.reason.id().to_string(),
            harness_caused: miss.reason.harness_caused(),
            missed_tokens: miss.missed_tokens,
            expected_tokens: miss.expected_tokens,
            read_tokens: miss.read_tokens,
            message: miss.message(),
            documented_cause: miss.documented_cause,
        })
    }
}

fn log_agent_provider_stream_lifecycle(
    level: logging::LogLevel,
    agent: &Agent,
    phase: &str,
    api_start: Instant,
    fields: Vec<(&str, String)>,
) {
    let mut owned = vec![
        ("phase".to_string(), phase.to_string()),
        ("provider".to_string(), agent.provider.name().to_string()),
        ("model".to_string(), agent.provider.model()),
        ("session_id".to_string(), agent.session.id.clone()),
        (
            "provider_session_id".to_string(),
            agent.provider_session_id.clone().unwrap_or_else(|| "none".to_string()),
        ),
        (
            "connection_type".to_string(),
            agent
                .last_connection_type
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        ),
        ("elapsed_ms".to_string(), api_start.elapsed().as_millis().to_string()),
    ];
    owned.extend(fields.into_iter().map(|(key, value)| (key.to_string(), value)));
    logging::event(level, "AGENT_PROVIDER_STREAM_LIFECYCLE", owned);
}

/// Token usage from the last API request
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
}

#[derive(Debug, Clone)]
struct RewindUndoSnapshot {
    head_event_id: String,
    provider_session_id: Option<String>,
    session_provider_session_id: Option<String>,
    visible_message_count: usize,
}

pub struct Agent {
    provider: Arc<dyn Provider>,
    registry: Registry,
    skills: Arc<SkillRegistry>,
    session: Session,
    active_skill: Option<String>,
    allowed_tools: Option<HashSet<String>>,
    disabled_tools: HashSet<String>,
    /// Generation-scoped ownership of this Agent's global tool-policy entry.
    _tool_policy_registration: crate::tool::SessionToolPolicyRegistration,
    /// Generation-scoped ownership of this Agent's in-memory credential vault, so a
    /// placeholder minted on one turn still restores on the next.
    _secret_vault_registration: crate::secrets::SessionSecretVaultRegistration,
    /// MCP top-level definition exposure policy captured when the session starts.
    mcp_tools_mode: crate::config::McpToolsMode,
    /// Built-in tool load tiers, resolved once when the session starts.
    ///
    /// Reading a stored snapshot instead of recomputing keeps the deferred set
    /// fixed for the session: a config reload or a late registration cannot
    /// move a tool between the eager and deferred sets and bust the provider
    /// prompt cache.
    load_tiers: crate::config::ToolLoadTiers,
    /// The `# Additional tools` system-prompt index, frozen for the session.
    ///
    /// It is a projection of the session-frozen tiers and the built-in registry,
    /// so nothing mid-session can change it: unlock, compaction, a late MCP
    /// registration, and a provider switch all leave it alone, because rewriting
    /// the cached system prefix would cost a prompt-cache miss. `None` means "not
    /// built yet", which the prompt builder reports once instead of guessing;
    /// `Some(vec![])` means "this session lists nothing". Interior mutability
    /// because the prompt is assembled behind `&self`.
    listed_tools_index: std::sync::Mutex<Option<Vec<(String, String)>>>,
    /// Whether [`Agent::listed_tools_index`] is final.
    ///
    /// A warmup builds the index before the session locks its tool surface, and
    /// MCP servers finish registering around then, so the lock rebuilds it once
    /// and freezes it here. After that only a session restore may drop it: an
    /// unlock, a compaction, a conversation clear, or a provider switch keeps the
    /// session and its tier snapshot, so rewriting the rows would only cost a
    /// prompt-cache miss.
    listed_tools_index_frozen: std::sync::atomic::AtomicBool,
    /// Whether the MCP fixed surface (`mcp_search`/`mcp_call`) stays eager.
    ///
    /// `None` until the tool surface first locks, then `true` when at least one
    /// MCP server was configured. Held for the session so a server connecting
    /// later cannot move the two schemas between the eager list and the
    /// system-prompt index, which would cost a prompt-cache miss. Interior
    /// mutability because the index is built behind `&self`.
    mcp_surface_eager: std::sync::Mutex<Option<bool>>,
    /// Provider-specific session ID for conversation resume (e.g., Claude Code CLI session)
    provider_session_id: Option<String>,
    /// Last upstream provider (OpenRouter) observed for this session
    last_upstream_provider: Option<String>,
    /// Last observed transport/connection type for this session
    last_connection_type: Option<String>,
    /// Last provider-supplied human-readable transport detail for this session
    last_status_detail: Option<String>,
    /// Pending swarm alerts to inject into the next turn
    pending_alerts: Vec<String>,
    /// Transient reminder injected into provider requests for the current turn only.
    /// Not persisted to session history.
    current_turn_system_reminder: Option<String>,
    /// Loop detection for the current turn: a repeated identical call is warned
    /// once, refused on the following repeats, and ends the turn after two
    /// refusals. Reset at the start of every turn, so a loop the model broke
    /// last turn starts clean.
    loop_guard: loop_guard::LoopGuard,
    /// Progress and exit state of the current (or most recent) turn. Both turn
    /// loops replace it at turn start and advance it through the recovery
    /// helpers, so no loop owns a private copy of a recovery counter.
    turn_state: TurnState,
    /// Tool call ids observed in the current session transcript.
    tool_call_ids: HashSet<String>,
    /// Tool result ids observed in the current session transcript.
    tool_result_ids: HashSet<String>,
    /// Number of stored session messages already indexed for missing tool-output repair.
    tool_output_scan_index: usize,
    /// Soft interrupt queue: messages to inject at next safe point without cancelling
    /// Uses std::sync::Mutex so it can be accessed without async, even while agent is processing
    soft_interrupt_queue: SoftInterruptQueue,
    /// Signal from client to move the currently executing tool to background
    background_tool_signal: InterruptSignal,
    /// Signal to gracefully stop generation (checkpoint partial response and exit)
    graceful_shutdown: InterruptSignal,
    /// Client-side cache tracking for detecting append-only violations
    cache_tracker: CacheTracker,
    /// Classifies provider-reported KV cache misses for every client.
    kv_cache_monitor: crate::kv_cache_monitor::KvCacheMonitor,
    /// When the provider last answered; the prompt cache is warm until its TTL
    /// passes. Tool-result aging rewrites the prefix only once it is cold.
    last_provider_response_at: Option<std::time::Instant>,
    /// Last token usage from API request (for debug socket queries)
    last_usage: TokenUsage,
    /// Locked tool list: once the first API request is sent, freeze the tool list
    /// to avoid cache invalidation when MCP tools arrive asynchronously.
    /// Cleared on compaction/reset.
    locked_tools: Option<Vec<ToolDefinition>>,
    /// One-shot guard for the async MCP-registration race (#206).
    ///
    /// MCP servers connect on a background task and register `mcp__*` tools
    /// seconds after the session starts (we deliberately do NOT block the first
    /// turn on MCP connection, so the user can talk to the agent immediately).
    /// The first turn therefore locks a snapshot without MCP tools. We allow
    /// exactly one rebuild to pick them up — an intentional, one-time provider
    /// prompt-cache miss. Once that rebuild happens (or we confirm there are no
    /// MCP tools to wait for), this is set so the per-turn registry scan stops.
    /// Reset whenever the tool list is intentionally unlocked.
    mcp_late_register_resolved: bool,
    /// Whether `locked_tools` was built for provider-native deferred MCP
    /// loading. A mid-session model/provider switch can change that
    /// capability; the snapshot must then be rebuilt, or the new provider
    /// would get a surface built for the other path (for example
    /// `mcp_search` without `mcp_call` or any MCP tools).
    locked_tools_native_deferred: bool,
    /// MCP tools already described to the model, either in the locked tool
    /// snapshot or by a late-tool transcript announcement.
    announced_mcp_tools: HashSet<String>,
    /// Transcript index already scanned for MCP tools described there.
    announced_mcp_scan_index: usize,
    /// AGENTS.md is session bootstrap input. Keep the captured text stable so
    /// tool writes do not mutate the provider's cacheable prefix mid-session.
    agents_md_snapshot: (Option<String>, crate::prompt::ContextInfo),
    /// The "Available Skills" system-prompt section is session bootstrap input
    /// too. Installing a skill mid-session must not rewrite the cached system
    /// prefix (that forces a KV cache miss on everything after it), so the
    /// list is frozen here and later installs are announced in the transcript.
    prompt_skills_snapshot: Vec<crate::prompt::SkillInfo>,
    /// Skill names already described to the model, either in the frozen
    /// system-prompt list or by a late-skill transcript announcement.
    announced_skills: HashSet<String>,
    /// Transcript index already scanned for late-skill announcements.
    announced_skills_scan_index: usize,
    /// Whether memory features are enabled for this session
    memory_enabled: bool,
    /// One-step undo snapshot captured before the most recent rewind.
    rewind_undo_snapshot: Option<RewindUndoSnapshot>,
    /// Channel for tools to request stdin input from the user
    stdin_request_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::tool::StdinInputRequest>>,
    /// Canonical reducer-backed view of runtime provider/model selection.
    provider_runtime_state: ProviderRuntimeState,
    /// When true, this session is an inline swarm worker: stream a throttled
    /// output tail to the global bus so the coordinator's inline gallery can
    /// render a live viewport. Off for normal sessions to avoid bus traffic.
    inline_output_tap: bool,
    /// Rolling activity tail (text + tool markers) for the inline output tap.
    /// Persists across turns so the coordinator's viewport never blanks at
    /// turn boundaries or freezes during long tool calls.
    inline_tail: inline_tail::InlineTailBuffer,
    /// True while this Agent is a provisional placeholder whose session
    /// bootstrap is still deferred; see `provisional_init`.
    provisional_init_pending: bool,
    /// Session id whose restored state is prepared but not yet durable.
    ///
    /// Every external entry point refuses to start while this is set, so an
    /// attachment can deliver already-saved History first without letting a
    /// turn or tool run ahead of the commit the resumed state depends on.
    pending_resume: Option<resume_commit::PendingResumeCommit>,
    /// Automatic failover attempt budget for the current run chain.
    /// Persisted separately from the session journal so a restart cannot refund attempts.
    failover_budget: failover_budget::FailoverBudgetState,
    /// True when this agent runs inside the persistent serve/api-bridge daemon.
    /// Idle background compaction only starts here; a one-shot `run` leaves it
    /// false so it never compacts a transcript nobody will resume.
    persistent_runtime: bool,
    /// Opted-in advisor worker, created on the session's first completed tool
    /// round. `None` until then, after a disable, or when `[advisor]` is off.
    advisor: Option<advisor::Handle>,
    /// Number of main-session messages the advisor has already been fed.
    advisor_cursor: usize,
    /// User-turn index at which a blocker interrupt was last delivered, so the
    /// configured immune window can route later notes to the reminder slot.
    advisor_blocker_turn: Option<usize>,
}

impl Agent {
    fn refresh_agents_md_snapshot(&mut self) {
        let working_dir = self.session.working_dir.as_deref().map(std::path::Path::new);
        self.agents_md_snapshot = crate::prompt::load_agents_md_files_from_dir(working_dir);
        self.refresh_prompt_skills_snapshot();
    }

    /// Re-capture the frozen skills list. Only called at session boundaries
    /// (new/restored session, working directory change), never mid-turn.
    fn refresh_prompt_skills_snapshot(&mut self) {
        self.prompt_skills_snapshot = self.current_prompt_skill_infos();
        self.announced_skills = self
            .prompt_skills_snapshot
            .iter()
            .map(|skill| skill.name.clone())
            .collect();
        self.announced_skills_scan_index = 0;
    }

    fn current_prompt_skill_infos(&self) -> Vec<crate::prompt::SkillInfo> {
        self.current_skills_snapshot()
            .list()
            .iter()
            .map(|skill| crate::prompt::SkillInfo {
                name: skill.name.clone(),
                description: skill.description.clone(),
            })
            .collect()
    }

    fn should_track_client_cache(&self) -> bool {
        match std::env::var("HORON_AGENT_TRACK_CLIENT_CACHE") {
            Ok(value) => {
                let value = value.trim();
                !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
            }
            Err(_) => false,
        }
    }

    /// Drop the dispatcher from a session that cannot use it.
    ///
    /// `toolbox` is registered under `core` and `full`, so an allow-list derived
    /// from the registry picks it up even in a session that lists nothing (a
    /// swarm worker builds exactly such a list). Only a `core`-listed session
    /// and the `full` profile reach the dispatcher; anywhere else it would be
    /// one eager tool with nothing to list, so the session sends exactly its
    /// allow-list.
    fn without_unreachable_dispatcher(
        tools: &crate::config::ToolConfig,
        allowed_tools: Option<HashSet<String>>,
        core_listed: bool,
    ) -> Option<HashSet<String>> {
        let full = tools.profile.trim().eq_ignore_ascii_case("full");
        if core_listed || full {
            return allowed_tools;
        }
        allowed_tools.map(|mut allowed| {
            allowed.remove(crate::config::TOOLBOX_TOOL);
            allowed
        })
    }

    fn build_base(
        provider: Arc<dyn Provider>,
        registry: Registry,
        session: Session,
        allowed_tools: Option<HashSet<String>>,
        disabled_tools: HashSet<String>,
        default_core_profile: bool,
    ) -> Self {
        let skills = SkillRegistry::shared_snapshot();
        let tool_config = &crate::config::config().tools;
        // `default_core_profile` is already "the untouched default `core`
        // profile and no explicit allow-list": `new_with_session` clears it for
        // a caller-supplied list. Listing the core tools additionally needs the
        // dispatcher that reaches them, so an operator who removed `toolbox`
        // gets every core tool eager again instead of tools nothing can call.
        let core_listed = default_core_profile && tool_config.toolbox_reachable();
        let allowed_tools = Self::without_unreachable_dispatcher(tool_config, allowed_tools, core_listed);
        let load_tiers = tool_config.load_tiers(allowed_tools.as_ref(), core_listed);
        let working_dir = session.working_dir.as_deref().map(std::path::Path::new);
        let agents_md_snapshot = crate::prompt::load_agents_md_files_from_dir(working_dir);
        let initial_provider_model = provider.model();
        let tool_policy_registration =
            crate::tool::register_session_tool_policy(&session.id, allowed_tools.clone(), disabled_tools.clone());
        let secret_vault_registration = crate::secrets::register_session_vault(&session.id);
        // The dispatcher must read the same frozen tier snapshot the tool list
        // uses, not a later configuration reload.
        crate::tool::register_session_load_tiers(&session.id, load_tiers.clone());
        let mut agent = Self {
            provider,
            registry,
            skills,
            session,
            active_skill: None,
            allowed_tools,
            disabled_tools,
            _tool_policy_registration: tool_policy_registration,
            _secret_vault_registration: secret_vault_registration,
            mcp_tools_mode: tool_config.mcp_tools,
            load_tiers,
            listed_tools_index: std::sync::Mutex::new(None),
            listed_tools_index_frozen: std::sync::atomic::AtomicBool::new(false),
            mcp_surface_eager: std::sync::Mutex::new(None),
            provider_session_id: None,
            last_upstream_provider: None,
            last_connection_type: None,
            last_status_detail: None,
            pending_alerts: Vec::new(),
            current_turn_system_reminder: None,
            loop_guard: loop_guard::LoopGuard::new(),
            turn_state: TurnState::new(),
            tool_call_ids: HashSet::new(),
            tool_result_ids: HashSet::new(),
            tool_output_scan_index: 0,
            soft_interrupt_queue: SoftInterruptQueue::new(),
            background_tool_signal: InterruptSignal::new(),
            graceful_shutdown: InterruptSignal::new(),
            cache_tracker: CacheTracker::new(),
            kv_cache_monitor: Default::default(),
            last_provider_response_at: None,
            last_usage: TokenUsage::default(),
            locked_tools: None,
            mcp_late_register_resolved: false,
            locked_tools_native_deferred: false,
            announced_mcp_tools: HashSet::new(),
            announced_mcp_scan_index: 0,
            agents_md_snapshot,
            prompt_skills_snapshot: Vec::new(),
            announced_skills: HashSet::new(),
            announced_skills_scan_index: 0,
            memory_enabled: crate::config::config().features.memory,
            rewind_undo_snapshot: None,
            stdin_request_tx: None,
            provider_runtime_state: ProviderRuntimeState::observed(initial_provider_model),
            inline_output_tap: false,
            inline_tail: inline_tail::InlineTailBuffer::default(),
            provisional_init_pending: false,
            pending_resume: None,
            failover_budget: failover_budget::FailoverBudgetState::default(),
            persistent_runtime: false,
            advisor: None,
            advisor_cursor: 0,
            advisor_blocker_turn: None,
        };
        agent.restore_failover_budget();
        agent.refresh_prompt_skills_snapshot();
        agent
    }

    /// Restore this session's automatic-failover budget (IS-4).
    ///
    /// A stored chain is reused with its attempt count, so a restart cannot
    /// refund attempts. A missing record is a first run and starts a fresh
    /// chain at zero. An unreadable record leaves the budget unknown, which
    /// keeps automatic failover blocked instead of silently resetting it.
    fn restore_failover_budget(&mut self) {
        match failover_budget::load(&self.session.id) {
            Ok(Some(stored)) => {
                logging::info(&format!(
                    "failover_budget: restored session={} chain={} attempts={}",
                    self.session.id, stored.chain_id, stored.attempts
                ));
                self.failover_budget = failover_budget::FailoverBudgetState::restored(stored);
            }
            Ok(None) => {
                self.failover_budget = failover_budget::FailoverBudgetState::fresh(&self.session.id);
            }
            Err(error) => {
                logging::warn(&format!(
                    "failover_budget: unreadable state for session={}: {error}; automatic failover stays blocked",
                    self.session.id
                ));
                self.failover_budget = failover_budget::FailoverBudgetState::unreadable(&self.session.id);
            }
        }
    }

    /// Start a new failover chain for a new user prompt and return its id.
    pub(crate) fn begin_failover_chain(&mut self) -> String {
        let chain_id = failover_budget::new_chain_id();
        match failover_budget::begin_chain(&self.session.id, &chain_id) {
            Ok(stored) => {
                self.failover_budget = failover_budget::FailoverBudgetState::restored(stored);
            }
            Err(error) => {
                logging::warn(&format!(
                    "failover_budget: cannot begin chain {chain_id} for session={}: {error}; automatic failover stays blocked",
                    self.session.id
                ));
                self.failover_budget.set_unreadable_chain(chain_id.clone());
            }
        }
        chain_id
    }

    /// The id of the chain this session's attempts are counted against, or
    /// `None` before a chain was started. Reloads the persisted budget first,
    /// so a stale in-memory chain never outlives a re-pointed session.
    pub(crate) fn failover_chain_id(&mut self) -> Option<&str> {
        self.reload_failover_budget_if_session_changed();
        self.failover_budget.chain_id()
    }

    /// Attempts already spent in the current chain, reloaded from the persisted
    /// budget first so a restart cannot refund them.
    pub(crate) fn failover_attempts(&mut self) -> u32 {
        self.reload_failover_budget_if_session_changed();
        self.failover_budget.attempts()
    }

    /// Whether the stored budget is unreadable rather than merely exhausted,
    /// so a caller can tell "the cap was reached" from "the budget cannot be
    /// trusted". Reloads the persisted budget first.
    pub(crate) fn failover_budget_unknown(&mut self) -> bool {
        self.reload_failover_budget_if_session_changed();
        self.failover_budget.is_unknown()
    }

    /// Re-read the stored budget when this Agent has been re-pointed at another
    /// session without being reconstructed. The persisted record of the current
    /// session is the source of truth, so a chain id from a previous session
    /// must never be reported or spent against it.
    fn reload_failover_budget_if_session_changed(&mut self) {
        if !self.failover_budget.belongs_to(&self.session.id) {
            self.restore_failover_budget();
        }
    }

    /// Spend one automatic-failover attempt for the current chain.
    ///
    /// Returns false when the cap is reached or the persisted budget cannot be
    /// trusted, and surfaces `RunNeedsHumanResume` so an unattended run stops
    /// for a human instead of retrying without a limit. `request_id` is `None`
    /// when the stop is not tied to a permission request; the failover path has
    /// no request to name, and its chain id belongs in the error and reason
    /// text, not in a field that resolves to nothing.
    pub(crate) fn consume_failover_attempt(&mut self, request_id: Option<&str>, max: u32) -> bool {
        self.reload_failover_budget_if_session_changed();
        let chain_id = match self.failover_budget.chain_id().map(str::to_string) {
            Some(chain_id) => chain_id,
            None => self.begin_failover_chain(),
        };
        if self.failover_budget.is_unknown() {
            let attempts = self.failover_budget.attempts();
            self.surface_failover_needs_human(request_id, "failover_budget_unreadable", attempts);
            return false;
        }
        match failover_budget::try_consume(&self.session.id, &chain_id, max) {
            failover_budget::ConsumeOutcome::Allowed { attempts } => {
                self.failover_budget.set_attempts(attempts);
                true
            }
            failover_budget::ConsumeOutcome::Exhausted { attempts } => {
                self.failover_budget.set_attempts(attempts);
                self.surface_failover_needs_human(request_id, "failover_budget_exhausted", attempts);
                false
            }
            failover_budget::ConsumeOutcome::Unavailable(error) => {
                logging::warn(&format!(
                    "failover_budget: cannot record an attempt for session={} chain={chain_id}: {error}",
                    self.session.id
                ));
                self.failover_budget.mark_unknown();
                let attempts = self.failover_budget.attempts();
                self.surface_failover_needs_human(request_id, "failover_budget_unreadable", attempts);
                false
            }
        }
    }

    /// Hand a stop-for-human event to this session's clients.
    ///
    /// `request_id` is `None` for a stop that no request can resolve, so the
    /// event never carries an id that a human would be told to look up.
    fn surface_failover_needs_human(&self, request_id: Option<&str>, reason: &str, attempts: u32) {
        let event = ServerEvent::RunNeedsHumanResume {
            request_id: request_id.map(str::to_string),
            session_id: self.session.id.clone(),
            reason: reason.to_string(),
            attempts,
        };
        let outcome = crate::server::emit_session_event(&self.session.id, event);
        if !matches!(outcome, crate::server::EmitOutcome::Delivered) {
            let attached = crate::server::session_has_attached_client(&self.session.id);
            logging::warn(&format!(
                "failover_budget: {reason} for session={} was not delivered (attached_client={attached}, outcome={outcome:?})",
                self.session.id
            ));
        }
    }

    fn current_skills_snapshot(&self) -> Arc<SkillRegistry> {
        // Global skills come from the process-wide shared registry; the
        // project-local overlay is composed fresh from this session's
        // workspace root so per-repo skills are session-scoped, immediately
        // visible, and never leak across sessions (issue #457).
        let global = self
            .registry
            .skills()
            .try_read()
            .map(|skills| Arc::new(skills.clone()))
            .unwrap_or_else(|_| self.skills.clone());
        let working_dir = self.session.working_dir.as_deref().map(std::path::Path::new);
        Arc::new(SkillRegistry::effective_for_working_dir(&global, working_dir))
    }

    pub fn available_skill_names(&self) -> Vec<String> {
        self.current_skills_snapshot()
            .list()
            .iter()
            .map(|skill| skill.name.clone())
            .collect()
    }

    pub fn new(provider: Arc<dyn Provider>, registry: Registry) -> Self {
        Self::new_with_initial_working_dir(provider, registry, None)
    }

    /// Progress and exit state of the current (or most recent) turn.
    pub(crate) fn turn_state(&self) -> &TurnState {
        &self.turn_state
    }

    pub(crate) fn new_with_initial_working_dir(
        provider: Arc<dyn Provider>,
        registry: Registry,
        working_dir: Option<&str>,
    ) -> Self {
        Self::new_with_initial_ownership(provider, registry, working_dir, None, true)
    }

    /// A connection may only be a viewer attaching to an existing Agent.
    /// Do not count its provisional session before that choice is resolved.
    pub(crate) fn new_provisional_with_initial_working_dir(
        provider: Arc<dyn Provider>,
        registry: Registry,
        working_dir: Option<&str>,
    ) -> Self {
        Self::new_with_initial_ownership(provider, registry, working_dir, None, false)
    }

    pub(crate) fn new_with_parent_and_initial_working_dir(
        provider: Arc<dyn Provider>,
        registry: Registry,
        working_dir: Option<&str>,
        parent_id: Option<String>,
    ) -> Self {
        Self::new_with_initial_ownership(provider, registry, working_dir, parent_id, true)
    }

    /// Whether a session's allow-list is the untouched default `core` profile.
    ///
    /// Only the default qualifies: an explicit `[tools] enabled` list, `disable_base_tools`, and
    /// every other profile are the operator's own choice and are never widened here. The user's
    /// explicit allow and deny remain the final authority.
    fn default_core_profile(tools: &crate::config::ToolConfig) -> bool {
        tools
            .profile
            .trim()
            .eq_ignore_ascii_case(crate::config::CORE_TOOL_PROFILE)
            && tools.enabled.is_empty()
            && !tools.disable_base_tools
    }

    fn new_with_initial_ownership(
        provider: Arc<dyn Provider>,
        registry: Registry,
        working_dir: Option<&str>,
        parent_id: Option<String>,
        confirmed: bool,
    ) -> Self {
        let start = Instant::now();
        let tool_selection = crate::config::config().tools.selection();
        let default_core_profile = Self::default_core_profile(&crate::config::config().tools);
        let mut session = Session::create(parent_id, None);
        if let Some(working_dir) = working_dir {
            session.working_dir = Some(working_dir.to_string());
        }
        let mut agent = Self::build_base(
            provider,
            registry,
            session,
            tool_selection.allowed_tools,
            tool_selection.disabled_tools,
            default_core_profile,
        );
        let model = agent.provider_model();
        agent.session.model = (!model.is_empty()).then_some(model);
        agent.session.provider_key = agent.provider_key_for_new_session();
        agent.reconcile_explicit_provider_pin_route();
        provisional_init::finish_new_agent(&mut agent, confirmed);
        let setup_ms = start.elapsed().as_millis();
        logging::info(&format!(
            "[TIMING] agent_new: setup={}ms, total={}ms",
            setup_ms,
            start.elapsed().as_millis(),
        ));
        agent
    }

    pub fn new_with_session(
        provider: Arc<dyn Provider>,
        registry: Registry,
        session: Session,
        allowed_tools: Option<HashSet<String>>,
    ) -> Self {
        let explicit_allow = allowed_tools.is_some();
        let tool_selection = if let Some(allowed_tools) = allowed_tools {
            crate::config::ToolSelection {
                allowed_tools: Some(allowed_tools),
                disabled_tools: HashSet::new(),
            }
        } else {
            crate::config::config().tools.selection()
        };
        // An explicit allow passed by a caller is never widened; only the default `core` profile is.
        let default_core_profile = !explicit_allow && Self::default_core_profile(&crate::config::config().tools);
        let mut agent = Self::build_base(
            provider,
            registry,
            session,
            tool_selection.allowed_tools,
            tool_selection.disabled_tools,
            default_core_profile,
        );
        agent.session.mark_active();
        if agent.session.provider_key.is_none() {
            agent.session.provider_key = agent.provider_key_for_new_session();
        }
        agent.restore_session_model_from_session();
        agent.restore_reasoning_effort_from_session();
        agent.session.ensure_initial_session_context_message();
        agent.sync_memory_dedup_state_from_session();
        agent.seed_compaction_from_session();
        agent.log_env_snapshot("attach");
        agent.fire_session_lifecycle_hook("session_start", "attach");
        agent
    }

    fn seed_compaction_from_session(&mut self) {
        logging::info(&format!(
            "seed_compaction_from_session: session has {} messages",
            self.session.active_context().len()
        ));
        let compaction = self.registry.compaction();
        let mut manager = match compaction.try_write() {
            Ok(manager) => manager,
            Err(_) => {
                logging::warn("seed_compaction_from_session: compaction lock unavailable, skipping restore");
                return;
            }
        };
        manager.reset();
        let budget = self.provider.context_window();
        manager.set_budget(budget);
        // Restoring must not hand ownership to the wrong engine: a provider that
        // compacts natively keeps the summarizer out of the same turn, and the
        // restored route decides whether a persisted native window is replayable.
        let owned_strategy = self.provider_owned_compaction_strategy();
        let provider_name = self.provider.name().to_string();
        let provider_model = self.provider.model();
        let route_method = self.session.route_api_method.clone();
        manager.reconcile_strategy(owned_strategy);
        manager.observe_route_identity_with_route(&provider_name, &provider_model, route_method.as_deref());
        // A persisted state whose Compacted event is absent (a route the event
        // store cannot bind, or a state written before the event existed) is
        // still the session's active compaction. Restoring the messages alone
        // would resurrect the compacted prefix and re-send it.
        //
        // The store records its boundary over the full history, so the count the
        // manager needs depends on which list it is handed: when the session
        // still carries the compacted prefix the full count applies; when the
        // prefix was already dropped the offset-relative count does.
        let active = self.session.active_context();
        let restored = match self.session.compaction_for_active_context() {
            Some(state) if active.len() >= state.original_turn_count => self.session.compaction.clone().or(Some(state)),
            other => other.or_else(|| self.session.compaction.clone()),
        };
        if let Some(state) = restored {
            manager.restore_persisted_stored_state_with(&state, self.session.active_context());
        } else {
            manager.seed_restored_stored_messages_with(self.session.active_context());
        }
        let dropped_foreign_native = manager.discard_foreign_openai_native_compaction();
        let sanitized_state = if manager.discard_oversized_openai_native_compaction() || dropped_foreign_native {
            Some(manager.persisted_state())
        } else {
            None
        };
        logging::info(&format!(
            "seed_compaction_from_session: seeded compaction with {} messages",
            self.session.active_context().len()
        ));
        drop(manager);
        if let Some(state) = sanitized_state {
            self.session.compaction = state;
            self.persist_session_best_effort("sanitized oversized OpenAI native compaction");
        }
    }

    fn sync_memory_dedup_state_from_session(&self) {
        crate::memory::sync_injected_memories(&self.session.id, &self.session.injected_memory_ids());
    }

    fn record_memory_injection_in_session(&mut self, memory: &crate::memory::PendingMemory) {
        let count = memory.count.max(1);
        let age_ms = memory.computed_at.elapsed().as_millis() as u64;
        let summary = if count == 1 {
            "🧠 auto-recalled 1 memory".to_string()
        } else {
            format!("🧠 auto-recalled {} memories", count)
        };
        let display_prompt = memory.display_prompt.clone().unwrap_or_else(|| {
            if memory.prompt.trim().is_empty() {
                "# Memory\n\n## Notes\n1. (empty injection payload)".to_string()
            } else {
                memory.prompt.clone()
            }
        });

        self.session
            .record_memory_injection(summary, display_prompt, count as u32, age_ms, memory.memory_ids.clone());
        if let Err(err) = self.save_session() {
            logging::warn(&format!(
                "Failed to persist memory injection for session {}: {}",
                self.session.id, err
            ));
        }
    }

    fn memory_injection_message(memory: &crate::memory::PendingMemory) -> Message {
        Message::user(&format!("<system-reminder>\n{}\n</system-reminder>", memory.prompt))
    }

    pub(super) fn prepare_memory_injection_message(
        &mut self,
        memory: &crate::memory::PendingMemory,
    ) -> (Message, bool) {
        let message = Self::memory_injection_message(memory);
        let persist = crate::config::config().features.persist_memory_injections;
        if persist {
            self.add_message_with_display_role(Role::User, message.content.clone(), Some(StoredDisplayRole::System));
            self.persist_session_best_effort("persisted memory injection message");
        }
        (message, persist)
    }

    fn persist_session_best_effort(&mut self, context: &str) {
        if let Err(err) = self.save_session() {
            logging::warn(&format!(
                "Failed to persist {} for session {}: {}",
                context, self.session.id, err
            ));
        }
    }

    fn reset_runtime_state_for_session_change(&mut self) {
        self.active_skill = None;
        self.last_upstream_provider = None;
        self.last_connection_type = None;
        self.last_status_detail = None;
        self.pending_alerts.clear();
        self.current_turn_system_reminder = None;
        self.reset_tool_output_tracking();
        if let Ok(mut queue) = self.soft_interrupt_queue.lock() {
            queue.clear();
        }
        self.background_tool_signal.reset();
        self.graceful_shutdown.reset();
        self.cache_tracker.reset();
        self.kv_cache_monitor.reset();
        self.last_usage = TokenUsage::default();
        self.locked_tools = None;
        self.mcp_late_register_resolved = false;
        self.rewind_undo_snapshot = None;
    }

    /// Drop the frozen listed-tools index.
    ///
    /// Only a session restore needs this: the replacement session resolves its
    /// own allow-list and tiers, so the rows describe the wrong session. A
    /// conversation clear, an unlock, a compaction, and a provider switch all
    /// keep the same session and therefore keep the same index, which is what
    /// lets the cached system prefix survive them.
    pub(crate) fn reset_listed_tools_index(&mut self) {
        self.listed_tools_index_frozen
            .store(false, std::sync::atomic::Ordering::Release);
        *self
            .listed_tools_index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    /// Synchronize the remote client's selected skill, accepting only names
    /// present in the daemon's own registry snapshot.
    pub(super) fn set_remote_active_skill(&mut self, active_skill: Option<String>) -> bool {
        let skills = self.current_skills_snapshot();
        let recognized = active_skill.as_ref().is_none_or(|name| skills.get(name).is_some());
        self.active_skill = active_skill.filter(|name| skills.get(name).is_some());
        recognized
    }

    fn sync_session_compaction_state_from_manager(&mut self, manager: &mut crate::compaction::CompactionManager) {
        self.capture_compaction_route();
        let mut new_state = manager.persisted_state_with_recovery();
        if let Some(state) = &mut new_state {
            state.compacted_count += self.session.active_history_offset();
        }
        if self.session.compaction != new_state {
            self.session.compaction = new_state;
            if let Err(err) = self.save_session() {
                logging::error(&format!(
                    "Failed to persist compaction state for session {}: {}",
                    self.session.id, err
                ));
            } else if let Some(state) = self.session.compaction_for_active_context() {
                manager.restore_persisted_stored_state_with(&state, self.session.active_context());
            }
        }
    }

    /// Apply a provider-native compaction window to the session.
    ///
    /// The window is the canonical next context window: the compaction item
    /// plus every item the provider retained, in order. It is stored whole and
    /// bound to the route that produced it.
    ///
    /// A window that cannot be replayed is *not* applied. Recording a boundary
    /// over a payload the next request cannot send would hide the originals
    /// from the summarizer while replaying a substitute, so the transcript keeps
    /// its previous boundary and the originals stay available.
    fn apply_openai_native_compaction_window(
        &mut self,
        window: Vec<serde_json::Value>,
        compacted_count: usize,
    ) -> Result<()> {
        self.capture_compaction_route();
        let Some(encrypted_content) = window.iter().find_map(|item| {
            (item.get("type").and_then(|value| value.as_str()) == Some("compaction"))
                .then(|| item.get("encrypted_content").and_then(|value| value.as_str()))
                .flatten()
                .map(str::to_owned)
        }) else {
            logging::warn(
                "[OI] native compaction window has no compaction item; leaving the compacted boundary unchanged",
            );
            return Ok(());
        };
        if !crate::provider::openai_request::openai_encrypted_content_is_sendable(&encrypted_content) {
            logging::warn(&format!(
                "Discarding oversized [OI] native compaction window before persist ({} chars); leaving the compacted boundary unchanged",
                encrypted_content.len(),
            ));
            crate::runtime_memory_log::emit_event(
                crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                    "native_compaction_window_rejected",
                    "openai_encrypted_content_too_large",
                )
                .with_session_id(self.session.id.clone())
                .force_attribution(),
            );
            return Ok(());
        }
        if window
            .iter()
            .any(|item| !item.is_object() || item.get("type").and_then(|value| value.as_str()).is_none())
        {
            logging::warn(
                "[OI] native compaction window has an item that cannot be replayed; leaving the compacted boundary unchanged",
            );
            return Ok(());
        }

        let state = crate::session::StoredCompactionState {
            summary_text: String::new(),
            openai_encrypted_content: Some(encrypted_content),
            native_window: window,
            native_route: Some(crate::session::StoredNativeRoute {
                provider: self.provider.name().to_string(),
                model: self.provider.model(),
                route: self.session.route_api_method.clone(),
            }),
            covers_up_to_turn: compacted_count,
            original_turn_count: compacted_count,
            compacted_count: compacted_count + self.session.active_history_offset(),
            structured: None,
        };

        self.session.compaction = Some(state.clone());
        let compaction = self.registry.compaction();
        if let Ok(mut manager) = compaction.try_write() {
            manager.set_budget(self.provider.context_window());
            manager.observe_route_identity_with_route(
                self.provider.name(),
                &self.provider.model(),
                self.session.route_api_method.as_deref(),
            );
            manager.restore_persisted_stored_state_with(
                &self.session.compaction_for_active_context().unwrap_or(state.clone()),
                self.session.active_context(),
            );
        }

        self.cache_tracker.reset();
        self.kv_cache_monitor.reset();
        self.locked_tools = None;
        self.mcp_late_register_resolved = false;
        self.provider_session_id = None;
        self.session.provider_session_id = None;
        self.save_session()?;
        self.seed_compaction_from_session();
        crate::runtime_memory_log::emit_event(
            crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                "native_compaction_applied",
                "provider_native_compaction_persisted",
            )
            .with_session_id(self.session.id.clone())
            .force_attribution(),
        );
        Ok(())
    }

    /// The turn's single owned provider-facing request snapshot.
    ///
    /// `Arc` rather than `Vec` so the rest of request preparation (memory
    /// injection, cache signature) can share one transcript instead of taking
    /// its own copy of every message.
    fn messages_for_provider(&mut self) -> (Arc<[Message]>, Option<CompactionEvent>) {
        // Before the projection is built, so an aged batch is what gets sent.
        self.maybe_age_tool_results();
        if self.provider.supports_compaction() || self.session.compaction.is_some() {
            let compaction = self.registry.compaction();
            let owned_strategy = self.provider_owned_compaction_strategy();
            let provider_name = self.provider.name().to_string();
            let provider_model = self.provider.model();
            let route_method = self.session.route_api_method.clone();
            match compaction.try_write() {
                Ok(mut manager) => {
                    // One owner per turn: the provider's native mode wins over a
                    // stale configured strategy, so both never compact at once.
                    manager.reconcile_strategy(owned_strategy);
                    manager.observe_route_identity_with_route(&provider_name, &provider_model, route_method.as_deref());
                    // A native window produced on another model or credential
                    // route cannot be replayed; drop it and restore the
                    // originals instead of keeping a boundary over them.
                    let discarded_foreign_native = manager.discard_foreign_openai_native_compaction();
                    let discarded_oversized_native =
                        manager.discard_oversized_openai_native_compaction() || discarded_foreign_native;
                    let messages = {
                        let all_messages = self.session.provider_messages();
                        if self.provider.uses_horon_agent_compaction() {
                            let action = manager.ensure_context_fits(all_messages, self.provider.clone());
                            match action {
                                crate::compaction::CompactionAction::BackgroundStarted { trigger } => {
                                    logging::info(&format!("Background compaction started ({})", trigger));
                                }
                                crate::compaction::CompactionAction::HardCompacted(dropped) => {
                                    logging::warn(&format!(
                                        "Emergency hard compact: dropped {} messages (context was critical)",
                                        dropped
                                    ));
                                }
                                crate::compaction::CompactionAction::None => {}
                            }
                        }
                        let messages = manager.messages_for_api_with(all_messages);
                        // Measured after compaction applied, so the sample
                        // describes the request actually sent.
                        let estimate = manager.token_estimate_with(all_messages);
                        manager.note_request_estimate(u64::try_from(estimate).unwrap_or(u64::MAX));
                        messages
                    };
                    let event = manager.take_compaction_event();
                    if manager.take_projection_truncated() {
                        // A shortened payload may be an earlier `read` result.
                        crate::tool::read_state::forget_session(&self.session.id);
                    }
                    if event.is_some() || discarded_oversized_native {
                        self.sync_session_compaction_state_from_manager(&mut manager);
                    }
                    if event.is_some() {
                        self.note_compaction_applied();
                        self.persist_session_best_effort("compaction completion");
                    }
                    let messages = self.prune_provider_messages(messages);
                    let user_count = messages
                        .iter()
                        .filter(|message| matches!(message.role, Role::User))
                        .count();
                    let assistant_count = messages.len().saturating_sub(user_count);
                    logging::info(&format!(
                        "messages_for_provider (compaction): returning {} messages (user={}, assistant={})",
                        messages.len(),
                        user_count,
                        assistant_count,
                    ));
                    return (Arc::from(messages), event);
                }
                Err(_) => {
                    logging::info("messages_for_provider: compaction lock failed, using session");
                }
            };
        }

        // Borrow the cached projection only long enough to own it once. Every
        // later transform borrows this snapshot.
        let messages: Arc<[Message]> = if crate::config::config().compaction.prune_tool_results {
            let owned = self.session.provider_messages().to_vec();
            Arc::from(self.prune_provider_messages_enabled(owned))
        } else {
            Arc::from(self.session.provider_messages())
        };
        horon_agent_perf_counters::record_request_deep_copy(
            u64::try_from(self.session.provider_projection_json_bytes()).unwrap_or(u64::MAX),
            u64::try_from(messages.len()).unwrap_or(u64::MAX),
        );
        let user_count = messages
            .iter()
            .filter(|message| matches!(message.role, Role::User))
            .count();
        let assistant_count = messages.len().saturating_sub(user_count);
        logging::info(&format!(
            "messages_for_provider (session): returning {} messages (user={}, assistant={})",
            messages.len(),
            user_count,
            assistant_count,
        ));
        (messages, None)
    }

    /// Replace stale tool results on the provider copy of the transcript.
    ///
    /// Stored history is never touched: only the `Vec<Message>` built for this
    /// request carries the marker, so a resumed session or a rewind still has
    /// the original output. Newly prunable ids join the session ledger, which
    /// persists with the session's auxiliary state.
    fn prune_provider_messages(&mut self, messages: Vec<Message>) -> Vec<Message> {
        if !crate::config::config().compaction.prune_tool_results {
            return messages;
        }
        self.prune_provider_messages_enabled(messages)
    }

    /// [`Self::prune_provider_messages`] without the config gate.
    fn prune_provider_messages_enabled(&mut self, mut messages: Vec<Message>) -> Vec<Message> {
        use crate::compaction::pruning::{PRUNED_SUPERSEDED_TEXT, PRUNED_USELESS_TEXT, PruneKind, plan_prunes};

        // Take the ledger instead of cloning it: planning needs the transcript
        // borrow, so the map cannot be read in place.
        let already = std::mem::take(&mut self.session.pruned_tool_results);
        let planned = {
            let active = self.session.provider_messages();
            plan_prunes(active, &already, chrono::Utc::now())
        };
        let mut ledger = already;
        if !planned.is_empty() {
            logging::debug(&format!(
                "[pruning] planned {} new tool result(s) for elision",
                planned.len()
            ));
            ledger.extend(planned);
        }
        self.session.pruned_tool_results = ledger;
        if self.session.pruned_tool_results.is_empty() {
            return messages;
        }

        let ledger = &self.session.pruned_tool_results;
        let mut replaced = 0usize;
        for message in messages.iter_mut() {
            for block in message.content.iter_mut() {
                let ContentBlock::ToolResult {
                    tool_use_id, content, ..
                } = block
                else {
                    continue;
                };
                let Some(kind) = ledger.get(tool_use_id) else {
                    continue;
                };
                let marker = match kind {
                    PruneKind::Superseded => PRUNED_SUPERSEDED_TEXT,
                    PruneKind::Useless => PRUNED_USELESS_TEXT,
                };
                if content != marker {
                    content.clear();
                    content.push_str(marker);
                    replaced += 1;
                }
            }
        }
        if replaced > 0 {
            logging::info(&format!(
                "pruning: elided {replaced} stale tool result(s) from the provider request ({} tracked)",
                self.session.pruned_tool_results.len()
            ));
        }
        messages
    }

    fn record_client_cache_request(&mut self, messages: &[Message]) {
        if !self.should_track_client_cache() {
            return;
        }

        let fast_snapshot = if !self.provider.uses_horon_agent_compaction() && self.session.compaction.is_none() {
            let previous_count = self.cache_tracker.previous_message_count();
            let prefix_hashes = self.session.provider_message_prefix_hashes();
            let current_count = prefix_hashes.len();
            let current_full_hash = prefix_hashes.last().copied();
            let prefix_hash_at_previous_count = if previous_count == 0 || previous_count > current_count {
                None
            } else {
                Some(prefix_hashes[previous_count - 1])
            };
            Some((current_count, prefix_hash_at_previous_count, current_full_hash))
        } else {
            None
        };

        let violation = if let Some((current_count, prefix_hash_at_previous_count, current_full_hash)) = fast_snapshot {
            self.cache_tracker.record_prefix_hash_snapshot(
                current_count,
                prefix_hash_at_previous_count,
                current_full_hash,
            )
        } else {
            self.cache_tracker.record_request(messages)
        };

        if let Some(violation) = violation {
            logging::warn(&format!(
                "CLIENT_CACHE_VIOLATION: {} | turn={} messages={}",
                violation.reason, violation.turn, violation.message_count
            ));
        }
    }

    fn repair_missing_tool_outputs(&mut self) -> usize {
        if self.tool_output_scan_index > self.session.active_context().len() {
            self.reset_tool_output_tracking();
        }

        let scan_start = self.tool_output_scan_index;
        let mut new_result_ids = Vec::new();
        let mut assistant_tool_uses: Vec<(usize, Vec<String>)> = Vec::new();

        for (index, msg) in self.session.active_context().iter().enumerate().skip(scan_start) {
            match msg.role {
                Role::User => {
                    for block in &msg.content {
                        if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                            new_result_ids.push(tool_use_id.clone());
                        }
                    }
                }
                Role::Assistant => {
                    let tool_uses = msg
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    if !tool_uses.is_empty() {
                        assistant_tool_uses.push((index, tool_uses));
                    }
                }
            }
        }

        self.tool_result_ids.extend(new_result_ids);

        let mut missing_repairs: Vec<(usize, Vec<String>)> = Vec::new();
        for (index, tool_uses) in assistant_tool_uses {
            let mut missing_for_message = Vec::new();
            for id in tool_uses {
                self.tool_call_ids.insert(id.clone());
                if self.tool_result_ids.contains(&id) {
                    continue;
                }
                // A tool that is still executing is not an interrupted tool:
                // its real result is on the way, and synthesizing a
                // placeholder now produces a duplicate tool_result that
                // Anthropic rejects outright. See `tool::inflight`.
                if crate::tool::inflight::is_tool_in_flight(&id) {
                    logging::info(&format!(
                        "Skipping missing tool-output repair for {id}: tool is still executing"
                    ));
                    continue;
                }
                missing_for_message.push(id);
            }
            if !missing_for_message.is_empty() {
                missing_repairs.push((index, missing_for_message));
            }
        }

        self.tool_output_scan_index = self.session.active_context().len();

        let mut repaired = 0usize;
        let mut inserted = 0usize;
        for (index, missing_for_message) in missing_repairs {
            for (offset, id) in missing_for_message.iter().enumerate() {
                let tool_block = ContentBlock::ToolResult {
                    tool_use_id: id.clone(),
                    content: TOOL_OUTPUT_MISSING_TEXT.to_string(),
                    is_error: Some(true),
                };
                let stored_message = StoredMessage {
                    turn_start: false,
                    id: id::new_id("message"),
                    role: Role::User,
                    content: vec![tool_block],
                    display_role: None,
                    timestamp: Some(chrono::Utc::now()),
                    tool_duration_ms: None,
                    token_usage: None,
                };
                self.session
                    .insert_message(index + 1 + inserted + offset, stored_message);
                self.tool_result_ids.insert(id.clone());
                repaired += 1;
            }
            inserted += missing_for_message.len();
        }

        self.tool_output_scan_index = self.session.active_context().len();

        if repaired > 0 {
            self.persist_session_best_effort("missing tool-output repair");
            self.cache_tracker.reset();
            self.kv_cache_monitor.reset();
            self.locked_tools = None;
            self.mcp_late_register_resolved = false;
        }

        repaired
    }

    fn reset_tool_output_tracking(&mut self) {
        self.tool_call_ids.clear();
        self.tool_result_ids.clear();
        self.tool_output_scan_index = 0;
    }

    pub fn session_id(&self) -> &str {
        &self.session.id
    }

    /// The session lineage a provider may use to pin its cache identity.
    ///
    /// A provider that keys a backend prompt cache on the lineage needs the same
    /// identity on every call, so speculative prewarm and foreground completion
    /// build it from this one place.
    pub(crate) fn provider_request_context(&self) -> horon_agent_provider_core::ProviderRequestContext {
        horon_agent_provider_core::ProviderRequestContext {
            session_id: Some(self.session.id.clone()),
            parent_session_id: self.session.parent_id.clone(),
            cache_root_id: self.session.cache_root_id.clone(),
        }
    }

    pub(crate) fn set_working_dir_for_pending_context(&mut self, working_dir: Option<String>) {
        if working_dir.is_some() {
            self.session.working_dir = working_dir;
            self.unlock_tools();
            self.session.refresh_initial_session_context_message();
        }
    }

    /// Mark this agent session as closed and persist it.
    pub fn mark_closed(&mut self) {
        // Closing releases ownership and persists the whole session; it is
        // never the resume commit, so it must not announce `session_start`.
        let persisted = !self.session.active_context().is_empty();
        self.close_session_binding();
        if persisted {
            self.persist_session_best_effort("session close state");
        }
        self.fire_session_lifecycle_hook("session_end", "close");
    }

    /// Close the current binding without writing its session back.
    ///
    /// A restore flushes the outgoing session before it reads the replacement,
    /// because a close write landing after that read appends a tail the fresh
    /// facade refuses as a source conflict. So it uses this half, and
    /// [`Self::mark_closed`] adds the write for every other caller.
    pub(crate) fn close_session_binding(&mut self) {
        self.clear_pending_resume();
        self.persist_soft_interrupt_snapshot();
        self.session.mark_closed();
    }

    /// Fire a session lifecycle observer hook (`session_start`/`session_end`).
    /// No-op when the hook is not configured.
    pub(crate) fn fire_session_lifecycle_hook(&self, event_name: &'static str, source: &str) {
        if !crate::hooks::hook_configured(event_name) {
            return;
        }
        let mut event = crate::hooks::HookEvent::new(event_name)
            .session_id(self.session.id.clone())
            .field("SOURCE", source)
            .field("MODEL", self.provider_model());
        if let Some(cwd) = self.working_dir() {
            event = event.cwd(cwd);
        }
        crate::hooks::dispatch_observer(event);
    }

    pub fn mark_crashed(&mut self, message: Option<String>) {
        self.clear_pending_resume();
        self.persist_soft_interrupt_snapshot();
        self.session.mark_crashed(message);
        if !self.session.active_context().is_empty() {
            self.persist_session_best_effort("session crash state");
        }
    }

    /// Get the last token usage from the most recent API request
    pub fn last_usage(&self) -> &TokenUsage {
        &self.last_usage
    }

    pub fn token_usage_totals(&self) -> crate::protocol::TokenUsageTotals {
        self.session.token_usage_totals()
    }
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
