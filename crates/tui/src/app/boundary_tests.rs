//! What the client does where a turn ends and a session changes, both of which
//! can land on either side of the other.
//!
//! A steer is answered for the session it was made in, a held model or effort
//! change leaves one pass after the turn ends, and a turn that ended before it
//! reached a steer's text says so. Each test here drives a real [`App`] through
//! those crossings and pins what the interface shows and sends.

use std::sync::mpsc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use horon_agent_sdk::api::ApiEvent;

use super::{Action, App};
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

/// A turn end, with the leftover steer count the daemon reported, if any.
fn turn_done(pending: Option<u32>) -> ApiEvent {
    ApiEvent::TurnDone {
        session_id: "session_a".to_string(),
        pending_soft_interrupts: pending,
    }
}

/// The notice a turn end leaves when the daemon still held a soft interrupt.
const LATE_STEER: &str = "the turn ended before reading everything sent into it; the next turn carries the rest";

/// The notices the transcript holds about a steer the turn did not reach.
fn late_steer_notices(app: &App) -> Vec<&str> {
    app.transcript
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::Notice(text) if text == LATE_STEER => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Whether the transcript says a steer arrived too late to be read.
fn mentions_a_late_steer(app: &App) -> bool {
    !late_steer_notices(app).is_empty()
}

/// Whether the transcript says a steer's rows became messages the model read.
fn mentions_a_landed_steer(app: &App) -> bool {
    app.transcript
        .items()
        .iter()
        .any(|item| matches!(item, Item::Notice(text) if text == "sent into the running turn"))
}

#[test]
fn an_esc_steers_the_new_session_while_the_old_steer_is_unanswered() {
    let mut app = app();
    app.session_id = Some("session_a".to_string());
    running(&mut app);
    submit(&mut app, "for the old session");
    assert_eq!(steer_of(esc(&mut app)), "for the old session");

    app.attach_session(attached("session_b"), false);
    running(&mut app);
    submit(&mut app, "for the new session");

    // The old answer is still in flight; it belongs to the session just left,
    // so it must not turn this key into a cancel.
    assert_eq!(esc(&mut app), Some(Action::Steer("for the new session".to_string())));
    assert_eq!(conversation(&app), ["queued for the new session"]);
}

#[test]
fn a_late_steer_of_the_old_session_promotes_nothing_in_the_new_one() {
    let mut app = app();
    app.session_id = Some("session_a".to_string());
    running(&mut app);
    submit(&mut app, "for the old session");
    let text = steer_of(esc(&mut app));

    app.attach_session(attached("session_b"), false);
    app.steered(&text);

    assert!(
        conversation(&app).is_empty(),
        "the old session's message was drawn in the new one"
    );
    assert!(
        !app.transcript
            .items()
            .iter()
            .any(|item| matches!(item, Item::Notice(text) if text == "sent into the running turn")),
        "the new session claimed a message it never sent"
    );
}

#[test]
fn a_lost_connection_does_not_restore_the_steer_of_the_session_left() {
    let mut app = app();
    app.session_id = Some("session_a".to_string());
    running(&mut app);
    submit(&mut app, "left behind");
    assert_eq!(steer_of(esc(&mut app)), "left behind");

    app.attach_session(attached("session_b"), false);
    app.disconnect("the event stream ended");

    assert!(queued(&app).is_empty(), "the old session's text entered the new queue");
    assert!(
        !conversation(&app).iter().any(|row| row.contains("left behind")),
        "the old session's message was drawn in the new one"
    );
}

#[test]
fn a_late_refusal_of_the_older_steer_does_not_send_the_same_text_twice() {
    let mut app = app();
    app.session_id = Some("session_a".to_string());
    running(&mut app);
    submit(&mut app, "same text");
    assert_eq!(steer_of(esc(&mut app)), "same text");

    app.attach_session(attached("session_b"), false);
    running(&mut app);
    submit(&mut app, "same text");
    assert_eq!(steer_of(esc(&mut app)), "same text");

    // The bridge answers the steer of the left session first. Its refusal
    // settles that record, not the one this session is waiting on.
    app.steer_failed("same text", "could not send the queued messages into the turn: refused");
    app.steered("same text");

    assert!(queued(&app).is_empty(), "a refusal restored text the turn had taken");
    assert_eq!(conversation(&app), ["user same text"]);
    assert!(mentions_a_landed_steer(&app), "the accepted steer drew no notice");

    app.apply_event(&turn_done(None));
    assert_eq!(app.take_queued(), None, "the text was sent a second time");
}

#[test]
fn a_late_acceptance_of_the_older_steer_keeps_the_new_refusal() {
    let mut app = app();
    app.session_id = Some("session_a".to_string());
    running(&mut app);
    submit(&mut app, "same text");
    assert_eq!(steer_of(esc(&mut app)), "same text");

    app.attach_session(attached("session_b"), false);
    running(&mut app);
    submit(&mut app, "same text");
    assert_eq!(steer_of(esc(&mut app)), "same text");

    // The acceptance belongs to the session on the left, so the rows this one
    // drew stay queued and no notice claims they were read.
    app.steered("same text");
    assert_eq!(conversation(&app), ["queued same text"]);
    assert!(
        !mentions_a_landed_steer(&app),
        "this session's rows claimed the old acceptance"
    );

    app.steer_failed("same text", "could not send the queued messages into the turn: refused");

    assert_eq!(queued(&app), ["same text"]);
    assert_eq!(
        last_line(&app),
        "could not send the queued messages into the turn: refused"
    );
}

#[test]
fn a_refusal_of_the_steer_left_behind_leaves_the_new_queue_alone() {
    let mut app = app();
    app.session_id = Some("session_a".to_string());
    running(&mut app);
    submit(&mut app, "for the old session");
    assert_eq!(steer_of(esc(&mut app)), "for the old session");

    app.attach_session(attached("session_b"), false);
    running(&mut app);
    submit(&mut app, "for the new session");
    assert_eq!(steer_of(esc(&mut app)), "for the new session");

    app.steer_failed(
        "for the old session",
        "could not send the queued messages into the turn: refused",
    );

    assert!(queued(&app).is_empty(), "the old session's text entered the new queue");
    assert_eq!(
        last_line(&app),
        "could not send the queued messages into the turn: refused"
    );

    // The steer of the session in force still settles from its own answer.
    app.steered("for the new session");
    assert_eq!(conversation(&app), ["user for the new session"]);
    assert!(mentions_a_landed_steer(&app), "the new steer's acceptance went unshown");
}

#[test]
fn a_lost_connection_restores_only_the_steer_of_the_session_in_force() {
    let mut app = app();
    app.session_id = Some("session_a".to_string());
    running(&mut app);
    submit(&mut app, "for the old session");
    assert_eq!(steer_of(esc(&mut app)), "for the old session");

    app.attach_session(attached("session_b"), false);
    running(&mut app);
    submit(&mut app, "for the new session");
    assert_eq!(steer_of(esc(&mut app)), "for the new session");

    app.disconnect("the event stream ended");

    assert_eq!(queued(&app), ["for the new session"]);
    assert!(
        !conversation(&app).iter().any(|row| row.contains("old session")),
        "the old session's message was drawn in the new one"
    );
}

#[test]
fn a_held_effort_holds_back_a_session_change() {
    let mut app = app();
    running(&mut app);
    assert_eq!(submit(&mut app, "/effort high"), None);
    assert_eq!(app.effort_after_turn.as_deref(), Some("high"));

    // What a turn end leaves behind: the flag is down and the level is still
    // waiting for its own pass.
    app.pending_turn = false;

    assert_eq!(submit(&mut app, "/new"), None);
    assert!(
        !app.take_pending_new_session(),
        "a refused change still claimed a session"
    );
    assert_eq!(
        last_line(&app),
        "a held model or effort change is still being applied; /new works in a moment"
    );
}

#[test]
fn a_session_change_waits_until_both_held_values_have_left() {
    let mut app = app();
    running(&mut app);
    assert_eq!(submit(&mut app, "/model mock-large"), None);
    assert_eq!(submit(&mut app, "/effort high"), None);
    app.pending_turn = false;

    assert_eq!(app.take_queued(), Some(Action::Model("mock-large".to_string())));
    assert_eq!(
        submit(&mut app, "/switch"),
        None,
        "the list opened with a change still held"
    );
    assert_eq!(
        last_line(&app),
        "a held model or effort change is still being applied; /switch works in a moment"
    );

    assert_eq!(app.take_queued(), Some(Action::Effort("high".to_string())));
    assert_eq!(
        submit(&mut app, "/resume session_b"),
        Some(Action::Attach("session_b".to_string()))
    );
}

#[test]
fn the_running_turn_refusal_keeps_its_own_sentence() {
    let mut app = app();
    running(&mut app);
    app.model_after_turn = Some("mock-model".to_string());

    assert_eq!(submit(&mut app, "/new"), None);
    assert_eq!(last_line(&app), "the turn is still running; /new works once it ends");
}

#[test]
fn a_turn_that_left_a_steer_queued_says_when_it_lands() {
    let mut app = app();
    running(&mut app);
    submit(&mut app, "one");
    submit(&mut app, "two");
    // Both lines leave as one steer, so the daemon's queue grew by one item.
    let text = steer_of(esc(&mut app));
    app.steered(&text);

    app.apply_event(&turn_done(Some(1)));

    assert_eq!(last_line(&app), LATE_STEER);
    assert_eq!(
        late_steer_notices(&app),
        [LATE_STEER],
        "the notice was printed more than once"
    );
}

#[test]
fn a_soft_interrupt_count_is_not_reported_as_a_number_of_messages() {
    let mut app = app();
    running(&mut app);
    submit(&mut app, "only line I typed");
    let text = steer_of(esc(&mut app));
    app.steered(&text);

    // The queue also holds background results and other clients' interrupts,
    // so a count above one need not mean a second message was typed.
    app.apply_event(&turn_done(Some(3)));

    let said = late_steer_notices(&app);
    assert_eq!(said, [LATE_STEER], "a larger count changed the sentence");
}

#[test]
fn a_turn_that_reached_every_steer_says_nothing_extra() {
    for pending in [None, Some(0)] {
        let mut app = app();
        running(&mut app);
        submit(&mut app, "read it");
        let text = steer_of(esc(&mut app));
        app.steered(&text);

        app.apply_event(&turn_done(pending));

        assert!(
            !mentions_a_late_steer(&app),
            "a turn that read the steer said one arrived late"
        );
    }
}

#[test]
fn a_steer_made_before_the_identity_event_settles_when_it_arrives() {
    let mut app = app();
    assert!(app.session_id.is_none());
    running(&mut app);
    submit(&mut app, "early");
    let text = steer_of(esc(&mut app));

    // The identity event names the session after the steer left; an unknown id
    // never proves a change, so the answer still settles the rows.
    app.session_id = Some("session_a".to_string());
    app.steered(&text);

    assert_eq!(conversation(&app), ["user early"]);
    assert_eq!(last_line(&app), "sent into the running turn");
}

#[test]
fn a_refusal_after_the_turn_ends_waits_for_the_next_turn() {
    let mut app = app();
    running(&mut app);
    submit(&mut app, "late");
    let text = steer_of(esc(&mut app));

    app.apply_event(&turn_done(None));
    assert_eq!(app.take_queued(), None, "nothing was queued to send");

    app.steer_failed(&text, "could not send the queued messages into the turn: refused");

    assert_eq!(queued(&app), ["late"]);
    assert_eq!(app.take_queued(), Some(Action::Send("late".to_string())));
}

#[test]
fn two_messages_with_the_same_text_both_go_in_one_steer() {
    let mut app = app();
    running(&mut app);
    submit(&mut app, "same");
    submit(&mut app, "same");

    assert_eq!(esc(&mut app), Some(Action::Steer("same\n\nsame".to_string())));
    app.steered("same\n\nsame");

    assert_eq!(conversation(&app), ["user same", "user same"]);
}
