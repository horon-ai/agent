//! Resume-once tests for blocked approvals.
//!
//! Every test asserts the invariant the plan names, not merely that a call
//! returned `Ok`: a session is continued **exactly once** for an approval (and
//! exactly zero times for a denial), counted from the session transcript and,
//! independently, from the provider's turn counter.

use super::persistence::{
    ExecClaimOutcome, ResumeClaimState, ResumeClaimStore, ResumeExecClaim, ResumeExecClaimStore, ResumeExecState,
    reconcile_orphan_requests, resume_exec_claims_dir,
};
use super::resume::{
    RESUME_ATTEMPT_CAP, RESUME_EXEC_ATTEMPT_CAP, RESUME_UNVERIFIABLE_SENTINEL, ResumeOrigin, build_resume_prompt,
    resume_exec_lease,
};
use super::runner::AmbientRunnerHandle;
use super::{BlockedApproval, BlockedApprovalStore};
use crate::message::{ContentBlock, Message, Role, StreamEvent, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::safety::{PermissionRequest, SafetySystem, Urgency};
use crate::session::{Session, SessionStatus};
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Isolates `~/.horon-agent` for one test and serializes env mutation.
struct TestHome {
    previous: Option<std::ffi::OsString>,
    dir: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl TestHome {
    fn new() -> Self {
        let lock = crate::storage::lock_test_env();
        let dir = tempfile::tempdir().expect("temp home");
        let previous = std::env::var_os("HORON_AGENT_HOME");
        crate::env::set_var("HORON_AGENT_HOME", dir.path());
        crate::config::Config::invalidate_cache();
        Self {
            previous,
            dir,
            _lock: lock,
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.path().to_path_buf()
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

/// A provider that answers every turn with one line and counts its turns.
#[derive(Clone, Default)]
struct CountingProvider {
    turns: Arc<AtomicUsize>,
    reply: Arc<StdMutex<String>>,
}

impl CountingProvider {
    fn new() -> Self {
        Self {
            turns: Arc::new(AtomicUsize::new(0)),
            reply: Arc::new(StdMutex::new("continued".to_string())),
        }
    }

    fn replying(text: &str) -> Self {
        let provider = Self::new();
        *provider.reply.lock().expect("reply lock") = text.to_string();
        provider
    }

    fn turns(&self) -> usize {
        self.turns.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Provider for CountingProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        self.turns.fetch_add(1, Ordering::SeqCst);
        let reply = self.reply.lock().expect("reply lock").clone();
        let stream = async_stream::stream! {
            yield Ok(StreamEvent::TextDelta(reply));
            yield Ok(StreamEvent::MessageEnd { stop_reason: None });
        };
        Ok(Box::pin(stream))
    }

    fn name(&self) -> &str {
        "counting"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn closed_session(id: &str) -> Session {
    let mut session = Session::create_with_id(id.to_string(), None, Some("Blocked run".to_string()));
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "Do the long job".to_string(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "Working on it".to_string(),
            cache_control: None,
        }],
    );
    session.status = SessionStatus::Closed;
    session.save().expect("save session");
    session
}

fn record(session_id: &str, request_id: &str) -> BlockedApproval {
    BlockedApproval {
        session_id: session_id.to_string(),
        request_id: request_id.to_string(),
        created_at: Utc::now(),
        ..BlockedApproval::default()
    }
}

/// How many resume turns the session transcript holds.
///
/// This is the invariant itself: the recorded marker says a turn was created,
/// and the transcript says how many actually reached the model.
fn resume_turns(session_id: &str) -> usize {
    Session::load(session_id)
        .expect("load session")
        .active_context()
        .iter()
        .filter(|message| {
            message.content.iter().any(
                |block| matches!(block, ContentBlock::Text { text, .. } if text.starts_with("[Resume after approval]")),
            )
        })
        .count()
}

/// How many marker events the session's event log holds for `request_id`.
///
/// The marker is a `MetadataChanged` event, so this reads the same durable
/// original the session loads from. The read-back API dedupes by key, so only a
/// count over the log itself can show that a duplicate append did not happen.
fn resume_marker_events(session_id: &str, request_id: &str) -> usize {
    let log = Session::load(session_id)
        .expect("load session")
        .event_log_path()
        .expect("event log path");
    let needle = format!("\"resume_key\":\"{request_id}\"");
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(&needle))
        .count()
}

fn runner() -> AmbientRunnerHandle {
    AmbientRunnerHandle::new(Arc::new(SafetySystem::new()))
}

fn store() -> BlockedApprovalStore {
    BlockedApprovalStore::open_default().expect("approval store")
}

/// Collect the events emitted to one session, in order.
fn event_sink(session_id: &str) -> tokio::sync::mpsc::UnboundedReceiver<crate::protocol::ServerEvent> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    crate::server::register_session_event_sink(session_id, tx);
    rx
}

fn drain(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::protocol::ServerEvent>,
) -> Vec<crate::protocol::ServerEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

// ---------------------------------------------------------------------------
// (1)(2) an approval resumes once, and a second pass resumes zero times
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_approval_resumes_the_session_exactly_once() {
    let _home = TestHome::new();
    let session = closed_session("sess_resume_once");
    let request_id = "req_resume_once";
    store().add(record(&session.id, request_id)).expect("add record");

    let runner = runner();
    runner
        .safety()
        .record_decision(request_id, true, "file", None)
        .expect("record approval");

    let provider = CountingProvider::new();
    let provider_dyn: Arc<dyn Provider> = Arc::new(provider.clone());
    runner.resume_blocked_approvals(&provider_dyn).await;

    assert_eq!(
        resume_turns(&session.id),
        1,
        "an approval must continue the run exactly once"
    );
    assert_eq!(
        provider.turns(),
        1,
        "the model must be called for exactly one resumed turn"
    );
    assert!(Session::journal_has_resume_key(&session.id, request_id).unwrap());

    let stored = store().get(request_id).unwrap().expect("record present");
    assert_eq!(stored.outcome.as_deref(), Some("resumed"));
    assert!(stored.resumed_at.is_some(), "the record must show when it resumed");
    assert_eq!(
        stored.pending_event.as_ref().and_then(|event| event.get("approved")),
        Some(&serde_json::Value::Bool(true)),
        "an ApprovalResolved event must be recorded for the client"
    );

    // (2) the next loop iteration must not resume it again.
    runner.resume_blocked_approvals(&provider_dyn).await;
    assert_eq!(
        resume_turns(&session.id),
        1,
        "a second pass must not resume the run again"
    );
    assert_eq!(provider.turns(), 1);
}

// ---------------------------------------------------------------------------
// (3) a live waiter lease keeps the runner out
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_live_waiter_lease_keeps_the_runner_out() {
    let _home = TestHome::new();
    let session = closed_session("sess_live_lease");
    let request_id = "req_live_lease";

    let mut held = record(&session.id, request_id);
    held.waiter_pid = Some(std::process::id());
    held.waiter_started_at = Some(Utc::now());
    held.waiter_lease_expires_at = Some(Utc::now() + chrono::Duration::seconds(30));
    store().add(held).expect("add record");

    let runner = runner();
    runner
        .safety()
        .record_decision(request_id, true, "file", None)
        .expect("record approval");

    let provider: Arc<dyn Provider> = Arc::new(CountingProvider::new());
    runner.resume_blocked_approvals(&provider).await;

    assert_eq!(
        resume_turns(&session.id),
        0,
        "a waiter that still holds the lease owns the run"
    );
    assert!(!Session::journal_has_resume_key(&session.id, request_id).unwrap());
}

// ---------------------------------------------------------------------------
// (4) an expired lease hands the run over
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_expired_lease_hands_the_run_to_the_runner() {
    let _home = TestHome::new();
    let session = closed_session("sess_expired_lease");
    let request_id = "req_expired_lease";

    let mut abandoned = record(&session.id, request_id);
    abandoned.waiter_pid = Some(999_999);
    abandoned.waiter_started_at = Some(Utc::now() - chrono::Duration::minutes(5));
    abandoned.waiter_lease_expires_at = Some(Utc::now() - chrono::Duration::seconds(1));
    store().add(abandoned).expect("add record");

    let runner = runner();
    runner
        .safety()
        .record_decision(request_id, true, "file", None)
        .expect("record approval");

    let provider: Arc<dyn Provider> = Arc::new(CountingProvider::new());
    runner.resume_blocked_approvals(&provider).await;

    assert_eq!(
        resume_turns(&session.id),
        1,
        "a waiter whose lease expired no longer owns the run"
    );
}

// ---------------------------------------------------------------------------
// (6) a denial resumes zero times
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_denial_resumes_zero_times() {
    let _home = TestHome::new();
    let session = closed_session("sess_denied");
    let request_id = "req_denied";
    store().add(record(&session.id, request_id)).expect("add record");

    let runner = runner();
    runner
        .safety()
        .record_decision(request_id, false, "file", None)
        .expect("record denial");

    let provider = CountingProvider::new();
    let provider_dyn: Arc<dyn Provider> = Arc::new(provider.clone());
    runner.resume_blocked_approvals(&provider_dyn).await;

    assert_eq!(
        resume_turns(&session.id),
        0,
        "a denial must never re-run work a person refused"
    );
    assert_eq!(provider.turns(), 0);

    let stored = store().get(request_id).unwrap().expect("record present");
    assert_eq!(stored.outcome.as_deref(), Some("denied"));
    assert_eq!(
        stored.pending_event.as_ref().and_then(|event| event.get("approved")),
        Some(&serde_json::Value::Bool(false))
    );

    // And a later pass still must not resume it.
    runner.resume_blocked_approvals(&provider_dyn).await;
    assert_eq!(resume_turns(&session.id), 0);
}

// ---------------------------------------------------------------------------
// (7) a decision another process wrote is observed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_decision_written_by_another_process_is_observed() {
    let _home = TestHome::new();
    let session = closed_session("sess_external_decision");
    let request_id = "req_external_decision";
    store().add(record(&session.id, request_id)).expect("add record");

    // The runner's SafetySystem was constructed before the decision existed, so
    // its in-memory history is empty; only the file fallback can find it.
    let runner = runner();
    assert!(runner.safety().decision_for(request_id).is_none());

    crate::safety::record_permission_via_file(request_id, true, "file", None)
        .expect("an external approver writes the decision");

    let provider: Arc<dyn Provider> = Arc::new(CountingProvider::new());
    runner.resume_blocked_approvals(&provider).await;

    assert_eq!(
        resume_turns(&session.id),
        1,
        "a decision written by another process must be picked up"
    );
}

// ---------------------------------------------------------------------------
// (8) journal idempotency
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_resume_key_is_idempotent() {
    let _home = TestHome::new();
    let session = closed_session("sess_key_idempotent");
    let request_id = "req_key_idempotent";

    Session::append_resume_key(&session.id, request_id).expect("first append");
    let first = Session::resume_key_recorded_at(&session.id, request_id)
        .expect("read the marker")
        .expect("the marker is recorded");
    Session::append_resume_key(&session.id, request_id).expect("second append");
    let second = Session::resume_key_recorded_at(&session.id, request_id)
        .expect("read the marker")
        .expect("the marker is recorded");

    assert_eq!(
        resume_marker_events(&session.id, request_id),
        1,
        "the same key must be recorded once"
    );
    assert_eq!(
        first, second,
        "a repeated append must not move the moment the turn was created"
    );
    assert!(Session::journal_has_resume_key(&session.id, request_id).unwrap());
}

/// Recording a marker must not move the session's own `updated_at`.
///
/// The resume decision reads `session.updated_at > recorded_at` to tell a turn
/// that was created and then ran from one that was only created. A marker that
/// stamped the session's timestamp would make every created turn look like it
/// had already run, so recovery would stop executing it: the crash rows would
/// silently stop recovering while the whole matrix stayed green.
#[tokio::test]
async fn a_resume_marker_does_not_move_the_session_timestamp() {
    let _home = TestHome::new();
    let session = closed_session("sess_marker_timestamp");
    let request_id = "req_marker_timestamp";
    let before = Session::load(&session.id).expect("load session").updated_at;

    Session::append_resume_key(&session.id, request_id).expect("append the key");

    let after = Session::load(&session.id).expect("load session").updated_at;
    assert_eq!(
        after, before,
        "the marker is not a session change, so it must not move updated_at"
    );
    // The timestamp assertion alone would also hold if the append did nothing,
    // which is exactly how a commented-out call once stayed green.
    assert!(
        Session::journal_has_resume_key(&session.id, request_id).unwrap(),
        "the marker must actually be recorded"
    );
    assert_eq!(resume_marker_events(&session.id, request_id), 1);
}

// ---------------------------------------------------------------------------
// (9-e)(9-f) inline outcomes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_inline_denial_is_never_resumed() {
    let _home = TestHome::new();
    let session = closed_session("sess_inline_denied");
    let request_id = "req_inline_denied";

    let mut record = record(&session.id, request_id);
    record.inline_at = Some(Utc::now());
    record.inline_approved = Some(false);
    record.outcome = Some("inline".to_string());
    store().add(record).expect("add record");

    let runner = runner();
    let provider: Arc<dyn Provider> = Arc::new(CountingProvider::new());
    runner.resume_blocked_approvals(&provider).await;

    assert_eq!(
        resume_turns(&session.id),
        0,
        "an inline denial is final and must not be picked up"
    );
    let stored = store().get(request_id).unwrap().expect("record present");
    assert_eq!(stored.outcome.as_deref(), Some("denied"));
    assert_eq!(
        stored.pending_event.as_ref().and_then(|event| event.get("approved")),
        Some(&serde_json::Value::Bool(false))
    );
}

#[tokio::test]
async fn an_inline_approval_on_a_stopped_session_resumes_once() {
    let _home = TestHome::new();
    let session = closed_session("sess_inline_approved");
    let request_id = "req_inline_approved";

    let mut record = record(&session.id, request_id);
    record.inline_at = Some(Utc::now());
    record.inline_approved = Some(true);
    record.outcome = Some("inline".to_string());
    store().add(record).expect("add record");

    let runner = runner();
    let provider: Arc<dyn Provider> = Arc::new(CountingProvider::new());
    runner.resume_blocked_approvals(&provider).await;

    assert_eq!(
        resume_turns(&session.id),
        1,
        "an approval the waiting turn could not continue must be picked up once"
    );
    // (9-f) the recovery records its own key, so the same crash cannot produce a
    // second turn.
    assert!(Session::journal_has_resume_key(&session.id, request_id).unwrap());
    runner.resume_blocked_approvals(&provider).await;
    assert_eq!(resume_turns(&session.id), 1);
}

#[tokio::test]
async fn an_inline_approval_on_a_live_session_waits() {
    let _home = TestHome::new();
    let mut session = closed_session("sess_inline_live");
    session.status = SessionStatus::Active;
    session.save().expect("save active session");
    let request_id = "req_inline_live";

    let mut record = record(&session.id, request_id);
    record.inline_at = Some(Utc::now());
    record.inline_approved = Some(true);
    record.outcome = Some("inline".to_string());
    store().add(record).expect("add record");

    let runner = runner();
    let provider: Arc<dyn Provider> = Arc::new(CountingProvider::new());
    runner.resume_blocked_approvals(&provider).await;

    assert_eq!(
        resume_turns(&session.id),
        0,
        "a session that is still running must not be resumed underneath itself"
    );
}

// ---------------------------------------------------------------------------
// (9-j) per-call human verdicts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn human_verdicts_resume_once_and_name_the_call() {
    let _home = TestHome::new();
    let session = closed_session("sess_human_verdict");
    let request_id = "req_human_verdict";

    let mut record = record(&session.id, request_id);
    record.outcome = Some("human_skip".to_string());
    record
        .tool_verdicts
        .insert("call_skip_me".to_string(), "human_skip".to_string());
    record
        .tool_verdicts
        .insert("call_retry_me".to_string(), "human_retry".to_string());
    store().add(record).expect("add record");

    let runner = runner();
    let provider: Arc<dyn Provider> = Arc::new(CountingProvider::new());
    runner.resume_blocked_approvals(&provider).await;

    assert_eq!(
        resume_turns(&session.id),
        1,
        "retry and skip both continue the session exactly once"
    );

    let resumed = Session::load(&session.id).expect("load session");
    let prompt = resumed
        .active_context()
        .iter()
        .find_map(|message| {
            message.content.iter().find_map(|block| match block {
                ContentBlock::Text { text, .. } if text.starts_with("[Resume after approval]") => Some(text.clone()),
                _ => None,
            })
        })
        .expect("the resume prompt is in the transcript");
    assert!(
        prompt.contains("call_skip_me: human_skip - do not run this call again"),
        "the prompt must tell the model to skip the named call: {prompt}"
    );
    assert!(
        prompt.contains("call_retry_me: human_retry - run exactly this call again"),
        "the prompt must tell the model to retry the named call: {prompt}"
    );
}

