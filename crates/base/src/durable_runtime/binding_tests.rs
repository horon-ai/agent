//! Run rows carry the host-local session binding, and a checkpoint keeps the
//! newest one across compaction.
//!
//! These tests pin four properties the session layer will rely on: the newest
//! `rev` wins (with the later row winning a tie), the binding survives a
//! checkpoint rewrite, a row an older build wrote is byte-for-byte unchanged and
//! still parses, and a binding row whose flush fails leaves no state behind.

use super::*;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;

use horon_agent_session_types::SessionStatus as Status;

fn temp_root() -> TempDir {
    TempDir::new().expect("temp dir")
}

fn journal_path(root: &Path, session: &str) -> std::path::PathBuf {
    root.join(session).join(JOURNAL_FILE_NAME)
}

/// Write a journal fixture directly, creating the session directory an open
/// would normally create.
fn seed(root: &Path, session: &str, bytes: &str) {
    let path = journal_path(root, session);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("session dir");
    std::fs::write(&path, bytes).expect("seed the fixture");
}

fn binding(rev: u64, status: Status, pid: u32, working_dir: &str) -> Binding {
    Binding {
        rev,
        status,
        last_pid: Some(pid),
        last_active_at: None,
        working_dir: Some(working_dir.to_string()),
        migration_epoch: Some(0),
        provider_session_id: Some(format!("provider-{rev}")),
    }
}

#[test]
fn the_newest_revision_of_a_binding_wins_across_a_reopen() {
    let temp = temp_root();
    let session = "ses_binding_newest";
    let mut store = DurableRuntimeStore::open_in(temp.path(), session).expect("open");

    let first = new_id();
    store
        .append_run_started(&first, None, Some(binding(1, Status::Active, 100, "/work/one")))
        .expect("first start row");
    store
        .append_run_stopped(
            &first,
            StopCause::Completed,
            None,
            Some(binding(2, Status::Closed, 100, "/work/one")),
        )
        .expect("first stop row");

    let second = new_id();
    store
        .append_run_started(&second, None, Some(binding(3, Status::Active, 200, "/work/two")))
        .expect("second start row");
    drop(store);

    let mut reopened = DurableRuntimeStore::open_in(temp.path(), session).expect("reopen");
    let latest = reopened
        .latest_binding()
        .expect("lookup")
        .expect("a binding is recorded");
    assert_eq!(latest.rev, 3, "the highest revision wins, not the last row written");
    assert_eq!(latest.status, Status::Active);
    assert_eq!(latest.last_pid, Some(200));
    assert_eq!(latest.working_dir.as_deref(), Some("/work/two"));
    assert_eq!(latest.provider_session_id.as_deref(), Some("provider-3"));
    assert_eq!(reopened.state().runs.len(), 2, "both runs are still replayed");
}

#[test]
fn an_equal_revision_is_won_by_the_later_row() {
    let temp = temp_root();
    let session = "ses_binding_tie";
    let mut store = DurableRuntimeStore::open_in(temp.path(), session).expect("open");

    let run = new_id();
    store
        .append_run_started(&run, None, Some(binding(7, Status::Active, 41, "/work")))
        .expect("start row");
    store
        .append_run_stopped(
            &run,
            StopCause::Completed,
            None,
            Some(binding(7, Status::Closed, 41, "/work")),
        )
        .expect("stop row");

    let latest = store.latest_binding().expect("lookup").expect("a binding is recorded");
    assert_eq!(latest.rev, 7);
    assert_eq!(
        latest.status,
        Status::Closed,
        "the stop row after a start row at the same revision is the state a restart reads"
    );
    drop(store);

    let mut reopened = DurableRuntimeStore::open_in(temp.path(), session).expect("reopen");
    let replayed = reopened
        .latest_binding()
        .expect("lookup")
        .expect("a binding is recorded");
    assert_eq!(replayed, latest, "a replay picks the same row the live store did");
}

#[test]
fn a_run_row_without_a_binding_leaves_the_lookup_empty() {
    let temp = temp_root();
    let session = "ses_binding_absent";
    let mut store = DurableRuntimeStore::open_in(temp.path(), session).expect("open");
    let run = new_id();
    store
        .append_run_started(&run, None, None)
        .expect("start row without a binding");
    store
        .append_run_stopped(&run, StopCause::Completed, None, None)
        .expect("stop row without a binding");
    assert_eq!(store.latest_binding().expect("lookup"), None);
    assert!(store.state().binding.is_none());
}

