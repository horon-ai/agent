//! The overnight handoff-ready pass: it compacts once, and a failure is a log
//! line rather than the end of the run.

use super::*;
use chrono::{Duration as ChronoDuration, Utc};
use std::cell::Cell;

fn test_manifest(root: &std::path::Path, run_id: &str) -> OvernightManifest {
    let run_dir = root.join("run");
    let now = Utc::now();
    OvernightManifest {
        version: OVERNIGHT_VERSION,
        run_id: run_id.to_string(),
        parent_session_id: "parent".to_string(),
        coordinator_session_id: "coord".to_string(),
        coordinator_session_name: "coordinator".to_string(),
        started_at: now,
        target_wake_at: now + ChronoDuration::hours(7),
        handoff_ready_at: now + ChronoDuration::hours(6),
        post_wake_grace_until: now + ChronoDuration::hours(9),
        morning_report_posted_at: None,
        completed_at: None,
        cancel_requested_at: None,
        status: OvernightRunStatus::Running,
        mission: Some("verify things".to_string()),
        working_dir: Some("/tmp/project".to_string()),
        provider_name: "test-provider".to_string(),
        model: "test-model".to_string(),
        max_agents_guidance: 2,
        process_id: 123,
        run_dir: run_dir.clone(),
        events_path: run_dir.join("events.jsonl"),
        human_log_path: run_dir.join("run.log"),
        review_path: run_dir.join("review.html"),
        review_notes_path: run_dir.join("review-notes.md"),
        preflight_path: run_dir.join("preflight.json"),
        task_cards_dir: run_dir.join("task-cards"),
        issue_drafts_dir: run_dir.join("issue-drafts"),
        validation_dir: run_dir.join("validation"),
        last_activity_at: now,
    }
}

fn events(manifest: &OvernightManifest) -> String {
    std::fs::read_to_string(&manifest.events_path).unwrap_or_default()
}

#[test]
fn overnight_handoff_pass_compacts_once_inside_the_window() {
    let temp = tempfile::tempdir().expect("tempdir");
    let manifest = test_manifest(temp.path(), "overnight_handoff_once");
    let calls = Cell::new(0);
    let now = manifest.handoff_ready_at + ChronoDuration::minutes(1);

    let first = ready_pass(&manifest, false, now, || {
        calls.set(calls.get() + 1);
        HandoffOutcome::Committed { covered: 3 }
    })
    .expect("the pass must not fail the run");
    assert!(first.is_some(), "the pass must send the handoff-ready prompt");
    assert_eq!(calls.get(), 1);

    let second = ready_pass(&manifest, true, now, || {
        calls.set(calls.get() + 1);
        HandoffOutcome::Committed { covered: 3 }
    })
    .expect("an already-noticed pass is not an error");
    assert!(second.is_none());
    assert_eq!(calls.get(), 1, "the compaction must not run twice");
    assert!(events(&manifest).contains("handoff_compaction"));
}

#[test]
fn overnight_handoff_provider_error_is_logged_and_the_run_continues() {
    let temp = tempfile::tempdir().expect("tempdir");
    let manifest = test_manifest(temp.path(), "overnight_handoff_error");
    let now = manifest.handoff_ready_at + ChronoDuration::minutes(1);

    let prompt = ready_pass(&manifest, false, now, || {
        HandoffOutcome::ProviderError("quota exceeded".to_string())
    })
    .expect("a provider error must not fail the run");
    assert!(prompt.is_some());

    let log = events(&manifest);
    assert!(log.contains("provider_error"));
    assert!(log.contains("quota exceeded"));
}

#[test]
fn overnight_handoff_before_the_window_leaves_the_transcript_alone() {
    let temp = tempfile::tempdir().expect("tempdir");
    let manifest = test_manifest(temp.path(), "overnight_handoff_early");
    let calls = Cell::new(0);

    let result = ready_pass(&manifest, false, manifest.handoff_ready_at, || {
        calls.set(calls.get() + 1);
        HandoffOutcome::Committed { covered: 1 }
    })
    .expect("not being due is not an error");
    assert!(result.is_none());
    assert_eq!(calls.get(), 0);
}