/// A human verdict the waiting turn already used is spent.
///
/// The CLI writes the verdict to the approval record *and* to the decision
/// history, so a turn blocked in `request_permission(wait = true)` wakes with it
/// instead of applying its deny default. That turn then acts on the verdict and
/// finishes; the record keeps `tool_verdicts` and the inline decision fields.
/// Resuming it again would tell the model to run the named call a second time.
#[tokio::test]
async fn a_human_verdict_the_waiting_turn_already_used_is_not_resumed() {
    let _home = TestHome::new();
    let session = closed_session("sess_verdict_consumed");
    let request_id = "req_verdict_consumed";

    let mut record = record(&session.id, request_id);
    record.outcome = Some("human_retry".to_string());
    record
        .tool_verdicts
        .insert("call_retry_me".to_string(), "human_retry".to_string());
    record.decided_at = Some(Utc::now());
    record.inline_at = Some(Utc::now());
    record.inline_approved = Some(true);
    store().add(record).expect("add record");

    let runner = runner();
    runner
        .safety()
        .record_decision(request_id, true, "human_retry", Some("human retry".to_string()))
        .expect("record the verdict the CLI wrote");

    let provider: Arc<dyn Provider> = Arc::new(CountingProvider::new());
    runner.resume_blocked_approvals(&provider).await;

    assert_eq!(
        resume_turns(&session.id),
        0,
        "a verdict the waiting turn already received as its tool result must not run the call again"
    );
}

