use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

use super::*;

const CHILD_TEST: &str = "durable_runtime::tests::child_writer_process";
const CHILD_DEADLINE: Duration = Duration::from_secs(120);

fn temp_root() -> TempDir {
    TempDir::new().expect("temp dir")
}

fn open(root: &Path, session: &str) -> DurableRuntimeStore {
    DurableRuntimeStore::open_in(root, session).expect("open journal")
}

fn open_with(root: &Path, session: &str, limits: JournalLimits, hooks: Option<JournalHook>) -> DurableRuntimeStore {
    DurableRuntimeStore::open_in_with(root, session, JournalOptions { limits, hooks })
        .expect("open journal with options")
}

fn journal_path(root: &Path, session: &str) -> PathBuf {
    root.join(session).join(JOURNAL_FILE_NAME)
}

fn accepted(content: &str) -> Payload {
    Payload::InputAccepted(InputAccepted {
        client_id: Some("client-1".to_string()),
        content: content.to_string(),
        images: Vec::new(),
        urgent: false,
        source: "user".to_string(),
    })
}

fn started() -> Payload {
    Payload::InputStarted(InputStarted { run_id: None })
}

fn committed() -> Payload {
    Payload::InputCommitted(InputCommitted { run_id: None })
}

fn run_started() -> Payload {
    Payload::RunStarted(RunStarted::new(None))
}

fn cut_final_row(path: &Path, cut_from_end: usize) -> Vec<u8> {
    let bytes = std::fs::read(path).expect("read journal");
    let cut = bytes
        .len()
        .checked_sub(cut_from_end)
        .expect("journal is longer than the cut");
    let truncated = bytes[..cut].to_vec();
    let tail_start = truncated
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|position| position + 1)
        .unwrap_or(0);
    let torn = truncated[tail_start..].to_vec();
    std::fs::write(path, &truncated).expect("write the truncated journal");
    torn
}

fn rewrite_rows(path: &Path, edit: impl FnOnce(&mut Vec<Value>)) {
    let text = std::fs::read_to_string(path).expect("read journal text");
    let mut rows: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("row is json"))
        .collect();
    edit(&mut rows);
    let mut out = String::new();
    for row in rows {
        out.push_str(&serde_json::to_string(&row).expect("row serializes"));
        out.push('\n');
    }
    std::fs::write(path, out).expect("write rewritten journal");
}

fn recovery_files(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("list the journal directory")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(RECOVERY_PREFIX))
        })
        .collect();
    paths.sort();
    paths
}

#[test]
fn append_and_replay_round_trip() {
    let temp = temp_root();
    let session = "ses_round_trip";
    let mut store = open(temp.path(), session);

    let input = new_id();
    let run = new_id();
    let operation = new_id();
    let monitor = new_id();
    let wake = new_id();

    let first = store.append(&input, accepted("hello")).expect("accept");
    assert_eq!(first.seq, 2, "a new journal starts with its checkpoint at seq 1");
    assert_eq!(first.kind, RecordKind::InputAccepted);
    assert_eq!(
        store
            .append(
                &input,
                Payload::InputStarted(InputStarted {
                    run_id: Some(run.clone())
                })
            )
            .expect("start")
            .seq,
        3
    );
    store
        .append(&run, Payload::RunStarted(RunStarted::new(Some(input.clone()))))
        .expect("run started");
    store
        .append(
            &run,
            Payload::RunStopped(RunStopped::new(StopCause::Completed, Some(input.clone()))),
        )
        .expect("run stopped");
    store
        .append(
            &input,
            Payload::InputCommitted(InputCommitted {
                run_id: Some(run.clone()),
            }),
        )
        .expect("commit");
    store
        .append(
            &operation,
            Payload::ControlApplying(ControlApplying {
                action: "abort_run".to_string(),
                digest: "sha256:abc".to_string(),
            }),
        )
        .expect("control applying");
    store
        .append(
            &operation,
            Payload::ControlTerminal(ControlTerminal {
                result: json!({"status": "applied"}),
            }),
        )
        .expect("control terminal");
    store
        .append(
            &monitor,
            Payload::MonitorUpsert(MonitorUpsert {
                monitor_kind: "file".to_string(),
                spec: json!({"path": "/tmp/watched"}),
            }),
        )
        .expect("monitor upsert");
    store
        .append(
            &monitor,
            Payload::MonitorCheckpoint(MonitorCheckpoint {
                monitor_seq: 3,
                summary: "changed".to_string(),
                detail: Value::Null,
            }),
        )
        .expect("monitor checkpoint");
    store
        .append(
            &wake,
            Payload::WakePending(WakePending {
                monitor_id: monitor.clone(),
                monitor_seq: 3,
                summary: "changed".to_string(),
            }),
        )
        .expect("wake pending");
    store
        .append(
            &wake,
            Payload::WakeAcked(WakeAcked {
                delivered_input_id: Some(input.clone()),
            }),
        )
        .expect("wake acked");

    let last_seq = store.last_seq();
    let state = store.snapshot().expect("snapshot");
    assert!(state.inputs.is_empty());
    assert_eq!(state.undetermined_inputs().count(), 0);
    assert_eq!(state.input_receipts[&input].phase, InputReceiptPhase::Committed);
    assert_eq!(
        state.runs[&run].stop.as_ref().expect("stop row").cause,
        StopCause::Completed
    );
    assert_eq!(state.last_stop().expect("last stop").0, &run);
    assert_eq!(state.controls[&operation].phase, ControlPhase::Terminal);
    assert_eq!(
        state.monitors[&monitor]
            .last_checkpoint
            .as_ref()
            .expect("monitor checkpoint")
            .monitor_seq,
        3
    );
    assert!(state.wakes[&wake].acked);
    assert_eq!(state.pending_wakes().count(), 0);

    drop(store);
    let mut reopened = open(temp.path(), session);
    assert_eq!(reopened.last_seq(), last_seq);
    assert_eq!(reopened.snapshot().expect("replayed state"), state);
    let records = read_journal_records(reopened.journal_path()).expect("read rows");
    assert_eq!(records.len() as u64, last_seq, "one row per sequence number");
    assert_eq!(
        records.iter().map(|record| record.seq).collect::<Vec<_>>(),
        (1..=last_seq).collect::<Vec<_>>()
    );
    assert_eq!(records[0].kind, RecordKind::Checkpoint);
}

