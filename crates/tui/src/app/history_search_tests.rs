//! The trailing backslash that continues a draft, and the history search.
//!
//! Both are editor behaviour, so these drive a real [`App`] with the keystrokes
//! the terminal delivers: a draft that asks for another line must leave the
//! queue, the history and the bridge exactly as they were, and a recalled draft
//! must reach the editor without a turn starting.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use horon_agent_sdk::api::ApiEvent;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;

use crate::app::{Action, App};
use crate::overlays::picker::{Kind, draw};
use crate::sdk_bridge::AttachedSession;
use crate::theme::{Symbols, Theme};

fn app() -> App {
    let (_sender, receiver) = std::sync::mpsc::channel();
    App::new("socket · session_a".to_string(), receiver)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn enter() -> KeyEvent {
    key(KeyCode::Enter)
}

/// `ctrl+r`, the default key of `editor.history_search`.
fn search() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)
}

/// Draw the open list into a blank terminal and return its text.
fn drawn(app: &mut App) -> String {
    let area = Rect::new(0, 0, 60, 24);
    let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).expect("a test terminal");
    let theme = Theme::default();
    let symbols = Symbols::default();
    let picker = app.picker.as_mut().expect("the search is not open");
    terminal
        .draw(|frame| draw(frame, area, picker, &theme, symbols))
        .expect("draw the list");
    let buffer = terminal.backend().buffer().clone();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                text.push_str(cell.symbol());
            }
        }
        text.push('\n');
    }
    text
}

#[test]
fn an_odd_backslash_breaks_the_line_without_sending() {
    let mut app = app();
    app.input.set_text("line one\\");

    assert_eq!(app.on_key(enter()), None, "a continued draft was sent");
    assert_eq!(app.input.text(), "line one\n");
    assert!(!app.pending_turn, "a turn started for an unfinished draft");
    assert!(app.in_flight.is_none(), "the draft reached the bridge");
    assert!(app.queue.is_empty(), "the draft was queued");
    assert_eq!(app.history.drafts().count(), 0, "an unfinished draft was remembered");
}

#[test]
fn an_even_run_of_backslashes_is_sent_as_typed() {
    let mut app = app();
    app.input.set_text("a\\\\");

    assert_eq!(app.on_key(enter()), Some(Action::Send("a\\\\".to_string())));
    assert!(app.input.is_empty(), "the sent draft stayed in the editor");
}

#[test]
fn only_the_escaping_backslash_of_a_run_is_taken_off() {
    let mut app = app();
    app.input.set_text("a\\\\\\");

    assert_eq!(app.on_key(enter()), None);
    assert_eq!(app.input.text(), "a\\\\\n");
    // The newline is the last character now, so the next Enter sends the draft
    // with the two backslashes the escape left behind.
    assert_eq!(app.on_key(enter()), Some(Action::Send("a\\\\\n".to_string())));
}

#[test]
fn typing_after_a_continuation_joins_the_lines_of_one_message() {
    let mut app = app();
    app.input.set_text("line one\\");
    app.on_key(enter());
    for character in "line two".chars() {
        app.on_key(key(KeyCode::Char(character)));
    }

    assert_eq!(app.input.text(), "line one\nline two");
    assert_eq!(
        app.on_key(enter()),
        Some(Action::Send("line one\nline two".to_string()))
    );
    assert_eq!(
        app.history.drafts().next(),
        Some("line one\nline two"),
        "the sent message was not remembered as one draft"
    );
}

#[test]
fn a_backslash_inside_the_draft_is_left_alone() {
    let mut app = app();
    app.input.set_text("one\\ two");

    assert_eq!(app.on_key(enter()), Some(Action::Send("one\\ two".to_string())));
}

#[test]
fn a_backslash_before_trailing_space_does_not_break_the_line() {
    let mut app = app();
    app.input.set_text("one\\ ");

    assert_eq!(app.on_key(enter()), Some(Action::Send("one\\ ".to_string())));
}

#[test]
fn a_draft_of_only_a_backslash_breaks_the_line() {
    let mut app = app();
    app.input.set_text("\\");

    assert_eq!(app.on_key(enter()), None);
    assert_eq!(app.input.text(), "\n", "the escape was not taken off");
    assert_eq!(app.history.drafts().count(), 0);
}

#[test]
fn a_multibyte_last_line_survives_the_continuation() {
    let mut app = app();
    app.input.set_text("가\\");
    assert_eq!(app.on_key(enter()), None);
    assert_eq!(app.input.text(), "가\n", "the last character was cut");
}

#[test]
fn a_multibyte_draft_without_a_backslash_is_sent() {
    let mut app = app();
    app.input.set_text("가");
    assert_eq!(app.on_key(enter()), Some(Action::Send("가".to_string())));
}

#[test]
fn the_search_lists_the_drafts_newest_first() {
    let mut app = app();
    for draft in ["one", "two", "three"] {
        app.history.record(draft);
    }

    assert_eq!(app.on_key(search()), None, "the search sent something");
    let picker = app.picker.as_ref().expect("ctrl+r opened no list");
    assert_eq!(picker.kind(), Kind::History);
    assert_eq!(picker.len(), 3);
    let visible = picker.visible();
    let rows: Vec<(&str, &str)> = visible
        .iter()
        .map(|row| (row.label.as_str(), row.value.as_str()))
        .collect();
    // A row is named by its place in the walk, not by its text.
    assert_eq!(rows, vec![("three", "0"), ("two", "1"), ("one", "2")]);
}

