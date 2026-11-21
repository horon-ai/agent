//! `/jobs` and the one key that acts on a row: the list a session reports, the
//! terminal a cancellation names, and the rows that survive it.
//!
//! Every test drives a real [`App`] the way the interface does, so the command,
//! the key routing and the refreshed list are exercised together. No harness is
//! involved: the bridge's half is pinned beside it in `sdk_bridge::jobs_tests`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{Action, App};
use crate::overlays::picker::Kind;
use crate::sdk_bridge::{AttachedSession, BackgroundJobRow};
use crate::transcript::Item;

fn app() -> App {
    let (_sender, receiver) = std::sync::mpsc::channel();
    App::new("socket · session_a".to_string(), receiver)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Type a whole line and press Enter, as a user would.
fn submit(app: &mut App, line: &str) -> Option<Action> {
    app.input.set_text(line);
    app.on_key(key(KeyCode::Enter))
}

/// One row as the bridge would build it.
fn job(call_id: &str, kind: &str) -> BackgroundJobRow {
    BackgroundJobRow {
        call_id: call_id.to_string(),
        task_id: format!("task-{call_id}"),
        kind: kind.to_string(),
        command: Some(format!("sleep 30 # {call_id}")),
        cwd: Some("/work".to_string()),
        output: vec!["waiting".to_string()],
    }
}

/// The last line a command left in the transcript.
fn last_line(app: &App) -> String {
    match app.transcript.items().last() {
        Some(Item::Notice(text)) | Some(Item::Warning(text)) => text.clone(),
        other => panic!("expected a notice or a warning, got {other:?}"),
    }
}

fn open(app: &mut App, jobs: Vec<BackgroundJobRow>) {
    assert_eq!(submit(app, "/jobs"), Some(Action::BackgroundJobs));
    app.show_jobs(jobs);
}

#[test]
fn jobs_asks_for_a_live_list_rather_than_the_transcript() {
    let mut app = app();
    assert_eq!(submit(&mut app, "/jobs"), Some(Action::BackgroundJobs));
    let picker = app.picker.as_ref().expect("/jobs opened no list");
    assert_eq!(picker.kind(), Kind::Jobs);
    assert_eq!(picker.title(), "Background jobs");
    assert!(picker.is_loading(), "a list nobody asked the daemon for is drawn");
}

#[test]
fn delete_cancels_the_selected_terminal_and_nothing_else() {
    let mut app = app();
    open(&mut app, vec![job("call-1", "terminal"), job("call-2", "terminal")]);
    assert_eq!(app.on_key(key(KeyCode::Down)), None);
    assert_eq!(
        app.on_key(key(KeyCode::Delete)),
        Some(Action::CancelBackgroundTerminal("call-2".to_string()))
    );
    // The request names one job; the session and the other row stay as they
    // were until the daemon answers.
    assert_eq!(app.background_jobs.len(), 2);
    assert!(!app.pending_turn);
}

#[test]
fn enter_changes_nothing_and_a_tool_call_cannot_be_cancelled_here() {
    let mut app = app();
    open(&mut app, vec![job("call-3", "tool_call")]);
    assert_eq!(app.on_key(key(KeyCode::Enter)), None);
    assert!(app.picker.is_some(), "enter closed the list");
    assert_eq!(app.on_key(key(KeyCode::Delete)), None);
    assert!(
        last_line(&app).contains("only a background terminal can be cancelled"),
        "the refusal does not say why: {}",
        last_line(&app)
    );
    assert_eq!(app.background_jobs.len(), 1, "a refused cancellation dropped a row");
}

#[test]
fn an_accepted_cancellation_refreshes_only_the_job_that_left() {
    let mut app = app();
    open(&mut app, vec![job("call-1", "terminal"), job("call-2", "terminal")]);
    app.job_cancelled("call-1", true, None, Ok(vec![job("call-2", "terminal")]));
    assert_eq!(app.background_jobs.len(), 1);
    assert_eq!(app.background_jobs[0].call_id, "call-2");
    let picker = app.picker.as_ref().expect("the refreshed list closed");
    assert_eq!(picker.visible().len(), 1);
    assert!(
        last_line(&app).contains("cancelling background job call-1"),
        "the verdict is not drawn: {}",
        last_line(&app)
    );
}

#[test]
fn an_unreadable_refresh_keeps_the_rows_and_says_why_they_are_stale() {
    let mut app = app();
    open(&mut app, vec![job("call-1", "terminal")]);
    app.job_cancelled(
        "call-1",
        true,
        None,
        Err(horon_agent_sdk::Error::new(
            horon_agent_sdk::ErrorKind::Disconnected,
            "the harness connection closed",
        )),
    );
    // The verdict and the failure are both useful: the row stays because the
    // client could not learn whether the stop landed, and the sentence says so.
    assert_eq!(app.background_jobs.len(), 1);
    let line = last_line(&app);
    assert!(
        line.contains("could not refresh the background jobs") && line.contains("the harness connection closed"),
        "the refresh failure was swallowed: {line}"
    );
}

#[test]
fn a_session_switch_takes_the_jobs_with_it() {
    let mut app = app();
    open(&mut app, vec![job("call-1", "terminal")]);
    app.attach_session(
        AttachedSession {
            description: "socket · session_b".to_string(),
            session_id: "session_b".to_string(),
            status: "idle".to_string(),
            model: None,
            effort: None,
            history: Vec::new(),
            title: None,
        },
        false,
    );
    assert!(app.background_jobs.is_empty(), "a call id followed the client");
    assert!(app.picker.is_none(), "a job list survived the session it named");
}