/// The same record, but the wait elapsed before the verdict reached it.
///
/// This is the state the CLI leaves when the blocked turn applied its own
/// default: `inline_at` is set, yet the history holds the timeout decision, not
/// the human verdict. Nothing else can deliver the verdict, so the runner must.
#[tokio::test]
async fn a_human_verdict_the_waiting_turn_never_saw_still_resumes() {
    let _home = TestHome::new();
    let session = closed_session("sess_verdict_missed");
    let request_id = "req_verdict_missed";

    let mut record = record(&session.id, request_id);
    record.outcome = Some("human_retry".to_string());
    record
        .tool_verdicts
        .insert("call_retry_me".to_string(), "human_retry".to_string());
    record.decided_at = Some(Utc::now());
    record.inline_at = Some(Utc::now());
    record.inline_approved = Some(false);
    store().add(record).expect("add record");

    let runner = runner();
    runner
        .safety()
        .record_decision(
            request_id,
            false,
            crate::safety::TIMEOUT_DEFAULT_VIA,
            Some("No decision arrived before the wait elapsed".to_string()),
        )
        .expect("record the timeout default");

    let provider: Arc<dyn Provider> = Arc::new(CountingProvider::new());
    runner.resume_blocked_approvals(&provider).await;

    assert_eq!(
        resume_turns(&session.id),
        1,
        "a verdict the waiting turn never saw is the runner's to deliver"
    );
}

