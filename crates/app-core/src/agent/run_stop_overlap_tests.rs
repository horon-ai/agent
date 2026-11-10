//! The deferred `run.stopped` join: what `horon-agent run` overlaps, and what
//! every other caller keeps doing.

use super::*;

use crate::agent::turn_loops::run_stop_provenance_tests::{
    ProvenanceProvider, completed_round, journal_root, journal_rows, journal_stop_cause, probe_agent,
};
use crate::message::{ContentBlock, Role};
use crate::session::{Session, SessionStatus, find_recent_crashed_sessions};
use horon_agent_base::durable_runtime::{JOURNAL_FILE_NAME, RecordKind};
use horon_agent_harness_api::RuntimeStateSummary;
use std::sync::atomic::Ordering;

/// One session's runtime summary, as the API builds it.
async fn summary_of(session_id: &str) -> RuntimeStateSummary {
    crate::server::runtime_state_api::runtime_state_summary(session_id)
        .await
        .expect("the runtime summary answers")
}

/// Cut `cut_from_end` bytes off `path` and hand back the partial row they left,
/// newline-aligned the way a torn tail actually looks on disk.
fn cut_tail(path: &std::path::Path, cut_from_end: usize) -> Vec<u8> {
    let bytes = std::fs::read(path).expect("the journal is readable");
    let keep = bytes
        .len()
        .checked_sub(cut_from_end)
        .expect("the journal is longer than the cut");
    let truncated = bytes[..keep].to_vec();
    let tail_start = truncated
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|position| position + 1)
        .unwrap_or(0);
    let torn = truncated[tail_start..].to_vec();
    std::fs::write(path, &truncated).expect("the truncated journal is written");
    torn
}

#[tokio::test]
async fn run_stop_overlap_unarmed_run_records_the_stop_before_it_returns() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (provider, _failing) = ProvenanceProvider::scripted(vec![completed_round("done")]);
    let mut agent = probe_agent(provider);
    let session_id = agent.session.id.clone();

    agent.run_turn(false).await.expect("the turn completes");

    let stop = agent
        .current_stop()
        .expect("a long-lived caller still joins at the turn's tail");
    assert!(stop.persisted(), "the row is on disk before the turn returns");
    assert_eq!(
        journal_stop_cause(&session_id, stop.run_id()),
        Some(JournalStopCause::Completed),
        "the unarmed path writes and joins the row exactly as before"
    );
}

#[tokio::test]
async fn run_stop_overlap_armed_finish_holds_the_row_until_settle() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (provider, _failing) = ProvenanceProvider::scripted(vec![completed_round("done")]);
    let mut agent = probe_agent(provider);
    let session_id = agent.session.id.clone();
    let settler = agent.arm_deferred_run_stop_join();

    agent.run_turn(false).await.expect("the turn completes");

    let run_id = agent.current_run().expect("a run was started").run_id().to_string();
    assert!(
        agent.current_stop().is_none(),
        "an armed session holds the ledger record until the close path joins the row"
    );

    agent.settle_run_stop();

    let stop = agent.current_stop().expect("settling records the deferred stop");
    assert_eq!(stop.run_id(), run_id);
    assert_eq!(stop.provenance(), StopProvenance::Completed);
    assert!(stop.persisted(), "the joined row reached the journal");
    assert_eq!(
        journal_stop_cause(&session_id, &run_id),
        Some(JournalStopCause::Completed)
    );

    // Both the close path and the guard settle, so the second one is a no-op.
    drop(settler);
    let again = agent.current_stop().expect("the stop survives a second settle");
    assert_eq!(again.run_id(), run_id);
}

