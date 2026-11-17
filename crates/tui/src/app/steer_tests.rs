//! `esc` handing the queue to the running turn: what leaves, what the rows
//! show while the answer is in flight, what a refusal restores, and what a
//! fresh submit does to a held view.
//!
//! Every test drives a real [`App`] the way the interface does, so the key, the
//! queue, the transcript rows and the turn-end pass are exercised together.
//! Nothing here talks to a harness: the bridge's own half is pinned beside it in
//! `sdk_bridge::steer_tests`.

use std::sync::mpsc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use horon_agent_sdk::api::ApiEvent;

use super::{Action, App, Message};
use crate::sdk_bridge::{AttachedSession, Incoming};
use crate::transcript::Item;

fn app() -> App {
    let (_sender, receiver) = mpsc::channel::<Incoming>();
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

/// Press `esc`.
fn esc(app: &mut App) -> Option<Action> {
    app.on_key(key(KeyCode::Esc))
}

/// Mark the turn as running, as the submit that started one does.
fn running(app: &mut App) {
    app.pending_turn = true;
}

fn queued(app: &App) -> Vec<&str> {
    app.queue.iter().map(|message| message.text.as_str()).collect()
}

/// The message rows the transcript holds, queued and sent, in display order.
fn conversation(app: &App) -> Vec<String> {
    app.transcript
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::User(text) => Some(format!("user {text}")),
            Item::Queued(text) => Some(format!("queued {text}")),
            _ => None,
        })
        .collect()
}

/// The last line a command or a failure left in the transcript.
fn last_line(app: &App) -> String {
    match app.transcript.items().last() {
        Some(Item::Notice(text)) | Some(Item::Warning(text)) => text.clone(),
        other => panic!("expected a notice or a warning, got {other:?}"),
    }
}

/// The steer action an `esc` produced, or the failure that it did not.
fn steer_of(action: Option<Action>) -> String {
    match action {
        Some(Action::Steer(text)) => text,
        other => panic!("expected a steer, got {other:?}"),
    }
}

fn attached(session_id: &str) -> AttachedSession {
    AttachedSession {
        description: format!("socket · {session_id}"),
        session_id: session_id.to_string(),
        status: "idle".to_string(),
        model: None,
        effort: None,
        history: Vec::new(),
        title: None,
    }
}

fn turn_done() -> ApiEvent {
    ApiEvent::TurnDone {
        session_id: "session_a".to_string(),
        pending_soft_interrupts: None,
    }
}

#[test]
fn esc_spends_one_queued_message_into_the_running_turn() {
    let mut app = app();
    running(&mut app);
    assert_eq!(submit(&mut app, "also do this"), None);
    assert_eq!(queued(&app), ["also do this"]);
    assert_eq!(conversation(&app), ["queued also do this"]);

    assert_eq!(esc(&mut app), Some(Action::Steer("also do this".to_string())));
    // The queue is spent, so the turn-end pass has nothing left to send, while
    // the row still says queued until the harness answers.
    assert!(queued(&app).is_empty(), "the queue survived the steer");
    assert_eq!(conversation(&app), ["queued also do this"]);
    // The notice waits for the answer rather than claiming a success the
    // harness has not reported yet.
    assert!(
        !app.transcript
            .items()
            .iter()
            .any(|item| matches!(item, Item::Notice(_))),
        "a notice was shown before the harness answered"
    );

    app.steered("also do this");
    assert_eq!(conversation(&app), ["user also do this"]);
    assert_eq!(last_line(&app), "sent into the running turn");
}

#[test]
fn several_queued_messages_go_in_as_one_body_in_their_own_order() {
    let mut app = app();
    running(&mut app);
    submit(&mut app, "first");
    submit(&mut app, "second");
    assert_eq!(esc(&mut app), Some(Action::Steer("first\n\nsecond".to_string())));
    assert!(queued(&app).is_empty());

    app.steered("first\n\nsecond");
    assert_eq!(conversation(&app), ["user first", "user second"]);
    assert_eq!(last_line(&app), "sent into the running turn");
}

#[test]
fn a_message_blank_once_trimmed_adds_no_paragraph() {
    let mut app = app();
    running(&mut app);
    // A command that expands to whitespace is the one way a blank message is
    // queued; it still owns the row it was drawn as.
    app.transcript.push_queued("/blank");
    app.queue.push(Message {
        text: "   ".to_string(),
        display: "/blank".to_string(),
    });
    submit(&mut app, "real text");

    assert_eq!(esc(&mut app), Some(Action::Steer("real text".to_string())));
    app.steered("real text");
    assert_eq!(conversation(&app), ["user /blank", "user real text"]);
}