/// The event a client sees when the recorded copy is already acknowledged.
///
/// A client that acked the recorded `ApprovalResolved` leaves `pending_event`
/// empty, so the live delivery is rebuilt from the record's `tool_verdicts`.
/// Nothing else exercises that rebuild, so a wrong `approved` or `via` there
/// would reach clients untested. Both verdicts are pinned: the mapping runs in
/// both directions and a copy-paste error would hide in one of them.
async fn approval_resolved_after_a_client_ack(
    session_id: &str,
    request_id: &str,
    verdict: &str,
) -> crate::protocol::ServerEvent {
    let session = closed_session(session_id);

    let mut record = record(&session.id, request_id);
    record.outcome = Some("needs-human".to_string());
    record.tool_verdicts.insert("call_x".to_string(), verdict.to_string());
    // The whole point: a client acknowledged the recorded event, so only the
    // rebuilt copy is left to deliver.
    record.pending_event = None;
    store().add(record).expect("add record");

    let runner = runner();
    let mut events = event_sink(&session.id);
    let provider: Arc<dyn Provider> = Arc::new(CountingProvider::new());
    runner.resume_blocked_approvals(&provider).await;

    assert_eq!(resume_turns(&session.id), 1, "the verdict continues the run once");
    let emitted: Vec<_> = drain(&mut events)
        .into_iter()
        .filter(|event| matches!(event, crate::protocol::ServerEvent::ApprovalResolved { .. }))
        .collect();
    assert_eq!(
        emitted.len(),
        1,
        "exactly one ApprovalResolved reaches the client: {emitted:?}"
    );
    emitted.into_iter().next().expect("one event")
}