#[test]
fn supported_kinds_are_exactly_the_contract_set() {
    let names: Vec<&str> = RecordKind::ALL.iter().map(|kind| kind.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "checkpoint",
            "input.accepted",
            "input.started",
            "input.committed",
            "input.cancelled",
            "input.returned",
            "input.in_doubt",
            "run.started",
            "run.stopped",
            "control.applying",
            "control.terminal",
            "monitor.upsert",
            "monitor.checkpoint",
            "monitor.stopped",
            "wake.pending",
            "wake.acked",
        ]
    );
    for kind in RecordKind::ALL {
        assert_eq!(RecordKind::parse(kind.as_str()), Some(kind));
    }
    for kind in RecordKind::ALL {
        let admission = matches!(kind, RecordKind::InputAccepted | RecordKind::MonitorUpsert);
        assert_eq!(
            kind.is_admission(),
            admission,
            "{} must be classified as {}",
            kind.as_str(),
            if admission { "an admission" } else { "a lifecycle row" }
        );
    }
}

#[test]
fn limits_match_the_contract() {
    assert_eq!(MAX_JOURNAL_BYTES, 32 * 1024 * 1024);
    assert_eq!(LIFECYCLE_RESERVE_BYTES, 1024 * 1024);
    assert_eq!(COMPACT_AFTER_BYTES, 4 * 1024 * 1024);
    assert_eq!(COMPACT_AFTER_RECORDS, 4_096);
    assert_eq!(MAX_LIVE_INPUTS, 1_000);
    assert_eq!(MAX_LIVE_INPUT_BYTES, 16 * 1024 * 1024);
    assert_eq!(MAX_INPUT_PAYLOAD_BYTES, 1024 * 1024);
    assert_eq!(TERMINAL_RUN_CAP, 256);
    assert_eq!(TERMINAL_RECEIPT_CAP, 1_024);
    let defaults = JournalLimits::default();
    assert_eq!(defaults.max_journal_bytes, MAX_JOURNAL_BYTES);
    assert_eq!(defaults.reserve_bytes, LIFECYCLE_RESERVE_BYTES);
    assert_eq!(defaults.max_live_inputs, MAX_LIVE_INPUTS);
    assert_eq!(defaults.terminal_run_cap, TERMINAL_RUN_CAP);
    assert_eq!(defaults.terminal_receipt_cap, TERMINAL_RECEIPT_CAP);
    assert_eq!(
        RetentionCaps::default(),
        RetentionCaps::new(TERMINAL_RUN_CAP, TERMINAL_RECEIPT_CAP)
    );
}

#[test]
fn transitions_cover_every_kind_and_refuse_invalid_orders() {
    let temp = temp_root();
    let session = "ses_transitions";
    let mut store = open(temp.path(), session);

    let retried = new_id();
    store.append(&retried, accepted("retry me")).expect("accept");
    store.append(&retried, started()).expect("start");
    store
        .append(
            &retried,
            Payload::InputReturned(InputReturned {
                reason: "provider error".to_string(),
            }),
        )
        .expect("returned");
    assert_eq!(store.state().inputs[&retried].phase, InputPhase::Returned);
    store
        .append(
            &retried,
            Payload::InputCancelled(InputCancelled {
                reason: Some("user".to_string()),
            }),
        )
        .expect("cancel a returned input");
    assert_eq!(
        store.state().input_receipts[&retried].phase,
        InputReceiptPhase::Cancelled
    );

    let doubtful = new_id();
    store.append(&doubtful, accepted("uncertain")).expect("accept");
    store.append(&doubtful, started()).expect("start");
    store
        .append(
            &doubtful,
            Payload::InputInDoubt(InputInDoubt {
                cause: Some("shutdown mid-run".to_string()),
            }),
        )
        .expect("in doubt");
    assert_eq!(store.state().inputs[&doubtful].phase, InputPhase::InDoubt);
    assert_eq!(store.state().undetermined_inputs().count(), 1);

    let monitor = new_id();
    store
        .append(
            &monitor,
            Payload::MonitorUpsert(MonitorUpsert {
                monitor_kind: "command".to_string(),
                spec: json!({"command": "true"}),
            }),
        )
        .expect("monitor upsert");
    store
        .append(
            &monitor,
            Payload::MonitorCheckpoint(MonitorCheckpoint {
                monitor_seq: 1,
                summary: "ran".to_string(),
                detail: json!({"exit": 0}),
            }),
        )
        .expect("monitor checkpoint");
    store
        .append(
            &monitor,
            Payload::MonitorStopped(MonitorStopped {
                reason: "cancelled".to_string(),
            }),
        )
        .expect("monitor stopped");

    let finished = new_id();
    store.append(&finished, accepted("finish me")).expect("accept");
    store.append(&finished, started()).expect("start");
    store.append(&finished, committed()).expect("commit");

    let run = new_id();
    store.append(&run, run_started()).expect("run started");
    store
        .append(
            &run,
            Payload::RunStopped(RunStopped::new(StopCause::UserCancelled, None)),
        )
        .expect("run stopped");

    let operation = new_id();
    store
        .append(
            &operation,
            Payload::ControlApplying(ControlApplying {
                action: "cancel_inputs".to_string(),
                digest: "sha256:digest".to_string(),
            }),
        )
        .expect("control applying");
    store
        .append(
            &operation,
            Payload::ControlTerminal(ControlTerminal {
                result: json!({"status": "applied"}),
            }),
        )
        .expect("control terminal");

    let wake = new_id();
    store
        .append(
            &wake,
            Payload::WakePending(WakePending {
                monitor_id: monitor.clone(),
                monitor_seq: 1,
                summary: "ran".to_string(),
            }),
        )
        .expect("wake pending");
    store
        .append(
            &wake,
            Payload::WakeAcked(WakeAcked {
                delivered_input_id: Some(finished.clone()),
            }),
        )
        .expect("wake acked");

    fn assert_refused(store: &mut DurableRuntimeStore, label: &str, id: &str, payload: Payload) {
        let seq = store.last_seq();
        let len = store.journal_bytes();
        let error = store.append(id, payload).expect_err(label);
        assert_eq!(error.kind(), JournalErrorKind::InvalidRequest, "{label}");
        assert_eq!(store.last_seq(), seq, "{label} advanced the sequence");
        assert_eq!(store.journal_bytes(), len, "{label} wrote bytes");
    }

    assert_refused(&mut store, "started for an unknown input", &new_id(), started());
    let unstarted = new_id();
    store.append(&unstarted, accepted("never started")).expect("accept");
    assert_refused(&mut store, "commit before start", &unstarted, committed());
    assert_refused(&mut store, "in doubt cannot restart", &doubtful, started());
    assert_refused(
        &mut store,
        "stopped monitor cannot checkpoint",
        &monitor,
        Payload::MonitorCheckpoint(MonitorCheckpoint {
            monitor_seq: 2,
            summary: "after stop".to_string(),
            detail: Value::Null,
        }),
    );
    assert_refused(
        &mut store,
        "ack for an unknown wake",
        &new_id(),
        Payload::WakeAcked(WakeAcked {
            delivered_input_id: None,
        }),
    );
    assert_refused(
        &mut store,
        "checkpoint by a caller",
        &new_id(),
        Payload::Checkpoint(Checkpoint {
            generation: 99,
            start_seq: 99,
            last_seq: 98,
            state: RuntimeState::default(),
        }),
    );

    let kinds: Vec<RecordKind> = read_journal_records(store.journal_path())
        .expect("rows")
        .into_iter()
        .map(|record| record.kind)
        .collect();
    for kind in RecordKind::ALL {
        assert!(kinds.contains(&kind), "no row of kind {} was written", kind.as_str());
    }
}

