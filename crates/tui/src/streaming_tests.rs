//! Rendering tests for assistant text that is still arriving.
//!
//! The transcript pane used to draw only committed items, so a streamed reply
//! was invisible until the turn ended. These tests draw in the middle of a turn,
//! which is the case the old tests never covered: they all called `turn_done`
//! first. They live in their own file so `lib.rs` stays inside its size budget.

use std::sync::mpsc;

use horon_agent_sdk::api::ApiEvent;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::app::App;
use crate::draw;

fn app() -> App {
    let (_sender, receiver) = mpsc::channel();
    App::new("default socket".to_string(), receiver)
}

fn draw_screen(app: &mut App) -> String {
    let backend = TestBackend::new(80, 24);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => panic!("test backend failed: {error}"),
    };
    if let Err(message) = draw(&mut terminal, app) {
        panic!("draw failed: {message}");
    }
    format!("{}", terminal.backend())
}

fn delta(text: &str) -> ApiEvent {
    ApiEvent::TextDelta {
        session_id: "s1".into(),
        text: text.into(),
        message_id: Some("m1".into()),
    }
}

fn turn_done() -> ApiEvent {
    ApiEvent::TurnDone {
        session_id: "s1".into(),
        pending_soft_interrupts: None,
    }
}

#[test]
fn streamed_text_is_on_screen_before_the_turn_ends() {
    let mut app = app();
    app.apply_event(&delta("STREAMING "));
    app.apply_event(&delta("REPLY"));

    let screen = draw_screen(&mut app);
    assert!(
        screen.contains("STREAMING REPLY"),
        "text that has already arrived was not drawn mid-turn:\n{screen}"
    );
}

#[test]
fn streamed_text_keeps_its_place_as_the_newest_entry() {
    let mut app = app();
    app.apply_event(&ApiEvent::TextDelta {
        session_id: "s1".into(),
        text: "FINISHED".into(),
        message_id: Some("m0".into()),
    });
    app.apply_event(&ApiEvent::TextDone {
        session_id: "s1".into(),
        message_id: Some("m0".into()),
    });
    app.apply_event(&delta("LIVE"));

    let screen = draw_screen(&mut app);
    assert!(screen.contains("FINISHED"), "the earlier message vanished:\n{screen}");
    assert!(
        screen.contains("LIVE"),
        "the streaming message was not drawn:\n{screen}"
    );
    assert!(
        screen.find("LIVE").unwrap_or(0) > screen.find("FINISHED").unwrap_or(0),
        "the streaming message must be the newest row:\n{screen}"
    );
}

#[test]
fn a_committed_message_is_drawn_exactly_once() {
    let mut app = app();
    app.apply_event(&delta("ONLYONCE"));
    app.apply_event(&turn_done());

    let screen = draw_screen(&mut app);
    assert_eq!(
        screen.matches("ONLYONCE").count(),
        1,
        "the text was drawn both as open and as committed:\n{screen}"
    );
}

#[test]
fn a_retracted_message_is_not_drawn() {
    let mut app = app();
    app.apply_event(&delta("WITHDRAWN"));
    app.apply_event(&ApiEvent::TextReplace {
        session_id: "s1".into(),
        text: String::new(),
        message_id: Some("m1".into()),
    });

    let screen = draw_screen(&mut app);
    assert!(
        !screen.contains("WITHDRAWN"),
        "retracted text was still drawn:\n{screen}"
    );
    assert!(
        screen.contains("Send a message to start."),
        "the pane should be empty again:\n{screen}"
    );
}

#[test]
fn whitespace_only_streaming_leaves_the_pane_empty() {
    // `finish_stream` drops a whitespace-only message rather than publishing
    // it, so drawing one here would show a bubble that disappears on commit.
    let mut app = app();
    app.apply_event(&delta("   "));
    app.apply_event(&delta("\n\t"));

    let screen = draw_screen(&mut app);
    assert!(
        screen.contains("Send a message to start."),
        "a blank streaming message was drawn:\n{screen}"
    );
}
