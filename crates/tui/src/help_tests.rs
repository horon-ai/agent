//! The `/help` listing and the slash popup, as the client builds them.
//!
//! The listing is read against the key map in force, so most of these tests
//! change the map and read the text again; the popup tests drive a real [`App`]
//! with the same keys a user presses. The last group draws frames, because the
//! popup takes its rows from the transcript pane and a held view must not move
//! when it opens.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;
use toml::{Table, Value};

use crate::app::{Action, App};
use crate::commands::Registry;
use crate::help::{self, SlashPopup};
use crate::item_lines;
use crate::keys::{ACTIONS, Bindings, ids};
use crate::prompts::{Approval, Prompt};
use crate::theme::{SymbolPreset, Symbols, Theme};
use crate::transcript::Item;
use crate::wrap;

/// A registry over empty temporary command directories.
fn registry() -> (tempfile::TempDir, tempfile::TempDir, Registry) {
    let user = tempfile::tempdir().expect("a user command directory");
    let project = tempfile::tempdir().expect("a project directory");
    let mut state = Registry::new(user.path().to_path_buf());
    state.sync(Some(project.path()));
    state.load();
    (user, project, state)
}

/// A registry with one command file in the user directory.
fn registry_with(name: &str, text: &str) -> (tempfile::TempDir, tempfile::TempDir, Registry) {
    let (user, project, mut state) = registry();
    std::fs::write(user.path().join(format!("{name}.md")), text).expect("write the command file");
    state.load();
    (user, project, state)
}

/// An app whose registry is handed to it, so no real home directory is read.
fn app_with(state: Registry) -> App {
    let (_sender, receiver) = std::sync::mpsc::channel();
    let mut app = App::new("test".to_string(), receiver);
    app.commands = state;
    app
}

/// A default app over empty command directories.
fn app() -> App {
    let (_user, _project, state) = registry();
    app_with(state)
}

/// The key map one override table produces.
fn bindings_with(entries: &[(&str, &[&str])]) -> Bindings {
    let mut table = Table::new();
    for (id, keys) in entries {
        let array = keys.iter().map(|key| Value::String((*key).to_string())).collect();
        table.insert((*id).to_string(), Value::Array(array));
    }
    let (bindings, warnings) = Bindings::from_table(&table);
    assert!(warnings.is_empty(), "the override warned: {warnings:?}");
    bindings
}

/// The listing line whose left column is `left`.
fn row_for(rows: &[help::Row], left: &str) -> help::Row {
    rows.iter()
        .find(|row| row.left == left)
        .cloned()
        .unwrap_or_else(|| panic!("no row for `{left}`: {rows:?}"))
}

fn press(app: &mut App, code: KeyCode) -> Option<Action> {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// Type a whole line, one character at a time.
fn type_text(app: &mut App, text: &str) {
    for character in text.chars() {
        assert!(
            press(app, KeyCode::Char(character)).is_none(),
            "typing produced an action"
        );
    }
}

/// Draw one frame and return the screen as text.
fn screen(app: &mut App, width: u16, height: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = match ratatui::Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => panic!("test backend failed: {error}"),
    };
    if let Err(message) = crate::draw(&mut terminal, app) {
        panic!("draw failed: {message}");
    }
    format!("{}", terminal.backend())
}

#[test]
fn the_command_column_is_one_width() {
    let (_user, _project, state) = registry_with(
        "deploy",
        "---\ndescription: Ship the current branch\nargument-hint: <environment>\n---\nship it",
    );
    let text = state.help();
    let mut columns: Vec<usize> = Vec::new();
    for entry in state.entries() {
        let label = entry.label();
        let line = text
            .lines()
            .find(|line| line.trim_start().starts_with(&label))
            .unwrap_or_else(|| panic!("`{label}` is missing from:\n{text}"));
        if entry.description.is_empty() {
            continue;
        }
        let column = line.find(&entry.description).expect("the description is on its row");
        columns.push(column);
    }
    assert!(columns.len() > 5, "too few rows to compare: {columns:?}");
    let first = columns[0];
    assert!(
        columns.iter().all(|column| *column == first),
        "the descriptions do not share a column: {columns:?}"
    );
}