#[test]
fn a_checkpoint_keeps_the_newest_binding_after_compaction() {
    let temp = temp_root();
    let session = "ses_binding_compact";
    let mut store = DurableRuntimeStore::open_in(temp.path(), session).expect("open");

    let run = new_id();
    store
        .append_run_started(&run, None, Some(binding(1, Status::Active, 11, "/work/one")))
        .expect("run started");
    store
        .append_run_stopped(
            &run,
            StopCause::Completed,
            None,
            Some(binding(2, Status::Closed, 11, "/work/one")),
        )
        .expect("run stopped");

    // Compaction folds the replayed state into a new checkpoint, which is the
    // path that must carry the binding forward.
    store.compact().expect("compact");
    assert!(store.generation() >= 2, "compaction must have written a checkpoint");

    let expected = binding(2, Status::Closed, 11, "/work/one");
    assert_eq!(
        store.latest_binding().expect("lookup").as_ref(),
        Some(&expected),
        "the checkpoint carries the binding the rows established"
    );
    drop(store);

    // The file now begins with a checkpoint: read the binding out of that row.
    let records = read_journal_records(&journal_path(temp.path(), session)).expect("rows");
    assert_eq!(records[0].kind, RecordKind::Checkpoint);
    let Payload::Checkpoint(checkpoint) = &records[0].payload else {
        panic!("the first row is a checkpoint");
    };
    assert_eq!(checkpoint.state.latest_binding(), Some(&expected));

    let mut reopened = DurableRuntimeStore::open_in(temp.path(), session).expect("reopen");
    assert_eq!(
        reopened.latest_binding().expect("lookup").as_ref(),
        Some(&expected),
        "a freshly loaded checkpoint restores the binding"
    );
}

#[test]
fn a_checkpoint_without_a_binding_still_loads() {
    let temp = temp_root();
    let session = "ses_binding_absent_checkpoint";
    // Write the pre-change checkpoint shape by hand: no `binding` key at all.
    let fixture = concat!(
        r#"{"v":1,"seq":1,"session_id":"ses_binding_absent_checkpoint","#,
        r#""kind":"checkpoint","id":"24e284eb-c5ec-4c61-8e07-fab4e0708f72","#,
        r#""at":"2026-11-07T13:35:09.211453Z","payload":{"generation":1,"last_seq":0,"start_seq":1,"#,
        r#""state":{"controls":{},"input_receipts":{},"inputs":{},"monitors":{},"runs":{},"wakes":{}}}}"#,
        "\n"
    );
    seed(temp.path(), session, fixture);

    let mut reopened = DurableRuntimeStore::open_in(temp.path(), session).expect("a binding-less checkpoint loads");
    assert_eq!(reopened.latest_binding().expect("lookup"), None);
    assert_eq!(reopened.generation(), 1);
}

/// The journal bytes this build wrote before run rows carried a binding, copied
/// from a real journal. They may not change: an older binary has to read these
/// rows, and a new binary has to leave them byte-for-byte as they were.
const PRE_BINDING_JOURNAL: &str = concat!(
    r#"{"v":1,"seq":1,"session_id":"ses_fixture","kind":"checkpoint","id":"24e284eb-c5ec-4c61-8e07-fab4e0708f72","at":"2026-11-07T13:35:09.211453Z","payload":{"generation":1,"last_seq":0,"start_seq":1,"state":{"controls":{},"input_receipts":{},"inputs":{},"monitors":{},"runs":{},"wakes":{}}}}"#,
    "\n",
    r#"{"v":1,"seq":2,"session_id":"ses_fixture","kind":"run.started","id":"5b976ee6-ed66-4e8c-8271-a80891e034ab","at":"2026-11-07T13:35:09.211532Z","payload":{}}"#,
    "\n",
    r#"{"v":1,"seq":3,"session_id":"ses_fixture","kind":"run.stopped","id":"5b976ee6-ed66-4e8c-8271-a80891e034ab","at":"2026-11-07T13:35:09.546835Z","payload":{"cause":"completed"}}"#,
    "\n",
    r#"{"v":1,"seq":4,"session_id":"ses_fixture","kind":"run.started","id":"d5fb0680-ceb9-46b4-99ec-1c63065101c6","at":"2026-11-07T13:35:29.608221Z","payload":{"input_id":"5e1f0b0a-8b3b-4a1a-9a5e-1b2c3d4e5f60"}}"#,
    "\n",
    r#"{"v":1,"seq":5,"session_id":"ses_fixture","kind":"run.stopped","id":"d5fb0680-ceb9-46b4-99ec-1c63065101c6","at":"2026-11-07T13:35:31.194002Z","payload":{"cause":"completed","input_id":"5e1f0b0a-8b3b-4a1a-9a5e-1b2c3d4e5f60"}}"#,
    "\n"
);

