//! `/pin`, `/unpin` and the session list's pin key.
//!
//! These drive the client the way a reader does: type the command or press the
//! key in the open list, then read the order and the marker the list shows. The
//! file is a real one under a temporary directory, so a second app over the
//! same path is the restart the pin has to survive, and a path made
//! unwritable is the failure the screen must not claim.

use std::fs;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{Action, App};
use crate::sdk_bridge::SessionRow;
use crate::session_pins::PinStore;
use crate::transcript::Item;

fn app() -> App {
    let (_sender, receiver) = std::sync::mpsc::channel();
    App::new("socket · session_new".to_string(), receiver)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl_p() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)
}

/// Type a whole line and press Enter, as a user would.
fn submit(app: &mut App, line: &str) -> Option<Action> {
    app.input.set_text(line);
    app.on_key(key(KeyCode::Enter))
}

/// One stored session as the bridge answers with it, newest first in `rows`.
fn row(id: &str, label: &str, detail: &str) -> SessionRow {
    SessionRow {
        id: id.to_string(),
        label: label.to_string(),
        detail: detail.to_string(),
        current: false,
        preview: vec![format!("/work/project · {detail}"), format!("you: {label}")],
    }
}

fn rows() -> Vec<SessionRow> {
    vec![
        row("session_new", "newest", "11-21 12:00"),
        row("session_mid", "middle", "11-21 11:00"),
        row("session_old", "oldest", "11-21 10:00"),
    ]
}

/// Open the session list and fill it with the newest-first answer.
fn open_list(app: &mut App) {
    assert_eq!(submit(app, "/switch"), Some(Action::Sessions));
    app.show_sessions(rows());
}

/// Close the list, so the next line typed is a command for the client.
fn close_list(app: &mut App) {
    app.picker = None;
}

/// The ids of the rows the open list shows, in display order.
fn shown(app: &App) -> Vec<String> {
    app.picker
        .as_ref()
        .expect("the session list is not open")
        .visible()
        .iter()
        .map(|row| row.value.clone())
        .collect()
}

/// The notices and warnings on screen, from the oldest one forward.
fn said(app: &App) -> Vec<String> {
    app.transcript
        .items()
        .iter()
        .map(|item| match item {
            Item::Notice(text) | Item::Warning(text) => text.clone(),
            Item::User(text) => format!("user:{text}"),
            other => format!("other:{other:?}"),
        })
        .collect()
}

fn selected(app: &App) -> (String, bool) {
    let row = app
        .picker
        .as_ref()
        .expect("the session list is not open")
        .selected_row()
        .expect("no row is selected");
    (row.value.clone(), row.pinned)
}

#[test]
fn the_pin_key_moves_a_session_to_the_top_and_keeps_each_group_in_recent_order() {
    let mut app = app();
    open_list(&mut app);

    // Walk to the oldest row and pin it: it leaves the recent group without
    // disturbing the order of the two sessions that stay behind it.
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    app.on_key(ctrl_p());
    assert_eq!(shown(&app), vec!["session_old", "session_new", "session_mid"]);
    assert_eq!(selected(&app), ("session_old".to_string(), true));
    assert!(
        said(&app).iter().any(|line| line == "pinned oldest"),
        "the pin was not confirmed: {:?}",
        said(&app)
    );

    // Pinning a newer session puts it above the older pin: the pinned group
    // keeps the same recent order the whole list had.
    app.on_key(key(KeyCode::Down));
    app.on_key(ctrl_p());
    assert_eq!(shown(&app), vec!["session_new", "session_old", "session_mid"]);
    assert_eq!(selected(&app), ("session_new".to_string(), true));

    // The same key takes a pin back, and the session returns to its place in
    // the recent order.
    app.on_key(ctrl_p());
    assert_eq!(shown(&app), vec!["session_old", "session_new", "session_mid"]);
    assert_eq!(selected(&app), ("session_new".to_string(), false));
}

#[test]
fn a_pin_survives_a_restart_and_still_leads_the_list() {
    let directory = tempfile::tempdir().expect("no temporary directory");
    let path = directory.path().join("tui-pins.json");

    let mut first = app();
    first.pins.store = PinStore::at(&path, "/run/one.sock");
    open_list(&mut first);
    close_list(&mut first);
    assert_eq!(submit(&mut first, "/pin session_mid"), None);
    open_list(&mut first);
    assert_eq!(shown(&first), vec!["session_mid", "session_new", "session_old"]);

    // The second client is the next launch: same file and endpoint, and an
    // answer whose recent order changed since the pin was taken.
    let mut second = app();
    second.pins.store = PinStore::at(&path, "/run/one.sock");
    assert_eq!(submit(&mut second, "/switch"), Some(Action::Sessions));
    second.show_sessions(vec![
        row("session_old", "oldest", "11-21 13:00"),
        row("session_new", "newest", "11-21 12:30"),
        row("session_mid", "middle", "11-21 12:00"),
    ]);
    assert_eq!(
        shown(&second),
        vec!["session_mid", "session_old", "session_new"],
        "the pin did not survive the restart, or the recent group lost its order"
    );
    assert_eq!(selected(&second), ("session_mid".to_string(), true));
}

