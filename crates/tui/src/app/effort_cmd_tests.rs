//! `/effort`, the effort cycle key, the model key, and the runtime report that
//! puts a confirmed level on the status line.
//!
//! Every test drives a real [`App`] the way the interface does, so the
//! resolution, the refusal, the list and the key that steps the ladder are
//! exercised together. Nothing here talks to a harness: the bridge's own half
//! is pinned beside it in `sdk_bridge::runtime_tests`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{Action, App};
use crate::keys::{Bindings, Key, ids};
use crate::overlays::picker::Kind;
use crate::sdk_bridge::{AttachedSession, Command, Incoming};
use crate::transcript::Item;

/// Every level, in the order the ladder lists them.
const LEVELS: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

fn app() -> App {
    let (_sender, receiver) = std::sync::mpsc::channel();
    App::new("socket · session_a".to_string(), receiver)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// `shift+tab`, which a terminal reports as the back tab key.
fn back_tab() -> KeyEvent {
    KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)
}

/// `alt+p`.
fn alt_p() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT)
}

/// Type a whole line and press Enter, as a user would.
fn submit(app: &mut App, line: &str) -> Option<Action> {
    app.input.set_text(line);
    app.on_key(key(KeyCode::Enter))
}

/// The last line a command left in the transcript.
fn last_line(app: &App) -> String {
    match app.transcript.items().last() {
        Some(Item::Notice(text)) | Some(Item::Warning(text)) => text.clone(),
        other => panic!("expected a notice or a warning, got {other:?}"),
    }
}

#[test]
fn a_level_on_the_ladder_is_the_action_and_nothing_else() {
    let mut app = app();
    assert_eq!(
        submit(&mut app, "/effort high"),
        Some(Action::Effort("high".to_string()))
    );
    // The level leaves the client as the ladder spells it, and nothing is
    // claimed on screen before the harness answers.
    assert_eq!(app.status_data().effort, None, "a level was shown before it was set");
    assert!(app.transcript.items().is_empty(), "an unconfirmed level was reported");
}

#[test]
fn a_level_typed_with_space_and_capitals_is_the_same_level() {
    let mut app = app();
    assert_eq!(
        submit(&mut app, "/effort  HIGH "),
        Some(Action::Effort("high".to_string()))
    );
}

#[test]
fn a_level_outside_the_ladder_is_refused_with_the_list_and_sends_nothing() {
    let mut app = app();
    assert_eq!(submit(&mut app, "/effort bogus"), None);
    let line = last_line(&app);
    assert!(
        line.contains("none, minimal, low, medium, high, xhigh, max"),
        "the refusal does not list the ladder: {line}"
    );
    assert!(
        !line.contains("reasoning effort set to bogus"),
        "a level that was never sent was reported as set: {line}"
    );
    assert!(app.effort_requested.is_none(), "a refused level steered the cycle key");
}

#[test]
fn effort_alone_opens_the_ladder_with_the_level_in_force_marked() {
    let mut app = app();
    app.reasoning_effort = Some("high".to_string());
    assert_eq!(submit(&mut app, "/effort"), None);
    let picker = app.picker.as_ref().expect("/effort opened no list");
    assert_eq!(picker.kind(), Kind::Efforts);
    assert_eq!(picker.title(), "Select reasoning effort");
    assert!(!picker.is_loading(), "the ladder is local, so nothing is on its way");
    let rows = picker.visible();
    let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
    assert_eq!(labels, LEVELS, "the list is not the ladder");
    assert_eq!(
        rows.iter().filter(|row| row.current).count(),
        1,
        "the level in force is not marked exactly once"
    );
    assert!(rows[4].current, "the marked row is not `high`");
    for row in &rows {
        assert!(
            !row.preview.is_empty(),
            "`{}` has no line saying what it asks the model to do",
            row.label
        );
    }
}