#[tokio::test]
async fn a_rebuilt_human_verdict_event_names_the_verdict_and_its_direction() {
    let _home = TestHome::new();

    let skipped =
        approval_resolved_after_a_client_ack("sess_verdict_rebuilt_skip", "req_verdict_rebuilt_skip", "human_skip")
            .await;
    match &skipped {
        crate::protocol::ServerEvent::ApprovalResolved {
            request_id,
            approved,
            via,
            resolved_by,
        } => {
            assert_eq!(
                request_id, "req_verdict_rebuilt_skip",
                "the event names the record's request"
            );
            assert_eq!(via, "human_skip", "the rebuilt event carries the skip verdict");
            assert!(!*approved, "a skip is not an approval");
            assert_eq!(resolved_by, "human");
        }
        other => panic!("expected an ApprovalResolved, got {other:?}"),
    }

    let retried =
        approval_resolved_after_a_client_ack("sess_verdict_rebuilt_retry", "req_verdict_rebuilt_retry", "human_retry")
            .await;
    match &retried {
        crate::protocol::ServerEvent::ApprovalResolved { approved, via, .. } => {
            assert_eq!(via, "human_retry", "the rebuilt event carries the retry verdict");
            assert!(*approved, "a retry is an approval");
        }
        other => panic!("expected an ApprovalResolved, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// (9-g) an unverified side effect stops the run
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unverifiable_side_effect_stops_the_run_for_a_human() {
    let _home = TestHome::new();
    let mut session = closed_session("sess_unverifiable");
    // A tool that started and never recorded a result: whether the side effect
    // happened is unknown.
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "call_risky_write".to_string(),
            name: "write".to_string(),
            input: serde_json::json!({"path": "notes/out.md", "content": "x"}),
            thought_signature: None,
        }],
    );
    session.save().expect("save session");
    let request_id = "req_unverifiable";
    store().add(record(&session.id, request_id)).expect("add record");

    let runner = runner();
    let mut events = event_sink(&session.id);
    runner
        .safety()
        .record_decision(request_id, true, "file", None)
        .expect("record approval");

    let provider: Arc<dyn Provider> = Arc::new(CountingProvider::replying(
        "[resume-unverifiable] call_risky_write no trace of the write on disk",
    ));
    runner.resume_blocked_approvals(&provider).await;

    assert_eq!(resume_turns(&session.id), 1, "the turn runs once, then stops");
    let stored = store().get(request_id).unwrap().expect("record present");
    assert_eq!(
        stored.outcome.as_deref(),
        Some("needs-human"),
        "an unverifiable side effect must wait for a person"
    );

    let emitted = drain(&mut events);
    assert_eq!(
        emitted
            .iter()
            .filter(|event| matches!(event, crate::protocol::ServerEvent::RunNeedsHumanResume { .. }))
            .count(),
        1,
        "the stop must be announced exactly once: {emitted:?}"
    );

    // Nothing resumes it again while a person has not decided.
    runner.resume_blocked_approvals(&provider).await;
    assert_eq!(resume_turns(&session.id), 1);
}