#[test]
fn a_chosen_draft_fills_the_editor_without_sending() {
    let mut app = app();
    app.history.record("one");
    app.history.record("two");
    app.input.set_text("half typed");

    app.on_key(search());
    app.on_key(key(KeyCode::Down));
    assert_eq!(app.on_key(enter()), None, "recalling a draft sent it");

    assert_eq!(app.input.text(), "one");
    assert!(app.picker.is_none(), "the list stayed open");
    assert!(!app.pending_turn);
    assert!(app.in_flight.is_none());
}

#[test]
fn escape_leaves_the_draft_on_screen() {
    let mut app = app();
    app.history.record("older");
    app.input.set_text("half typed");

    app.on_key(search());
    app.on_key(key(KeyCode::Esc));

    assert!(app.picker.is_none(), "the list stayed open");
    assert_eq!(app.input.text(), "half typed");
}

#[test]
fn the_search_says_so_when_nothing_was_sent() {
    let mut app = app();
    app.on_key(search());

    let picker = app.picker.as_ref().expect("the list did not open");
    assert_eq!(picker.kind(), Kind::History);
    assert!(picker.is_empty());
    assert!(!picker.is_loading(), "the drafts are local, so the list never waits");
    assert_eq!(picker.empty_text(), "no earlier drafts");
}

#[test]
fn the_preview_body_keeps_the_list_rows_left_margin() {
    let mut app = app();
    app.history.record("first line\nsecond line");
    app.on_key(search());

    let text = drawn(&mut app);
    let body = text
        .lines()
        .find(|row| row.contains("second line"))
        .expect("the preview's second line was not drawn");
    let cells: Vec<char> = body.chars().collect();
    let border = cells
        .iter()
        .position(|cell| *cell == '│')
        .expect("the preview row has no left border");
    assert_eq!(
        cells.get(border + 1),
        Some(&' '),
        "the preview body starts against the border:\n{text}"
    );
    let after: String = cells.iter().skip(border + 2).collect();
    assert!(
        after.starts_with("second line"),
        "the body does not start one cell inside the border:\n{text}"
    );
    // The same margin has to leave the right border where it was: a body that
    // still wraps at the border-to-border width would run over it.
    assert!(
        body.trim_end().ends_with('│'),
        "the preview body ran over the right border:\n{text}"
    );
}

#[test]
fn a_recalled_draft_ends_the_history_walk() {
    let mut app = app();
    app.history.record("one");
    app.history.record("two");
    app.input.set_text("draft text");

    app.on_key(key(KeyCode::Up));
    assert_eq!(app.input.text(), "two", "up did not walk the history");

    app.on_key(search());
    app.on_key(key(KeyCode::Down));
    assert_eq!(app.on_key(enter()), None);
    assert_eq!(app.input.text(), "one");

    // Without the reset the walk would still hold `draft text` and this key
    // would put it back over the recalled draft.
    app.on_key(key(KeyCode::Down));
    assert_eq!(app.input.text(), "one", "the walk replaced the recalled draft");
}

#[test]
fn the_search_panel_names_itself_and_says_when_it_is_empty() {
    let mut app = app();
    app.on_key(search());

    let text = drawn(&mut app);
    assert!(text.contains("Search history"), "the panel has no title:\n{text}");
    assert!(
        text.contains("no earlier drafts"),
        "an empty search says nothing:\n{text}"
    );
}

#[test]
fn a_search_row_draws_the_whole_draft_below_it() {
    let mut app = app();
    app.history.record("first line\nsecond line");
    app.on_key(search());

    let text = drawn(&mut app);
    assert!(text.contains("first line"), "the row is missing:\n{text}");
    assert!(text.contains("2 lines"), "the row hides its line count:\n{text}");
    assert!(
        text.contains("second line"),
        "the preview does not show the whole draft:\n{text}"
    );
}

#[test]
fn a_multi_line_draft_counts_its_lines_and_previews_all_of_them() {
    let mut app = app();
    app.history.record("a\nb\nc");
    app.history.record("single");
    app.on_key(search());

    let picker = app.picker.as_ref().expect("the list did not open");
    let rows = picker.visible();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].label, "single");
    assert_eq!(rows[0].detail, "", "a one-line draft needs no line count");
    assert_eq!(rows[0].preview, vec!["single".to_string()]);
    assert_eq!(rows[1].label, "a");
    assert_eq!(rows[1].detail, "3 lines");
    assert_eq!(rows[1].value, "1");
    assert_eq!(rows[1].preview, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
}

#[test]
fn a_trailing_newline_counts_as_the_empty_line_it_leaves() {
    let mut app = app();
    app.history.record("done\n");
    app.on_key(search());

    let picker = app.picker.as_ref().expect("the list did not open");
    let row = picker.selected_row().expect("the recorded draft is not listed");
    assert_eq!(row.label, "done");
    assert_eq!(row.detail, "2 lines");
    assert_eq!(
        row.preview,
        vec!["done".to_string(), String::new()],
        "the empty line after the newline is not in the preview"
    );
}

#[test]
fn attaching_a_session_clears_the_effort_the_old_model_reported() {
    let mut app = app();
    app.apply_event(&ApiEvent::ModelInfo {
        session_id: "session_a".into(),
        provider: Some("anthropic".into()),
        model: Some("claude-sonnet-4".into()),
        reasoning_effort: Some("high".into()),
        auth_method: None,
    });
    assert_eq!(app.reasoning_effort.as_deref(), Some("high"));

    app.attach_session(AttachedSession {
        description: "socket · session_b".to_string(),
        session_id: "session_b".to_string(),
        status: "idle".to_string(),
        model: Some("gpt-5".to_string()),
        history: Vec::new(),
    });

    assert_eq!(app.model.as_deref(), Some("gpt-5"));
    assert!(
        app.reasoning_effort.is_none(),
        "the previous session's effort survived the switch: {:?}",
        app.reasoning_effort
    );
}