#[test]
fn a_chosen_row_of_the_ladder_is_the_level_it_names() {
    let mut app = app();
    submit(&mut app, "/effort");
    app.on_key(key(KeyCode::Down));
    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        Some(Action::Effort("minimal".to_string()))
    );
    assert!(app.picker.is_none(), "the list stayed open after a choice");
}

#[test]
fn the_runtime_answer_puts_the_confirmed_level_on_the_status_line() {
    let mut app = app();
    app.runtime_info(
        Some("mock".to_string()),
        Some("mock-model".to_string()),
        Some("high".to_string()),
        Some("high".to_string()),
    );
    let status = app.status_data();
    assert_eq!(status.provider, Some("mock"));
    assert_eq!(status.model, Some("mock-model"));
    assert_eq!(status.effort, Some("high"));
    assert_eq!(last_line(&app), "reasoning effort set to high");
}

#[test]
fn a_report_that_confirms_no_level_says_nothing_about_one() {
    let mut app = app();
    app.runtime_info(
        Some("mock".to_string()),
        Some("mock-model".to_string()),
        Some("low".to_string()),
        None,
    );
    assert_eq!(app.status_data().effort, Some("low"));
    assert!(app.transcript.items().is_empty(), "a plain report printed a notice");
}

#[test]
fn a_confirmed_level_is_shown_even_when_the_report_names_no_effort() {
    let mut app = app();
    app.runtime_info(None, None, None, Some("max".to_string()));
    // The provider has no name for the level it stored, but the harness took
    // it, so the status line says so and the model is left as it was.
    assert_eq!(app.status_data().effort, Some("max"));
    assert_eq!(app.status_data().model, None);
    assert_eq!(last_line(&app), "reasoning effort set to max");
}

#[test]
fn the_cycle_key_steps_up_from_the_reported_level_and_wraps() {
    let mut app = app();
    app.reasoning_effort = Some("high".to_string());
    assert_eq!(app.on_key(back_tab()), Some(Action::Effort("xhigh".to_string())));
    // The second press steps from the level just asked for: the first answer
    // has not arrived and the key must not ask for the same level twice.
    assert_eq!(app.on_key(back_tab()), Some(Action::Effort("max".to_string())));
    assert_eq!(app.on_key(back_tab()), Some(Action::Effort("none".to_string())));
}

#[test]
fn the_cycle_key_starts_at_the_first_level_when_the_current_one_is_unknown() {
    let mut app = app();
    app.reasoning_effort = Some("auto".to_string());
    assert_eq!(app.on_key(back_tab()), Some(Action::Effort("none".to_string())));
}

#[test]
fn a_confirmed_answer_hands_the_cycle_key_back_to_the_reported_level() {
    let mut app = app();
    app.reasoning_effort = Some("low".to_string());
    assert_eq!(app.on_key(back_tab()), Some(Action::Effort("medium".to_string())));
    app.runtime_info(None, None, Some("medium".to_string()), Some("medium".to_string()));
    assert_eq!(app.on_key(back_tab()), Some(Action::Effort("high".to_string())));
}

#[test]
fn a_failed_request_hands_the_cycle_key_back_to_the_reported_level() {
    let mut app = app();
    app.reasoning_effort = Some("low".to_string());
    assert_eq!(app.on_key(back_tab()), Some(Action::Effort("medium".to_string())));
    app.request_failed("could not set the reasoning effort: the provider refused");
    assert_eq!(
        app.status_data().effort,
        Some("low"),
        "a refused level reached the status line"
    );
    assert_eq!(app.on_key(back_tab()), Some(Action::Effort("medium".to_string())));
}

#[test]
fn the_cycle_key_refuses_when_the_connection_is_gone() {
    let mut app = app();
    app.disconnect("the event stream ended");
    assert_eq!(app.on_key(back_tab()), None);
    let line = last_line(&app);
    assert!(
        line.contains("connection is gone"),
        "the refusal does not say why: {line}"
    );
}