#[test]
fn a_harness_command_gets_its_own_row_with_a_description() {
    let (_user, _project, state) = registry();
    let entries = state.entries();
    let text = state.help();
    let commands: Vec<&str> = text.lines().filter(|line| line.trim_start().starts_with('/')).collect();
    assert_eq!(
        commands.len(),
        entries.len(),
        "one row per command, not a packed list:\n{text}"
    );
    for entry in &entries {
        if entry.group == crate::commands::Group::Harness {
            assert!(
                !entry.description.is_empty(),
                "`/{}` landed with no description",
                entry.name
            );
        }
    }
    let skills = row_for(&help::command_rows(&entries), "/skills");
    assert!(skills.right.contains("skills"), "unexpected row: {skills:?}");
}

#[test]
fn the_key_list_is_the_map_in_force() {
    let (_user, _project, state) = registry();
    let text = state.help();
    assert!(text.contains("keys:"), "/help has no keys section:\n{text}");
    let rows = help::key_rows(&Bindings::default());
    assert_eq!(rows.len(), ACTIONS.len(), "one row per action");
    // Every action earlier rows added is on the list, under the key it ships on.
    for key in [
        "ctrl+c",
        "esc",
        "tab",
        "ctrl+r",
        "ctrl+u",
        "ctrl+k",
        "ctrl+w",
        "alt+left",
        "alt+right",
        "up",
        "down",
        "home",
        "end",
        "pageup",
        "pagedown",
        "shift+up",
        "shift+down",
        "ctrl+end",
    ] {
        assert!(
            rows.iter().any(|row| row.left.split(", ").any(|part| part == key)),
            "`{key}` is missing from the key list: {rows:?}"
        );
    }
    let interrupt = rows
        .iter()
        .find(|row| row.right == "interrupt the running turn and nothing else")
        .expect("the interrupt row");
    assert_eq!(interrupt.left, "esc");
}

#[test]
fn a_remapped_key_is_the_one_the_listing_shows() {
    let bindings = bindings_with(&[(ids::APP_INTERRUPT, &["ctrl+x"])]);
    let rows = help::key_rows(&bindings);
    let interrupt = rows
        .iter()
        .find(|row| row.right == "interrupt the running turn and nothing else")
        .expect("the interrupt row");
    assert_eq!(interrupt.left, "ctrl+x");
    assert!(
        !rows
            .iter()
            .any(|row| row.left == "esc" && row.right.contains("interrupt")),
        "the old key survived the remap: {rows:?}"
    );
    // The key it moved off is still listed for the action that keeps it.
    let dismiss = rows
        .iter()
        .find(|row| row.right == "deny the parked call; a question stays open")
        .expect("the dismiss row");
    assert_eq!(dismiss.left, "esc");
}

#[test]
fn an_unbound_action_says_unbound() {
    let bindings = bindings_with(&[(ids::APP_SCROLL_BOTTOM, &[])]);
    let rows = help::key_rows(&bindings);
    let bottom = rows
        .iter()
        .find(|row| row.right == "follow the end of the conversation again")
        .expect("the scroll-bottom row");
    assert_eq!(bottom.left, "(unbound)");
}

#[test]
fn every_action_has_a_row_and_a_description() {
    let rows = help::key_rows(&Bindings::default());
    let ids: Vec<&str> = ACTIONS.iter().map(|spec| spec.id).collect();
    for (row, id) in rows.iter().zip(ids.iter()) {
        assert!(!row.right.is_empty(), "`{id}` has no description: {row:?}");
        assert!(!row.left.is_empty(), "`{id}` has no key column");
    }
}

#[test]
fn the_popup_opens_only_for_a_bare_prefix() {
    assert_eq!(help::prefix("/he"), Some("he"));
    assert_eq!(help::prefix("/"), Some(""));
    assert_eq!(help::prefix("  /he"), Some("he"), "a leading space is not a word");
    assert_eq!(
        help::prefix("/help now"),
        None,
        "the argument position completes nothing"
    );
    assert_eq!(
        help::prefix("send /help"),
        None,
        "a slash inside prose is not a command"
    );
    assert_eq!(
        help::prefix("//literal"),
        None,
        "the escape is a message, not a command"
    );
    assert_eq!(help::prefix("hello"), None);
    assert_eq!(help::prefix("/he\nmore"), None, "a draft's second line is a message");
}