#[test]
fn missing_empty_and_torn_journals_are_distinguishable() {
    let temp = temp_root();

    let fresh = open(temp.path(), "ses_missing");
    assert_eq!(fresh.generation(), 0);
    assert_eq!(fresh.last_seq(), 0);
    assert!(
        !fresh.journal_path().exists(),
        "opening a session must not create a journal"
    );

    let path = journal_path(temp.path(), "ses_missing");
    std::fs::create_dir_all(path.parent().expect("session dir")).expect("create session dir");
    std::fs::write(&path, b"").expect("write an empty journal");
    let mut empty = open(temp.path(), "ses_missing");
    assert_eq!(empty.generation(), 0);
    assert_eq!(empty.snapshot().expect("empty state").inputs.len(), 0);
    assert_eq!(empty.append(&new_id(), accepted("after empty")).expect("append").seq, 2);

    let session = "ses_torn";
    let mut store = open(temp.path(), session);
    let input = new_id();
    store.append(&input, accepted("kept")).expect("accept");
    store.append(&input, started()).expect("start");
    let path = journal_path(temp.path(), session);
    let torn = cut_final_row(&path, 12);

    let mut repaired = open(temp.path(), session);
    assert_eq!(repaired.recoveries().len(), 1);
    let report = repaired.recoveries()[0].clone();
    assert_eq!(report.preserved_bytes, torn.len() as u64);
    assert_eq!(
        std::fs::read(&report.preserved_tail).expect("read preserved tail"),
        torn
    );
    let state = repaired.snapshot().expect("replayed prefix");
    assert_eq!(state.inputs[&input].phase, InputPhase::Accepted);
    assert_eq!(
        repaired.last_seq(),
        3,
        "the repair checkpoint takes the sequence the torn row never completed"
    );
}

#[test]
fn unknown_version_blocks_open() {
    let temp = temp_root();
    let session = "ses_version";
    let path = journal_path(temp.path(), session);
    let mut store = open(temp.path(), session);
    store.append(&new_id(), accepted("x")).expect("accept");
    drop(store);

    rewrite_rows(&path, |rows| {
        rows[0]["v"] = json!(2);
    });
    let error = DurableRuntimeStore::open_in(temp.path(), session).expect_err("version 2 must block");
    assert_eq!(error.kind(), JournalErrorKind::UnsupportedVersion);
    assert!(error.is_corruption());
}

#[test]
fn corrupt_complete_row_blocks_open() {
    let temp = temp_root();

    let session = "ses_corrupt_json";
    let mut store = open(temp.path(), session);
    store.append(&new_id(), accepted("x")).expect("accept");
    drop(store);
    let path = journal_path(temp.path(), session);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open for corrupt append");
    file.write_all(b"{\"v\":1,\"seq\":3\n").expect("write corrupt row");
    drop(file);
    let error = DurableRuntimeStore::open_in(temp.path(), session).expect_err("corrupt row must block");
    assert_eq!(error.kind(), JournalErrorKind::Corrupt);

    let session = "ses_corrupt_shape";
    let mut store = open(temp.path(), session);
    store.append(&new_id(), accepted("x")).expect("accept");
    drop(store);
    let path = journal_path(temp.path(), session);
    rewrite_rows(&path, |rows| {
        let last = rows.last_mut().expect("accept row");
        last["payload"] = json!({"unexpected": true});
    });
    let error = DurableRuntimeStore::open_in(temp.path(), session).expect_err("payload mismatch must block");
    assert_eq!(error.kind(), JournalErrorKind::Corrupt);

    let session = "ses_no_checkpoint";
    let mut store = open(temp.path(), session);
    store.append(&new_id(), accepted("x")).expect("accept");
    drop(store);
    let path = journal_path(temp.path(), session);
    rewrite_rows(&path, |rows| {
        rows.remove(0);
    });
    let error = DurableRuntimeStore::open_in(temp.path(), session)
        .expect_err("a journal without its first checkpoint must block");
    assert_eq!(error.kind(), JournalErrorKind::Corrupt);
}