#[test]
fn the_pre_binding_format_replays_and_reserializes_to_the_same_bytes() {
    let temp = temp_root();
    let session = "ses_fixture";
    seed(temp.path(), session, PRE_BINDING_JOURNAL);

    let mut store = DurableRuntimeStore::open_in(temp.path(), session).expect("a pre-change journal loads");
    assert_eq!(store.last_seq(), 5);
    assert_eq!(store.state().runs.len(), 2, "both fixture runs replay");
    assert!(
        store.state().runs.values().all(|run| run.stop.is_some()),
        "both fixture runs are closed"
    );
    assert_eq!(
        store.latest_binding().expect("lookup"),
        None,
        "the fixture carries no binding, and none is invented"
    );
    drop(store);

    // Every row must reserialize to the exact bytes it was read from: a field
    // that is `None` is skipped, not written as `null`.
    for line in PRE_BINDING_JOURNAL.lines() {
        let record = JournalRecord::from_line(line.as_bytes()).expect("fixture row parses");
        let rewritten = record.to_line().expect("fixture row reserializes");
        assert_eq!(
            rewritten.trim_end_matches('\n'),
            line,
            "a binding-less row must serialize to the bytes it was read from"
        );
    }
}

/// A build without the binding field, written out again so the compatibility
/// test below does not depend on this build's definition being the desired one.
mod previous_build {
    use serde::Deserialize;
    use serde_json::Value;

    #[derive(Deserialize)]
    pub struct RunStarted {
        #[serde(default)]
        pub input_id: Option<String>,
    }

    #[derive(Deserialize)]
    pub struct RunStopped {
        #[serde(default)]
        pub cause: Option<Value>,
        #[serde(default)]
        pub input_id: Option<String>,
    }
}

#[test]
fn a_row_carrying_a_binding_still_parses_with_the_previous_definition() {
    let started = RunStarted::with_binding(
        Some("input-1".to_string()),
        Some(binding(9, Status::Active, 77, "/work/three")),
    );
    let value = serde_json::to_value(&started).expect("start row serializes");
    let previous: previous_build::RunStarted =
        serde_json::from_value(value).expect("the previous definition ignores the binding field");
    assert_eq!(previous.input_id.as_deref(), Some("input-1"));

    let stopped = RunStopped::with_binding(
        StopCause::Completed,
        None,
        Some(binding(10, Status::Closed, 77, "/work/three")),
    );
    let value = serde_json::to_value(&stopped).expect("stop row serializes");
    let previous: previous_build::RunStopped =
        serde_json::from_value(value).expect("the previous definition ignores the binding field");
    assert_eq!(previous.input_id, None);
    assert!(previous.cause.is_some());
}

#[test]
fn the_binding_field_is_absent_from_the_payload_json_when_unset() {
    assert_eq!(
        serde_json::to_string(&RunStarted::new(None)).expect("serializes"),
        "{}",
        "a start row with neither field writes nothing, as before"
    );
    assert_eq!(
        serde_json::to_string(&RunStarted::new(Some("input-1".to_string()))).expect("serializes"),
        r#"{"input_id":"input-1"}"#
    );
    assert_eq!(
        serde_json::to_string(&RunStopped::new(StopCause::Completed, None)).expect("serializes"),
        r#"{"cause":"completed"}"#
    );
}