#[tokio::test]
async fn run_stop_overlap_pending_stop_is_joined_before_the_next_start() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (provider, _failing) = ProvenanceProvider::scripted(vec![completed_round("one"), completed_round("two")]);
    let mut agent = probe_agent(provider);
    let session_id = agent.session.id.clone();
    let settler = agent.arm_deferred_run_stop_join();

    agent.run_turn(false).await.expect("the first turn completes");
    let first = agent.current_run().expect("run one is current").run_id().to_string();
    assert!(agent.current_stop().is_none(), "run one's stop is deferred");

    agent.run_turn(false).await.expect("the second turn completes");
    let second = agent.current_run().expect("run two is current").run_id().to_string();
    assert_ne!(first, second, "a new input issues a new run id");

    let rows = journal_rows(&session_id);
    let position = |kind: RecordKind, run_id: &str| {
        rows.iter()
            .position(|(row_kind, id)| *row_kind == kind && id == run_id)
            .unwrap_or_else(|| panic!("no {kind:?} row for {run_id}: {rows:?}"))
    };
    let start_first = position(RecordKind::RunStarted, &first);
    let stop_first = position(RecordKind::RunStopped, &first);
    let start_second = position(RecordKind::RunStarted, &second);
    assert!(start_first < stop_first, "run one's start row precedes its stop row");
    assert!(
        stop_first < start_second,
        "a deferred stop is joined before the next run's start row is dispatched"
    );

    drop(settler);
    let stop = agent.current_stop().expect("the guard settles the second run too");
    assert_eq!(stop.run_id(), second);
    assert!(stop.persisted());
}

#[tokio::test]
async fn run_stop_overlap_drop_guard_settles_a_failed_turn() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (provider, failing) = ProvenanceProvider::scripted(vec![completed_round("never read")]);
    let mut agent = probe_agent(provider);
    let session_id = agent.session.id.clone();
    failing.store(true, Ordering::SeqCst);

    {
        let _settler = agent.arm_deferred_run_stop_join();
        let error = agent.run_turn(false).await.expect_err("the provider fails");
        assert!(error.to_string().contains("provenance provider failure"), "{error}");
        // The guard drops here, and `mark_closed` is never called: a turn that
        // ends in an error must still leave its stop row behind.
    }

    let rows = journal_rows(&session_id);
    assert!(
        rows.iter().any(|(kind, _)| *kind == RecordKind::RunStopped),
        "the guard wrote the stop row on a path that skips the close path: {rows:?}"
    );
    let stop = agent.current_stop().expect("the settled stop is recorded");
    assert_eq!(stop.provenance(), StopProvenance::ProviderError);
    assert!(stop.persisted());
    assert_eq!(
        journal_stop_cause(&session_id, stop.run_id()),
        Some(JournalStopCause::ProviderError)
    );
}

#[tokio::test]
async fn run_stop_overlap_torn_stop_tail_is_preserved_and_the_run_stays_open() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let session_id = "ses_overlap_torn";
    let run_id = Agent::mint_run_id();
    let root = journal_root();
    let path = root.join(session_id).join(JOURNAL_FILE_NAME);

    {
        let mut store = DurableRuntimeStore::open_in(&root, session_id).expect("the journal opens");
        store
            .append(&run_id, Payload::RunStarted(RunStarted { input_id: None }))
            .expect("run.started is written");
        store
            .append(
                &run_id,
                Payload::RunStopped(RunStopped {
                    cause: JournalStopCause::Completed,
                    input_id: None,
                }),
            )
            .expect("run.stopped is written");
    }

    let torn = cut_tail(&path, 12);
    assert!(!torn.is_empty(), "the cut left a partial row behind");

    let mut repaired = DurableRuntimeStore::open_in(&root, session_id).expect("the journal reopens");
    let report = repaired
        .recoveries()
        .first()
        .cloned()
        .expect("the torn tail was recorded");
    assert_eq!(report.preserved_bytes, torn.len() as u64);
    assert_eq!(
        std::fs::read(&report.preserved_tail).expect("the preserved tail is readable"),
        torn,
        "the appended bytes outlive the repair (fsync precedent)"
    );
    let state = repaired.snapshot().expect("the prefix replays");
    assert_eq!(
        state.live_runs().map(|(id, _)| id.clone()).collect::<Vec<String>>(),
        vec![run_id.clone()],
        "a torn stop leaves the run open"
    );
    assert!(state.last_stop().is_none(), "no stop is claimed for a torn row");

    drop(repaired);
    let summary = summary_of(session_id).await;
    assert!(summary.last_stop.is_none(), "the API reports no stop either");
    assert_eq!(summary.runs.len(), 1);
    assert_eq!(summary.runs[0].run_id, run_id);
}

