use super::*;
use crate::durable_runtime::{RunStarted, RunStopped, StopCause, read_journal_records};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;

struct Gate {
    started: Arc<tokio::sync::Semaphore>,
    release: Mutex<Option<Receiver<()>>>,
}

impl Gate {
    fn new() -> (Arc<Self>, std::sync::mpsc::Sender<()>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let gate = Arc::new(Self {
            started: Arc::new(tokio::sync::Semaphore::new(0)),
            release: Mutex::new(Some(rx)),
        });
        (gate, tx)
    }

    fn enter_and_wait(&self) {
        self.started.add_permits(1);
        let receiver = self.release.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(receiver) = receiver {
            let _ = receiver.recv();
        }
    }

    async fn wait_started(&self) {
        let _permit = self
            .started
            .acquire()
            .await
            .expect("the gate semaphore is never closed");
    }
}

struct CountingEffect {
    aborts: AtomicUsize,
    cancels: AtomicUsize,
    gate: Option<Arc<Gate>>,
}

impl Default for CountingEffect {
    fn default() -> Self {
        Self {
            aborts: AtomicUsize::new(0),
            cancels: AtomicUsize::new(0),
            gate: None,
        }
    }
}

impl CountingEffect {
    fn gated(gate: Arc<Gate>) -> Self {
        Self {
            aborts: AtomicUsize::new(0),
            cancels: AtomicUsize::new(0),
            gate: Some(gate),
        }
    }

    fn applications(&self) -> usize {
        self.aborts.load(Ordering::SeqCst) + self.cancels.load(Ordering::SeqCst)
    }
}

impl ControlEffect for CountingEffect {
    fn abort_run(&self, run_id: Option<&str>) -> Result<Value, String> {
        self.aborts.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            gate.enter_and_wait();
        }
        Ok(json!({ "aborted": true, "run_id": run_id }))
    }

    fn cancel_inputs(&self, input_ids: &[String]) -> Result<Value, String> {
        self.cancels.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            gate.enter_and_wait();
        }
        Ok(json!({ "cancelled": input_ids }))
    }
}

fn ledger(root: &std::path::Path, session: &str, terminal_cap: usize, nonterminal_cap: usize) -> ControlLedger {
    ControlLedger::new(Some(root.to_path_buf()), session, terminal_cap, nonterminal_cap)
}

fn cancel_action() -> ControlAction {
    ControlAction::cancel_inputs(&["input-1".to_string()])
}

fn journal_control_rows(root: &std::path::Path, session: &str) -> Vec<(String, String, Option<Value>)> {
    let mut store = DurableRuntimeStore::open_in(root, session).expect("open the control journal");
    let path = store.journal_path().to_path_buf();
    let records = read_journal_records(&path).expect("read the control journal");
    let _ = store.snapshot().expect("replay the control journal");
    records
        .iter()
        .filter_map(|record| match &record.payload {
            Payload::ControlApplying(applying) => Some((record.id.clone(), applying.digest.clone(), None)),
            Payload::ControlTerminal(terminal) => {
                Some((record.id.clone(), String::new(), Some(terminal.result.clone())))
            }
            _ => None,
        })
        .collect()
}

fn seed_run(root: &std::path::Path, session: &str, run_id: &str, stop: Option<StopCause>) {
    let mut store = DurableRuntimeStore::open_in(root, session).expect("open the control journal");
    store
        .append(run_id, Payload::RunStarted(RunStarted { input_id: None }))
        .expect("run started");
    if let Some(cause) = stop {
        store
            .append(run_id, Payload::RunStopped(RunStopped { cause, input_id: None }))
            .expect("run stopped");
    }
}

fn run_id(tag: &str) -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, format!("horon-run/{tag}").as_bytes()).to_string()
}

#[tokio::test]
async fn a_lost_response_is_replayed_instead_of_applied_twice() {
    let temp = tempfile::tempdir().expect("tempdir");
    let session = "control-session-lost-reply";
    let ledger = ledger(temp.path(), session, TERMINAL_CAP, NONTERMINAL_CAP);
    let effect = Arc::new(CountingEffect::default());

    let first = ledger.apply("op-1", &cancel_action(), effect.clone()).await;
    let result = first.result().cloned().expect("the first request applied");
    assert_eq!(first.label(), "applied");

    let second = ledger.apply("op-1", &cancel_action(), effect.clone()).await;
    assert_eq!(second.label(), "replayed");
    assert_eq!(second.result(), Some(&result));
    assert_eq!(effect.applications(), 1, "a retry must not apply the action again");
}