#[test]
fn the_resume_prompt_names_interrupted_calls_and_args() {
    let _home = TestHome::new();
    let mut session = closed_session("sess_prompt");
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "call_write".to_string(),
            name: "write".to_string(),
            input: serde_json::json!({"path": "report.md"}),
            thought_signature: None,
        }],
    );
    session.save().expect("save session");
    let loaded = Session::load(&session.id).expect("load session");

    let prompt = build_resume_prompt(
        &loaded,
        &record(&session.id, "req_prompt"),
        &ResumeOrigin::Approved,
        None,
    );

    assert!(
        prompt.contains("call_write: write("),
        "the prompt names the call and its args: {prompt}"
    );
    assert!(prompt.contains("report.md"));
    assert!(prompt.contains("Do not run any of them again"));
    assert!(prompt.contains(RESUME_UNVERIFIABLE_SENTINEL));
}

// ---------------------------------------------------------------------------
// (11) attempt budget exhaustion escalates once
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_exhausted_resume_budget_asks_for_a_human() {
    let _home = TestHome::new();
    let session = closed_session("sess_budget");
    let request_id = "req_budget";
    store().add(record(&session.id, request_id)).expect("add record");

    // A previous process spent both attempts without the key ever landing.
    let claims = ResumeClaimStore::open_default().expect("claim store");
    let mut claim = claims.acquire(request_id).expect("claim");
    claim.attempts = RESUME_ATTEMPT_CAP;
    claim.state = ResumeClaimState::Resuming;
    claims.save(&claim).expect("save claim");

    let runner = runner();
    let mut events = event_sink(&session.id);
    runner
        .safety()
        .record_decision(request_id, true, "file", None)
        .expect("record approval");

    let provider: Arc<dyn Provider> = Arc::new(CountingProvider::new());
    runner.resume_blocked_approvals(&provider).await;

    assert_eq!(
        resume_turns(&session.id),
        0,
        "a spent budget must not create another turn"
    );
    let stored = store().get(request_id).unwrap().expect("record present");
    assert_eq!(stored.outcome.as_deref(), Some("needs-human"));
    let closed = claims.get(request_id).expect("claim").expect("claim present");
    assert_eq!(closed.state, ResumeClaimState::NeedsHuman);

    let emitted = drain(&mut events);
    assert_eq!(
        emitted
            .iter()
            .filter(|event| matches!(event, crate::protocol::ServerEvent::RunNeedsHumanResume { .. }))
            .count(),
        1,
        "the escalation must be announced exactly once: {emitted:?}"
    );

    // A further pass must not announce it again.
    runner.resume_blocked_approvals(&provider).await;
    assert_eq!(resume_turns(&session.id), 0);
}