#[test]
fn matches_are_sorted_and_skip_a_shadowed_file() {
    let (_user, _project, state) = registry_with("help", "a file the built-in hides");
    let entries = state.entries();
    let matches = help::popup_matches(&entries, "he");
    let names: Vec<&str> = matches.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(
        names.first().copied(),
        Some("help"),
        "the shadowed file is not offered, and the prefix hit comes first: {matches:?}"
    );
    assert_eq!(
        names.iter().filter(|name| **name == "help").count(),
        1,
        "`help` is offered once: {names:?}"
    );
    let all = help::popup_matches(&entries, "");
    let mut sorted: Vec<&str> = all.iter().map(|entry| entry.name.as_str()).collect();
    let offered = sorted.clone();
    sorted.sort_unstable();
    assert_eq!(offered, sorted, "the popup is in name order");
    assert_eq!(
        offered.iter().filter(|name| **name == "help").count(),
        1,
        "`help` is offered once: {offered:?}"
    );
}

#[test]
fn the_window_keeps_the_selection_on_screen() {
    assert_eq!(help::window(25, 0, 8), (0, 8));
    assert_eq!(help::window(25, 7, 8), (0, 8));
    assert_eq!(help::window(25, 12, 8), (5, 8), "the last chosen row is the last drawn");
    assert_eq!(help::window(25, 24, 8), (17, 8));
    assert_eq!(help::window(3, 2, 8), (0, 3), "a short list shows what it has");
    assert_eq!(help::window(0, 0, 8), (0, 0));
}

#[test]
fn popup_rows_are_cut_to_the_width() {
    let (_user, _project, state) = registry();
    let entries = state.entries();
    let matches = help::popup_matches(&entries, "");
    let theme = Theme::default();
    let width = 30;
    let lines = help::popup_lines(
        &matches,
        1,
        0,
        4,
        width,
        help::Look {
            needle: "",
            symbols: Symbols::default(),
        },
        &theme,
    );
    assert_eq!(lines.len(), 4);
    for line in &lines {
        let text: String = line.spans.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(
            unicode_width::UnicodeWidthStr::width(text.as_str()),
            width,
            "a popup row does not fill its width: {text:?}"
        );
    }
    let longest = matches
        .iter()
        .max_by_key(|entry| entry.description.len())
        .expect("a description");
    let drawn: String = lines[1].spans.iter().map(|span| span.content.as_ref()).collect();
    assert!(
        !drawn.contains(longest.description.as_str()) || longest.description.len() < 20,
        "the row was not cut: {drawn:?}"
    );
}

#[test]
fn a_notice_keeps_its_hanging_indent() {
    let theme = Theme::default();
    let look = item_lines::Look {
        theme: &theme,
        symbols: Symbols::default(),
        width: 40,
        markdown: true,
    };
    let text = "the transcript pane draws the newest rows first and stops when it fills";
    let rows: Vec<String> = wrap::wrap_rows(item_lines::lines(&Item::Notice(text.to_string()), &look), 40)
        .iter()
        .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect::<String>())
        .collect();
    assert!(rows.len() > 1, "the notice did not wrap: {rows:?}");
    assert!(rows[0].contains('•'), "no bullet: {rows:?}");
    for row in rows.iter().skip(1) {
        assert!(
            row.starts_with("  "),
            "a continuation row started at the first column: {row:?}"
        );
    }
}