#[test]
fn sequence_gap_blocks_open() {
    let temp = temp_root();

    let session = "ses_gap";
    let mut store = open(temp.path(), session);
    store.append(&new_id(), accepted("one")).expect("accept one");
    store.append(&new_id(), accepted("two")).expect("accept two");
    drop(store);
    let path = journal_path(temp.path(), session);
    rewrite_rows(&path, |rows| {
        rows[1]["seq"] = json!(9);
    });
    let error = DurableRuntimeStore::open_in(temp.path(), session).expect_err("a jumped seq must block");
    assert_eq!(error.kind(), JournalErrorKind::SequenceGap);

    let session = "ses_gap_deleted";
    let mut store = open(temp.path(), session);
    for index in 0..3 {
        store
            .append(&new_id(), accepted(&format!("row {index}")))
            .expect("accept");
    }
    drop(store);
    let path = journal_path(temp.path(), session);
    rewrite_rows(&path, |rows| {
        rows.remove(1);
    });
    let error = DurableRuntimeStore::open_in(temp.path(), session).expect_err("a removed middle row leaves a gap");
    assert_eq!(error.kind(), JournalErrorKind::SequenceGap);
}

#[test]
fn partial_tail_is_preserved_and_prefix_replayed() {
    let temp = temp_root();
    let session = "ses_partial";
    let mut store = open(temp.path(), session);
    let input = new_id();
    store.append(&input, accepted("first")).expect("accept one");
    let before = store.snapshot().expect("state before the crash");
    store.append(&new_id(), accepted("second")).expect("accept two");
    let path = journal_path(temp.path(), session);
    let torn = cut_final_row(&path, 17);
    assert!(!torn.contains(&b'\n'), "the preserved tail is an incomplete row");

    let mut repaired = open(temp.path(), session);
    assert_eq!(repaired.recoveries().len(), 1);
    let report = repaired.recoveries()[0].clone();
    assert_eq!(std::fs::read(&report.preserved_tail).expect("tail"), torn);
    assert_eq!(report.source_generation, 1);
    assert_eq!(repaired.generation(), 2);
    let state = repaired.snapshot().expect("replayed prefix");
    assert_eq!(state, before);
    assert_eq!(repaired.last_seq(), report.rewritten_seq);

    let next = new_id();
    let appended = repaired
        .append(&next, accepted("after recovery"))
        .expect("append after recovery");
    assert_eq!(appended.seq, report.rewritten_seq + 1);
    drop(repaired);
    let mut reopened = open(temp.path(), session);
    assert_eq!(reopened.last_seq(), appended.seq);
    assert_eq!(reopened.snapshot().expect("state").inputs.len(), 2);
}

#[test]
fn repeated_interruptions_preserve_each_tail() {
    let temp = temp_root();
    let session = "ses_repeated";
    let dir = temp.path().join(session);
    let mut preserved = Vec::new();

    for round in 0..3 {
        let mut store = open(temp.path(), session);
        store
            .append(&new_id(), accepted(&format!("round {round}")))
            .expect("append");
        let path = journal_path(temp.path(), session);
        let torn = cut_final_row(&path, 11 + round);
        drop(store);

        let mut repaired = open(temp.path(), session);
        let report = repaired.recoveries().last().expect("recovery report").clone();
        assert_eq!(std::fs::read(&report.preserved_tail).expect("tail"), torn);
        preserved.push(report.preserved_tail.clone());
        repaired
            .append(&new_id(), accepted("survivor"))
            .expect("append after repair");
    }

    preserved.sort();
    preserved.dedup();
    assert_eq!(preserved.len(), 3, "each interruption keeps its own tail");
    assert_eq!(recovery_files(&dir).len(), 3);
    let mut reopened = open(temp.path(), session);
    assert_eq!(
        reopened.snapshot().expect("state").inputs.len(),
        3,
        "only the rows that survived each repair are replayed"
    );
}

#[test]
fn compaction_preserves_live_state() {
    let temp = temp_root();
    let session = "ses_compact";
    let limits = JournalLimits {
        compact_records: 8,
        ..JournalLimits::default()
    };
    let mut store = open_with(temp.path(), session, limits, None);

    let pending = new_id();
    store.append(&pending, accepted("waiting")).expect("pending");
    let uncertain = new_id();
    store.append(&uncertain, accepted("uncertain")).expect("uncertain");
    store.append(&uncertain, started()).expect("start uncertain");
    let run = new_id();
    store.append(&run, run_started()).expect("run started");
    let monitor = new_id();
    store
        .append(
            &monitor,
            Payload::MonitorUpsert(MonitorUpsert {
                monitor_kind: "file".to_string(),
                spec: json!({"path": "/tmp/watched"}),
            }),
        )
        .expect("monitor");
    let wake = new_id();
    store
        .append(
            &wake,
            Payload::WakePending(WakePending {
                monitor_id: monitor.clone(),
                monitor_seq: 1,
                summary: "changed".to_string(),
            }),
        )
        .expect("wake");
    let delivered = new_id();
    store.append(&delivered, accepted("delivered")).expect("delivered");
    store.append(&delivered, started()).expect("delivered start");
    store.append(&delivered, committed()).expect("delivered commit");

    for index in 0..20 {
        let churn = new_id();
        store
            .append(&churn, accepted(&format!("churn {index}")))
            .expect("churn accept");
        store.append(&churn, started()).expect("churn start");
        store.append(&churn, committed()).expect("churn commit");
    }

    let state = store.snapshot().expect("state after compaction");
    assert!(store.generation() >= 2, "compaction must have rewritten the file");
    let records = read_journal_records(store.journal_path()).expect("rows");
    assert_eq!(records[0].kind, RecordKind::Checkpoint);

    assert!(state.inputs.contains_key(&pending));
    assert_eq!(state.inputs[&uncertain].phase, InputPhase::Started);
    assert_eq!(state.undetermined_inputs().count(), 1);
    assert!(state.runs.contains_key(&run));
    assert_eq!(state.live_runs().count(), 1);
    assert_eq!(state.pending_wakes().count(), 1);
    assert!(state.pending_wakes().any(|(id, _)| id == &wake));
    assert_eq!(state.input_receipts[&delivered].phase, InputReceiptPhase::Committed);

    let last_seq = store.last_seq();
    drop(store);
    let mut reopened = open(temp.path(), session);
    assert_eq!(reopened.last_seq(), last_seq);
    assert_eq!(reopened.snapshot().expect("replayed state"), state);
}