// ---------------------------------------------------------------------------
// (9-b)(9-d) a created turn that never ran
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_created_turn_that_never_ran_is_executed_without_a_new_turn() {
    let _home = TestHome::new();
    let session = closed_session("sess_created_not_run");
    let request_id = "req_created_not_run";
    store().add(record(&session.id, request_id)).expect("add record");

    // A previous process created the turn (the key is durable) and died before
    // running it.
    Session::append_resume_key(&session.id, request_id).expect("append key");

    let runner = runner();
    runner
        .safety()
        .record_decision(request_id, true, "file", None)
        .expect("record approval");

    let provider: Arc<dyn Provider> = Arc::new(CountingProvider::new());
    runner.resume_blocked_approvals(&provider).await;

    assert_eq!(
        resume_turns(&session.id),
        1,
        "the created turn must be executed, not duplicated"
    );
    assert_eq!(
        resume_marker_events(&session.id, request_id),
        1,
        "no second turn may be created for the same key"
    );
    let exec = ResumeExecClaimStore::open_default()
        .expect("exec claim store")
        .try_acquire(request_id, Utc::now(), resume_exec_lease(), RESUME_EXEC_ATTEMPT_CAP)
        .expect("read exec claim");
    match exec {
        ExecClaimOutcome::Done(claim) => {
            assert_eq!(claim.state, ResumeExecState::Done);
            assert_eq!(claim.exec_attempts, 1, "one execution was spent");
        }
        other => panic!("the execution claim must be closed as done, got {other:?}"),
    }
}

#[tokio::test]
async fn a_spent_execution_budget_asks_for_a_human() {
    let _home = TestHome::new();
    let session = closed_session("sess_exec_budget");
    let request_id = "req_exec_budget";
    store().add(record(&session.id, request_id)).expect("add record");
    Session::append_resume_key(&session.id, request_id).expect("append key");

    // Two owners died holding the execution claim.
    let dir = resume_exec_claims_dir().expect("exec claims dir");
    std::fs::write(
        dir.join(format!("{request_id}.json")),
        serde_json::to_vec_pretty(&ResumeExecClaim {
            request_id: request_id.to_string(),
            state: ResumeExecState::Idle,
            exec_attempts: RESUME_EXEC_ATTEMPT_CAP,
            ..Default::default()
        })
        .expect("serialize claim"),
    )
    .expect("seed exec claim");

    let runner = runner();
    let mut events = event_sink(&session.id);
    runner
        .safety()
        .record_decision(request_id, true, "file", None)
        .expect("record approval");

    let provider: Arc<dyn Provider> = Arc::new(CountingProvider::new());
    runner.resume_blocked_approvals(&provider).await;

    assert_eq!(resume_turns(&session.id), 0);
    assert_eq!(
        store().get(request_id).unwrap().unwrap().outcome.as_deref(),
        Some("needs-human")
    );
    let emitted = drain(&mut events);
    assert_eq!(
        emitted
            .iter()
            .filter(|event| matches!(event, crate::protocol::ServerEvent::RunNeedsHumanResume { .. }))
            .count(),
        1
    );
}

