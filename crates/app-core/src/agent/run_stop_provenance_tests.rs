//! Run identity and stop provenance: one run id per externally triggered run,
//! one observed cause per stop, and ordering that a late stop cannot break.

use super::*;

use horon_agent_agent_runtime::{
    RunIdentity, RunLedger, RunStop, StopDisposition, StopProvenance, begin_run, conversation_saved, current_run,
    current_stop, note_conversation_save, observed_cause, provider_failure_provenance, record_stop,
};
use horon_agent_base::durable_runtime::{
    DurableRuntimeStore, JournalErrorKind, JournalLimits, JournalOptions, Payload, RecordKind, RunStarted,
    StopCause as JournalStopCause, hook,
};

use crate::provider::EventStream;
use crate::tool::{Registry, Tool};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct ProvenanceProvider {
    rounds: Arc<Mutex<VecDeque<Vec<StreamEvent>>>>,
    failing: Arc<AtomicBool>,
}

impl ProvenanceProvider {
    fn scripted(rounds: Vec<Vec<StreamEvent>>) -> (Arc<dyn Provider>, Arc<AtomicBool>) {
        let failing = Arc::new(AtomicBool::new(false));
        let provider: Arc<dyn Provider> = Arc::new(Self {
            rounds: Arc::new(Mutex::new(rounds.into())),
            failing: Arc::clone(&failing),
        });
        (provider, failing)
    }
}

#[async_trait::async_trait]
impl Provider for ProvenanceProvider {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
        if self.failing.load(Ordering::SeqCst) {
            return Err(anyhow::anyhow!("provenance provider failure"));
        }
        let events = self
            .rounds
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![StreamEvent::MessageEnd { stop_reason: None }]);
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }

    fn name(&self) -> &str {
        "provenance-probe"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn completed_round(text: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::TextDelta(text.into()),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".into()),
        },
    ]
}

fn truncated_round() -> Vec<StreamEvent> {
    vec![
        StreamEvent::TextDelta("partial".into()),
        StreamEvent::MessageEnd {
            stop_reason: Some("max_tokens".into()),
        },
    ]
}

#[test]
fn run_stop_provenance_truncation_spends_the_continuation_budget() {
    let mut state = TurnState::new();
    state
        .finish(outcome_for_stop_reason(Some("max_tokens")))
        .expect("Length is recorded");
    assert_eq!(state.outcome(), Some(TurnOutcome::Length));
    assert_eq!(
        truncated_round().len(),
        2,
        "the truncation round carries partial text and its stop reason"
    );
}

fn probe_agent(provider: Arc<dyn Provider>) -> Agent {
    let mut agent = Agent::new(provider, Registry::empty());
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "go".into(),
            cache_control: None,
        }],
    );
    agent
}

fn loop_guard_round(prefix: &str, count: usize, input: &serde_json::Value) -> Vec<StreamEvent> {
    let mut events = Vec::new();
    for index in 0..count {
        let id = format!("{prefix}-{index}");
        events.push(StreamEvent::ToolUseStart {
            id: id.clone(),
            name: "read".into(),
        });
        events.push(StreamEvent::ToolInputDeltaFor {
            id: id.clone(),
            delta: input.to_string(),
        });
        events.push(StreamEvent::ToolUseEndFor { id });
    }
    events.push(StreamEvent::MessageEnd {
        stop_reason: Some("tool_use".into()),
    });
    events
}

struct EchoReadTool;

#[async_trait::async_trait]
impl Tool for EchoReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "run provenance test double for read"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    fn concurrency_class(&self) -> crate::tool::ConcurrencyClass {
        crate::tool::ConcurrencyClass::Concurrent
    }

    fn effects(&self) -> crate::tool::ToolEffects {
        crate::tool::ToolEffects::read()
    }

    async fn execute(&self, input: serde_json::Value, _: ToolContext) -> Result<crate::tool::ToolOutput> {
        Ok(crate::tool::ToolOutput::new(format!(
            "read:{}",
            input["file_path"].as_str().unwrap_or_default()
        )))
    }
}