#[test]
fn replay_after_compaction_matches() {
    let temp = temp_root();
    let session = "ses_stale_state";
    let mut store = open(temp.path(), session);
    let input = new_id();
    store.append(&input, accepted("kept")).expect("accept");
    store.append(&input, started()).expect("start");
    let before = store.snapshot().expect("state before compaction");
    store.compact().expect("compact");
    let generation = store.generation();
    let seq = store.last_seq();
    assert_eq!(generation, 2);
    assert_eq!(
        store.snapshot().expect("state after compaction"),
        before,
        "compaction must not change replayed state"
    );
    drop(store);

    let mut reopened = open(temp.path(), session);
    assert_eq!(reopened.snapshot().expect("replayed state"), before);
    assert_eq!(reopened.last_seq(), seq);
    reopened.append(&input, committed()).expect("commit after compaction");
    drop(reopened);

    let mut again = open(temp.path(), session);
    assert_eq!(
        again.snapshot().expect("state").input_receipts[&input].phase,
        InputReceiptPhase::Committed
    );
    assert_eq!(again.last_seq(), seq + 1);
}

#[test]
fn admission_caps_reject_with_busy() {
    let temp = temp_root();
    let session = "ses_live_inputs";
    let limits = JournalLimits {
        max_live_inputs: 3,
        ..JournalLimits::default()
    };
    let mut store = open_with(temp.path(), session, limits, None);
    for index in 0..3 {
        store
            .append(&new_id(), accepted(&format!("held {index}")))
            .expect("within the live input cap");
    }
    let error = store
        .append(&new_id(), accepted("one too many"))
        .expect_err("cap must refuse");
    assert_eq!(error.kind(), JournalErrorKind::Busy);

    let session = "ses_live_bytes";
    let limits = JournalLimits {
        max_live_input_bytes: 512,
        ..JournalLimits::default()
    };
    let mut store = open_with(temp.path(), session, limits, None);
    store.append(&new_id(), accepted(&"a".repeat(200))).expect("first fits");
    let error = store
        .append(&new_id(), accepted(&"b".repeat(200)))
        .expect_err("payload cap");
    assert_eq!(error.kind(), JournalErrorKind::Busy);
}

#[test]
fn control_and_run_rows_are_recordable_at_the_live_input_cap() {
    let temp = temp_root();
    let session = "ses_control_at_cap";
    let limits = JournalLimits {
        max_live_inputs: 1,
        ..JournalLimits::default()
    };
    let mut store = open_with(temp.path(), session, limits, None);
    let queued = new_id();
    store.append(&queued, accepted("queued")).expect("first input");

    let error = store
        .append(&new_id(), accepted("one too many"))
        .expect_err("the live input cap must refuse a new input");
    assert_eq!(error.kind(), JournalErrorKind::Busy);

    let operation = new_id();
    store
        .append(
            &operation,
            Payload::ControlApplying(ControlApplying {
                action: "abort_run".to_string(),
                digest: "sha256:abort".to_string(),
            }),
        )
        .expect("an abort request must be recordable while the live input cap is full");
    let still_busy = store
        .append(&new_id(), accepted("second too many"))
        .expect_err("a control row must not free a live input slot");
    assert_eq!(still_busy.kind(), JournalErrorKind::Busy);
    store
        .append(
            &operation,
            Payload::ControlTerminal(ControlTerminal {
                result: json!({"status": "applied"}),
            }),
        )
        .expect("the control result must be recordable at the cap");

    let run = new_id();
    store
        .append(&run, run_started())
        .expect("run started is a lifecycle row");
    store
        .append(
            &run,
            Payload::RunStopped(RunStopped::new(StopCause::UserCancelled, None)),
        )
        .expect("run stopped is a lifecycle row");

    store.append(&queued, started()).expect("start the queued input");
    store.append(&queued, committed()).expect("commit the queued input");
    store
        .append(&new_id(), accepted("the freed slot admits again"))
        .expect("committing the live input frees the admission budget");
    assert_eq!(store.state().inputs.len(), 1);
}

#[test]
fn terminal_runs_are_bounded_and_unfinished_work_survives() {
    let temp = temp_root();
    let session = "ses_run_history";
    let limits = JournalLimits {
        compact_bytes: 64 * 1024 * 1024,
        compact_records: 1_000_000,
        ..JournalLimits::default()
    };
    let mut store = open_with(temp.path(), session, limits, None);

    let pending = new_id();
    store.append(&pending, accepted("waiting")).expect("pending input");
    let uncertain = new_id();
    store.append(&uncertain, accepted("uncertain")).expect("accept");
    store.append(&uncertain, started()).expect("start");
    store
        .append(
            &uncertain,
            Payload::InputInDoubt(InputInDoubt {
                cause: Some("restart mid-run".to_string()),
            }),
        )
        .expect("in doubt");
    let live_run = new_id();
    store.append(&live_run, run_started()).expect("live run");
    let monitor = new_id();
    store
        .append(
            &monitor,
            Payload::MonitorUpsert(MonitorUpsert {
                monitor_kind: "file".to_string(),
                spec: json!({"path": "/tmp/watched"}),
            }),
        )
        .expect("monitor");
    let wake = new_id();
    store
        .append(
            &wake,
            Payload::WakePending(WakePending {
                monitor_id: monitor.clone(),
                monitor_seq: 1,
                summary: "changed".to_string(),
            }),
        )
        .expect("wake");

    let over_cap = TERMINAL_RUN_CAP + 44;
    let mut stopped_runs = Vec::new();
    for _ in 0..over_cap {
        let run = new_id();
        store.append(&run, run_started()).expect("run started");
        store
            .append(&run, Payload::RunStopped(RunStopped::new(StopCause::Completed, None)))
            .expect("run stopped");
        stopped_runs.push(run);
    }

    let bounded = store.snapshot().expect("bounded state");
    assert_eq!(
        bounded.runs.len(),
        TERMINAL_RUN_CAP + 1,
        "{over_cap} terminal runs must collapse to the cap plus the live run"
    );
    assert!(bounded.runs.contains_key(&live_run), "a live run is never pruned");
    assert_eq!(bounded.runs[&live_run].stop, None);
    let newest = stopped_runs.last().expect("newest run");
    let oldest = stopped_runs.first().expect("oldest run");
    assert!(bounded.runs.contains_key(newest), "the newest terminal run is kept");
    assert!(!bounded.runs.contains_key(oldest), "the oldest terminal run is pruned");
    assert!(bounded.inputs.contains_key(&pending), "a live input survives pruning");
    assert_eq!(bounded.inputs[&uncertain].phase, InputPhase::InDoubt);
    assert_eq!(bounded.pending_wakes().count(), 1);
    assert!(bounded.pending_wakes().any(|(id, _)| id == &wake));

    let bytes_before = store.journal_bytes();
    store.compact().expect("compact");
    let after = store.snapshot().expect("state after compaction");
    assert_eq!(after, bounded, "compaction must keep the bounded state unchanged");
    assert!(
        store.journal_bytes() < bytes_before,
        "the checkpoint is smaller than the appended journal: {} >= {}",
        store.journal_bytes(),
        bytes_before
    );

    let last_seq = store.last_seq();
    drop(store);
    let mut reopened = open(temp.path(), session);
    assert_eq!(reopened.last_seq(), last_seq);
    assert_eq!(
        reopened.snapshot().expect("replayed state"),
        after,
        "the checkpoint carries the same bounded state"
    );
}