#[test]
fn the_popup_offers_the_commands_a_prefix_matches() {
    let mut app = app();
    type_text(&mut app, "/re");
    assert!(app.slash.is_open(), "the popup is closed for a matching prefix");
    let names: Vec<String> = app.slash.matches().iter().map(|entry| entry.name.clone()).collect();
    assert_eq!(
        names.iter().take(5).cloned().collect::<Vec<_>>(),
        vec!["refresh_model_list", "remote", "resume", "resume_all", "review"],
        "the prefix hits keep their order and come first: {names:?}"
    );
    assert!(
        names.iter().skip(5).all(|name| !name.starts_with("re")),
        "a prefix match was ranked below a fuzzy one: {names:?}"
    );
    assert!(
        names.len() > 5,
        "the letters `re` appear inside a command, so a fuzzy row was expected: {names:?}"
    );
    // A slash that matches nothing offers nothing.
    app.input.clear();
    type_text(&mut app, "/zzzz");
    assert!(!app.slash.is_open(), "a prefix with no match opened a popup");
    assert!(app.slash.is_empty());
}

#[test]
fn a_closed_popup_leaves_up_and_down_to_the_history() {
    let mut app = app();
    app.history.record("earlier line");
    type_text(&mut app, "/re");
    assert!(app.slash.is_open());
    assert_eq!(press(&mut app, KeyCode::Down), None);
    assert_eq!(app.input.text(), "/re", "down moved the draft, not the selection");
    assert_eq!(app.slash.selected(), 1);
    // Escape closes the list and does not reach the interrupt: the turn is
    // still running and nothing was cancelled.
    app.pending_turn = true;
    assert_eq!(press(&mut app, KeyCode::Esc), None);
    assert!(!app.slash.is_open());
    assert!(
        app.pending_turn,
        "escape interrupted the turn instead of closing the popup"
    );
    // With the popup gone, the same key walks the history again.
    assert_eq!(press(&mut app, KeyCode::Up), None);
    assert_eq!(app.input.text(), "earlier line");
}

#[test]
fn the_selection_stops_at_both_ends_and_tab_fills_it() {
    let mut app = app();
    type_text(&mut app, "/re");
    for _ in 0..4 {
        press(&mut app, KeyCode::Down);
    }
    assert_eq!(app.slash.selected(), 4, "down stopped before the last row");
    assert_eq!(press(&mut app, KeyCode::Tab), None);
    assert_eq!(app.input.text(), "/review");
    assert!(!app.slash.is_open(), "the popup stayed open after the fill");
    app.input.set_text("/re");
    app.sync_slash();
    assert!(app.slash.is_open(), "editing the draft did not bring the popup back");
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.input.text(), "/refresh_model_list", "the first row is the default");
}

#[test]
fn tab_fills_the_row_a_fuzzy_match_offers() {
    let mut app = app();
    type_text(&mut app, "/mdl");
    assert!(app.slash.is_open(), "the letters of `/model` did not offer it");
    assert_eq!(
        app.slash.selected_name(),
        Some("model"),
        "the marker is not on the row Tab would fill"
    );
    assert_eq!(press(&mut app, KeyCode::Tab), None);
    assert_eq!(app.input.text(), "/model", "tab did not fill the offered row");
    assert!(
        !app.slash.is_open(),
        "the popup stayed open over the command it had just filled"
    );
}

#[test]
fn a_finished_name_with_a_longer_sibling_keeps_the_list_open() {
    let mut app = app();
    type_text(&mut app, "/resume");
    assert!(
        app.slash.is_open(),
        "`/resume` has a sibling, so the list must stay for it"
    );
    assert_eq!(
        app.slash.selected_name(),
        Some("resume"),
        "the exact name is not the first row"
    );
    let names: Vec<String> = app.slash.matches().iter().map(|entry| entry.name.clone()).collect();
    assert!(
        names.iter().any(|name| name == "resume_all"),
        "the longer name left the list: {names:?}"
    );
    assert_eq!(press(&mut app, KeyCode::Down), None);
    assert_eq!(press(&mut app, KeyCode::Tab), None);
    assert_eq!(app.input.text(), "/resume_all");
    assert!(!app.slash.is_open(), "the filled name left the popup open");
}

#[test]
fn the_underscore_sibling_offers_its_longer_name() {
    let mut app = app();
    type_text(&mut app, "/subagent");
    assert!(
        app.slash.is_open(),
        "`/subagent` has a sibling, so the list must stay for it"
    );
    assert_eq!(app.slash.selected_name(), Some("subagent"));
    assert_eq!(press(&mut app, KeyCode::Down), None);
    assert_eq!(press(&mut app, KeyCode::Tab), None);
    assert_eq!(app.input.text(), "/subagent_model");
}

