//! Turn state: what a submit does while a turn is running, what each ending
//! does to the status line, and what happens to the text when a send fails.

use std::sync::mpsc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use horon_agent_sdk::api::{ApiEvent, ErrorCode, TurnStopReason};

use crate::app::{Action, App};
use crate::prompts::Prompt;
use crate::sdk_bridge::Incoming;
use crate::transcript::{Item, TURN_ENDED, ToolState};

fn app() -> App {
    let (_sender, receiver) = mpsc::channel::<Incoming>();
    App::new("socket · session".to_string(), receiver)
}

/// A key event with no modifier.
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn control(character: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL)
}

fn enter() -> KeyEvent {
    key(KeyCode::Enter)
}

fn esc() -> KeyEvent {
    key(KeyCode::Esc)
}

/// Type `text` into the input line without submitting it.
fn type_text(app: &mut App, text: &str) {
    for character in text.chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
}

fn done(session: &str) -> ApiEvent {
    ApiEvent::TurnDone {
        session_id: session.to_string(),
        pending_soft_interrupts: None,
    }
}

fn stopped(session: &str, reason: TurnStopReason, message: &str) -> ApiEvent {
    ApiEvent::TurnStopped {
        session_id: session.to_string(),
        reason,
        message: message.to_string(),
        provider_stop_reason: None,
    }
}

fn failed_error(message: &str) -> ApiEvent {
    ApiEvent::Error {
        code: ErrorCode::Internal,
        message: message.to_string(),
        details: None,
    }
}

fn items(app: &App) -> &[Item] {
    app.transcript.items()
}

fn warnings(app: &App) -> usize {
    items(app)
        .iter()
        .filter(|item| matches!(item, Item::Warning(_)))
        .count()
}

#[test]
fn a_submit_during_a_turn_is_queued_and_sent_when_the_turn_ends() {
    let mut app = app();
    app.pending_turn = true;

    type_text(&mut app, "first");
    assert_eq!(
        app.on_key(enter()),
        None,
        "a submit during a turn must not go to the server"
    );
    type_text(&mut app, "second");
    assert_eq!(app.on_key(enter()), None);

    assert_eq!(app.queue, vec!["first".to_string(), "second".to_string()]);
    assert!(items(&app).contains(&Item::Queued("first".to_string())));
    assert!(items(&app).contains(&Item::Queued("second".to_string())));
    assert_eq!(app.take_queued(), None, "a running turn sends nothing");

    app.apply_event(&done("s1"));
    assert_eq!(app.take_queued(), Some(Action::Send("first".to_string())));
    assert!(app.pending_turn, "the message that went out starts a turn");
    assert_eq!(app.queue, vec!["second".to_string()]);
    assert_eq!(app.take_queued(), None, "one message per turn");
    assert!(items(&app).contains(&Item::User("first".to_string())));
    assert!(
        !items(&app).contains(&Item::Queued("first".to_string())),
        "the queued line became the sent message"
    );
    assert!(items(&app).contains(&Item::Queued("second".to_string())));
}

#[test]
fn an_error_mid_turn_does_not_stop_ctrl_c_from_cancelling() {
    let mut app = app();
    app.pending_turn = true;
    app.apply_event(&failed_error("the stream broke"));

    assert!(app.pending_turn, "an error event is not the end of the turn");
    assert_eq!(app.on_key(control('c')), Some(Action::Cancel));
    assert!(app.running, "Ctrl-C cancelled instead of leaving");
}

#[test]
fn esc_interrupts_a_running_turn_and_does_nothing_when_idle() {
    let mut app = app();
    assert_eq!(app.on_key(esc()), None, "esc on an idle client does nothing");
    assert!(app.running, "esc never leaves the client");

    app.pending_turn = true;
    assert_eq!(app.on_key(esc()), Some(Action::Cancel));
    assert!(app.running);
}

#[test]
fn an_open_prompt_takes_esc_before_the_interrupt() {
    let mut app = app();
    app.pending_turn = true;
    app.prompt = Some(Prompt::approval("r1".to_string(), "bash".to_string()));

    assert_eq!(app.on_key(esc()), None, "the prompt answered the key");
    assert!(app.prompt.is_some(), "the prompt stays parked");
}

#[test]
fn ctrl_c_twice_cancels_twice_instead_of_leaving() {
    let mut app = app();
    app.pending_turn = true;
    assert_eq!(app.on_key(control('c')), Some(Action::Cancel));
    assert_eq!(app.on_key(control('c')), Some(Action::Cancel));
    assert!(app.running);

    app.apply_event(&done("s1"));
    assert_eq!(app.on_key(control('c')), Some(Action::Quit), "idle Ctrl-C leaves");
}

