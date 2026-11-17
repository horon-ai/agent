//! The picker in the client: `/model` and `/resume` from the slash line to the
//! state a chosen row leaves behind.
//!
//! These drive a real [`App`] the way the interface does, so a pass means the
//! command opened the list, the key that should be the list's stayed the
//! list's, and a session switch really cleared what belonged to the session
//! that was open.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{Action, App};
use crate::overlays::picker::{Kind, Picker, Row};
use crate::prompts::Prompt;
use crate::sdk_bridge::{AttachedSession, SessionRow};
use crate::transcript::Item;
use horon_agent_sdk::HistoryMessage;

fn app() -> App {
    let (_sender, receiver) = std::sync::mpsc::channel();
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

fn models(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

fn sessions(rows: &[(&str, &str, &str)]) -> Vec<SessionRow> {
    rows.iter()
        .map(|(id, label, detail)| SessionRow {
            id: (*id).to_string(),
            label: (*label).to_string(),
            detail: (*detail).to_string(),
            current: false,
            preview: Vec::new(),
        })
        .collect()
}

/// A message waiting to be sent, in the shape the queue holds.
fn queued(text: String) -> crate::app::Message {
    crate::app::Message {
        text: text.clone(),
        display: text,
    }
}

fn history(rows: &[(&str, &str)]) -> Vec<HistoryMessage> {
    rows.iter()
        .map(|(role, content)| HistoryMessage {
            role: (*role).to_string(),
            content: (*content).to_string(),
            response_stats: None,
        })
        .collect()
}

#[test]
fn model_with_no_argument_opens_the_list_and_a_row_switches() {
    let mut app = app();
    assert_eq!(submit(&mut app, "/model"), Some(Action::Models));
    let picker = app.picker.as_ref().expect("/model opened no list");
    assert_eq!(picker.kind(), Kind::Models);
    assert!(picker.is_loading(), "the list did not say it was waiting");

    // Nothing is claimed before the daemon answers: the status line still has
    // no model, and the row in force is the one the catalog marked.
    app.show_models(models(&["alpha", "beta"]), Some("alpha".to_string()));
    let picker = app.picker.as_ref().expect("the list went away");
    assert!(!picker.is_loading(), "the list is still waiting for rows");
    assert_eq!(picker.len(), 2);
    assert_eq!(picker.selected_label(), Some("alpha"));
    assert!(app.model.is_none(), "the client named a model the daemon never sent");

    assert_eq!(app.on_key(key(KeyCode::Down)), None);
    assert_eq!(app.on_key(key(KeyCode::Enter)), Some(Action::Model("beta".to_string())));
    assert!(app.picker.is_none(), "the list stayed open after a choice");
    assert!(app.model.is_none(), "the status line moved before the daemon answered");
}

#[test]
fn a_catalog_without_the_model_in_force_still_shows_it_marked() {
    let mut app = app();
    submit(&mut app, "/model");
    app.show_models(models(&["alpha", "beta"]), Some("mock-model".to_string()));
    let picker = app.picker.as_ref().expect("the list went away");
    let rows = picker.visible();
    assert_eq!(rows.len(), 3, "the model in force has no row: {rows:?}");
    assert_eq!(rows[0].label, "mock-model");
    assert!(rows[0].current, "the model in force is not marked");
    assert!(!rows[1].current && !rows[2].current);
    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        Some(Action::Model("mock-model".to_string()))
    );
}

#[test]
fn model_with_a_name_still_switches_without_a_list() {
    let mut app = app();
    assert_eq!(
        submit(&mut app, "/model gamma"),
        Some(Action::Model("gamma".to_string()))
    );
    assert!(app.picker.is_none(), "a named model opened the list");
}

#[test]
fn escape_closes_the_model_list_and_changes_nothing() {
    let mut app = app();
    app.model = Some("alpha".to_string());
    submit(&mut app, "/model");
    app.show_models(models(&["alpha", "beta"]), Some("alpha".to_string()));
    app.on_key(key(KeyCode::Down));

    assert_eq!(app.on_key(key(KeyCode::Esc)), None);
    assert!(app.picker.is_none(), "escape left the list open");
    assert_eq!(app.model.as_deref(), Some("alpha"));
}

#[test]
fn every_key_belongs_to_the_list_while_it_is_open() {
    let mut app = app();
    app.pending_turn = true;
    app.history.record("an earlier line");
    app.transcript.push_user("kept");
    app.scroll.up(4);
    submit(&mut app, "/model");
    app.show_models(models(&["alpha", "beta"]), None);

    // A letter filters the list instead of typing into the draft.
    app.on_key(key(KeyCode::Char('b')));
    assert!(
        app.input.is_empty(),
        "a letter reached the editor: {:?}",
        app.input.text()
    );
    assert_eq!(app.picker.as_ref().map(|picker| picker.len()), Some(1));

    // `down` moves the selection, not the history, and `esc` closes the list
    // instead of interrupting the turn.
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Up));
    assert_eq!(app.input.text(), "", "up reached the history");
    assert_eq!(app.on_key(key(KeyCode::Esc)), None);
    assert!(app.pending_turn, "escape cancelled the turn");
    assert_eq!(app.scroll.offset(), 4, "a key reached the scroll keys");
}