#[tokio::test]
async fn a_concurrent_duplicate_answers_pending_and_applies_once() {
    let temp = tempfile::tempdir().expect("tempdir");
    let session = "control-session-concurrent";
    let ledger = Arc::new(ledger(temp.path(), session, TERMINAL_CAP, NONTERMINAL_CAP));
    let (gate, release) = Gate::new();
    let effect = Arc::new(CountingEffect::gated(Arc::clone(&gate)));

    let running = {
        let ledger = Arc::clone(&ledger);
        let effect = effect.clone();
        tokio::spawn(async move { ledger.apply("op-dup", &cancel_action(), effect).await })
    };
    gate.wait_started().await;

    let duplicate = ledger.apply("op-dup", &cancel_action(), effect.clone()).await;
    assert_eq!(duplicate.label(), "pending");
    assert_eq!(effect.applications(), 1, "the duplicate must not become a second apply");

    release.send(()).expect("release the first apply");
    let first = running.await.expect("the first apply finished");
    assert_eq!(first.label(), "applied");
    assert_eq!(effect.applications(), 1);

    let after = ledger.apply("op-dup", &cancel_action(), effect.clone()).await;
    assert_eq!(after.label(), "replayed");
    assert_eq!(after.result(), first.result());
}

#[tokio::test]
async fn the_same_id_with_a_different_action_conflicts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let session = "control-session-conflict";
    let ledger = ledger(temp.path(), session, TERMINAL_CAP, NONTERMINAL_CAP);
    let effect = Arc::new(CountingEffect::default());

    let first = ledger.apply("op-shared", &cancel_action(), effect.clone()).await;
    assert_eq!(first.label(), "applied");

    let other = ControlAction::abort_run(run_id("conflict-run"));
    let conflicting = ledger.apply("op-shared", &other, effect.clone()).await;
    assert_eq!(conflicting.label(), "conflict");
    assert_eq!(effect.applications(), 1, "a conflicting payload must never apply");

    let replay = ledger.apply("op-shared", &cancel_action(), effect.clone()).await;
    assert_eq!(replay.label(), "replayed");
    assert_eq!(replay.result(), first.result());
}

#[tokio::test]
async fn an_applying_receipt_from_a_previous_process_is_indeterminate() {
    let temp = tempfile::tempdir().expect("tempdir");
    let session = "control-session-restart";
    let action = cancel_action();
    let digest = action.digest().expect("digest");
    let id = record_id(session, "op-crashed");
    {
        let mut store = DurableRuntimeStore::open_in(temp.path(), session).expect("open the control journal");
        store
            .append(
                &id,
                Payload::ControlApplying(ControlApplying {
                    action: action.label().to_string(),
                    digest,
                }),
            )
            .expect("the previous process claimed the operation");
    }

    let effect = Arc::new(CountingEffect::default());
    let restarted = ledger(temp.path(), session, TERMINAL_CAP, NONTERMINAL_CAP);
    let outcome = restarted.apply("op-crashed", &action, effect.clone()).await;
    assert_eq!(outcome.label(), "indeterminate");
    assert_eq!(effect.applications(), 0, "an interrupted claim is never re-applied");

    let again = ledger(temp.path(), session, TERMINAL_CAP, NONTERMINAL_CAP)
        .apply("op-crashed", &action, effect.clone())
        .await;
    assert_eq!(again.label(), "indeterminate");
    assert_eq!(effect.applications(), 0);
}

#[tokio::test]
async fn an_abort_naming_a_replaced_run_does_not_cancel() {
    let temp = tempfile::tempdir().expect("tempdir");
    let session = "control-session-stale";
    let old_run = run_id("stale-old");
    let new_run = run_id("stale-new");
    seed_run(temp.path(), session, &old_run, Some(StopCause::Completed));
    seed_run(temp.path(), session, &new_run, None);

    let ledger = ledger(temp.path(), session, TERMINAL_CAP, NONTERMINAL_CAP);
    let effect = Arc::new(CountingEffect::default());
    let outcome = ledger
        .apply("op-stale", &ControlAction::abort_run(old_run.clone()), effect.clone())
        .await;
    match &outcome {
        ControlOutcome::StaleRun {
            expected_run_id,
            current_run_id,
        } => {
            assert_eq!(expected_run_id, &old_run);
            assert_eq!(current_run_id.as_deref(), Some(new_run.as_str()));
        }
        other => panic!("expected stale_run, got {other:?}"),
    }
    assert_eq!(effect.applications(), 0, "the new run must not be cancelled");

    let replay = ledger
        .apply("op-stale", &ControlAction::abort_run(old_run), effect.clone())
        .await;
    assert_eq!(replay.label(), "replayed", "the stale answer is a receipt too");
    assert_eq!(
        replay.result().and_then(|result| result["status"].as_str()),
        Some("stale_run"),
        "the retry must return the stored stale_run answer"
    );
    assert_eq!(effect.applications(), 0);
}