#[test]
fn terminal_receipt_caps_bound_controls_and_input_receipts() {
    let temp = temp_root();
    let session = "ses_record_caps";
    let limits = JournalLimits {
        terminal_receipt_cap: 4,
        compact_bytes: 64 * 1024 * 1024,
        compact_records: 1_000_000,
        ..JournalLimits::default()
    };
    let mut store = open_with(temp.path(), session, limits, None);

    let in_flight = new_id();
    store
        .append(
            &in_flight,
            Payload::ControlApplying(ControlApplying {
                action: "abort_run".to_string(),
                digest: "sha256:open".to_string(),
            }),
        )
        .expect("nonterminal control");

    let mut terminal_ops = Vec::new();
    for _ in 0..12 {
        let operation = new_id();
        store
            .append(
                &operation,
                Payload::ControlApplying(ControlApplying {
                    action: "cancel_inputs".to_string(),
                    digest: "sha256:digest".to_string(),
                }),
            )
            .expect("applying");
        store
            .append(
                &operation,
                Payload::ControlTerminal(ControlTerminal {
                    result: json!({"status": "applied"}),
                }),
            )
            .expect("terminal");
        terminal_ops.push(operation);
    }

    let mut finished_inputs = Vec::new();
    for index in 0..12 {
        let input = new_id();
        store
            .append(&input, accepted(&format!("done {index}")))
            .expect("accept");
        store.append(&input, started()).expect("start");
        store.append(&input, committed()).expect("commit");
        finished_inputs.push(input);
    }

    let bounded = store.snapshot().expect("state");
    let terminal = bounded
        .controls
        .values()
        .filter(|entry| entry.phase == ControlPhase::Terminal)
        .count();
    assert_eq!(terminal, 4, "terminal controls are capped");
    assert_eq!(
        bounded.controls[&in_flight].phase,
        ControlPhase::Applying,
        "a nonterminal control is never pruned"
    );
    assert!(
        bounded
            .controls
            .contains_key(terminal_ops.last().expect("newest operation")),
        "the newest terminal control is kept"
    );
    assert!(
        !bounded.controls.contains_key(&terminal_ops[0]),
        "the oldest terminal control is pruned"
    );
    assert_eq!(bounded.input_receipts.len(), 4, "input receipts are capped");
    assert!(
        bounded
            .input_receipts
            .contains_key(finished_inputs.last().expect("newest input")),
        "the newest receipt is kept"
    );
    assert!(!bounded.input_receipts.contains_key(&finished_inputs[0]));

    store.compact().expect("compact");
    drop(store);
    let mut reopened = open(temp.path(), session);
    assert_eq!(reopened.snapshot().expect("replayed state"), bounded);
}

#[test]
fn lifecycle_rows_use_the_reserve_then_read_only() {
    let temp = temp_root();
    let session = "ses_reserve";
    let limits = JournalLimits {
        max_journal_bytes: 3_000,
        reserve_bytes: 600,
        compact_bytes: 64 * 1024 * 1024,
        compact_records: 1_000_000,
        ..JournalLimits::default()
    };
    let mut store = open_with(temp.path(), session, limits, None);

    let mut refusals = 0;
    for index in 0..200 {
        match store.append(&new_id(), accepted(&format!("admission {index}"))) {
            Ok(_) => {}
            Err(error) if error.kind() == JournalErrorKind::Busy => {
                refusals += 1;
                if refusals > 3 {
                    break;
                }
            }
            Err(error) => panic!("unexpected admission error: {error}"),
        }
    }
    assert!(refusals > 0, "the admission ceiling must be reached");
    assert!(store.journal_bytes() <= 3_000 - 600);

    let operation = new_id();
    store
        .append(
            &operation,
            Payload::ControlApplying(ControlApplying {
                action: "cancel_inputs".to_string(),
                digest: "sha256:cancel".to_string(),
            }),
        )
        .expect("a control row is a lifecycle row and uses the reserve");
    store
        .append(
            &operation,
            Payload::ControlTerminal(ControlTerminal {
                result: json!({"status": "applied"}),
            }),
        )
        .expect("a control completion also uses the reserve");
    let refused = store
        .append(&new_id(), accepted("still an admission"))
        .expect_err("admissions stay busy inside the reserve");
    assert_eq!(refused.kind(), JournalErrorKind::Busy);

    let mut read_only = 0;
    for index in 0..200 {
        match store.append(&new_id(), run_started()) {
            Ok(_) => {}
            Err(error) if error.kind() == JournalErrorKind::ReadOnly => {
                read_only += 1;
                break;
            }
            Err(error) => panic!("unexpected lifecycle error {index}: {error}"),
        }
    }
    assert_eq!(read_only, 1, "lifecycle rows must run into the read-only ceiling");
    assert!(store.is_read_only());
    assert!(store.journal_bytes() <= 3_000);
    let error = store
        .append(&new_id(), run_started())
        .expect_err("read-only stays read-only");
    assert_eq!(error.kind(), JournalErrorKind::ReadOnly);
}