// ---------------------------------------------------------------------------
// (12) concurrent passes still create one turn
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_resume_passes_create_one_turn() {
    let _home = TestHome::new();
    let session = closed_session("sess_concurrent");
    let request_id = "req_concurrent";
    store().add(record(&session.id, request_id)).expect("add record");

    let runner = runner();
    runner
        .safety()
        .record_decision(request_id, true, "file", None)
        .expect("record approval");

    let provider: Arc<dyn Provider> = Arc::new(CountingProvider::new());
    let first = runner.resume_blocked_approvals(&provider);
    let second = runner.resume_blocked_approvals(&provider);
    tokio::join!(first, second);

    assert_eq!(
        resume_turns(&session.id),
        1,
        "two passes must not double-resume one request"
    );
    assert_eq!(resume_marker_events(&session.id, request_id), 1);
}

// ---------------------------------------------------------------------------
// (15) orphan reconciliation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_orphaned_request_gets_a_record_and_resumes() {
    let _home = TestHome::new();
    let session = closed_session("sess_orphan");
    let request_id = "req_orphan";

    // The request reached the queue and the record never got written.
    let safety = SafetySystem::new();
    safety.request_permission(PermissionRequest {
        id: request_id.to_string(),
        action: "bash".to_string(),
        description: "rm -rf build".to_string(),
        rationale: "cleanup".to_string(),
        urgency: Urgency::Normal,
        wait: true,
        created_at: Utc::now(),
        context: Some(serde_json::json!({"session_id": session.id})),
        run_id: None,
        expires_at: Some(Utc::now() + chrono::Duration::minutes(30)),
        default_on_timeout: crate::safety::ApprovalDefault::Deny,
    });
    assert!(store().get(request_id).unwrap().is_none());

    let report = reconcile_orphan_requests(&store(), &safety).expect("reconcile");
    assert_eq!(report.created, vec![request_id.to_string()]);
    let rebuilt = store().get(request_id).unwrap().expect("record rebuilt");
    assert_eq!(rebuilt.session_id, session.id);
    assert_eq!(rebuilt.waiter_pid, None, "nobody is waiting on a rebuilt record");

    safety
        .record_decision(request_id, true, "file", None)
        .expect("record approval");

    let runner = runner();
    let provider: Arc<dyn Provider> = Arc::new(CountingProvider::new());
    runner.resume_blocked_approvals(&provider).await;

    assert_eq!(
        resume_turns(&session.id),
        1,
        "a rebuilt record must make the parked run resumable again"
    );

    // Re-running the sweep never duplicates the record.
    let report = reconcile_orphan_requests(&store(), &safety).expect("reconcile again");
    assert!(report.is_empty());
}

#[test]
fn an_orphan_whose_session_is_gone_is_expired() {
    let _home = TestHome::new();
    let request_id = "req_orphan_dead";

    let safety = SafetySystem::new();
    safety.request_permission(PermissionRequest {
        id: request_id.to_string(),
        action: "bash".to_string(),
        description: "rm -rf build".to_string(),
        rationale: "cleanup".to_string(),
        urgency: Urgency::Normal,
        wait: true,
        created_at: Utc::now(),
        context: Some(serde_json::json!({"session_id": "sess_that_never_existed"})),
        run_id: None,
        expires_at: Some(Utc::now() + chrono::Duration::minutes(30)),
        default_on_timeout: crate::safety::ApprovalDefault::Deny,
    });

    let report = reconcile_orphan_requests(&store(), &safety).expect("reconcile");
    assert!(report.created.is_empty(), "no record may be built for a dead session");
    assert_eq!(report.expired, vec![request_id.to_string()]);
    assert!(store().get(request_id).unwrap().is_none());
    assert!(safety.pending_requests().is_empty(), "the queue entry must be closed");
}

// ---------------------------------------------------------------------------
// (9-i) a pending event is kept until a client acknowledges it
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_pending_event_is_kept_after_it_is_emitted() {
    let _home = TestHome::new();
    let session = closed_session("sess_pending_event");
    let request_id = "req_pending_event";
    store().add(record(&session.id, request_id)).expect("add record");

    let runner = runner();
    runner
        .safety()
        .record_decision(request_id, true, "file", None)
        .expect("record approval");

    let provider: Arc<dyn Provider> = Arc::new(CountingProvider::new());
    runner.resume_blocked_approvals(&provider).await;

    let stored = store().get(request_id).unwrap().expect("record present");
    assert!(
        stored.pending_event.is_some(),
        "emitting an event must not clear it; only a client ack may"
    );
    let event = stored.pending_event.unwrap();
    assert_eq!(event.get("resolved_by").and_then(|v| v.as_str()), Some("runner"));
}

#[cfg(unix)]
#[path = "runner_resume_crash_tests.rs"]
mod crash_tests;