#[test]
fn esc_with_an_empty_queue_cancels_the_turn() {
    let mut app = app();
    running(&mut app);
    assert_eq!(esc(&mut app), Some(Action::Cancel));
    assert_eq!(last_line(&app), "cancelling the turn");
}

#[test]
fn esc_with_no_turn_leaves_the_client_alone() {
    let mut app = app();
    assert_eq!(esc(&mut app), None);
    assert!(app.running, "esc left the client");
    assert!(app.transcript.items().is_empty());
}

#[test]
fn a_refused_steer_puts_its_messages_ahead_of_the_queue() {
    let mut app = app();
    running(&mut app);
    submit(&mut app, "first");
    submit(&mut app, "second");
    let text = steer_of(esc(&mut app));
    // Typed while the steer was in flight: it keeps its place behind them.
    submit(&mut app, "queued after");
    assert_eq!(queued(&app), ["queued after"]);

    app.steer_failed(&text, "could not send the queued messages into the turn: refused");

    assert_eq!(queued(&app), ["first", "second", "queued after"]);
    // Their own rows are still the ones drawn: nothing was retracted and
    // nothing was drawn a second time.
    assert_eq!(
        conversation(&app),
        ["queued first", "queued second", "queued queued after"]
    );
    assert_eq!(
        last_line(&app),
        "could not send the queued messages into the turn: refused"
    );
}

#[test]
fn a_refused_steer_after_a_session_change_is_dropped() {
    let mut app = app();
    running(&mut app);
    app.session_id = Some("session_a".to_string());
    submit(&mut app, "for the old session");
    let text = steer_of(esc(&mut app));

    app.attach_session(attached("session_b"), false);
    assert!(queued(&app).is_empty());

    app.steer_failed(&text, "could not send the queued messages into the turn: refused");

    assert!(
        queued(&app).is_empty(),
        "the old session's messages entered the new session's queue"
    );
    assert!(
        !conversation(&app).iter().any(|row| row.contains("for the old session")),
        "the old session's message was drawn in the new one"
    );
    assert_eq!(
        last_line(&app),
        "could not send the queued messages into the turn: refused"
    );
}

#[test]
fn a_steered_message_is_not_sent_again_when_the_turn_ends() {
    let mut app = app();
    running(&mut app);
    submit(&mut app, "only once");
    let text = steer_of(esc(&mut app));
    app.steered(&text);

    app.apply_event(&turn_done());

    assert_eq!(app.take_queued(), None, "the steered message was sent twice");
}

#[test]
fn a_held_model_and_effort_still_leave_when_a_steered_turn_ends() {
    let mut app = app();
    running(&mut app);
    submit(&mut app, "hurry up");
    let text = steer_of(esc(&mut app));
    app.steered(&text);
    app.model_after_turn = Some("mock-model".to_string());
    app.effort_after_turn = Some("high".to_string());

    app.apply_event(&turn_done());

    assert_eq!(app.take_queued(), Some(Action::Model("mock-model".to_string())));
    assert_eq!(app.take_queued(), Some(Action::Effort("high".to_string())));
    assert_eq!(app.take_queued(), None);
}

#[test]
fn a_second_esc_while_a_steer_waits_cancels() {
    let mut app = app();
    running(&mut app);
    submit(&mut app, "first");
    assert_eq!(esc(&mut app), Some(Action::Steer("first".to_string())));
    submit(&mut app, "second");

    // A second steer would leave two sets of messages the client could not put
    // back together, so the key falls back to the stop it always was.
    assert_eq!(esc(&mut app), Some(Action::Cancel));
    assert_eq!(queued(&app), ["second"]);
}

#[test]
fn a_submitted_message_brings_a_held_view_back_to_the_end() {
    let mut app = app();
    app.scroll.up(3);
    assert!(!app.scroll.is_following(), "the view did not leave the end");

    submit(&mut app, "hello");

    assert!(
        app.scroll.is_following(),
        "the view stayed held above the message just sent"
    );
}

#[test]
fn a_message_queued_behind_a_turn_also_follows_the_end() {
    let mut app = app();
    running(&mut app);
    app.scroll.up(3);

    submit(&mut app, "queued");

    assert!(app.scroll.is_following());
}

#[test]
fn a_listing_keeps_the_view_where_the_user_put_it() {
    let mut app = app();
    app.scroll.up(3);
    submit(&mut app, "/keys");
    assert!(
        !app.scroll.is_following(),
        "the key listing pulled the view back to the end"
    );

    app.scroll.up(3);
    submit(&mut app, "/help");
    assert!(
        !app.scroll.is_following(),
        "the help listing pulled the view back to the end"
    );
}