#[test]
fn oversized_input_payload_is_rejected() {
    let temp = temp_root();
    let session = "ses_oversized";
    let limits = JournalLimits {
        max_input_payload_bytes: 128,
        ..JournalLimits::default()
    };
    let mut store = open_with(temp.path(), session, limits, None);
    let error = store
        .append(&new_id(), accepted(&"x".repeat(256)))
        .expect_err("an oversized input must be refused");
    assert_eq!(error.kind(), JournalErrorKind::InvalidRequest);
    assert_eq!(store.last_seq(), 0);
    assert!(!store.journal_path().exists());
}

#[test]
fn fsync_failure_rolls_back_the_row_and_keeps_state() {
    let temp = temp_root();
    let session = "ses_fsync";
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let attempts = Arc::clone(&counter);
    let hooks = hook(move |point| {
        if point == JournalIoPoint::RecordFileSync {
            let attempt = attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            if attempt == 2 {
                return Err(std::io::Error::other("injected file sync failure"));
            }
        }
        Ok(())
    });
    let mut store = open_with(temp.path(), session, JournalLimits::default(), Some(hooks));

    let kept = new_id();
    store.append(&kept, accepted("kept")).expect("first append is durable");
    let durable_len = store.journal_bytes();
    let durable_seq = store.last_seq();

    let lost = new_id();
    let error = store
        .append(&lost, accepted("lost"))
        .expect_err("a failed file sync must not be reported as success");
    assert_eq!(error.kind(), JournalErrorKind::Io);
    assert_eq!(store.last_seq(), durable_seq);
    assert_eq!(store.journal_bytes(), durable_len);
    assert_eq!(
        std::fs::metadata(store.journal_path()).expect("journal metadata").len(),
        durable_len,
        "the failed row is truncated away before the lock is released"
    );
    assert!(!store.state().inputs.contains_key(&lost));

    drop(store);
    let mut reopened = open(temp.path(), session);
    let state = reopened.snapshot().expect("state");
    assert!(state.inputs.contains_key(&kept));
    assert!(!state.inputs.contains_key(&lost));
    assert_eq!(reopened.last_seq(), durable_seq);
    let resumed = reopened
        .append(&new_id(), accepted("resumed"))
        .expect("append after rollback");
    assert_eq!(resumed.seq, durable_seq + 1, "no sequence is burned by a failed append");
}

#[test]
fn publication_waits_for_sync_and_never_reports_a_broken_row_as_success() {
    let temp = temp_root();
    let session = "ses_publication";
    let points: Arc<Mutex<Vec<JournalIoPoint>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&points);
    let mut failed = false;
    let hooks = hook(move |point| {
        sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(point);
        if point == JournalIoPoint::RecordFileSync && !failed {
            failed = true;
            return Err(std::io::Error::other("injected first sync failure"));
        }
        Ok(())
    });
    let mut store = open_with(temp.path(), session, JournalLimits::default(), Some(hooks));
    points.lock().expect("points").clear();

    let lost = new_id();
    let error = store.append(&lost, accepted("lost")).expect_err("first publish fails");
    assert_eq!(error.kind(), JournalErrorKind::Io);
    let seen = points.lock().expect("points").clone();
    assert_eq!(
        seen,
        vec![
            JournalIoPoint::WriterLockAcquire,
            JournalIoPoint::RecordWrite,
            JournalIoPoint::RecordFileSync,
            JournalIoPoint::RecordRollbackFileSync,
            JournalIoPoint::WriterLockRelease,
        ],
        "the failure is observed after the write and before any directory sync"
    );
    assert_eq!(store.last_seq(), 0);
    assert_eq!(store.journal_bytes(), 0);

    let kept = new_id();
    let appended = store.append(&kept, accepted("kept")).expect("second publish succeeds");
    assert_eq!(appended.seq, 2);
    let seen = points.lock().expect("points").clone();
    let tail = &seen[5..];
    assert_eq!(
        tail,
        &[
            JournalIoPoint::WriterLockAcquire,
            JournalIoPoint::RecordWrite,
            JournalIoPoint::RecordFileSync,
            JournalIoPoint::RecordDirectorySync,
            JournalIoPoint::WriterLockRelease,
        ],
        "the new file's directory entry is synced before the row is published"
    );
}

#[test]
fn rename_failure_keeps_the_previous_journal() {
    let temp = temp_root();
    let session = "ses_rename";
    let hooks = hook(|point| {
        if point == JournalIoPoint::RewriteRename {
            return Err(std::io::Error::other("injected rename failure"));
        }
        Ok(())
    });
    let mut store = open_with(temp.path(), session, JournalLimits::default(), Some(hooks));
    let input = new_id();
    store.append(&input, accepted("kept")).expect("append");
    let durable_len = store.journal_bytes();
    let generation = store.generation();
    let before = store.snapshot().expect("state");

    let error = store
        .compact()
        .expect_err("a failed rename must not publish a checkpoint");
    assert_eq!(error.kind(), JournalErrorKind::Io);
    assert_eq!(store.journal_bytes(), durable_len);
    assert_eq!(store.generation(), generation);
    assert_eq!(store.snapshot().expect("state"), before);
    let temp_files: Vec<PathBuf> = std::fs::read_dir(store.directory())
        .expect("list journal dir")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".runtime.tmp-"))
        })
        .collect();
    assert!(temp_files.is_empty(), "a failed replace removes its temp file");

    drop(store);
    let mut reopened = open(temp.path(), session);
    assert_eq!(reopened.snapshot().expect("state"), before);
    reopened
        .append(&new_id(), accepted("after"))
        .expect("append after the failure");
}