#[test]
fn a_binding_row_whose_flush_fails_is_rolled_back_and_changes_no_state() {
    let temp = temp_root();
    let session = "ses_binding_flush";
    // The first append publishes its checkpoint and row with one file sync; the
    // binding row's own flush is the second, and that is the one that fails.
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let hooks = hook(move |point| {
        if point == JournalIoPoint::RecordFileSync {
            let attempt = counter.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt == 2 {
                return Err(std::io::Error::other("injected file sync failure"));
            }
        }
        Ok(())
    });
    let mut store = DurableRuntimeStore::open_in_with(
        temp.path(),
        session,
        JournalOptions {
            limits: JournalLimits::default(),
            hooks: Some(hooks),
        },
    )
    .expect("open with a fault hook");

    // Settle the journal on disk first, so the failure below is the second sync.
    let settled = new_id();
    store
        .append_run_started(&settled, None, None)
        .expect("the first append settles the checkpoint");

    let durable_len = store.journal_bytes();
    let durable_seq = store.last_seq();
    let run = new_id();
    let error = store
        .append_run_started(&run, None, Some(binding(1, Status::Active, 5, "/work")))
        .expect_err("a failed file sync must not be reported as success");
    assert_eq!(error.kind(), JournalErrorKind::Io);
    assert_eq!(store.last_seq(), durable_seq, "the sequence is not consumed");
    assert_eq!(store.journal_bytes(), durable_len);
    assert_eq!(store.state().runs.len(), 1, "only the settled run is in state");
    assert!(store.state().binding.is_none(), "the binding never entered state");
    assert_eq!(
        std::fs::metadata(store.journal_path()).expect("journal metadata").len(),
        durable_len,
        "the failed row is truncated back to the durable prefix"
    );
    drop(store);

    let mut reopened = DurableRuntimeStore::open_in(temp.path(), session).expect("reopen");
    assert_eq!(reopened.last_seq(), durable_seq);
    assert_eq!(reopened.latest_binding().expect("lookup"), None);
    assert_eq!(reopened.state().runs.len(), 1);
}

#[test]
fn a_binding_row_whose_rollback_also_fails_keeps_state_unchanged() {
    let temp = temp_root();
    let session = "ses_binding_poisoned";
    // The row's own flush fails and so does the flush of the truncation that
    // would undo it, so the store turns read-only without ever adopting the row.
    let hooks = hook(|point| match point {
        JournalIoPoint::RecordFileSync | JournalIoPoint::RecordRollbackFileSync => {
            Err(std::io::Error::other("injected sync failure"))
        }
        _ => Ok(()),
    });
    let mut store = DurableRuntimeStore::open_in_with(
        temp.path(),
        session,
        JournalOptions {
            limits: JournalLimits::default(),
            hooks: Some(hooks),
        },
    )
    .expect("open with a fault hook");

    let run = new_id();
    store
        .append_run_started(&run, None, Some(binding(1, Status::Active, 5, "/work")))
        .expect_err("the row cannot be published");
    assert!(store.state().binding.is_none(), "state never took the row");
    assert!(store.state().runs.is_empty());
    assert!(store.is_read_only(), "an unrolled-back row makes the store read-only");
}

#[test]
fn a_binding_survives_a_tail_repair() {
    let temp = temp_root();
    let session = "ses_binding_repair";
    let mut store = DurableRuntimeStore::open_in(temp.path(), session).expect("open");
    let run = new_id();
    store
        .append_run_started(&run, None, Some(binding(4, Status::Active, 9, "/work/four")))
        .expect("start row");
    // A row after the binding one, which is the row the tear below damages.
    store
        .append(
            &new_id(),
            Payload::InputAccepted(InputAccepted {
                client_id: None,
                content: "after the binding".to_string(),
                images: Vec::new(),
                urgent: false,
                source: "user".to_string(),
            }),
        )
        .expect("trailing row");
    drop(store);

    // Tear the last row's newline away: the repair folds state into a checkpoint.
    let path = journal_path(temp.path(), session);
    let bytes = std::fs::read(&path).expect("read journal");
    let cut = bytes.iter().rposition(|byte| *byte == b'\n').expect("a final newline");
    std::fs::write(&path, &bytes[..cut]).expect("cut the final row's newline");

    let mut reopened = DurableRuntimeStore::open_in(temp.path(), session).expect("repair on open");
    assert_eq!(
        reopened.recoveries().len(),
        1,
        "the torn tail was preserved and rewritten"
    );
    assert_eq!(
        reopened.latest_binding().expect("lookup").map(|entry| entry.rev),
        Some(4),
        "the repair checkpoint carries the binding of the rows it folded"
    );
}
