//! The operator todo view: the complete list, its explicit keys, and the
//! revision every edit carries.
//!
//! Every test drives the app the way the bridge and the keyboard do, so the
//! behavior under test is what a user reaches, not a helper the view happens to
//! call.

use std::sync::mpsc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use horon_agent_sdk::todo::{TodoEditRefusal, TodoItem};

use crate::app::{Action, App};
use crate::commands::Local;
use crate::overlays::picker::Kind;
use crate::sdk_bridge::{AttachedSession, Incoming};
use crate::todo_panel::TodoStatusRequest;

fn app() -> App {
    let (_sender, receiver) = mpsc::channel::<Incoming>();
    let mut app = App::new("socket · session".to_string(), receiver);
    app.session_id = Some("s1".to_string());
    app
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn item(id: &str, content: &str, status: &str) -> TodoItem {
    TodoItem {
        id: id.to_string(),
        content: content.to_string(),
        status: status.to_string(),
        priority: "high".to_string(),
        ..TodoItem::default()
    }
}

/// The list the harness would send, applied the way the loop applies it.
fn list(app: &mut App, items: &[TodoItem], revision: Option<&str>) {
    let items = items.to_vec();
    assert!(app.set_todos("s1", items, revision.map(str::to_string)));
}

/// The last notice or warning on screen.
fn last_line(app: &App) -> String {
    match app.transcript.items().last() {
        Some(crate::transcript::Item::Notice(text)) | Some(crate::transcript::Item::Warning(text)) => text.clone(),
        other => format!("{other:?}"),
    }
}

fn open(app: &mut App) {
    assert!(app.run_local(Local::Todo, "").is_none());
    assert_eq!(app.picker.as_ref().map(|picker| picker.kind()), Some(Kind::Todos));
}

#[test]
fn the_view_lists_every_item_and_an_edit_carries_the_revision_sent() {
    let mut app = app();
    list(
        &mut app,
        &[
            item("t1", "ship the release", "pending"),
            item("t2", "review the diff", "completed"),
            item("t3", "drop the old path", "cancelled"),
        ],
        Some("rev-1"),
    );
    open(&mut app);
    assert_eq!(app.picker.as_ref().map(|picker| picker.len()), Some(3));

    let action = app.todo_panel_key(key(KeyCode::Enter));
    assert_eq!(
        action,
        Some(Action::TodoStatus(TodoStatusRequest {
            session_id: "s1".to_string(),
            revision: "rev-1".to_string(),
            id: "t1".to_string(),
            status: "completed".to_string(),
        }))
    );

    // A second edit before the answer would carry the same revision and be
    // refused after the first lands, so it is not sent at all.
    assert!(app.todo_panel_key(key(KeyCode::Enter)).is_none());
    assert!(
        last_line(&app).contains("waiting for the previous change"),
        "{}",
        last_line(&app)
    );
}

#[test]
fn enter_reopens_a_cancelled_item_and_delete_cancels_an_open_one() {
    let mut initial = app();
    list(
        &mut initial,
        &[item("t1", "ship", "cancelled"), item("t2", "review", "in_progress")],
        Some("rev-1"),
    );
    open(&mut initial);
    let reopened = initial.todo_panel_key(key(KeyCode::Enter));
    assert!(matches!(
        reopened,
        Some(Action::TodoStatus(request)) if request.id == "t1" && request.status == "pending"
    ));

    let mut restart = app();
    list(&mut restart, &[item("t1", "ship", "pending")], Some("rev-1"));
    open(&mut restart);
    let cancelled = restart.todo_panel_key(key(KeyCode::Delete));
    assert!(matches!(
        cancelled,
        Some(Action::TodoStatus(request)) if request.id == "t1" && request.status == "cancelled"
    ));
}

#[test]
fn a_stale_answer_refreshes_the_list_and_the_next_edit_uses_the_new_revision() {
    let mut app = app();
    list(&mut app, &[item("t1", "ship", "pending")], Some("rev-1"));
    open(&mut app);
    assert!(app.todo_panel_key(key(KeyCode::Enter)).is_some());

    // The list moved while the edit was in flight: the answer refuses the
    // write, carries the current list and revision, and the next press is made
    // against that revision.
    assert!(app.todo_edit_reply(
        "s1",
        false,
        Some(TodoEditRefusal::StaleRevision),
        vec![item("t1", "ship", "in_progress")],
        "rev-2".to_string(),
        ("t1", "completed"),
    ));
    assert!(last_line(&app).contains("changed on the server"), "{}", last_line(&app));
    assert_eq!(app.picker.as_ref().map(|picker| picker.len()), Some(1));
    let action = app.todo_panel_key(key(KeyCode::Enter));
    assert_eq!(
        action,
        Some(Action::TodoStatus(TodoStatusRequest {
            session_id: "s1".to_string(),
            revision: "rev-2".to_string(),
            id: "t1".to_string(),
            status: "completed".to_string(),
        }))
    );
}

#[test]
fn a_list_without_a_revision_is_read_only() {
    let mut app = app();
    list(&mut app, &[item("t1", "ship", "pending")], None);
    open(&mut app);
    assert!(app.todo_panel_key(key(KeyCode::Enter)).is_none());
    assert!(last_line(&app).contains("read-only"), "{}", last_line(&app));
}

#[test]
fn a_session_switch_closes_the_view_rather_than_editing_another_list() {
    let mut app = app();
    list(&mut app, &[item("t1", "ship", "pending")], Some("rev-1"));
    open(&mut app);
    app.attach_session(
        AttachedSession {
            description: "socket · session_b".to_string(),
            session_id: "s2".to_string(),
            status: "idle".to_string(),
            model: None,
            effort: None,
            history: Vec::new(),
            title: None,
        },
        false,
    );
    assert!(app.picker.is_none());
    assert_eq!(app.session_id.as_deref(), Some("s2"));
    assert!(app.todo_panel_key(key(KeyCode::Enter)).is_none());
}

#[test]
fn a_reply_for_another_session_changes_nothing() {
    let mut app = app();
    list(&mut app, &[item("t1", "ship", "pending")], Some("rev-1"));
    open(&mut app);
    assert!(!app.todo_edit_reply(
        "s2",
        true,
        None,
        vec![item("t1", "ship", "completed")],
        "rev-2".to_string(),
        ("t1", "completed"),
    ));
    assert_eq!(app.todos.items()[0].status, "pending");
}