#[test]
fn a_cancelled_turn_ends_as_a_notice_and_returns_to_idle() {
    let mut app = app();
    app.pending_turn = true;
    app.apply_event(&stopped("s1", TurnStopReason::Interrupted, "turn interrupted"));

    assert!(!app.pending_turn);
    assert!(app.status_line().ends_with("idle"), "{}", app.status_line());
    assert!(items(&app).contains(&Item::Notice("turn interrupted".to_string())));
    assert_eq!(warnings(&app), 0, "a cancel is not a warning");

    // The daemon then restates the cancelled turn; the state word stays idle
    // instead of reporting the same thing a second time.
    app.apply_event(&ApiEvent::SessionStatus {
        session_id: "s1".to_string(),
        status: "cancelled".to_string(),
        pending_soft_interrupts: None,
    });
    assert!(app.status_line().ends_with("idle"), "{}", app.status_line());
}

#[test]
fn a_failed_turn_still_ends_as_a_warning() {
    let mut app = app();
    app.pending_turn = true;
    app.apply_event(&stopped("s1", TurnStopReason::Failure, "provider failed"));

    assert!(!app.pending_turn);
    assert!(app.status_line().ends_with("idle"));
    assert_eq!(warnings(&app), 1);
}

#[test]
fn a_cancel_keeps_the_queue_and_sends_it_after_the_turn_stops() {
    let mut app = app();
    app.pending_turn = true;
    type_text(&mut app, "next");
    app.on_key(enter());
    assert_eq!(app.queue, vec!["next".to_string()]);

    assert_eq!(app.on_key(control('c')), Some(Action::Cancel));
    app.apply_event(&stopped("s1", TurnStopReason::Interrupted, "turn interrupted"));

    assert_eq!(
        app.take_queued(),
        Some(Action::Send("next".to_string())),
        "the queued message is not thrown away by a cancel"
    );
}

#[test]
fn a_disconnect_keeps_the_typed_text_and_never_shows_running() {
    let mut app = app();
    app.pending_turn = true;
    app.disconnect("the socket closed");

    assert!(!app.pending_turn);
    assert!(app.status_line().ends_with("disconnected"), "{}", app.status_line());

    type_text(&mut app, "hello");
    assert_eq!(app.on_key(enter()), None, "nothing is sent without a connection");
    assert_eq!(app.input.text(), "hello", "the typed text stays in the input line");
    assert!(!app.pending_turn, "a refused submit never starts a turn");
    assert!(!app.status_line().contains("running"));
    assert_eq!(warnings(&app), 1, "one line says the message did not go");
}

#[test]
fn a_failed_send_returns_the_text_to_the_input_line() {
    let mut app = app();
    type_text(&mut app, "hello");
    assert_eq!(app.on_key(enter()), Some(Action::Send("hello".to_string())));
    assert!(app.pending_turn);

    app.send_failed("hello", "could not send the message: broken pipe", false);

    assert!(!app.pending_turn);
    assert!(app.status_line().ends_with("idle"), "{}", app.status_line());
    assert_eq!(app.input.text(), "hello");
    assert!(items(&app).contains(&Item::Warning("could not send the message: broken pipe".to_string())));
}

#[test]
fn a_failed_send_with_a_busy_input_line_queues_instead_of_overwriting() {
    let mut app = app();
    type_text(&mut app, "hello");
    app.on_key(enter());
    type_text(&mut app, "typing");

    app.send_failed("hello", "could not send the message: broken pipe", false);

    assert_eq!(app.input.text(), "typing", "the newer typing is not overwritten");
    assert_eq!(app.queue, vec!["hello".to_string()], "the failed text is kept");
    assert!(items(&app).contains(&Item::Queued("hello".to_string())));
}

#[test]
fn a_send_that_found_a_closed_socket_marks_the_connection_lost() {
    let mut app = app();
    type_text(&mut app, "hello");
    app.on_key(enter());

    app.send_failed(
        "hello",
        "could not send the message: disconnected: harness connection closed",
        true,
    );

    assert!(app.disconnected, "the status line has to stop saying it can send");
    assert!(app.status_line().ends_with("disconnected"), "{}", app.status_line());
    assert_eq!(app.exit_code, 1, "the session failed even though the client stayed up");
    assert_eq!(app.input.text(), "hello");

    // The next submit is refused before it reaches the bridge, and the text
    // stays where the user can see it.
    type_text(&mut app, " again");
    assert_eq!(app.on_key(enter()), None);
    assert_eq!(app.input.text(), "hello again");
    assert!(!app.pending_turn);
}

#[test]
fn a_running_tool_line_ends_when_the_turn_does() {
    let mut app = app();
    app.pending_turn = true;
    app.apply_event(&ApiEvent::ToolStart {
        session_id: "s1".to_string(),
        call_id: "c1".to_string(),
        name: "bash".to_string(),
    });
    app.apply_event(&done("s1"));

    let call = items(&app)
        .iter()
        .find_map(|item| match item {
            Item::Tool(call) => Some(call),
            _ => None,
        })
        .expect("the tool line is there");
    assert_eq!(call.state, ToolState::Failed(TURN_ENDED.to_string()));
    assert_ne!(call.state, ToolState::Done, "an unfinished call is not a success");
}