#[test]
fn a_finished_name_closes_the_list_even_with_fuzzy_rows() {
    let mut app = app();
    type_text(&mut app, "/review");
    assert!(
        !app.slash.is_open(),
        "a row the letters only resemble kept a finished name open"
    );
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.input.text(), "/review", "tab moved a finished name");
}

#[test]
fn the_selection_stays_inside_a_shorter_match_list() {
    let mut app = app();
    type_text(&mut app, "/re");
    for _ in 0..5 {
        press(&mut app, KeyCode::Down);
    }
    assert_eq!(app.slash.selected(), 5);
    // A letter no command holds empties the list, and the row number goes with
    // it instead of pointing past the end of the next list.
    type_text(&mut app, "z");
    assert!(!app.slash.is_open(), "a letter no command holds left the popup open");
    assert_eq!(app.slash.selected(), 0, "the old row outlived the list");
    assert_eq!(press(&mut app, KeyCode::Backspace), None);
    assert!(app.slash.is_open(), "the rows did not come back");
    assert_eq!(app.slash.selected(), 0, "the old row number outlived its list");
}

#[test]
fn the_selected_popup_row_carries_the_prompt_marker_in_every_preset() {
    let (_user, _project, state) = registry();
    let entries = state.entries();
    let matches = help::popup_matches(&entries, "re");
    assert!(matches.len() > 2, "too few rows to compare: {matches:?}");
    let theme = Theme::default();
    for preset in [SymbolPreset::Unicode, SymbolPreset::Nerd, SymbolPreset::Ascii] {
        let symbols = preset.symbols();
        let lines = help::popup_lines(&matches, 1, 0, 3, 40, help::Look { needle: "re", symbols }, &theme);
        let text = |index: usize| -> String { lines[index].spans.iter().map(|span| span.content.as_ref()).collect() };
        assert!(
            text(1).starts_with(&format!("{} ", symbols.prompt_marker)),
            "the chosen row lost its marker in {}: {:?}",
            preset.as_str(),
            text(1)
        );
        let blank = " ".repeat(usize::from(symbols.prompt_marker_width()) + 1);
        assert!(
            text(0).starts_with(&blank) && text(2).starts_with(&blank),
            "an unchosen row did not line up under the marker in {}: {:?} {:?}",
            preset.as_str(),
            text(0),
            text(2)
        );
    }
}

#[test]
fn the_matched_characters_of_a_name_are_underlined() {
    let (_user, _project, state) = registry();
    let entries = state.entries();
    let matches = help::popup_matches(&entries, "mdl");
    assert_eq!(
        matches.first().map(|entry| entry.name.as_str()),
        Some("model"),
        "the rows are not ranked as expected: {matches:?}"
    );
    let theme = Theme::default();
    let lines = help::popup_lines(
        &matches,
        0,
        0,
        1,
        40,
        help::Look {
            needle: "mdl",
            symbols: Symbols::default(),
        },
        &theme,
    );
    let marked: String = lines[0]
        .spans
        .iter()
        .filter(|span| span.style.add_modifier.contains(Modifier::UNDERLINED))
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(
        marked, "mdl",
        "the underline is not on the characters the needle matched"
    );
}

#[test]
fn a_prompt_takes_the_keys_from_the_popup() {
    let mut app = app();
    type_text(&mut app, "/re");
    assert!(app.slash.is_open());
    app.show_prompt(Prompt::Approval(Approval {
        request_id: "req-1".to_string(),
        action: "bash".to_string(),
        always: false,
    }));
    assert!(!app.slash.is_open(), "the popup stayed on screen under a prompt");
    let answer = press(&mut app, KeyCode::Char('y'));
    assert!(
        matches!(answer, Some(Action::Approval { approved: true, .. })),
        "the prompt did not get its key: {answer:?}"
    );
}

#[test]
fn a_submitted_line_closes_the_popup() {
    let mut app = app();
    type_text(&mut app, "/he");
    assert!(app.slash.is_open());
    assert!(press(&mut app, KeyCode::Enter).is_none());
    assert!(app.input.is_empty());
    assert!(!app.slash.is_open(), "the popup survived the send");
    assert!(app.slash.is_empty());
}

