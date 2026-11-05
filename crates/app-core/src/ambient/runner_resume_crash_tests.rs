//! Crash matrix: a child process dies at a resume failpoint and the next
//! process must continue the run exactly once.
//!
//! Unix-only as a whole: the child is killed with `kill -9` and its exit status
//! is inspected through `ExitStatusExt::signal`, neither of which exists on
//! Windows.
#![cfg(unix)]

use super::super::persistence::{ResumeClaimState, ResumeClaimStore};
use super::{
    BlockedApproval, CountingProvider, TestHome, closed_session, record, resume_marker_events, resume_turns, runner,
    store,
};
use crate::provider::Provider;
use chrono::Utc;
use std::os::unix::process::ExitStatusExt;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Crash matrix: (a) claimed, (b) resuming, (d) append, (f) exec-claimed, (e) inline
// ---------------------------------------------------------------------------

/// The child process body: run one resume pass and die at the named failpoint.
///
/// Inert unless the parent sets the failpoint environment, so a normal test run
/// of this module never kills itself.
#[tokio::test]
async fn crash_child_resume() {
    let Ok(failpoint) = std::env::var("HORON_AGENT_RESUME_FAILPOINT") else {
        return;
    };
    let Ok(request_id) = std::env::var("HORON_AGENT_RESUME_REQUEST") else {
        return;
    };
    let _ = failpoint;

    let runner = runner();
    let provider: Arc<dyn Provider> = Arc::new(CountingProvider::new());
    runner.resume_blocked_approvals(&provider).await;
    panic!("the failpoint for {request_id} did not fire");
}

struct CrashCase {
    /// Name passed to `HORON_AGENT_RESUME_FAILPOINT`.
    failpoint: &'static str,
    /// Record shape the crashed process starts from.
    seed: fn(&str, &str) -> BlockedApproval,
}

fn seeded_decided(session_id: &str, request_id: &str) -> BlockedApproval {
    record(session_id, request_id)
}

fn seeded_inline(session_id: &str, request_id: &str) -> BlockedApproval {
    let mut record = record(session_id, request_id);
    record.inline_at = Some(Utc::now());
    record.inline_approved = Some(true);
    record.outcome = Some("inline".to_string());
    record
}

/// Run one crash case: the child dies at `failpoint`, then this process
/// recovers and the session must end with exactly one resume turn.
fn assert_crash_recovers_with_one_turn(case: CrashCase) {
    let home = TestHome::new();
    let session_id = format!("sess_crash_{}", case.failpoint.replace('-', "_"));
    let request_id = format!("req_crash_{}", case.failpoint.replace('-', "_"));
    closed_session(&session_id);
    store().add((case.seed)(&session_id, &request_id)).expect("add record");

    let runner = runner();
    if case.failpoint != "inline" {
        runner
            .safety()
            .record_decision(&request_id, true, "file", None)
            .expect("record approval");
    }

    // The child owns the crash: it runs the same recovery path and is killed at
    // the injected point, leaving exactly the on-disk state a `kill -9` would.
    let status = std::process::Command::new(std::env::current_exe().expect("current exe"))
        .args([
            "--exact",
            "ambient::resume_tests::crash_tests::crash_child_resume",
            "--nocapture",
        ])
        .env("HORON_AGENT_HOME", home.path())
        .env("HORON_AGENT_RESUME_FAILPOINT", case.failpoint)
        .env("HORON_AGENT_RESUME_REQUEST", &request_id)
        // A dead owner's execution lease must be reclaimable without waiting out
        // the production window.
        .env("HORON_AGENT_RESUME_EXEC_LEASE_SECS", "0")
        .status()
        .expect("spawn the crashing child");

    assert!(
        status.signal() == Some(9),
        "the child must be killed at the {} failpoint, got {status:?} (a panic would exit 101)",
        case.failpoint
    );

    // Recovery: a fresh pass over the state the crash left behind.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let provider: Arc<dyn Provider> = Arc::new(CountingProvider::new());
    runtime.block_on(runner.resume_blocked_approvals(&provider));
    // A second pass must not add a turn.
    runtime.block_on(runner.resume_blocked_approvals(&provider));

    assert_eq!(
        resume_turns(&session_id),
        1,
        "after a crash at {} the run must be continued exactly once",
        case.failpoint
    );
    assert_eq!(
        resume_marker_events(&session_id, &request_id),
        1,
        "after a crash at {} exactly one resume turn may exist",
        case.failpoint
    );
    let stored = store().get(&request_id).unwrap().expect("record present");
    assert_eq!(
        stored.outcome.as_deref(),
        Some("resumed"),
        "the recovered record must be closed as resumed after a crash at {}",
        case.failpoint
    );

    let claim = ResumeClaimStore::open_default()
        .expect("claim store")
        .get(&request_id)
        .expect("claim")
        .expect("claim present");
    println!(
        "crash={} claim_state={:?} attempts={} marker_events={} resume_turns={} outcome={:?}",
        case.failpoint,
        claim.state,
        claim.attempts,
        resume_marker_events(&session_id, &request_id),
        resume_turns(&session_id),
        stored.outcome,
    );
    assert_eq!(
        claim.state,
        ResumeClaimState::Done,
        "the claim must be closed as done after the recovery"
    );
}

#[test]
fn crash_after_claim_recovers_with_one_turn() {
    assert_crash_recovers_with_one_turn(CrashCase {
        failpoint: "claimed",
        seed: seeded_decided,
    });
}

#[test]
fn crash_after_resuming_recovers_with_one_turn() {
    assert_crash_recovers_with_one_turn(CrashCase {
        failpoint: "resuming",
        seed: seeded_decided,
    });
}

#[test]
fn crash_after_the_key_is_durable_recovers_with_one_turn() {
    assert_crash_recovers_with_one_turn(CrashCase {
        failpoint: "append",
        seed: seeded_decided,
    });
}

#[test]
fn crash_after_the_exec_claim_recovers_with_one_turn() {
    assert_crash_recovers_with_one_turn(CrashCase {
        failpoint: "exec-claimed",
        seed: seeded_decided,
    });
}

#[test]
fn crash_after_an_inline_approval_recovers_with_one_turn() {
    assert_crash_recovers_with_one_turn(CrashCase {
        failpoint: "inline",
        seed: seeded_inline,
    });
}
