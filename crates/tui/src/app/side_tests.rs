//! `/side` as the interface answers it: the command, the overlay's contents, and
//! the two ways out — esc back to the untouched conversation, and enter to the
//! stored child.
//!
//! The bridge is not running here. These tests drive the app with the same
//! deliveries the bridge sends, which is what the overlay actually consumes.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{Action, App};
use crate::commands::Registry;
use crate::transcript::Item;

/// A registry over empty temporary directories, so no real home is read.
fn registry() -> (tempfile::TempDir, tempfile::TempDir, Registry) {
    let user = tempfile::tempdir().expect("a user command directory");
    let project = tempfile::tempdir().expect("a project directory");
    let mut state = Registry::new(user.path().to_path_buf());
    state.sync(Some(project.path()));
    (user, project, state)
}

/// An app attached to a parent session, with its own command registry.
fn app_fixture() -> (tempfile::TempDir, tempfile::TempDir, App) {
    let (user, project, state) = registry();
    let (_sender, receiver) = std::sync::mpsc::channel();
    let mut app = App::new("test".to_string(), receiver);
    app.commands = state;
    app.session_id = Some("session_parent".to_string());
    (user, project, app)
}

/// Type a whole line and press Enter, as a user would.
fn type_line(app: &mut App, text: &str) -> Option<Action> {
    for character in text.chars() {
        let key = KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE);
        assert!(app.on_key(key).is_none(), "typing a character produced an action");
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
}

fn escape() -> KeyEvent {
    KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
}

fn enter() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
}

/// The transcript's rows, as text, so a test can compare it before and after.
fn transcript_text(app: &App) -> Vec<String> {
    app.transcript.items().iter().map(|item| format!("{item:?}")).collect()
}

/// Ask a question and return the client id the deliveries will carry.
fn open_side(app: &mut App, question: &str) -> u64 {
    match type_line(app, &format!("/side {question}")) {
        Some(Action::SideQuestion {
            id, question: asked, ..
        }) => {
            assert_eq!(asked, question, "the question the bridge was given changed");
            id
        }
        other => panic!("`/side` did not ask the bridge: {other:?}"),
    }
}

#[test]
fn side_asks_the_bridge_and_opens_the_screen_without_touching_the_transcript() {
    let (_user, _project, mut app) = app_fixture();
    let before = transcript_text(&app);
    open_side(&mut app, "what did we decide about the cache?");
    assert_eq!(
        app.side.as_ref().map(|side| side.question()),
        Some("what did we decide about the cache?")
    );
    assert_eq!(
        transcript_text(&app),
        before,
        "asking a side question wrote to the parent"
    );
}

#[test]
fn btw_is_the_same_command_under_the_other_name() {
    let (_user, _project, mut app) = app_fixture();
    match type_line(&mut app, "/btw why is it slow?") {
        Some(Action::SideQuestion { question, .. }) => assert_eq!(question, "why is it slow?"),
        other => panic!("`/btw` did not ask the bridge: {other:?}"),
    }
}

#[test]
fn at_selects_an_earlier_stored_boundary_and_keeps_the_question() {
    let (_user, _project, mut app) = app_fixture();
    match type_line(&mut app, "/side --at msg_12 what changed?") {
        Some(Action::SideQuestion { boundary, question, .. }) => {
            assert_eq!(boundary.as_deref(), Some("msg_12"));
            assert_eq!(question, "what changed?");
        }
        other => panic!("`/side --at` did not ask the bridge: {other:?}"),
    }
    assert_eq!(
        app.side.as_ref().and_then(|side| side.boundary()),
        Some("msg_12"),
        "the screen does not say which boundary was chosen"
    );
}

#[test]
fn at_without_an_id_or_question_is_refused() {
    let (_user, _project, mut app) = app_fixture();
    assert_eq!(type_line(&mut app, "/side --at"), None);
    assert_eq!(type_line(&mut app, "/side --at msg_12"), None);
    assert!(app.side.is_none(), "an incomplete --at opened a screen");
}

#[test]
fn a_blank_question_is_refused_and_opens_nothing() {
    let (_user, _project, mut app) = app_fixture();
    assert_eq!(type_line(&mut app, "/side"), None);
    assert!(app.side.is_none(), "an empty question opened a screen");
    assert!(
        matches!(app.transcript.items().last(), Some(Item::Warning(line)) if line.starts_with("side:")),
        "the refusal was not said"
    );
}