async fn echo_registry() -> Registry {
    let registry = Registry::empty();
    registry
        .register("read".to_string(), Arc::new(EchoReadTool) as Arc<dyn Tool>)
        .await;
    registry
}

/// The journal root this test's agent home resolves to.
fn journal_root() -> std::path::PathBuf {
    horon_agent_base::storage::horon_agent_dir()
        .expect("the sandbox home resolves")
        .join(horon_agent_base::durable_runtime::JOURNAL_DIR_NAME)
}

/// Rows one session's journal holds, in order.
fn journal_rows(session_id: &str) -> Vec<(RecordKind, String)> {
    let path = journal_root()
        .join(session_id)
        .join(horon_agent_base::durable_runtime::JOURNAL_FILE_NAME);
    horon_agent_base::durable_runtime::read_journal_records(&path)
        .expect("the journal is readable")
        .into_iter()
        .map(|record| (record.kind, record.id))
        .collect()
}

/// The stop cause the journal recorded for `run_id`, if any.
fn journal_stop_cause(session_id: &str, run_id: &str) -> Option<JournalStopCause> {
    let path = journal_root()
        .join(session_id)
        .join(horon_agent_base::durable_runtime::JOURNAL_FILE_NAME);
    horon_agent_base::durable_runtime::read_journal_records(&path)
        .expect("the journal is readable")
        .into_iter()
        .find_map(|record| match record.payload {
            Payload::RunStopped(stopped) if record.id == run_id => Some(stopped.cause),
            _ => None,
        })
}

/// The runs one session's journal started, in order.
fn journal_started_runs(session_id: &str) -> Vec<String> {
    journal_rows(session_id)
        .into_iter()
        .filter(|(kind, _)| *kind == RecordKind::RunStarted)
        .map(|(_, id)| id)
        .collect()
}

#[tokio::test]
async fn run_stop_provenance_completed_run_records_start_and_completed_stop() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (provider, _failing) = ProvenanceProvider::scripted(vec![completed_round("done")]);
    let mut agent = probe_agent(provider);
    let session_id = agent.session.id.clone();

    agent.run_turn(false).await.expect("the turn completes");

    let run = agent.current_run().expect("a run was started");
    assert_eq!(
        journal_started_runs(&session_id),
        vec![run.run_id().to_string()],
        "exactly one run.started row, named by the current run"
    );
    assert_eq!(
        journal_stop_cause(&session_id, run.run_id()),
        Some(JournalStopCause::Completed),
        "the journal records the observed cause"
    );
    let stop = agent.current_stop().expect("the current run has a stop");
    assert_eq!(stop.run_id(), run.run_id());
    assert_eq!(stop.provenance(), StopProvenance::Completed);
    assert!(stop.persisted(), "the stop row reached the journal");
    assert!(agent.stop_history().is_empty(), "nothing arrived late");
    assert_eq!(conversation_saved(&session_id), Some(true));
    assert_eq!(
        agent.turn_state().outcome(),
        Some(TurnOutcome::Stop),
        "TurnOutcome behavior is unchanged"
    );
}

#[tokio::test]
async fn run_stop_provenance_user_cancel_is_recorded() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (provider, _failing) = ProvenanceProvider::scripted(vec![completed_round("done")]);
    let mut agent = probe_agent(provider);
    let session_id = agent.session.id.clone();
    agent.request_graceful_shutdown();

    agent.run_turn(false).await.expect("a cancelled turn returns");

    let run = agent.current_run().expect("a run was started");
    let stop = agent.current_stop().expect("a cancelled run has a stop");
    assert_eq!(stop.provenance(), StopProvenance::UserCancelled);
    assert_eq!(
        journal_stop_cause(&session_id, run.run_id()),
        Some(JournalStopCause::UserCancelled)
    );
    assert_eq!(agent.turn_state().outcome(), Some(TurnOutcome::Aborted));
}

