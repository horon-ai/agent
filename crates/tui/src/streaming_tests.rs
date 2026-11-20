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
    let finished = screen.find("FINISHED").expect("the committed message is on screen");
    let live = screen.find("LIVE").expect("the streaming message is on screen");
    assert!(
        live > finished,
        "the streaming message must be the newest row:\n{screen}"
    );
}

#[test]
fn background_output_is_visible_without_splitting_an_open_answer() {
    let mut app = app();
    app.session_id = Some("s1".into());
    app.apply_event(&delta("Partial"));
    for (session, text) in [("s1", "LIVE-OUTPUT"), ("s2", "OTHER-OUTPUT")] {
        app.apply_event(&ApiEvent::BackgroundOutput {
            session_id: session.into(),
            text: text.into(),
        });
    }
    let live = draw_screen(&mut app);
    assert!(live.contains("LIVE-OUTPUT"), "{live}");
    assert!(!live.contains("OTHER-OUTPUT"), "{live}");
    app.apply_event(&delta(" answer"));
    app.apply_event(&turn_done());
    let completed = draw_screen(&mut app);
    assert!(completed.contains("Partial answer"), "{completed}");
    assert_eq!(completed.matches("LIVE-OUTPUT").count(), 1, "{completed}");
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

#[test]
fn a_cache_miss_marker_says_what_was_resent_under_the_answer() {
    let mut app = app();
    app.apply_event(&delta("ANSWER"));
    app.apply_event(&ApiEvent::TextDone {
        session_id: "s1".into(),
        message_id: Some("m1".into()),
    });
    app.apply_event(&ApiEvent::TokenUsage {
        session_id: "s1".into(),
        input: 50_000,
        output: 10,
        cache_read_input: Some(4_000),
        cache_creation_input: None,
    });
    app.apply_event(&ApiEvent::KvCacheMiss {
        session_id: "s1".into(),
        reason: "prefix_changed".into(),
        harness_caused: true,
        missed_tokens: 46_000,
        expected_tokens: 50_000,
        read_tokens: 4_000,
        documented_cause: None,
        message: "daemon wording that the client does not draw".into(),
    });

    let screen = draw_screen(&mut app);
    assert_eq!(screen.matches("cache miss").count(), 1, "{screen}");
    let answer = screen.find("ANSWER").expect("the answer is on screen");
    let mark = screen.find("cache miss").expect("the mark is on screen");
    assert!(mark > answer, "the mark must sit under its answer:\n{screen}");
    assert!(
        screen.contains("an earlier message changed, ~46K tokens resent"),
        "the mark must name the cause and the resend:\n{screen}"
    );
    // The marker is the client's own wording: the daemon's reason id and its
    // ready-made sentence are not part of it.
    assert!(!screen.contains("prefix_changed"), "{screen}");
    assert!(!screen.contains("daemon wording"), "{screen}");
}