#[test]
fn the_model_key_opens_the_list_and_refuses_when_the_connection_is_gone() {
    // Two clients, built before either binding shadows the helper: the second
    // one is the client whose connection is gone.
    let mut connected = app();
    let mut lost = app();

    assert_eq!(connected.on_key(alt_p()), Some(Action::Models));
    assert_eq!(
        connected.picker.as_ref().map(|picker| picker.kind()),
        Some(Kind::Models)
    );

    lost.disconnect("the event stream ended");
    assert_eq!(lost.on_key(alt_p()), None);
    assert!(lost.picker.is_none(), "a list opened over a connection that is gone");
}

#[test]
fn shift_tab_completes_the_draft_while_the_slash_popup_is_open() {
    let mut app = app();
    for character in "/eff".chars() {
        assert_eq!(app.on_key(key(KeyCode::Char(character))), None);
    }
    assert!(app.slash.is_open(), "the popup did not open");
    assert_eq!(app.on_key(back_tab()), None);
    assert_eq!(
        app.input.text(),
        "/effort",
        "shift+tab changed the effort instead of completing the command on screen"
    );
    assert!(app.effort_requested.is_none(), "the popup key asked for a level");
}

#[test]
fn the_two_keys_ship_on_shift_tab_and_alt_p() {
    let bindings = Bindings::default();
    let cycle_key = Key::parse("shift+tab").expect("a key");
    let model_key = Key::parse("alt+p").expect("a key");
    assert_eq!(bindings.resolve_editing(&cycle_key), Some(ids::APP_CYCLE_EFFORT));
    assert_eq!(bindings.resolve_editing(&model_key), Some(ids::APP_PICK_MODEL));
    // The unshifted tab still completes the draft.
    assert_eq!(
        bindings.resolve_editing(&Key::parse("tab").expect("a key")),
        Some(ids::EDITOR_COMPLETE)
    );
}

#[test]
fn the_answer_reaches_the_app_and_the_action_reaches_the_bridge() {
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut app = App::new("socket · session_a".to_string(), receiver);
    sender
        .send(Incoming::RuntimeInfo {
            provider: Some("mock".to_string()),
            model: Some("mock-model".to_string()),
            effort: Some("high".to_string()),
            level: Some("high".to_string()),
        })
        .expect("the app is listening");
    crate::loop_io::drain_events(&mut app);
    assert_eq!(app.status_data().effort, Some("high"));
    assert_eq!(app.status_data().model, Some("mock-model"));

    let (commands, sent) = std::sync::mpsc::channel();
    crate::loop_io::dispatch(&mut app, &commands, Action::Effort("max".to_string()));
    assert_eq!(sent.try_recv(), Ok(Command::Effort("max".to_string())));
}

#[test]
fn help_names_the_effort_command_and_its_ladder() {
    let mut app = app();
    assert_eq!(submit(&mut app, "/help effort"), None);
    let line = last_line(&app);
    assert!(
        line.contains("/effort"),
        "the help line does not name the command: {line}"
    );
    assert!(
        line.contains("reasoning effort"),
        "the help line does not say what the command does: {line}"
    );
}

#[test]
fn every_effort_and_model_path_waits_for_the_running_turn() {
    let mut app = app();
    app.reasoning_effort = Some("low".to_string());
    app.pending_turn = true;

    // Nothing may leave the client during a turn: the daemon answers these
    // requests only after the turn releases the agent.
    assert_eq!(app.on_key(back_tab()), None);
    assert_eq!(
        last_line(&app),
        "the turn is still running; the reasoning effort changes to medium once it ends"
    );

    assert_eq!(submit(&mut app, "/effort high"), None);
    assert_eq!(
        last_line(&app),
        "the turn is still running; the reasoning effort changes to high once it ends"
    );

    assert_eq!(submit(&mut app, "/effort"), None);
    // The ladder opens on the level in force, `low`, so one step down is
    // `medium`.
    assert_eq!(app.on_key(key(KeyCode::Down)), None);
    assert_eq!(app.on_key(key(KeyCode::Enter)), None);
    assert!(app.picker.is_none(), "the list stayed open after a choice");
    assert_eq!(
        last_line(&app),
        "the turn is still running; the reasoning effort changes to medium once it ends"
    );

    assert_eq!(submit(&mut app, "/model mock-large"), None);
    assert_eq!(
        last_line(&app),
        "the turn is still running; the model changes to mock-large once it ends"
    );

    assert_eq!(submit(&mut app, "/model"), Some(Action::Models));
    app.show_models(vec!["mock-large".to_string()], None);
    assert_eq!(app.on_key(key(KeyCode::Enter)), None);
    assert_eq!(
        last_line(&app),
        "the turn is still running; the model changes to mock-large once it ends"
    );

    assert!(app.queue.is_empty(), "a change was queued as if it were a message");
    assert!(app.in_flight.is_none(), "a change was sent during the turn");
}