#[tokio::test]
async fn run_stop_provenance_provider_error_is_recorded() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (provider, failing) = ProvenanceProvider::scripted(vec![completed_round("done")]);
    let mut agent = probe_agent(provider);
    let session_id = agent.session.id.clone();
    failing.store(true, Ordering::SeqCst);

    let error = agent.run_turn(false).await.expect_err("the provider fails");
    assert!(error.to_string().contains("provenance provider failure"), "{error}");

    let run = agent.current_run().expect("a run was started");
    let stop = agent.current_stop().expect("a failed run has a stop");
    assert_eq!(stop.provenance(), StopProvenance::ProviderError);
    assert_eq!(
        journal_stop_cause(&session_id, run.run_id()),
        Some(JournalStopCause::ProviderError)
    );
    assert_eq!(agent.turn_state().outcome(), Some(TurnOutcome::Error));
}

#[tokio::test]
async fn run_stop_provenance_timeout_is_read_from_the_error() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(TimeoutProbeProvider);
    let mut agent = probe_agent(provider);
    let session_id = agent.session.id.clone();

    let error = agent.run_turn(false).await.expect_err("the provider times out");
    assert!(error.to_string().contains("timed out"), "{error}");

    let run = agent.current_run().expect("a run was started");
    let stop = agent.current_stop().expect("a timed-out run has a stop");
    assert_eq!(stop.provenance(), StopProvenance::Timeout);
    assert_eq!(
        journal_stop_cause(&session_id, run.run_id()),
        Some(JournalStopCause::Timeout)
    );
}

struct TimeoutProbeProvider;

#[async_trait::async_trait]
impl Provider for TimeoutProbeProvider {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
        Err(anyhow::anyhow!(
            "WebSocket stream timed out waiting for first websocket activity (8s)"
        ))
    }

    fn name(&self) -> &str {
        "timeout-probe"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

#[tokio::test]
async fn run_stop_provenance_loop_guard_stop_is_recorded() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    assert!(
        crate::config::config().features.loop_guard,
        "the test needs the loop guard enabled"
    );
    let input = serde_json::json!({ "file_path": "src/lib.rs" });
    let (provider, _failing) = ProvenanceProvider::scripted(vec![
        loop_guard_round("first", 3, &input),
        loop_guard_round("second", 2, &input),
    ]);
    let mut agent = Agent::new(provider, echo_registry().await);
    let session_id = agent.session.id.clone();
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "read the same file five times".into(),
            cache_control: None,
        }],
    );

    agent.run_turn(false).await.expect("the guarded turn ends");

    let run = agent.current_run().expect("a run was started");
    let stop = agent.current_stop().expect("a guarded run has a stop");
    assert_eq!(
        stop.provenance(),
        StopProvenance::LoopGuard,
        "the loop guard stop is not a plain completion"
    );
    assert_eq!(
        journal_stop_cause(&session_id, run.run_id()),
        Some(JournalStopCause::LoopGuard)
    );
    assert_eq!(
        agent.turn_state().outcome(),
        Some(TurnOutcome::Stop),
        "the outcome still reads Stop, exactly as before this change"
    );
}

#[tokio::test]
async fn run_stop_provenance_late_stop_from_an_older_run_goes_to_history() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (provider, _failing) = ProvenanceProvider::scripted(vec![completed_round("first"), completed_round("second")]);
    let mut agent = probe_agent(provider);
    let session_id = agent.session.id.clone();

    agent.run_turn(false).await.expect("run A completes");
    let run_a = agent.current_run().expect("run A was started").run_id().to_string();
    assert_eq!(
        agent.current_stop().map(|stop| stop.provenance()),
        Some(StopProvenance::Completed)
    );

    agent.run_turn(false).await.expect("run B completes");
    let run_b = agent.current_run().expect("run B was started").run_id().to_string();
    assert_ne!(run_a, run_b, "a new external input issues a new run id");

    let disposition = agent.finish_run(&run_a, StopProvenance::ProviderError).await;
    assert_eq!(
        disposition,
        StopDisposition::History,
        "a late stop for run A must not become the current stop"
    );

    let current = agent.current_run().expect("run B is still current");
    assert_eq!(current.run_id(), run_b, "the current state still names run B");
    let current_stop = agent.current_stop().expect("run B's stop survives");
    assert_eq!(current_stop.run_id(), run_b);
    assert_eq!(
        current_stop.provenance(),
        StopProvenance::Completed,
        "run B keeps its own cause; A's late stop did not overwrite it"
    );

    let history = agent.stop_history();
    assert!(
        history.iter().any(|stop| stop.run_id() == run_a),
        "A's stop appears in history: {history:?}"
    );
    let recorded_a = history
        .iter()
        .find(|stop| stop.run_id() == run_a)
        .expect("run A is in history");
    assert_eq!(
        recorded_a.provenance(),
        StopProvenance::ProviderError,
        "the history entry keeps the cause it was given"
    );
    assert!(
        history.iter().all(|stop| stop.run_id() != run_b),
        "the current run's stop is not duplicated into history: {history:?}"
    );
    assert_eq!(session_id, agent.session.id);
}

