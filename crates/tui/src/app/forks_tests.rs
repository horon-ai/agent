//! `/fork` in the client: the list it opens, the boundary a choice carries, the
//! origin a branch records, and the row that returns to it.
//!
//! These drive a real [`App`] the way the interface does, so a pass means the
//! command reached the bridge with a stored message id, a refusal left the
//! session in force alone, and the way back is an ordinary attach.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::forks::ForkOrigin;
use super::{Action, App};
use crate::overlays::picker::Kind;
use crate::sdk_bridge::{AttachedSession, ForkRow, Incoming};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Type a whole line and press Enter, as a user would.
fn submit(app: &mut App, line: &str) -> Option<Action> {
    app.input.set_text(line);
    app.on_key(key(KeyCode::Enter))
}

fn attached(session_id: &str, title: Option<&str>) -> AttachedSession {
    AttachedSession {
        description: format!("socket · {session_id}"),
        session_id: session_id.to_string(),
        status: "idle".to_string(),
        model: None,
        effort: None,
        history: Vec::new(),
        title: title.map(str::to_string),
    }
}

fn fork_row(boundary: &str, label: &str) -> ForkRow {
    ForkRow {
        boundary: boundary.to_string(),
        label: label.to_string(),
        detail: "#1".to_string(),
        preview: vec![
            "fork before this recorded message".to_string(),
            format!("you: {label}"),
            "the branch keeps the 1 stored messages before it".to_string(),
            "the original session keeps every message".to_string(),
        ],
    }
}

fn app() -> (App, std::sync::mpsc::Sender<Incoming>) {
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut app = App::new("socket · session_a".to_string(), receiver);
    app.session_id = Some("session_a".to_string());
    app.session_title = Some("Original work".to_string());
    (app, sender)
}

#[test]
fn fork_without_an_argument_opens_the_boundary_list() {
    let (mut app, _sender) = app();
    assert_eq!(submit(&mut app, "/fork"), Some(Action::Forks));
    let picker = app.picker.as_ref().expect("/fork opened no list");
    assert_eq!(picker.kind(), Kind::Forks);
    assert!(picker.is_loading(), "the list did not say it was waiting");

    app.show_forks(vec![fork_row("m2", "the second question")]);
    let picker = app.picker.as_ref().expect("the list went away");
    assert_eq!(picker.len(), 1);
    assert_eq!(picker.selected_label(), Some("the second question"));
}

#[test]
fn branch_is_the_alias_for_fork() {
    let (mut app, _sender) = app();
    assert_eq!(submit(&mut app, "/branch"), Some(Action::Forks));
    assert!(matches!(
        app.picker.as_ref().map(|picker| picker.kind()),
        Some(Kind::Forks)
    ));
}

#[test]
fn choosing_a_boundary_sends_its_stored_id_and_records_the_origin() {
    let (mut app, _sender) = app();
    submit(&mut app, "/fork");
    app.show_forks(vec![fork_row("message_7", "the second question")]);
    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        Some(Action::Fork("message_7".to_string()))
    );
    assert!(app.picker.is_none(), "the list stayed open after a choice");

    let origin = app.take_pending_fork().expect("the fork kept no origin");
    assert_eq!(origin.session_id, "session_a");
    assert_eq!(origin.label, "Original work");
}

#[test]
fn a_branch_offers_the_way_back_and_switching_to_it_attaches() {
    let (mut app, sender) = app();
    submit(&mut app, "/fork");
    app.show_forks(vec![fork_row("message_7", "the second question")]);
    app.on_key(key(KeyCode::Enter));

    // The fork answer attaches the child; the handler records the origin it
    // took from the pending choice before the session moved.
    sender
        .send(Incoming::Attached {
            session: Box::new(attached("session_b", None)),
            working_dir: None,
        })
        .unwrap();
    crate::loop_io::drain_events(&mut app);

    assert_eq!(app.session_id.as_deref(), Some("session_b"));
    assert_eq!(
        submit(&mut app, "/fork"),
        Some(Action::Forks),
        "the branch did not reopen the list"
    );
    app.show_forks(vec![fork_row("message_3", "a branch question")]);
    let picker = app.picker.as_ref().expect("the list went away");
    let rows = picker.visible();
    assert_eq!(rows[0].label, "back to Original work", "{rows:?}");
    assert_eq!(rows[1].label, "a branch question");

    // The way back is first, so one enter on it leaves for the original.
    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        Some(Action::Attach("session_a".to_string()))
    );
}

#[test]
fn a_refused_fork_leaves_the_session_and_claims_no_origin() {
    let (mut app, _sender) = app();
    submit(&mut app, "/fork");
    app.show_forks(vec![fork_row("stale_message", "an old question")]);
    app.on_key(key(KeyCode::Enter));
    assert!(app.take_pending_fork().is_some());

    app.request_failed("the boundary is not in this session");
    assert_eq!(app.session_id.as_deref(), Some("session_a"));
    assert!(
        app.take_pending_fork().is_none(),
        "a refused fork kept its origin"
    );
    // No branch origin means the list has no way-back row.
    submit(&mut app, "/fork");
    app.show_forks(vec![fork_row("message_9", "a question")]);
    let picker = app.picker.as_ref().expect("the list went away");
    assert_eq!(picker.len(), 1);
}

#[test]
fn a_fork_waits_for_the_running_turn_like_a_session_switch() {
    let (mut app, _sender) = app();
    app.pending_turn = true;
    assert_eq!(submit(&mut app, "/fork"), None);
    assert!(app.picker.is_none());
    assert_eq!(
        app.transcript.items().last().map(|item| match item {
            crate::transcript::Item::Notice(text) => text.as_str(),
            _ => "",
        }),
        Some("the turn is still running; /fork works once it ends")
    );
}

#[test]
fn an_attach_that_is_not_the_fork_clears_the_way_back() {
    let (mut app, _sender) = app();
    app.fork_origin = Some(ForkOrigin {
        session_id: "session_a".to_string(),
        label: "Original work".to_string(),
    });
    app.session_id = Some("session_b".to_string());
    app.attach_session(attached("session_c", None), false);
    assert!(
        app.fork_origin.is_none(),
        "the way back survived a session the branch did not come from"
    );
}