#[test]
fn two_cycle_presses_during_a_turn_walk_two_levels() {
    let mut app = app();
    app.reasoning_effort = Some("low".to_string());
    app.pending_turn = true;

    assert_eq!(app.on_key(back_tab()), None);
    assert_eq!(app.effort_after_turn.as_deref(), Some("medium"));
    assert_eq!(app.on_key(back_tab()), None);
    assert_eq!(
        app.effort_after_turn.as_deref(),
        Some("high"),
        "the second press did not step from the level already waiting"
    );

    // The last answer wins: a later level replaces the waiting one.
    assert_eq!(submit(&mut app, "/effort low"), None);
    assert_eq!(app.effort_after_turn.as_deref(), Some("low"));

    app.pending_turn = false;
    assert_eq!(app.take_queued(), Some(Action::Effort("low".to_string())));
    assert!(app.effort_after_turn.is_none(), "the waiting level left twice");
    assert_eq!(app.take_queued(), None, "an effort left twice");
}

#[test]
fn the_end_of_a_turn_releases_the_model_then_the_effort_then_the_message() {
    let mut app = app();
    app.pending_turn = true;
    assert_eq!(submit(&mut app, "/model mock-large"), None);
    assert_eq!(submit(&mut app, "/effort high"), None);
    assert_eq!(submit(&mut app, "hello"), None);
    assert_eq!(app.queue.len(), 1, "the message did not wait for the turn");

    // What the `TurnDone` arm leaves behind.
    app.pending_turn = false;

    assert_eq!(
        app.take_queued(),
        Some(Action::Model("mock-large".to_string())),
        "the model does not leave before the effort the switch would clear"
    );
    assert_eq!(app.take_queued(), Some(Action::Effort("high".to_string())));
    assert_eq!(
        app.effort_requested.as_deref(),
        Some("high"),
        "the cycle key does not step from the level just sent"
    );
    assert_eq!(app.take_queued(), Some(Action::Send("hello".to_string())));
    assert_eq!(app.take_queued(), None, "a second message left in the same pass");
}

#[test]
fn nothing_leaves_the_client_while_the_turn_runs() {
    let mut app = app();
    app.pending_turn = true;
    assert_eq!(submit(&mut app, "/effort high"), None);
    assert_eq!(submit(&mut app, "/model mock-large"), None);

    assert_eq!(app.take_queued(), None, "a change left during the turn");
    // The values are held, not dropped: the end of the turn still releases them.
    assert_eq!(app.effort_after_turn.as_deref(), Some("high"));
    assert_eq!(app.model_after_turn.as_deref(), Some("mock-large"));
}

#[test]
fn a_refused_send_holds_the_message_but_not_the_waiting_changes() {
    let mut app = app();
    app.pending_turn = true;
    assert_eq!(submit(&mut app, "/effort high"), None);
    assert_eq!(submit(&mut app, "/model mock-large"), None);
    assert_eq!(submit(&mut app, "hello"), None);
    app.pending_turn = false;
    app.resend_held = true;

    assert_eq!(app.take_queued(), Some(Action::Model("mock-large".to_string())));
    assert_eq!(app.take_queued(), Some(Action::Effort("high".to_string())));
    assert_eq!(app.take_queued(), None, "a refused send was retried automatically");
    assert_eq!(app.queue.len(), 1, "the held message left the queue");
}