#[tokio::test]
async fn run_stop_provenance_failed_stop_write_leaves_persisted_false() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (provider, _failing) = ProvenanceProvider::scripted(vec![completed_round("done")]);
    let mut agent = probe_agent(provider);
    let session_id = agent.session.id.clone();

    // Every write to the journal fails, so the stop row cannot land while the
    // run must still be visible with its cause and flagged as not persisted.
    let run = RunIdentity::new(Agent::mint_run_id());
    let failing_root = journal_root();
    let failing_session = session_id.clone();
    let mut store = DurableRuntimeStore::open_in_with(
        &failing_root,
        &failing_session,
        JournalOptions {
            limits: JournalLimits::default(),
            hooks: Some(hook(|point| match point {
                horon_agent_base::durable_runtime::JournalIoPoint::RecordFileSync => {
                    Err(std::io::Error::other("injected fsync failure"))
                }
                _ => Ok(()),
            })),
        },
    )
    .expect("the store opens");
    let error = store
        .append(run.run_id(), Payload::RunStarted(RunStarted::new(None)))
        .expect_err("the injected failure refuses the write");
    assert_eq!(error.kind(), JournalErrorKind::Io);
    drop(store);

    begin_run(&session_id, run.clone(), false);
    agent.note_turn_conversation_save(true);
    let disposition = agent.finish_run(run.run_id(), StopProvenance::Completed).await;

    assert_eq!(disposition, StopDisposition::Current);
    let stop = agent.current_stop().expect("the in-memory stop is kept");
    assert_eq!(stop.run_id(), run.run_id());
    assert_eq!(stop.provenance(), StopProvenance::Completed);
    assert!(
        !stop.persisted(),
        "a stop whose journal write failed is not reported as persisted"
    );
}

#[tokio::test]
async fn run_stop_provenance_recovery_of_started_only_is_interrupted_unknown() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let session_id = "run_stop_provenance_crash_recovery";
    let root = journal_root();
    let run_id = Agent::mint_run_id();

    // A crash leaves only the start row: nothing about the cause was observed.
    {
        let mut store = DurableRuntimeStore::open_in(&root, session_id).expect("the store opens");
        store
            .append(&run_id, Payload::RunStarted(RunStarted::new(None)))
            .expect("run.started is written");
        assert!(store.state().live_runs().count() == 1, "the run is live with no stop");
    }

    let mut reopened = DurableRuntimeStore::open_in(&root, session_id).expect("the store reopens");
    let state = reopened.snapshot().expect("the journal replays");
    let live: Vec<&String> = state.live_runs().map(|(id, _)| id).collect();
    assert_eq!(live, vec![&run_id], "the reopened journal still has the run open");
    assert!(
        state.last_stop().is_none(),
        "a started-only journal has no stop to report"
    );

    let recovered = RunStop::recovered_interrupted(&run_id);
    assert_eq!(recovered.provenance(), StopProvenance::InterruptedUnknown);
    assert!(!recovered.persisted(), "nothing was written for a recovered stop");

    begin_run(session_id, RunIdentity::new(run_id.clone()), true);
    assert_eq!(
        record_stop(session_id, recovered),
        StopDisposition::Current,
        "the recovered stop describes the run that is current"
    );
    let stop = current_stop(session_id).expect("the recovered stop is current");
    assert_eq!(stop.provenance(), StopProvenance::InterruptedUnknown);
    assert_ne!(stop.provenance(), StopProvenance::Completed);
}