#[test]
fn resume_is_refused_while_a_turn_is_running() {
    let mut app = app();
    app.pending_turn = true;
    assert_eq!(submit(&mut app, "/resume"), None);
    assert!(app.picker.is_none(), "the session list opened during a turn");
    let noticed =
        app.transcript.items().iter().any(
            |item| matches!(item, Item::Notice(text) if text.contains("/resume") && text.contains("still running")),
        );
    assert!(noticed, "the refusal was not explained: {:?}", app.transcript.items());
}

#[test]
fn resume_lists_the_sessions_and_a_row_attaches() {
    let mut app = app();
    assert_eq!(submit(&mut app, "/resume"), Some(Action::Sessions));
    assert_eq!(app.picker.as_ref().map(|picker| picker.kind()), Some(Kind::Sessions));

    app.show_sessions(sessions(&[
        ("session_b", "a stored talk", "11-16 09:41"),
        ("session_a", "session_a", ""),
    ]));
    let picker = app.picker.as_ref().expect("the session list went away");
    assert_eq!(picker.len(), 2);
    assert_eq!(picker.selected_label(), Some("a stored talk"));

    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        Some(Action::Attach("session_b".to_string()))
    );
    assert!(app.picker.is_none(), "the list stayed open after a choice");
    assert!(
        app.transcript
            .items()
            .iter()
            .any(|item| matches!(item, Item::Notice(text) if text.contains("attaching to a stored talk"))),
        "the client did not say what it was attaching to"
    );
}

#[test]
fn attaching_clears_the_old_session_and_replays_the_stored_one() {
    let mut app = app();
    // State that belongs to the session about to be left behind.
    app.pending_turn = true;
    app.session_status = Some("generating".to_string());
    app.queue.push(queued("queued".to_string()));
    app.prompt = Some(Prompt::approval("req-1".to_string(), "bash".to_string()));
    app.resend_held = true;
    app.scroll.up(12);
    app.model = Some("old-model".to_string());
    app.provider = Some("old-provider".to_string());
    app.tokens = Some(crate::statusline::TokenTotals {
        input: 1,
        output: 2,
        cache_read: 0,
        cache_creation: 0,
    });
    app.transcript.push_user("the old session said this");

    app.attach_session(
        AttachedSession {
            description: "socket · session_b".to_string(),
            session_id: "session_b".to_string(),
            status: "idle".to_string(),
            model: Some("new-model".to_string()),
            effort: Some("high".to_string()),
            history: history(&[
                ("user", "hello from before"),
                ("assistant", "and the answer"),
                ("tool", "a tool row"),
            ]),
            title: None,
        },
        false,
    );

    assert!(!app.pending_turn, "the turn of the old session survived the switch");
    assert_eq!(app.session_status.as_deref(), Some("idle"));
    assert!(app.queue.is_empty(), "the queue of the old session survived the switch");
    assert!(app.prompt.is_none(), "a prompt of the old session survived the switch");
    assert!(app.waiting_prompts.is_empty());
    assert!(!app.resend_held);
    assert!(
        app.scroll.is_following(),
        "the view stayed held above the new conversation"
    );
    assert_eq!(app.session_id.as_deref(), Some("session_b"));
    assert_eq!(app.description, "socket · session_b");
    assert_eq!(app.model.as_deref(), Some("new-model"));
    assert_eq!(
        app.reasoning_effort.as_deref(),
        Some("high"),
        "the effort the attached session reported is not on the status line"
    );
    assert!(app.provider.is_none(), "the provider of the old session survived");
    assert!(app.tokens.is_none(), "the token counts of the old session survived");

    let items: Vec<String> = app
        .transcript
        .items()
        .iter()
        .map(|item| match item {
            Item::User(text) => format!("user:{text}"),
            Item::Assistant(text) => format!("assistant:{text}"),
            Item::Notice(text) => format!("notice:{text}"),
            other => format!("other:{other:?}"),
        })
        .collect();
    assert_eq!(
        items,
        vec![
            "user:hello from before".to_string(),
            "assistant:and the answer".to_string(),
            "notice:resumed session_b".to_string(),
        ],
        "the stored conversation was not replayed as it was stored"
    );
}