#[test]
fn run_stop_overlap_second_stop_for_the_same_run_is_rejected() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let session_id = "ses_overlap_duplicate_stop";
    let run_id = Agent::mint_run_id();
    let root = journal_root();
    let mut store = DurableRuntimeStore::open_in(&root, session_id).expect("the journal opens");
    store
        .append(&run_id, Payload::RunStarted(RunStarted { input_id: None }))
        .expect("run.started is written");
    store
        .append(
            &run_id,
            Payload::RunStopped(RunStopped {
                cause: JournalStopCause::Completed,
                input_id: None,
            }),
        )
        .expect("the first stop is written");

    let error = store
        .append(
            &run_id,
            Payload::RunStopped(RunStopped {
                cause: JournalStopCause::ProviderError,
                input_id: None,
            }),
        )
        .expect_err("a run is stopped once");
    assert!(error.to_string().contains("stopped twice"), "{error}");
    assert_eq!(
        journal_stop_cause(session_id, &run_id),
        Some(JournalStopCause::Completed),
        "the refused row never reaches the file, so the first cause stands"
    );
    let stops = journal_rows(session_id)
        .into_iter()
        .filter(|(kind, id)| *kind == RecordKind::RunStopped && *id == run_id)
        .count();
    assert_eq!(stops, 1);
}

#[tokio::test]
async fn run_stop_overlap_four_crash_states_recover_alike() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let root = journal_root();
    let states = ["both", "stop_only", "close_only", "neither"];

    for state in states {
        let session_id = format!("ses_overlap_{state}");
        let mut session = Session::create_with_id(session_id.clone(), None, Some("crash state".to_string()));
        session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: "recover me".to_string(),
                cache_control: None,
            }],
        );
        if state == "both" || state == "close_only" {
            session.mark_closed();
        }
        session.save().expect("the session binding is saved");
        if state == "both" || state == "stop_only" {
            let run_id = Agent::mint_run_id();
            let mut store = DurableRuntimeStore::open_in(&root, &session_id).expect("the journal opens");
            store
                .append(&run_id, Payload::RunStarted(RunStarted { input_id: None }))
                .expect("run.started is written");
            store
                .append(
                    &run_id,
                    Payload::RunStopped(RunStopped {
                        cause: JournalStopCause::Completed,
                        input_id: None,
                    }),
                )
                .expect("run.stopped is written");
        }
        // A dead owner is what the crash sweep keys on, whatever the two writes
        // did or did not record.
        crate::storage::register_active_pid(&session_id, 999_999);
    }

    let status = |state: &str| {
        Session::load(&format!("ses_overlap_{state}"))
            .expect("the session loads")
            .status
    };
    assert_eq!(status("both"), SessionStatus::Closed);
    assert_eq!(status("close_only"), SessionStatus::Closed);
    assert_eq!(
        status("stop_only"),
        SessionStatus::Active,
        "a stop row alone does not close a session"
    );
    assert_eq!(status("neither"), SessionStatus::Active);

    for state in ["both", "stop_only"] {
        let recorded = format!("ses_overlap_{state}");
        let summary = summary_of(&recorded).await;
        assert_eq!(
            summary.last_stop.map(|stop| stop.cause),
            Some("completed".to_string()),
            "{state} reports the stop it recorded"
        );
    }
    for state in ["close_only", "neither"] {
        let recorded = format!("ses_overlap_{state}");
        let summary = summary_of(&recorded).await;
        assert!(summary.last_stop.is_none(), "{state} has no stop to report");
    }
    let close_only = summary_of("ses_overlap_close_only").await;
    let neither = summary_of("ses_overlap_neither").await;
    assert_eq!(
        close_only, neither,
        "a binding with no stop reads exactly like neither write landing"
    );

    let crashed: Vec<String> = find_recent_crashed_sessions()
        .into_iter()
        .map(|(session_id, _)| session_id)
        .collect();
    for state in states {
        let session_id = format!("ses_overlap_{state}");
        assert!(
            crashed.contains(&session_id),
            "{session_id} was owned by a dead pid: {crashed:?}"
        );
        assert_eq!(
            Session::load(&session_id).expect("the session loads").status,
            SessionStatus::Closed,
            "{session_id} recovers to the same closed state"
        );
    }
    assert_eq!(
        status("close_only"),
        status("neither"),
        "after recovery the binding-only crash is indistinguishable from an unconfirmed one"
    );
}