#[test]
fn the_popup_keeps_a_held_view_where_it_was() {
    let mut app = app();
    for index in 0..40 {
        app.transcript.push_notice(format!("line {index:02}"));
    }
    screen(&mut app, 40, 20);
    app.scroll.up(5);
    screen(&mut app, 40, 20);
    let held = app.scroll.offset();
    assert!(held > 0, "the view did not leave the bottom");
    type_text(&mut app, "/");
    assert!(app.slash.is_open());
    let drawn = screen(&mut app, 40, 20);
    assert_eq!(app.scroll.offset(), held, "the popup moved the held view");
    assert!(
        drawn.contains(&format!("{held} rows below")),
        "the indicator lost the count:\n{drawn}"
    );
}

#[test]
fn a_following_frame_with_the_popup_open_still_starts_a_new_baseline() {
    let mut app = app();
    for index in 0..40 {
        app.transcript.push_notice(format!("line {index:02}"));
    }
    type_text(&mut app, "/");
    assert!(app.slash.is_open());
    // The frame that follows the end resets the baseline even while the popup
    // holds rows it would otherwise have drawn.
    screen(&mut app, 40, 20);
    assert!(app.scroll.is_following());
    for index in 0..10 {
        app.transcript.push_notice(format!("late {index:02}"));
    }
    screen(&mut app, 40, 20);
    assert!(app.scroll.is_following());
    app.scroll.up(3);
    screen(&mut app, 40, 20);
    assert_eq!(
        app.scroll.offset(),
        3,
        "rows that arrived while the view followed moved the held view"
    );
}

#[test]
fn the_help_text_keeps_command_files_and_their_hints() {
    let (user, _project, mut state) = registry();
    std::fs::write(
        user.path().join("deploy.md"),
        "---\ndescription: Ship the current branch\nargument-hint: <environment>\n---\nship it",
    )
    .expect("write the command file");
    state.load();
    let text = state.help();
    assert!(
        text.contains("/deploy <environment>"),
        "the argument hint is missing:\n{text}"
    );
    assert!(text.contains("Ship the current branch"), "{text}");
    assert!(!text.contains("shadowed"), "a fresh file is not shadowed:\n{text}");
    assert!(
        !text.contains("no command files"),
        "the hint about adding files is printed beside a file:\n{text}"
    );
}

#[test]
fn a_slash_popup_holds_nothing_before_a_draft_asks() {
    let state = Registry::default();
    let mut popup = SlashPopup::new();
    assert!(!popup.is_open());
    assert!(popup.is_empty());
    popup.sync(&state, "hello");
    assert!(!popup.is_open());
    assert_eq!(popup.selected_name(), None);
    assert_eq!(popup.visible(), (0, 0));
}

#[test]
fn a_draft_that_names_a_command_offers_nothing() {
    let (_user, _project, state) = registry();
    let mut popup = SlashPopup::new();
    popup.sync(&state, "/he");
    assert_eq!(
        popup.selected_name(),
        Some("help"),
        "an unfinished name still offers its command first"
    );
    assert!(popup.is_open());
    popup.sync(&state, "/help");
    assert!(
        popup.is_empty(),
        "a finished name offered a list of the one row it already names"
    );
    popup.sync(&state, "/help ");
    assert!(popup.is_empty(), "the argument position offers nothing");
}

#[test]
fn a_path_is_a_prefix_with_no_commands_behind_it() {
    // The popup reads the draft's first word, so a path is a prefix that names
    // no command: it opens nothing and leaves the draft alone.
    assert_eq!(help::prefix("/tmp/htui"), Some("tmp/htui"));
    let (_user, _project, state) = registry();
    let mut popup = SlashPopup::new();
    popup.sync(&state, "/tmp/htui");
    assert!(popup.is_empty(), "a path offered commands");
}