#[test]
fn directory_sync_failure_fails_the_first_publish() {
    let temp = temp_root();
    let session = "ses_directory_sync";
    let hooks = hook(|point| {
        if point == JournalIoPoint::RecordDirectorySync {
            return Err(std::io::Error::other("injected directory sync failure"));
        }
        Ok(())
    });
    let mut store = open_with(temp.path(), session, JournalLimits::default(), Some(hooks));
    let error = store
        .append(&new_id(), accepted("not durable"))
        .expect_err("a missing directory sync must not be reported as success");
    assert_eq!(error.kind(), JournalErrorKind::Io);
    assert_eq!(store.last_seq(), 0);
    assert_eq!(store.journal_bytes(), 0);
    assert_eq!(
        std::fs::metadata(store.journal_path()).expect("journal metadata").len(),
        0,
        "the rollback truncates the row it could not publish"
    );
    drop(store);

    let mut reopened = open(temp.path(), session);
    assert_eq!(reopened.last_seq(), 0);
    assert_eq!(reopened.snapshot().expect("state").inputs.len(), 0);
    assert_eq!(reopened.append(&new_id(), accepted("clean")).expect("append").seq, 2);
}

#[test]
fn terminal_receipts_are_bounded() {
    let temp = temp_root();
    let session = "ses_receipts";
    let limits = JournalLimits {
        terminal_receipt_cap: 4,
        ..JournalLimits::default()
    };
    let mut store = open_with(temp.path(), session, limits, None);
    for index in 0..12 {
        let id = new_id();
        store.append(&id, accepted(&format!("done {index}"))).expect("accept");
        store.append(&id, started()).expect("start");
        store.append(&id, committed()).expect("commit");
    }
    let state = store.snapshot().expect("state");
    assert_eq!(
        state.input_receipts.len(),
        4,
        "live state keeps only the newest terminal receipts"
    );
    assert!(state.inputs.is_empty());
    store.compact().expect("compact");
    drop(store);

    let mut reopened = open(temp.path(), session);
    assert_eq!(
        reopened.snapshot().expect("state").input_receipts.len(),
        4,
        "the checkpoint carries only the bounded receipts"
    );
}

struct ChildWriter {
    child: Child,
    lines: Receiver<String>,
    seen: Vec<String>,
}

impl ChildWriter {
    fn spawn(root: &Path, session: &str, writer: u64, writes: u64) -> Self {
        let exe = std::env::current_exe().expect("test binary path");
        let mut child = Command::new(exe)
            .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
            .env("DURABLE_RUNTIME_CHILD_ROOT", root)
            .env("DURABLE_RUNTIME_CHILD_SESSION", session)
            .env("DURABLE_RUNTIME_CHILD_WRITER", writer.to_string())
            .env("DURABLE_RUNTIME_CHILD_WRITES", writes.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn the writer child");
        let stdout = child.stdout.take().expect("child stdout");
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            child,
            lines,
            seen: Vec::new(),
        }
    }

    fn expect_line(&mut self, needle: &str) {
        let deadline = Instant::now() + CHILD_DEADLINE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                panic!("writer child did not print {needle:?}; output so far: {:?}", self.seen);
            }
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    if line.contains(needle) {
                        return;
                    }
                    self.seen.push(line);
                }
                Err(RecvTimeoutError::Timeout) => {
                    panic!("writer child timed out before {needle:?}; output: {:?}", self.seen)
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("writer child exited before {needle:?}; output: {:?}", self.seen)
                }
            }
        }
    }

    fn release(&mut self) {
        let stdin = self.child.stdin.as_mut().expect("child stdin");
        stdin.write_all(b"go\n").expect("release the child barrier");
        stdin.flush().expect("flush the child barrier");
    }

    fn finish(mut self) -> std::process::ExitStatus {
        self.child.wait().expect("wait for the writer child")
    }
}

#[test]
fn child_writer_process() {
    let Ok(root) = std::env::var("DURABLE_RUNTIME_CHILD_ROOT") else {
        return;
    };
    let session = std::env::var("DURABLE_RUNTIME_CHILD_SESSION").expect("child session");
    let writer: u64 = std::env::var("DURABLE_RUNTIME_CHILD_WRITER")
        .expect("child writer")
        .parse()
        .expect("writer id");
    let writes: u64 = std::env::var("DURABLE_RUNTIME_CHILD_WRITES")
        .expect("child writes")
        .parse()
        .expect("write count");

    println!("ready {writer}");
    eprintln!("child writer {writer} pid {}", std::process::id());
    let mut go = String::new();
    std::io::stdin()
        .read_line(&mut go)
        .expect("wait for the parent barrier");
    assert_eq!(go.trim(), "go");

    let mut store = DurableRuntimeStore::open_in(Path::new(&root), &session).expect("child journal");
    for index in 0..writes {
        let id = new_id();
        store
            .append(&id, run_started())
            .unwrap_or_else(|error| panic!("child {writer} row {index} failed: {error}"));
    }
    println!("done {writer} {writes}");
}

#[test]
fn concurrent_writers_preserve_every_seq() {
    let temp = temp_root();
    let session = "ses_concurrent";
    let writes = 20u64;
    let mut first = ChildWriter::spawn(temp.path(), session, 1, writes);
    let mut second = ChildWriter::spawn(temp.path(), session, 2, writes);

    first.expect_line("ready 1");
    second.expect_line("ready 2");
    first.release();
    second.release();
    first.expect_line("done 1");
    second.expect_line("done 2");
    assert!(first.finish().success(), "first writer child failed");
    assert!(second.finish().success(), "second writer child failed");

    let mut store = open(temp.path(), session);
    let state = store.snapshot().expect("state");
    assert_eq!(state.runs.len() as u64, 2 * writes);
    assert_eq!(store.last_seq(), 2 * writes + 1);
    let records = read_journal_records(store.journal_path()).expect("rows");
    assert_eq!(
        records.iter().map(|record| record.seq).collect::<Vec<_>>(),
        (1..=2 * writes + 1).collect::<Vec<_>>(),
        "two processes never share or skip a sequence number"
    );
    assert_eq!(records[0].kind, RecordKind::Checkpoint);
}