#[tokio::test]
async fn a_stale_abort_of_an_already_stopped_run_is_stale_with_no_live_run() {
    let temp = tempfile::tempdir().expect("tempdir");
    let session = "control-session-stopped";
    let old_run = run_id("stopped-old");
    seed_run(temp.path(), session, &old_run, Some(StopCause::UserCancelled));

    let ledger = ledger(temp.path(), session, TERMINAL_CAP, NONTERMINAL_CAP);
    let effect = Arc::new(CountingEffect::default());
    let outcome = ledger
        .apply("op-stopped", &ControlAction::abort_run(old_run), effect.clone())
        .await;
    assert_eq!(outcome.label(), "stale_run");
    assert_eq!(effect.applications(), 0);
}

#[tokio::test]
async fn retention_keeps_the_newest_terminal_receipts_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let session = "control-session-retention";
    let ledger = ledger(temp.path(), session, 2, NONTERMINAL_CAP);
    let effect = Arc::new(CountingEffect::default());

    for operation in ["op-a", "op-b", "op-c"] {
        let outcome = ledger.apply(operation, &cancel_action(), effect.clone()).await;
        assert_eq!(outcome.label(), "applied", "{operation} must apply once");
    }
    assert_eq!(effect.applications(), 3);

    let evicted = ledger.apply("op-a", &cancel_action(), effect.clone()).await;
    assert_eq!(evicted.label(), "unknown");
    assert_eq!(effect.applications(), 3, "an evicted id must not be applied again");

    let retained = ledger.apply("op-c", &cancel_action(), effect.clone()).await;
    assert_eq!(retained.label(), "replayed");
    assert_eq!(effect.applications(), 3);
}

#[tokio::test]
async fn the_nonterminal_cap_answers_busy() {
    let temp = tempfile::tempdir().expect("tempdir");
    let session = "control-session-busy";
    let ledger = Arc::new(ledger(temp.path(), session, TERMINAL_CAP, 1));
    let (gate, release) = Gate::new();
    let effect = Arc::new(CountingEffect::gated(Arc::clone(&gate)));

    let running = {
        let ledger = Arc::clone(&ledger);
        let effect = effect.clone();
        tokio::spawn(async move { ledger.apply("op-busy-1", &cancel_action(), effect).await })
    };
    gate.wait_started().await;

    let refused = ledger.apply("op-busy-2", &cancel_action(), effect.clone()).await;
    assert_eq!(refused.label(), "busy");
    assert_eq!(effect.applications(), 1);

    release.send(()).expect("release the in-flight apply");
    let first = running.await.expect("the first apply finished");
    assert_eq!(first.label(), "applied");
}

#[tokio::test]
async fn operation_ids_are_isolated_per_session() {
    let temp = tempfile::tempdir().expect("tempdir");
    let left = ledger(temp.path(), "control-session-left", TERMINAL_CAP, NONTERMINAL_CAP);
    let right = ledger(temp.path(), "control-session-right", TERMINAL_CAP, NONTERMINAL_CAP);
    let effect = Arc::new(CountingEffect::default());

    let left_first = left.apply("op-shared", &cancel_action(), effect.clone()).await;
    let right_first = right.apply("op-shared", &cancel_action(), effect.clone()).await;
    assert_eq!(left_first.label(), "applied");
    assert_eq!(
        right_first.label(),
        "applied",
        "the same id in another session is a different operation"
    );
    assert_eq!(effect.applications(), 2);

    let left_replay = left.apply("op-shared", &cancel_action(), effect.clone()).await;
    assert_eq!(left_replay.label(), "replayed");
    assert_eq!(left_replay.result(), left_first.result());
}