#[test]
fn a_pin_belongs_to_the_endpoint_that_stored_it() {
    let directory = tempfile::tempdir().expect("no temporary directory");
    let path = directory.path().join("tui-pins.json");

    let mut local = app();
    local.pins.store = PinStore::at(&path, "/run/local.sock");
    open_list(&mut local);
    close_list(&mut local);
    assert_eq!(submit(&mut local, "/pin session_old"), None);

    // Another daemon can issue the same ids. Its list starts unpinned, and
    // pinning there must not touch the first endpoint's list.
    let mut remote = app();
    remote.pins.store = PinStore::at(&path, "/run/remote.sock");
    open_list(&mut remote);
    assert_eq!(
        shown(&remote),
        vec!["session_new", "session_mid", "session_old"],
        "a pin from another endpoint reordered this list"
    );
    close_list(&mut remote);
    assert_eq!(submit(&mut remote, "/pin session_mid"), None);
    open_list(&mut remote);
    assert_eq!(shown(&remote), vec!["session_mid", "session_new", "session_old"]);

    // The first endpoint's list is exactly as it was left.
    let mut again = app();
    again.pins.store = PinStore::at(&path, "/run/local.sock");
    open_list(&mut again);
    assert_eq!(
        shown(&again),
        vec!["session_old", "session_new", "session_mid"],
        "the other endpoint's pin leaked into this list"
    );
}

#[test]
fn a_pin_for_a_session_the_list_no_longer_has_does_not_break_it() {
    let mut app = app();
    app.pins
        .store
        .set_pinned("session_gone", true)
        .expect("an in-memory pin cannot fail");
    open_list(&mut app);

    assert_eq!(shown(&app), vec!["session_new", "session_mid", "session_old"]);
    assert!(
        app.picker.as_ref().is_some_and(|picker| !picker.is_empty()),
        "an unknown pinned id hid the list"
    );
    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        Some(Action::Attach("session_new".to_string()))
    );
}

#[test]
fn a_pin_that_cannot_be_written_leaves_the_list_and_the_marker_unchanged() {
    let root = tempfile::tempdir().expect("no temporary directory");
    let state = root.path().join("state");
    fs::create_dir(&state).expect("no state directory");
    let path = state.join("tui-pins.json");

    let mut app = app();
    app.pins.store = PinStore::at(&path, "/run/one.sock");
    open_list(&mut app);

    // A parent the save cannot create is the write failing after the list was
    // read: the pin must stay untaken rather than appear on screen.
    fs::remove_dir(&state).expect("the state directory is not empty");
    fs::write(&state, b"not a directory").expect("no blocking file");

    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    app.on_key(ctrl_p());
    assert_eq!(shown(&app), vec!["session_new", "session_mid", "session_old"]);
    assert_eq!(selected(&app), ("session_old".to_string(), false));
    assert!(
        said(&app)
            .iter()
            .any(|line| line.contains("pins:") && line.contains("unchanged")),
        "the failed save was not reported: {:?}",
        said(&app)
    );
    assert!(
        !said(&app).iter().any(|line| line.starts_with("pinned ")),
        "a pin the file refused was announced as taken: {:?}",
        said(&app)
    );
}

#[test]
fn pin_and_unpin_without_a_name_act_on_the_attached_session() {
    let mut app = app();
    app.session_id = Some("session_mid".to_string());
    open_list(&mut app);
    close_list(&mut app);

    assert_eq!(submit(&mut app, "/pin"), None);
    open_list(&mut app);
    assert_eq!(shown(&app), vec!["session_mid", "session_new", "session_old"]);
    assert!(
        said(&app).iter().any(|line| line == "pinned middle"),
        "the command did not name the session it pinned: {:?}",
        said(&app)
    );

    close_list(&mut app);
    assert_eq!(submit(&mut app, "/unpin"), None);
    open_list(&mut app);
    assert_eq!(shown(&app), vec!["session_new", "session_mid", "session_old"]);

    close_list(&mut app);
    app.session_id = None;
    assert_eq!(submit(&mut app, "/pin"), None);
    assert!(
        said(&app).iter().any(|line| line.contains("no session is attached")),
        "pinning without a session said nothing: {:?}",
        said(&app)
    );
}

#[test]
fn a_pin_one_client_took_survives_a_pin_another_takes() {
    let directory = tempfile::tempdir().expect("no temporary directory");
    let path = directory.path().join("tui-pins.json");

    // Two clients start from the same empty file, as two launches would.
    let mut first = app();
    first.pins.store = PinStore::at(&path, "/run/one.sock");
    open_list(&mut first);
    let mut second = app();
    second.pins.store = PinStore::at(&path, "/run/one.sock");
    open_list(&mut second);

    close_list(&mut first);
    assert_eq!(submit(&mut first, "/pin session_old"), None);
    close_list(&mut second);
    assert_eq!(submit(&mut second, "/pin session_mid"), None);

    // The second save must not have replaced the file with only its own pin.
    let mut third = app();
    third.pins.store = PinStore::at(&path, "/run/one.sock");
    open_list(&mut third);
    assert_eq!(
        shown(&third),
        vec!["session_mid", "session_old", "session_new"],
        "a save from another client dropped a pin"
    );
}

#[test]
fn the_pin_key_does_nothing_in_a_list_without_pins() {
    let mut app = app();
    assert_eq!(app.open_models(), Some(Action::Models));
    app.show_models(vec!["alpha".to_string()], None);

    assert_eq!(app.on_key(ctrl_p()), None);
    let picker = app.picker.as_ref().expect("ctrl-p closed the model list");
    assert_eq!(picker.filter(), "", "ctrl-p typed into the model filter");
    assert_eq!(picker.selected_label(), Some("alpha"));
}