#[test]
fn attaching_another_session_drops_the_waiting_changes() {
    let mut app = app();
    app.pending_turn = true;
    assert_eq!(submit(&mut app, "/effort high"), None);
    assert_eq!(submit(&mut app, "/model mock-large"), None);

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

    assert!(app.effort_after_turn.is_none(), "a level of the old session survived");
    assert!(app.model_after_turn.is_none(), "a model of the old session survived");
    assert_eq!(app.take_queued(), None, "a change of the old session left");
}

#[test]
fn losing_the_connection_drops_the_waiting_changes() {
    let mut app = app();
    app.pending_turn = true;
    assert_eq!(submit(&mut app, "/effort high"), None);
    assert_eq!(submit(&mut app, "/model mock-large"), None);

    app.disconnect("the event stream ended");

    assert!(
        app.effort_after_turn.is_none(),
        "a level waited for a connection that is gone"
    );
    assert!(
        app.model_after_turn.is_none(),
        "a model waited for a connection that is gone"
    );
    assert_eq!(app.take_queued(), None);
}

#[test]
fn a_level_on_a_lost_connection_is_refused_before_it_leaves() {
    let mut app = app();
    app.disconnect("the event stream ended");

    assert_eq!(submit(&mut app, "/effort high"), None);
    assert_eq!(
        last_line(&app),
        "the harness connection is gone; the reasoning effort cannot change"
    );
    assert!(app.effort_requested.is_none(), "a refused level steered the cycle key");
    assert!(
        app.effort_after_turn.is_none(),
        "a level waited for a connection that is gone"
    );

    // The list answers the same way: a choice over a lost connection refuses.
    assert_eq!(submit(&mut app, "/effort"), None);
    assert_eq!(app.on_key(key(KeyCode::Enter)), None);
    assert_eq!(
        last_line(&app),
        "the harness connection is gone; the reasoning effort cannot change"
    );
}

#[test]
fn a_report_naming_another_level_keeps_the_request_in_flight() {
    let mut app = app();
    app.reasoning_effort = Some("low".to_string());
    assert_eq!(app.on_key(back_tab()), Some(Action::Effort("medium".to_string())));

    // A report that confirms some other level is not the answer to the request
    // still in flight, so the cycle key keeps stepping from it.
    app.runtime_info(None, None, Some("low".to_string()), Some("low".to_string()));
    assert_eq!(
        app.effort_requested.as_deref(),
        Some("medium"),
        "a report of another level dropped the level the cycle key steers by"
    );
    assert_eq!(last_line(&app), "reasoning effort set to low");
    assert_eq!(app.on_key(back_tab()), Some(Action::Effort("high".to_string())));
}

#[test]
fn the_model_list_says_what_a_row_would_switch_to() {
    let mut app = app();
    app.provider = Some("mock".to_string());
    app.model = Some("mock-model".to_string());

    assert_eq!(submit(&mut app, "/model"), Some(Action::Models));
    app.show_models(vec!["mock-large".to_string()], Some("mock-model".to_string()));

    let picker = app.picker.as_ref().expect("/model opened no list");
    let rows = picker.visible();
    let row = rows
        .iter()
        .find(|row| row.value == "mock-large")
        .expect("the catalog model has no row");
    let preview = row.preview.join("\n");
    assert!(
        preview.contains("model: mock-large"),
        "the preview does not name the model: {preview}"
    );
    assert!(
        preview.contains("provider: mock"),
        "the preview does not name the provider: {preview}"
    );
    assert!(
        preview.contains("enter switches this session to mock-large"),
        "the preview does not say what Enter does: {preview}"
    );

    let current = rows
        .iter()
        .find(|row| row.value == "mock-model")
        .expect("the model in force has no row");
    assert!(current.current, "the model in force is not marked");
    assert!(
        current.preview.join("\n").contains("the current model of this session"),
        "the row of the model in force does not say so"
    );
}