/// One listing row rendered at `width`, as the rows the pane would draw.
fn listing_rows(line: &str, width: u16) -> Vec<String> {
    let theme = Theme::default();
    let look = item_lines::Look {
        theme: &theme,
        symbols: Symbols::default(),
        width,
        markdown: false,
    };
    wrap::wrap_rows(item_lines::lines(&Item::Notice(line.to_string()), &look), width)
        .iter()
        .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect::<String>())
        .collect()
}

#[test]
fn a_wrapped_key_row_hangs_under_its_description() {
    let rows = listing_rows(
        "  ctrl+u  delete from the start of the line to the cursor, which is a long sentence",
        40,
    );
    assert!(rows.len() > 1, "the row did not wrap: {rows:?}");
    let description = rows
        .iter()
        .position(|row| row.contains("delete from"))
        .expect("the description row");
    for row in rows.iter().skip(description + 1) {
        assert_eq!(
            row.len() - row.trim_start().len(),
            12,
            "a wrapped row did not hang under the description column: {row:?}"
        );
    }
}

#[test]
fn a_key_row_too_wide_for_the_pane_hangs_under_the_keys() {
    // The key column of the real listing is this wide, and at forty columns its
    // description cannot have a column of its own.
    let rows = listing_rows(
        "  shift+enter, alt+enter, ctrl+j  start a new line without sending the draft",
        40,
    );
    assert!(rows.len() > 1, "the row did not wrap: {rows:?}");
    for row in rows.iter().skip(1) {
        let indent = row.len() - row.trim_start().len();
        assert!(
            indent >= 4,
            "a wrapped row slid back past the keys, which start at column 4: {row:?}"
        );
    }
}

#[test]
fn escape_closes_the_popup_even_when_the_interrupt_key_moved() {
    let mut app = app();
    app.bindings = bindings_with(&[(ids::APP_INTERRUPT, &["ctrl+x"])]);
    type_text(&mut app, "/re");
    assert!(app.slash.is_open(), "the popup did not open");
    assert_eq!(press(&mut app, KeyCode::Esc), None);
    assert!(
        !app.slash.is_open(),
        "esc left the popup open after `app.interrupt` was moved to another key"
    );

    // The key the action moved to closes it as well: both mean "not this".
    app.input.clear();
    type_text(&mut app, "/re");
    assert!(app.slash.is_open(), "the popup did not open again");
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)),
        None
    );
    assert!(!app.slash.is_open(), "the rebound interrupt key left the popup open");
}

#[test]
fn a_long_multi_line_draft_never_opens_the_popup() {
    let mut app = app();
    let mut draft = String::from("/mo\n");
    draft.push_str(&"a line of a long message\n".repeat(200));
    app.input.set_text(&draft);
    app.sync_slash();
    assert!(
        !app.slash.is_open(),
        "a multi-line draft opened the popup after the first line"
    );
    // The next keystroke keeps it closed, and the draft is still whole: the
    // popup's check reads no more of it than the first word.
    type_text(&mut app, "!");
    assert!(!app.slash.is_open());
    assert!(app.input.text().starts_with("/mo\n"), "the draft was lost");

    // A one-line draft that is a command opens it again.
    app.input.set_text("/mo");
    app.sync_slash();
    assert!(app.slash.is_open(), "the popup did not come back");
}

#[test]
fn the_fingerprint_reads_the_first_word_and_the_length() {
    use crate::input::fingerprint;
    // Only the first word can change what the popup offers, so a draft that
    // differs after it hashes the same: that is what keeps the per-frame check
    // from walking a pasted paragraph.
    let one = format!("/command {}", "x".repeat(10_000));
    let other = format!("/command {}", "y".repeat(10_000));
    let hash = |draft: &str| fingerprint(draft.chars().count(), draft.chars());
    assert_eq!(hash(&one), hash(&other));
    // A different first word, or a different length, is a different draft.
    assert_ne!(hash("/command"), hash("/commit"));
    assert_ne!(hash("/mo"), hash("/mo "));
    // The two drafts above hold ten thousand characters each and differ only
    // past their first word, and they hash alike: the scan stops at that word,
    // which is what bounds the per-frame check.
    let long = format!("/mo{}", "x".repeat(100_000));
    assert_ne!(hash("/mo"), hash(&long), "the length is part of the answer");
}