#[tokio::test]
async fn run_stop_provenance_streaming_turn_records_its_own_run() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (provider, _failing) =
        ProvenanceProvider::scripted(vec![completed_round("streamed"), completed_round("again")]);
    let mut agent = probe_agent(provider);
    let session_id = agent.session.id.clone();

    let (tx, _rx) = mpsc::unbounded_channel();
    agent
        .run_once_streaming_mpsc("first", Vec::new(), None, tx)
        .await
        .expect("the streaming turn completes");
    let run_a = agent.current_run().expect("the streaming turn started a run");
    assert_eq!(
        journal_started_runs(&session_id),
        vec![run_a.run_id().to_string()],
        "the streaming entry point records run.started"
    );
    assert_eq!(
        journal_stop_cause(&session_id, run_a.run_id()),
        Some(JournalStopCause::Completed),
        "the streaming entry point records the stop too"
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    agent
        .run_once_streaming_mpsc("second", Vec::new(), None, tx)
        .await
        .expect("the second streaming turn completes");
    let run_b = agent.current_run().expect("the second turn started a run");
    assert_ne!(
        run_a.run_id(),
        run_b.run_id(),
        "a second external input begins a new run"
    );
    assert_eq!(
        journal_started_runs(&session_id),
        vec![run_a.run_id().to_string(), run_b.run_id().to_string()],
        "the journal holds one start row per run, in order"
    );
    assert_eq!(
        agent.current_stop().map(|stop| stop.run_id().to_string()),
        Some(run_b.run_id().to_string()),
        "the current stop belongs to the newest run"
    );
}

#[tokio::test]
async fn run_stop_provenance_streaming_turn_cancel_is_recorded() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (provider, _failing) = ProvenanceProvider::scripted(vec![completed_round("streamed")]);
    let mut agent = probe_agent(provider);
    let session_id = agent.session.id.clone();
    agent.request_graceful_shutdown();
    let (tx, _rx) = mpsc::unbounded_channel();

    agent
        .run_once_streaming_mpsc("cancel me", Vec::new(), None, tx)
        .await
        .expect("a cancelled streaming turn returns");

    let run = agent.current_run().expect("the streaming turn started a run");
    assert_eq!(
        journal_stop_cause(&session_id, run.run_id()),
        Some(JournalStopCause::UserCancelled)
    );
}

#[tokio::test]
async fn run_stop_provenance_streaming_turn_provider_error_is_recorded() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (provider, failing) = ProvenanceProvider::scripted(vec![completed_round("streamed")]);
    let mut agent = probe_agent(provider);
    let session_id = agent.session.id.clone();
    failing.store(true, Ordering::SeqCst);
    let (tx, _rx) = mpsc::unbounded_channel();

    let error = agent
        .run_once_streaming_mpsc("fail me", Vec::new(), None, tx)
        .await
        .expect_err("the provider fails");
    assert!(error.to_string().contains("provenance provider failure"), "{error}");

    let run = agent.current_run().expect("the streaming turn started a run");
    assert_eq!(
        journal_started_runs(&session_id),
        vec![run.run_id().to_string()],
        "a failed streaming turn still records exactly one start row"
    );
    assert_eq!(
        journal_stop_cause(&session_id, run.run_id()),
        Some(JournalStopCause::ProviderError),
        "the failure's own cause reaches the journal"
    );
    assert_eq!(
        agent.turn_state().outcome(),
        Some(TurnOutcome::Error),
        "TurnOutcome behavior is unchanged by the recording"
    );
}