#[tokio::test]
async fn a_malformed_operation_id_is_refused_before_anything_is_recorded() {
    let temp = tempfile::tempdir().expect("tempdir");
    let session = "control-session-invalid";
    let ledger = ledger(temp.path(), session, TERMINAL_CAP, NONTERMINAL_CAP);
    let effect = Arc::new(CountingEffect::default());

    for operation in ["", "op with space", "op/with/slash"] {
        let outcome = ledger.apply(operation, &cancel_action(), effect.clone()).await;
        assert_eq!(outcome.label(), "invalid", "{operation:?} must be refused");
    }
    assert_eq!(effect.applications(), 0);
    assert!(
        journal_control_rows(temp.path(), session).is_empty(),
        "a refused request records nothing"
    );
}

#[tokio::test]
async fn a_terminal_write_failure_after_the_action_is_indeterminate_and_never_reapplied() {
    let temp = tempfile::tempdir().expect("tempdir");
    let session = "control-session-terminal-failure";
    let ledger = ledger(temp.path(), session, TERMINAL_CAP, NONTERMINAL_CAP);
    let effect = Arc::new(CountingEffect::default());
    ledger.fail_terminal_writes();

    let outcome = ledger
        .apply("op-terminal-failure", &cancel_action(), effect.clone())
        .await;
    assert_eq!(outcome.label(), "indeterminate");
    assert_eq!(effect.applications(), 1, "the action itself did run");

    let retry = ledger
        .apply("op-terminal-failure", &cancel_action(), effect.clone())
        .await;
    assert_eq!(retry.label(), "indeterminate");
    assert_eq!(
        effect.applications(),
        1,
        "a recorded-but-unfinished claim is never re-applied"
    );

    let rows = journal_control_rows(temp.path(), session);
    assert_eq!(rows.len(), 1, "only the claim row landed");
    assert!(rows[0].2.is_none());
}

#[tokio::test]
async fn manual_qa_a_dropped_response_returns_the_stored_result_on_retry() {
    let temp = tempfile::tempdir().expect("tempdir");
    let session = "control-session-qa-lost";
    let action = cancel_action();
    let effect = Arc::new(CountingEffect::default());

    let applied = ledger(temp.path(), session, TERMINAL_CAP, NONTERMINAL_CAP)
        .apply("op-qa-lost", &action, effect.clone())
        .await;
    let stored = applied.result().cloned().expect("the request applied");
    assert_eq!(effect.applications(), 1);
    drop(applied);

    let retried = ledger(temp.path(), session, TERMINAL_CAP, NONTERMINAL_CAP)
        .apply("op-qa-lost", &action, effect.clone())
        .await;
    assert_eq!(retried.label(), "replayed");
    assert_eq!(retried.result(), Some(&stored));
    assert_eq!(effect.applications(), 1, "exactly one apply");

    let rows = journal_control_rows(temp.path(), session);
    let id = record_id(session, "op-qa-lost");
    let applying = rows
        .iter()
        .filter(|(row_id, _, result)| row_id == &id && result.is_none())
        .count();
    let terminal = rows
        .iter()
        .filter(|(row_id, _, result)| row_id == &id && result.is_some())
        .count();
    assert_eq!(applying, 1, "one claim row");
    assert_eq!(terminal, 1, "one terminal row");
}

#[tokio::test]
async fn manual_qa_a_crash_between_apply_and_terminal_never_reapplies() {
    let temp = tempfile::tempdir().expect("tempdir");
    let session = "control-session-qa-crash";
    let action = cancel_action();
    let id = record_id(session, "op-qa-crash");
    let digest = action.digest().expect("digest");
    {
        let mut store = DurableRuntimeStore::open_in(temp.path(), session).expect("open the control journal");
        store
            .append(
                &id,
                Payload::ControlApplying(ControlApplying {
                    action: action.label().to_string(),
                    digest,
                }),
            )
            .expect("the crashed process claimed the operation");
        store.replay().expect("reopen the journal");
    }

    let effect = Arc::new(CountingEffect::default());
    let restarted = ledger(temp.path(), session, TERMINAL_CAP, NONTERMINAL_CAP);
    let outcome = restarted.apply("op-qa-crash", &action, effect.clone()).await;
    assert_eq!(outcome.label(), "indeterminate");
    assert_eq!(effect.applications(), 0, "the sealed operation is never re-applied");

    let rows = journal_control_rows(temp.path(), session);
    assert_eq!(
        rows.len(),
        1,
        "the crash left exactly one claim row and no terminal row"
    );
    assert!(rows[0].2.is_none());
}