#[test]
fn a_second_side_question_is_refused_while_one_is_open() {
    let (_user, _project, mut app) = app_fixture();
    let before = transcript_text(&app);
    open_side(&mut app, "first");
    // The screen owns the keyboard, so this is the programmatic path a future
    // entry point would take; it must not replace the question on screen.
    assert_eq!(app.run_side("second"), None);
    assert_eq!(
        app.side.as_ref().map(|side| side.question()),
        Some("first"),
        "the open question was replaced"
    );
    assert_eq!(
        transcript_text(&app).len(),
        before.len() + 1,
        "the refusal was not reported"
    );
}

#[test]
fn the_screen_tracks_the_child_from_start_to_stored_answer() {
    let (_user, _project, mut app) = app_fixture();
    let id = open_side(&mut app, "what changed?");
    app.side_started(id, "session_child".to_string());
    app.side_progress(id, "using read".to_string());
    app.side_answer(id, "the cache".to_string());
    app.side_answer(id, "the cache was removed".to_string());

    let side = app.side.as_ref().expect("the screen is open");
    assert_eq!(side.session_id(), Some("session_child"));
    assert_eq!(side.answer(), "the cache was removed");
    assert!(!side.is_done());

    app.side_notice(id, "the provider retried".to_string());
    app.side_done(id, "the cache was removed from the config".to_string());
    let side = app.side.as_ref().expect("the screen is open");
    assert!(side.is_done());
    assert_eq!(
        side.answer(),
        "the cache was removed from the config",
        "the stored answer must replace the streamed preview"
    );
    assert_eq!(side.notices().to_vec(), vec!["the provider retried".to_string()]);
}

#[test]
fn escape_returns_to_the_untouched_conversation() {
    let (_user, _project, mut app) = app_fixture();
    let before = transcript_text(&app);
    let id = open_side(&mut app, "what changed?");
    app.side_started(id, "session_child".to_string());
    app.side_answer(id, "an answer".to_string());

    assert_eq!(app.on_key(escape()), None, "esc left the client instead of the screen");
    assert!(app.side.is_none(), "esc did not close the side question");
    assert_eq!(
        transcript_text(&app),
        before,
        "closing the screen changed the conversation"
    );
}

#[test]
fn enter_opens_the_stored_child() {
    let (_user, _project, mut app) = app_fixture();
    let id = open_side(&mut app, "what changed?");
    app.side_started(id, "session_child".to_string());
    app.side_done(id, "an answer".to_string());

    assert_eq!(
        app.on_key(enter()),
        Some(Action::Attach("session_child".to_string())),
        "enter did not open the child"
    );
    assert!(app.side.is_none(), "the screen stayed open over the session it opened");
}

#[test]
fn enter_before_the_fork_answers_keeps_the_screen_open() {
    let (_user, _project, mut app) = app_fixture();
    open_side(&mut app, "what changed?");
    assert_eq!(app.on_key(enter()), None);
    assert!(app.side.is_some(), "the screen closed without a session to open");
}

#[test]
fn a_failure_is_shown_on_the_screen_and_stays_there_until_it_is_closed() {
    let (_user, _project, mut app) = app_fixture();
    let before = transcript_text(&app);
    let id = open_side(&mut app, "what changed?");
    app.side_failed(id, "the harness refused a side session: no such session".to_string());

    let side = app.side.as_ref().expect("the screen is open");
    assert_eq!(
        side.failure(),
        Some("the harness refused a side session: no such session")
    );
    assert_eq!(
        transcript_text(&app),
        before,
        "a failure on the open screen reached the transcript"
    );

    app.on_key(escape());
    assert!(app.side.is_none());
}

#[test]
fn a_failure_after_the_screen_closed_is_not_lost() {
    let (_user, _project, mut app) = app_fixture();
    let id = open_side(&mut app, "what changed?");
    app.on_key(escape());
    app.side_failed(id, "the connection closed".to_string());
    assert!(
        matches!(app.transcript.items().last(), Some(Item::Warning(line)) if line.contains("the connection closed")),
        "a late failure was swallowed"
    );
}

#[test]
fn switching_sessions_closes_the_side_question() {
    let (_user, _project, mut app) = app_fixture();
    let id = open_side(&mut app, "what changed?");
    app.side_started(id, "session_child".to_string());

    let session = crate::sdk_bridge::AttachedSession {
        description: "other".to_string(),
        session_id: "session_other".to_string(),
        status: "idle".to_string(),
        model: None,
        effort: None,
        history: Vec::new(),
        title: None,
    };
    app.attach_session(session, false);
    assert!(
        app.side.is_none(),
        "the side question outlived the session it was asked of"
    );
}