#[tokio::test]
async fn run_stop_provenance_streaming_turn_journal_failure_keeps_the_turn_result() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (provider, _failing) = ProvenanceProvider::scripted(vec![completed_round("streamed")]);
    let mut agent = probe_agent(provider);
    let session_id = agent.session.id.clone();

    // A regular file where this session's journal directory belongs makes every
    // write for it fail, which is the one journal failure a test can inject
    // without touching the store's own hooks.
    let blocked = journal_root().join(&session_id);
    std::fs::create_dir_all(blocked.parent().expect("the journal root has a parent"))
        .expect("the journal root is created");
    std::fs::write(&blocked, b"not a journal directory").expect("the blocker file is written");

    let (tx, _rx) = mpsc::unbounded_channel();
    let result = agent.run_once_streaming_mpsc("keep going", Vec::new(), None, tx).await;
    assert!(
        result.is_ok(),
        "a journal write failure must not change the turn result: {result:?}"
    );

    let run = agent.current_run().expect("the run is known even when unrecorded");
    let stop = agent.current_stop().expect("the stop is kept in memory");
    assert_eq!(stop.run_id(), run.run_id());
    assert_eq!(stop.provenance(), StopProvenance::Completed);
    assert!(
        !stop.persisted(),
        "a stop the journal refused is not reported as persisted"
    );
    assert!(
        blocked.is_file(),
        "the blocked path is still the file it was: no journal row was written"
    );
    assert_eq!(agent.turn_state().outcome(), Some(TurnOutcome::Stop));

    std::fs::remove_file(&blocked).expect("the blocker is removed");
}

#[test]
fn run_stop_provenance_cause_vocabulary_is_exact() {
    let labels: Vec<&str> = StopProvenance::ALL.iter().map(|cause| cause.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            "completed",
            "user_cancelled",
            "shutdown",
            "provider_error",
            "timeout",
            "loop_guard",
            "limit",
            "interrupted_unknown",
        ]
    );
    for cause in StopProvenance::ALL {
        assert_eq!(StopProvenance::parse(cause.as_str()), Some(cause));
    }
    assert_eq!(StopProvenance::parse("finished"), None);
}

#[test]
fn run_stop_provenance_ledger_clears_the_current_stop_on_a_new_run() {
    let mut ledger = RunLedger::default();
    ledger.begin_run(RunIdentity::new("run-a"), true);
    assert_eq!(
        ledger.record_stop(RunStop::capture("run-a", StopProvenance::Completed)),
        StopDisposition::Current
    );

    ledger.begin_run(RunIdentity::new("run-b"), true);
    assert!(
        ledger.current_stop().is_none(),
        "a new run starts with no stop of its own"
    );
    assert_eq!(
        ledger.history().len(),
        1,
        "the previous stop moved to history rather than being lost"
    );
    assert_eq!(ledger.history()[0].run_id(), "run-a");
}

#[test]
fn run_stop_provenance_observed_cause_wins_over_the_outcome() {
    let mut ledger = RunLedger::default();
    ledger.begin_run(RunIdentity::new("run-a"), true);
    ledger.observe_cause(StopProvenance::LoopGuard);
    ledger.observe_cause(StopProvenance::ProviderError);
    assert_eq!(
        ledger.observed_cause(),
        Some(StopProvenance::LoopGuard),
        "the first terminal observation is the cause"
    );
}

#[test]
fn run_stop_provenance_timeout_is_only_read_from_the_error_text() {
    assert_eq!(
        provider_failure_provenance("WebSocket stream timed out waiting for activity"),
        StopProvenance::Timeout
    );
    assert_eq!(
        provider_failure_provenance("connection reset by peer"),
        StopProvenance::ProviderError
    );
    assert_eq!(
        cause_for_turn_failure("Context limit exceeded after 5 compaction retries"),
        StopProvenance::Limit
    );
    assert_eq!(cause_for_turn_failure("rate limited"), StopProvenance::ProviderError);
}

#[test]
fn run_stop_provenance_conversation_save_is_recorded_with_the_stop() {
    let session_id = "run_stop_provenance_conversation_save";
    begin_run(session_id, RunIdentity::new("run-a"), true);
    assert_eq!(conversation_saved(session_id), None);
    note_conversation_save(session_id, false);
    assert_eq!(conversation_saved(session_id), Some(false));
    assert_eq!(
        current_run(session_id).map(|run| run.run_id().to_string()),
        Some("run-a".to_string())
    );
    assert_eq!(observed_cause(session_id), None);
}