#[test]
fn a_session_with_no_metadata_is_still_a_row() {
    let mut app = app();
    submit(&mut app, "/resume");
    app.show_sessions(sessions(&[("session_ant_1794", "session_ant_1794", "idle")]));
    let picker = app.picker.as_ref().expect("the session list went away");
    assert_eq!(picker.len(), 1);
    let row = picker.selected_row().expect("the row is missing");
    assert_eq!(row.value, "session_ant_1794");
    assert_eq!(row.detail, "idle");
}

#[test]
fn an_empty_session_list_says_so_and_enter_changes_nothing() {
    let mut app = app();
    submit(&mut app, "/resume");
    app.show_sessions(Vec::new());
    let picker = app.picker.as_ref().expect("the session list went away");
    assert!(picker.is_empty());
    assert_eq!(picker.empty_text(), "no sessions in this directory");
    assert_eq!(app.on_key(key(KeyCode::Enter)), None);
    assert!(app.picker.is_some(), "enter closed the empty list");
}

#[test]
fn a_failed_list_closes_the_list_and_warns_once() {
    let mut app = app();
    submit(&mut app, "/model");
    assert!(app.picker.is_some());
    app.request_failed("could not list the models: the harness refused the request");
    assert!(
        app.picker.is_none(),
        "the list kept waiting for rows that will not come"
    );
    assert!(
        app.transcript
            .items()
            .iter()
            .any(|item| matches!(item, Item::Warning(text) if text.contains("could not list the models"))),
        "the failure was not reported: {:?}",
        app.transcript.items()
    );
}

/// The whole screen as one string, drawn through the client's own renderer.
fn screen(app: &mut App, width: u16, height: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = match ratatui::Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => panic!("test backend failed: {error}"),
    };
    if let Err(message) = crate::draw(&mut terminal, app) {
        panic!("draw failed: {message}");
    }
    let buffer = terminal.backend().buffer().clone();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            text.push_str(buffer.cell((x, y)).map(|cell| cell.symbol()).unwrap_or(" "));
        }
        text.push('\n');
    }
    text
}

#[test]
fn the_picker_clears_the_conversation_behind_it() {
    let mut app = app();
    // One long message, wider than the pane and away from the panel's own
    // columns, so any cell of it left on screen is a character beside the list.
    app.transcript.push_user(format!("{}|end marker", "X".repeat(400)));
    let before = screen(&mut app, 100, 30);
    assert!(before.contains("XXXX"), "the conversation was not drawn:\n{before}");

    submit(&mut app, "/model");
    app.show_models(models(&["alpha", "beta"]), Some("alpha".to_string()));
    let after = screen(&mut app, 100, 30);
    assert!(after.contains("Select model"), "the list was not drawn:\n{after}");
    assert!(
        !after.contains('X'),
        "a character of the conversation is still on screen beside the list:\n{after}"
    );
    assert!(
        !after.contains("end marker"),
        "the tail of the conversation is still on screen:\n{after}"
    );
}

#[test]
fn a_request_that_never_leaves_the_client_closes_the_waiting_list() {
    let (sender, receiver) = std::sync::mpsc::channel();
    drop(receiver);
    let mut app = app();
    submit(&mut app, "/model");
    assert!(app.picker.is_some());

    crate::loop_io::dispatch(&mut app, &sender, Action::Models);
    assert!(
        app.picker.is_none(),
        "the list kept waiting for a request that never left"
    );
    assert!(
        app.transcript
            .items()
            .iter()
            .any(|item| matches!(item, Item::Warning(text) if text.contains("the harness bridge stopped"))),
        "the failure was not reported: {:?}",
        app.transcript.items()
    );
}

#[test]
fn losing_the_connection_closes_a_list_that_is_waiting() {
    let mut app = app();
    submit(&mut app, "/resume");
    assert!(app.picker.is_some());
    app.disconnect("the harness connection closed");
    assert!(app.picker.is_none(), "the list outlived the connection it was asking");
    assert!(app.disconnected);
}

#[test]
fn a_failed_attach_warns_and_leaves_the_client_alone() {
    let mut app = app();
    app.request_failed("could not attach to the session: the harness does not have session gone");
    assert!(app.running, "a refused attach stopped the client");
    assert_eq!(app.exit_code, 0);
    assert!(
        app.transcript
            .items()
            .iter()
            .any(|item| matches!(item, Item::Warning(_)))
    );
}

#[test]
fn a_preview_a_row_of_another_kind_carries_is_not_a_match_target() {
    // Model rows carry no preview today, and the filter must not read one if a
    // later kind grows a body: a body is drawn beside a row, not something the
    // row is chosen by.
    let mut app = app();
    app.picker = Some(Picker::new(
        Kind::Models,
        "Select model",
        vec![Row::new("alpha", "alpha").with_preview(vec!["beta body".to_string()])],
        "no models",
    ));

    for character in "beta".chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
    let picker = app.picker.as_ref().expect("the list went away");
    assert!(
        picker.is_empty(),
        "a model row matched a preview body it never offered as searchable text"
    );
}
