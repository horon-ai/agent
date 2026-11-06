//! Tests for the clientless approval wait: the tool result, the durable wait
//! record and the delivered events are asserted, not merely `Ok`.

use super::*;
use crate::agent::Agent;
use crate::config::{CORE_TOOL_PROFILE, ToolConfig};
use crate::message::{Message, StreamEvent, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::server::register_session_event_sink;
use crate::tool::{Registry, ToolExecutionMode, clear_session_tool_policy, set_session_tool_policy};
use async_trait::async_trait;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;
use tokio::sync::mpsc as tokio_mpsc;
use tokio_stream::wrappers::ReceiverStream;

struct TestHome {
    previous: Option<std::ffi::OsString>,
    _dir: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl TestHome {
    fn new() -> Self {
        let lock = crate::storage::lock_test_env();
        let dir = tempfile::tempdir().expect("temp home");
        let previous = std::env::var_os("HORON_AGENT_HOME");
        crate::env::set_var("HORON_AGENT_HOME", dir.path());
        // Own the store root and its mode before any writer can create it
        // first: the event store accepts only a 0700 root, and a directory that
        // another test's writer created with the default umask is refused as
        // "unsafe or non-private path" for the rest of this test.
        let sessions = dir.path().join("sessions");
        horon_agent_platform::fs::create_private_dir(&sessions).expect("private store root");
        crate::config::Config::invalidate_cache();
        Self {
            previous,
            _dir: dir,
            _lock: lock,
        }
    }
}

impl TestHome {
    fn path(&self) -> &std::path::Path {
        self._dir.path()
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
            None => crate::env::remove_var("HORON_AGENT_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

struct MockProvider;

#[async_trait]
impl Provider for MockProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        Err(anyhow::anyhow!("the approval tests never stream"))
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(MockProvider)
    }
}

fn context(session_id: &str, tool_call_id: &str) -> ToolContext {
    ToolContext {
        session_id: session_id.to_string(),
        message_id: "message-1".to_string(),
        tool_call_id: tool_call_id.to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

fn permission_input(wait: bool) -> Value {
    json!({
        "action": "push",
        "description": "Push the release branch",
        "rationale": "Ship the release",
        "wait": wait,
    })
}

fn wait_settings(timeout_secs: u64) -> ApprovalSettings {
    ApprovalSettings::for_test(Duration::from_secs(timeout_secs), ApprovalDefault::Deny, true)
}

fn record_for(request_id: &str) -> BlockedApproval {
    BlockedApprovalStore::open_default()
        .expect("open the approval record store")
        .get(request_id)
        .expect("read the approval records")
        .unwrap_or_else(|| panic!("a wait record must exist for {request_id}"))
}

struct DeliveredEvents {
    pending: Vec<ServerEvent>,
    resolved: Vec<ServerEvent>,
    others: Vec<ServerEvent>,
}

fn drain_events(rx: &mut mpsc::UnboundedReceiver<ServerEvent>, request_id: &str) -> DeliveredEvents {
    let mut delivered = DeliveredEvents {
        pending: Vec::new(),
        resolved: Vec::new(),
        others: Vec::new(),
    };
    while let Ok(event) = rx.try_recv() {
        match &event {
            ServerEvent::ApprovalPending { request_id: id, .. } if id == request_id => delivered.pending.push(event),
            ServerEvent::ApprovalResolved { request_id: id, .. } if id == request_id => delivered.resolved.push(event),
            _ => delivered.others.push(event),
        }
    }
    delivered
}

/// Polls the call once, so the queue entry, wait record and pending event are
/// written, without letting the call finish.
async fn poll_once<F: std::future::Future>(future: std::pin::Pin<&mut F>) -> Option<F::Output> {
    tokio::select! {
        biased;
        output = future => Some(output),
        _ = tokio::task::yield_now() => None,
    }
}

#[tokio::test]
async fn wait_true_returns_the_decision_instead_of_queued() {
    let _home = TestHome::new();
    let session_id = "approval-wait-approved";
    let (tx, mut rx) = mpsc::unbounded_channel();
    register_session_event_sink(session_id, tx);

    let system = Arc::new(SafetySystem::new());
    let call = execute_request_permission(
        permission_input(true),
        context(session_id, "call-approved"),
        Arc::clone(&system),
        wait_settings(30),
    );
    tokio::pin!(call);

    assert!(
        poll_once(call.as_mut()).await.is_none(),
        "a waiting call must not finish before a decision exists"
    );

    let pending = system.pending_requests();
    assert_eq!(pending.len(), 1, "exactly one request is queued");
    let request_id = pending[0].id.clone();
    assert!(pending[0].wait);
    assert_eq!(
        pending[0].context.as_ref().expect("request context")["tool_call_id"],
        "call-approved",
        "the queue entry must name the tool call that is blocked"
    );
    assert!(
        pending[0].expires_at.is_some(),
        "the request must carry the wait deadline"
    );

    let record = record_for(&request_id);
    assert_eq!(record.session_id, session_id);
    assert_eq!(record.request_id, request_id);
    assert_eq!(record.waiter_pid, Some(std::process::id()));
    assert!(record.waiter_started_at.is_some());
    assert!(
        record.waiter_lease_expires_at.is_some(),
        "the waiter must hold a lease the runner can respect"
    );
    assert_eq!(record.owner_epoch, 0);
    assert!(record.outcome.is_none(), "nothing is decided yet");
    assert!(
        record.tool_verdicts.is_empty(),
        "the tool must not invent per-call human verdicts"
    );

    system
        .record_decision(&request_id, true, "file", Some("looks good".to_string()))
        .expect("record the approval");

    let output = call.await.expect("an approved call returns a tool result").output;
    assert!(
        output.contains("Permission approved"),
        "the result must report the decision, got: {output}"
    );
    assert!(output.contains("looks good"), "got: {output}");
    assert!(!output.contains("queued"), "an approved wait is not queued: {output}");

    let record = record_for(&request_id);
    assert_eq!(record.outcome.as_deref(), Some("inline"));
    assert_eq!(record.inline_approved, Some(true));
    assert!(record.inline_at.is_some());
    assert!(record.decided_at.is_some());
    assert_eq!(record.waiter_pid, None, "the waiter released the record");
    assert_eq!(record.waiter_lease_expires_at, None);
    assert!(record.tool_verdicts.is_empty());

    // Plan C4: the resolved event is recorded before it is emitted, and it
    // stays recorded after the emit because nothing acknowledges it yet.
    let pending_event = record
        .pending_event
        .as_ref()
        .expect("the resolved event must be on the record before it is emitted");
    assert_eq!(pending_event["request_id"], serde_json::json!(request_id));
    assert_eq!(pending_event["approved"], serde_json::json!(true));
    assert_eq!(pending_event["via"], serde_json::json!("file"));
    assert_eq!(pending_event["resolved_by"], serde_json::json!("tool"));

    let delivered = drain_events(&mut rx, &request_id);
    assert_eq!(
        delivered.pending.len(),
        1,
        "ApprovalPending must be delivered exactly once"
    );
    assert_eq!(
        delivered.resolved.len(),
        1,
        "ApprovalResolved must be delivered exactly once"
    );
    assert!(delivered.others.is_empty(), "unexpected events: {:?}", delivered.others);
    match &delivered.resolved[0] {
        ServerEvent::ApprovalResolved {
            approved,
            via,
            resolved_by,
            ..
        } => {
            assert!(approved);
            assert_eq!(via, "file");
            assert_eq!(resolved_by, "tool");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Notification ordering: the alert must never precede the wait record
// ---------------------------------------------------------------------------

/// What the notification path found in the wait-record store at delivery time,
/// keyed by request id. The notifier performs the same read an approver's
/// immediate answer performs, so an empty result here is exactly the failure
/// that answer would report.
static STORE_AT_NOTIFICATION: Mutex<Vec<(String, bool)>> = Mutex::new(Vec::new());

fn store_observing_notifier(_action: &str, _description: &str, request_id: &str) {
    let present = BlockedApprovalStore::open_default()
        .ok()
        .and_then(|store| store.get(request_id).ok().flatten())
        .is_some();
    STORE_AT_NOTIFICATION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push((request_id.to_string(), present));
}

fn store_seen_at_notification(request_id: &str) -> Option<bool> {
    STORE_AT_NOTIFICATION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .rev()
        .find(|(seen, _)| seen == request_id)
        .map(|(_, present)| *present)
}

/// The alert and the wait record race for the same request: an approver who
/// answers the alert instantly must not read a store that does not have the
/// record yet. The notifier runs synchronously inside the tool's first poll,
/// so no sleep is involved: either the record was written first or it was not.
#[tokio::test]
async fn the_alert_is_delivered_only_after_the_wait_record_exists() {
    let _home = TestHome::new();
    let session_id = "approval-notify-order";
    safety::register_permission_notifier(store_observing_notifier);

    let system = Arc::new(SafetySystem::new());
    let call = execute_request_permission(
        permission_input(true),
        context(session_id, "call-notify-order"),
        Arc::clone(&system),
        wait_settings(30),
    );
    tokio::pin!(call);

    assert!(
        poll_once(call.as_mut()).await.is_none(),
        "a waiting call must not finish before a decision exists"
    );
    let request_id = system.pending_requests()[0].id.clone();

    assert_eq!(
        store_seen_at_notification(&request_id),
        Some(true),
        "the alert must fire only after BlockedApprovalStore::get returns the record; \
         an approver answering it immediately must not read an empty store"
    );

    system
        .record_decision(&request_id, false, "file", Some("stop here".to_string()))
        .expect("record a decision so the wait ends");
    let _ = call.await;
}

#[tokio::test]
async fn wait_false_queues_immediately_and_leaves_the_record_to_the_runner() {
    let _home = TestHome::new();
    let session_id = "approval-wait-queued";
    let (tx, mut rx) = mpsc::unbounded_channel();
    register_session_event_sink(session_id, tx);

    let system = Arc::new(SafetySystem::new());
    let output = execute_request_permission(
        permission_input(false),
        context(session_id, "call-queued"),
        Arc::clone(&system),
        wait_settings(30),
    )
    .await
    .expect("a non-waiting call returns at once")
    .output;

    assert!(
        output.contains("Permission request queued"),
        "wait = false must stay an immediate queue, got: {output}"
    );

    let pending = system.pending_requests();
    assert_eq!(pending.len(), 1);
    let request_id = pending[0].id.clone();

    let record = record_for(&request_id);
    assert_eq!(
        record.waiter_pid, None,
        "nobody is waiting, so the runner may inherit the request"
    );
    assert!(record.waiter_lease_expires_at.is_none());
    assert!(record.outcome.is_none(), "a queued request is not decided");

    let delivered = drain_events(&mut rx, &request_id);
    assert_eq!(
        delivered.pending.len(),
        0,
        "a queued call never blocks, so it emits no pending event"
    );
    assert_eq!(delivered.resolved.len(), 0, "nothing was decided");
    assert!(delivered.others.is_empty(), "unexpected events: {:?}", delivered.others);
}

#[tokio::test(start_paused = true)]
async fn an_unanswered_wait_applies_the_default_and_says_so() {
    let _home = TestHome::new();
    let session_id = "approval-wait-timeout";
    let (tx, mut rx) = mpsc::unbounded_channel();
    register_session_event_sink(session_id, tx);

    let system = Arc::new(SafetySystem::new());
    let call = execute_request_permission(
        permission_input(true),
        context(session_id, "call-timeout"),
        Arc::clone(&system),
        wait_settings(45),
    );
    tokio::pin!(call);

    assert!(poll_once(call.as_mut()).await.is_none());
    let request_id = system.pending_requests()[0].id.clone();

    let output = call.await.expect("a timeout is a normal result").output;
    assert!(
        output.contains("configured default (deny)"),
        "the timeout must name the default it applied, got: {output}"
    );
    assert!(
        output.contains("not a human decision"),
        "a timeout must not read as a person denying the action, got: {output}"
    );
    assert!(
        !output.starts_with("Permission denied"),
        "the timeout text must be distinguishable from a human denial, got: {output}"
    );

    let decision = system
        .decision_for(&request_id)
        .expect("the timeout records a decision");
    assert!(!decision.approved);
    assert_eq!(decision.decided_via, "timeout_default");

    let record = record_for(&request_id);
    assert_eq!(record.outcome.as_deref(), Some("inline"));
    assert_eq!(record.inline_approved, Some(false));
    assert!(record.decided_at.is_some());
    assert_eq!(record.waiter_pid, None);

    let delivered = drain_events(&mut rx, &request_id);
    assert_eq!(delivered.pending.len(), 1);
    assert_eq!(delivered.resolved.len(), 1);
    assert!(delivered.others.is_empty(), "unexpected events: {:?}", delivered.others);
    match &delivered.resolved[0] {
        ServerEvent::ApprovalResolved { approved, via, .. } => {
            assert!(!approved);
            assert_eq!(via, "timeout_default");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn a_non_ambient_session_with_the_gate_off_keeps_the_original_error() {
    let _home = TestHome::new();
    let system = Arc::new(SafetySystem::new());

    let error = execute_request_permission(
        permission_input(false),
        context("plain-session", "call-gate"),
        Arc::clone(&system),
        ApprovalSettings::for_test(Duration::from_secs(30), ApprovalDefault::Deny, false),
    )
    .await
    .expect_err("a plain session must be rejected while the gate is off");

    assert_eq!(
        error.to_string(),
        "request_permission is only available to ambient sessions (session 'plain-session')",
        "the existing error message must stay byte-identical"
    );
    assert!(
        system.pending_requests().is_empty(),
        "a rejected call must not queue a request"
    );
}

#[tokio::test]
async fn a_registered_ambient_session_still_passes_with_the_gate_off() {
    let _home = TestHome::new();
    let session_id = "approval-registered-ambient";
    register_ambient_session(session_id);

    let system = Arc::new(SafetySystem::new());
    let output = execute_request_permission(
        permission_input(false),
        context(session_id, "call-registered"),
        Arc::clone(&system),
        ApprovalSettings::for_test(Duration::from_secs(30), ApprovalDefault::Deny, false),
    )
    .await
    .expect("registered ambient sessions keep working")
    .output;
    assert!(output.contains("Permission request queued"), "got: {output}");

    unregister_ambient_session(session_id);
}

#[tokio::test]
async fn a_taken_over_lease_stops_the_turn_instead_of_returning_a_result() {
    let _home = TestHome::new();
    let session_id = "approval-wait-superseded";
    let (tx, mut rx) = mpsc::unbounded_channel();
    register_session_event_sink(session_id, tx);

    let signal = crate::agent::InterruptSignal::new();
    let _turn = crate::turn_cancel_registry::register_active_turn(session_id, signal.clone());

    let system = Arc::new(SafetySystem::new());
    let call = execute_request_permission(
        permission_input(true),
        context(session_id, "call-superseded"),
        Arc::clone(&system),
        wait_settings(30),
    );
    tokio::pin!(call);

    assert!(poll_once(call.as_mut()).await.is_none());
    let request_id = system.pending_requests()[0].id.clone();

    BlockedApprovalStore::open_default()
        .expect("open the record store")
        .update_with(&request_id, |stored| {
            stored.owner_epoch += 1;
            stored.waiter_pid = Some(std::process::id() + 1);
        })
        .expect("take the lease over");
    system
        .record_decision(&request_id, true, "file", Some("approved elsewhere".to_string()))
        .expect("record the approval");

    let error = call
        .await
        .expect_err("a superseded waiter must not return a tool result");
    assert!(
        error.to_string().contains("taken over"),
        "the error must name the takeover, got: {error}"
    );
    assert!(
        signal.is_set(),
        "the turn must be stopped so the resumed turn owns the work"
    );

    let delivered = drain_events(&mut rx, &request_id);
    assert_eq!(delivered.pending.len(), 1);
    assert_eq!(delivered.resolved.len(), 1);
    assert!(delivered.others.is_empty(), "unexpected events: {:?}", delivered.others);
    match &delivered.resolved[0] {
        ServerEvent::ApprovalResolved {
            approved,
            via,
            resolved_by,
            ..
        } => {
            assert!(approved, "the takeover event carries the real decision");
            assert_eq!(via, "superseded");
            assert_eq!(resolved_by, "runner");
        }
        other => panic!("unexpected event: {other:?}"),
    }

    // The takeover event is recorded before it is emitted too, so a crash
    // between the two replays it instead of losing it.
    let record = record_for(&request_id);
    let pending_event = record
        .pending_event
        .as_ref()
        .expect("the takeover event must be on the record");
    assert_eq!(pending_event["via"], serde_json::json!("superseded"));
    assert_eq!(pending_event["resolved_by"], serde_json::json!("runner"));
}

#[tokio::test]
async fn a_clientless_registry_with_the_setting_on_reaches_the_tool() {
    let home = TestHome::new();
    std::fs::write(
        home.path().join("config.toml"),
        "[ambient]\napproval_for_clientless_runs = true\n",
    )
    .expect("write the settings file");
    crate::config::Config::invalidate_cache();

    let session_id = "approval-reachability-clientless";
    let tools = ToolConfig {
        profile: CORE_TOOL_PROFILE.to_string(),
        ..ToolConfig::default()
    };
    let ambient = AmbientConfig {
        approval_for_clientless_runs: true,
        ..AmbientConfig::default()
    };
    let allowed = tools
        .selection_with_ambient(&ambient)
        .allowed_tools
        .expect("the core profile is an allow-list");
    assert!(
        allowed.contains("request_permission"),
        "the clientless setting must make the tool visible, got {allowed:?}"
    );

    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    // No `register_ambient_tools` and no `register_ambient_session`: this is
    // exactly what a clientless, non-ambient run builds for itself. Before the
    // registry registered the tool on its own, this call failed with
    // "Unknown tool: request_permission" and the allow-list entry was dead text.
    let registry = Registry::new(provider).await;
    set_session_tool_policy(session_id, Some(allowed), HashSet::new());

    let output = registry
        .execute(
            "request_permission",
            permission_input(false),
            context(session_id, "call-reachable"),
        )
        .await
        .expect("a clientless run must reach the tool, not get 'Unknown tool'")
        .output;
    assert!(
        output.contains("Permission request queued"),
        "the call must reach the tool body, got: {output}"
    );

    clear_session_tool_policy(session_id);
}

#[tokio::test]
async fn a_clientless_registry_with_the_setting_off_does_not_register_the_tool() {
    let _home = TestHome::new();
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let registry = Registry::new(provider).await;

    let error = registry
        .execute(
            "request_permission",
            permission_input(false),
            context("approval-setting-off", "call-setting-off"),
        )
        .await
        .expect_err("the tool must stay absent while the setting is off");
    assert!(
        error.to_string().starts_with("Unknown tool: request_permission"),
        "the setting must gate registration, got: {error}"
    );
}

// ---------------------------------------------------------------------------
// Turn-loop contract: a takeover stops the message, an ordinary error does not
// ---------------------------------------------------------------------------

/// Emits one assistant message with two tool calls: `first`, then `sibling`.
/// The second provider call (only reached when the turn keeps going) ends the
/// turn with plain text.
struct ScriptedToolsProvider {
    first: &'static str,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for ScriptedToolsProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let first = self.first;
        let (tx, rx) = tokio_mpsc::channel::<anyhow::Result<StreamEvent>>(16);
        tokio::spawn(async move {
            if call > 1 {
                let _ = tx.send(Ok(StreamEvent::TextDelta("done".to_string()))).await;
                let _ = tx
                    .send(Ok(StreamEvent::MessageEnd {
                        stop_reason: Some("end_turn".to_string()),
                    }))
                    .await;
                return;
            }
            for (id, name) in [("call_first", first), ("call_sibling", "sibling")] {
                let _ = tx
                    .send(Ok(StreamEvent::ToolUseStart {
                        id: id.to_string(),
                        name: name.to_string(),
                    }))
                    .await;
                let _ = tx
                    .send(Ok(StreamEvent::ToolInputDeltaFor {
                        id: id.to_string(),
                        delta: "{}".to_string(),
                    }))
                    .await;
                let _ = tx.send(Ok(StreamEvent::ToolUseEndFor { id: id.to_string() })).await;
            }
            let _ = tx
                .send(Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("tool_use".to_string()),
                }))
                .await;
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "scripted-tools"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(ScriptedToolsProvider {
            first: self.first,
            calls: Arc::clone(&self.calls),
        })
    }
}

/// Produces the exact error contract the approval tool produces when the
/// ambient runner takes its wait over.
struct TakeoverTool;

#[async_trait]
impl Tool for TakeoverTool {
    fn name(&self) -> &str {
        "superseding"
    }

    fn description(&self) -> &str {
        "stops its own turn"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        Err(stop_turn_for_takeover(&ctx.session_id, "req-turn-test"))
    }
}

struct RecordingSibling {
    ran: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for RecordingSibling {
    fn name(&self) -> &str {
        "sibling"
    }

    fn description(&self) -> &str {
        "runs unless the turn was stopped"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::new("sibling executed"))
    }
}

struct FailingTool;

#[async_trait]
impl Tool for FailingTool {
    fn name(&self) -> &str {
        "failing"
    }

    fn description(&self) -> &str {
        "fails like an ordinary tool"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        anyhow::bail!("boom")
    }
}

/// Fires the turn's cancel signal and then keeps blocking, which is what the
/// approval tool does while the runner is still taking its wait over. The
/// streaming loop therefore reaches its cancel branch before any typed error
/// exists, and only the out-of-band mark can name the reason.
struct HangingTakeoverTool;

#[async_trait]
impl Tool for HangingTakeoverTool {
    fn name(&self) -> &str {
        "hanging-takeover"
    }

    fn description(&self) -> &str {
        "fires the cancel signal and keeps blocking"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        crate::tool::superseded::mark(&ctx.session_id);
        for signal in crate::turn_cancel_registry::active_turn_signals(&ctx.session_id) {
            signal.fire();
        }
        std::future::pending::<()>().await;
        unreachable!("the turn loop aborts this tool")
    }
}

async fn registry_with_turn_tools(
    provider: Arc<dyn Provider>,
    first: &'static str,
    sibling_ran: Arc<AtomicUsize>,
) -> Registry {
    let registry = Registry::new(provider).await;
    let first_tool: Arc<dyn Tool> = match first {
        "superseding" => Arc::new(TakeoverTool),
        "hanging-takeover" => Arc::new(HangingTakeoverTool),
        _ => Arc::new(FailingTool),
    };
    registry.register(first.to_string(), first_tool).await;
    registry
        .register(
            "sibling".to_string(),
            Arc::new(RecordingSibling { ran: sibling_ran }) as Arc<dyn Tool>,
        )
        .await;
    registry
}

#[tokio::test]
async fn turn_loops_stop_a_takeover_before_its_siblings_and_record_nothing() {
    let _home = TestHome::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let sibling_ran = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(ScriptedToolsProvider {
        first: "superseding",
        calls: Arc::clone(&calls),
    });
    let registry = registry_with_turn_tools(provider.clone(), "superseding", Arc::clone(&sibling_ran)).await;
    let mut agent = Agent::new(provider, registry);

    agent
        .run_once("do the work")
        .await
        .expect("a takeover ends the turn without an error");

    assert_eq!(
        sibling_ran.load(Ordering::SeqCst),
        0,
        "a sibling in the same assistant message must not run after the takeover"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the turn must stop instead of asking the provider again"
    );
    let history = format!("{:?}", agent.messages());
    assert!(
        !history.contains("taken over"),
        "the supersede error must not be recorded as a tool result: {history}"
    );
    assert!(
        !history.contains("sibling executed"),
        "no sibling result may be recorded: {history}"
    );
}

#[tokio::test]
async fn turn_loops_record_an_ordinary_tool_error_and_still_run_its_siblings() {
    let _home = TestHome::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let sibling_ran = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(ScriptedToolsProvider {
        first: "failing",
        calls: Arc::clone(&calls),
    });
    let registry = registry_with_turn_tools(provider.clone(), "failing", Arc::clone(&sibling_ran)).await;
    let mut agent = Agent::new(provider, registry);

    agent.run_once("do the work").await.expect("the turn completes");

    assert_eq!(
        sibling_ran.load(Ordering::SeqCst),
        1,
        "an ordinary tool error must not stop the sibling"
    );
    let history = format!("{:?}", agent.messages());
    assert!(
        history.contains("Error: boom"),
        "an ordinary tool error must still be recorded: {history}"
    );
    assert!(
        history.contains("sibling executed"),
        "the sibling result must still be recorded: {history}"
    );
}

#[tokio::test]
async fn turn_streaming_stops_a_takeover_without_claiming_a_server_reload() {
    let _home = TestHome::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let sibling_ran = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(ScriptedToolsProvider {
        first: "superseding",
        calls: Arc::clone(&calls),
    });
    let registry = registry_with_turn_tools(provider.clone(), "superseding", Arc::clone(&sibling_ran)).await;
    let mut agent = Agent::new(provider, registry);
    let (tx, _rx) = mpsc::unbounded_channel();

    agent
        .run_once_streaming_mpsc("do the work", Vec::new(), None, tx)
        .await
        .expect("a takeover ends the streamed turn without an error");

    assert_eq!(
        sibling_ran.load(Ordering::SeqCst),
        0,
        "a sibling in the same assistant message must not run after the takeover"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the turn must stop instead of asking the provider again"
    );
    let history = format!("{:?}", agent.messages());
    assert!(
        !history.contains("taken over"),
        "the supersede error must not be recorded as a tool result: {history}"
    );
    assert!(
        !history.contains("server reload"),
        "a takeover is not a server reload: {history}"
    );
}

#[tokio::test]
async fn turn_streaming_names_a_takeover_the_tool_had_not_reported_yet() {
    let _home = TestHome::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let sibling_ran = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(ScriptedToolsProvider {
        first: "hanging-takeover",
        calls: Arc::clone(&calls),
    });
    let registry = registry_with_turn_tools(provider.clone(), "hanging-takeover", Arc::clone(&sibling_ran)).await;
    let mut agent = Agent::new(provider, registry);
    let (tx, _rx) = mpsc::unbounded_channel();

    agent
        .run_once_streaming_mpsc("do the work", Vec::new(), None, tx)
        .await
        .expect("a takeover ends the streamed turn without an error");

    assert_eq!(
        sibling_ran.load(Ordering::SeqCst),
        0,
        "the sibling must not run after the takeover"
    );
    let history = format!("{:?}", agent.messages());
    assert!(
        !history.contains("server reload"),
        "the cancel branch must not blame a reload for a takeover: {history}"
    );
    assert!(
        !history.contains("[Skipped"),
        "a takeover must not rewrite the remaining calls as skipped: {history}"
    );
    assert!(
        !crate::tool::superseded::is_marked(agent.session_id()),
        "the mark must be consumed, so a later turn is not relabelled"
    );
}
