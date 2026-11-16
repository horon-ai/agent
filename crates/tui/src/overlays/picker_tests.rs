//! Tests for the filterable list `/model` and `/resume` share.
//!
//! Each test drives the list with the keystrokes a person sends and, where the
//! result is only visible on screen, draws it into a `TestBackend`, so a pass
//! means the row was found, the window followed the selection and the panel
//! covered what was underneath it.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::picker::{Kind, Outcome, Picker, Row, draw, panel};
use crate::theme::{SymbolPreset, Symbols, Theme};
use unicode_width::UnicodeWidthStr;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn typed(picker: &mut Picker, text: &str) {
    for character in text.chars() {
        picker.on_key(key(KeyCode::Char(character)));
    }
}

fn rows(labels: &[&str]) -> Vec<Row> {
    labels.iter().map(|label| Row::new(*label, *label)).collect()
}

fn list(labels: &[&str]) -> Picker {
    Picker::new(Kind::Models, "Select model", rows(labels), "no models")
}

fn labels(picker: &Picker) -> Vec<String> {
    picker.visible().iter().map(|row| row.label.clone()).collect()
}

/// Draw the list over a blank screen, after painting `background` under it.
fn screen(picker: &mut Picker, width: u16, height: u16, background: Option<&str>) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => panic!("test backend failed: {error}"),
    };
    let theme = Theme::default();
    let symbols = Symbols::default();
    if let Err(error) = terminal.draw(|frame| {
        let area = frame.area();
        if let Some(text) = background {
            frame.render_widget(
                Paragraph::new(Line::from(text.to_string())).style(Style::default()),
                area,
            );
        }
        draw(frame, area, picker, &theme, symbols);
    }) {
        panic!("draw failed: {error}");
    }
    terminal.backend().buffer().clone()
}

fn buffer_text(buffer: &Buffer) -> String {
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
fn typing_narrows_the_rows_and_backspace_widens_them() {
    let mut picker = list(&["alpha", "beta", "gamma"]);
    assert_eq!(picker.len(), 3);

    typed(&mut picker, "al");
    assert_eq!(labels(&picker), vec!["alpha"]);
    assert_eq!(picker.filter(), "al");

    picker.on_key(key(KeyCode::Backspace));
    picker.on_key(key(KeyCode::Backspace));
    assert_eq!(labels(&picker), vec!["alpha", "beta", "gamma"]);
}

#[test]
fn the_filter_ignores_case_and_matches_the_second_column() {
    let mut picker = Picker::new(
        Kind::Sessions,
        "Resume session",
        vec![
            Row::new("first session", "one").with_detail("11-16 09:00"),
            Row::new("second session", "two").with_detail("11-16 10:30"),
        ],
        "no sessions",
    );
    typed(&mut picker, "SECOND");
    assert_eq!(labels(&picker), vec!["second session"]);

    for _ in 0.."SECOND".len() {
        picker.on_key(key(KeyCode::Backspace));
    }
    typed(&mut picker, "10:30");
    assert_eq!(labels(&picker), vec!["second session"]);
}

#[test]
fn arrows_and_enter_report_the_selected_row() {
    let mut picker = list(&["alpha", "beta", "gamma"]);
    assert_eq!(picker.on_key(key(KeyCode::Down)), Outcome::Handled);
    assert_eq!(picker.selected_row().map(|row| row.label.as_str()), Some("beta"));
    assert_eq!(
        picker.on_key(key(KeyCode::Enter)),
        Outcome::Selected("beta".to_string())
    );

    // The ends are clamped rather than wrapped, so a held key never jumps from
    // the last row to the first.
    picker.on_key(key(KeyCode::Up));
    picker.on_key(key(KeyCode::Up));
    assert_eq!(picker.selected_row().map(|row| row.label.as_str()), Some("alpha"));
}

#[test]
fn escape_closes_and_enter_on_an_empty_result_changes_nothing() {
    let mut picker = list(&["alpha", "beta"]);
    assert_eq!(picker.on_key(key(KeyCode::Esc)), Outcome::Closed);

    typed(&mut picker, "zzz");
    assert!(picker.is_empty());
    assert_eq!(picker.empty_text(), "nothing matches that filter");
    assert_eq!(picker.on_key(key(KeyCode::Enter)), Outcome::Handled);
    // A filter that matched nothing still owns the search: esc is the way out.
    assert_eq!(picker.filter(), "zzz");
}

#[test]
fn a_page_key_moves_by_the_rows_the_list_got() {
    let labels: Vec<String> = (0..30).map(|index| format!("model-{index:02}")).collect();
    let borrowed: Vec<&str> = labels.iter().map(String::as_str).collect();
    let mut picker = list(&borrowed);

    picker.set_page(5);
    picker.on_key(key(KeyCode::PageDown));
    assert_eq!(picker.selected_row().map(|row| row.label.as_str()), Some("model-05"));
    picker.on_key(key(KeyCode::PageDown));
    assert_eq!(picker.selected_row().map(|row| row.label.as_str()), Some("model-10"));
    picker.on_key(key(KeyCode::PageUp));
    assert_eq!(picker.selected_row().map(|row| row.label.as_str()), Some("model-05"));
}

#[test]
fn a_long_list_keeps_the_selection_on_screen() {
    let labels: Vec<String> = (0..40).map(|index| format!("row-{index:02}")).collect();
    let borrowed: Vec<&str> = labels.iter().map(String::as_str).collect();
    let mut picker = list(&borrowed);
    for _ in 0..30 {
        picker.on_key(key(KeyCode::Down));
    }
    assert_eq!(picker.selected_row().map(|row| row.label.as_str()), Some("row-30"));

    let buffer = screen(&mut picker, 60, 14, None);
    let text = buffer_text(&buffer);
    assert!(
        text.contains("row-30"),
        "the selected row scrolled out of the window:\n{text}"
    );
    assert!(
        !text.contains("row-00"),
        "the window did not follow the selection:\n{text}"
    );
    assert!(
        text.contains("31/40"),
        "the footer does not count the selection:\n{text}"
    );
}

#[test]
fn the_picker_clears_every_cell_of_the_area_it_covers() {
    let mut picker = list(&["alpha"]);
    let area = Rect::new(0, 0, 60, 14);
    let buffer = screen(&mut picker, area.width, area.height, Some(&"X".repeat(60)));

    let panel = panel(area, picker.len());
    assert!(panel.width > 0 && panel.height > 0, "the panel was not drawn");
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if (panel.x..panel.right()).contains(&x) && (panel.y..panel.bottom()).contains(&y) {
                continue;
            }
            let symbol = buffer.cell((x, y)).map(|cell| cell.symbol()).unwrap_or("");
            assert_ne!(
                symbol, "X",
                "the cell at ({x}, {y}) outside the panel still shows the conversation"
            );
        }
    }
}

#[test]
fn the_selection_keeps_its_mark_in_the_ascii_preset() {
    for preset in [SymbolPreset::Unicode, SymbolPreset::Nerd, SymbolPreset::Ascii] {
        let symbols = preset.symbols();
        let mut picker = list(&["alpha", "beta"]);
        picker.on_key(key(KeyCode::Down));
        let backend = TestBackend::new(50, 12);
        let mut terminal = match Terminal::new(backend) {
            Ok(terminal) => terminal,
            Err(error) => panic!("test backend failed: {error}"),
        };
        let theme = Theme::default();
        if let Err(error) = terminal.draw(|frame| {
            draw(frame, frame.area(), &mut picker, &theme, symbols);
        }) {
            panic!("draw failed: {error}");
        }
        let text = buffer_text(terminal.backend().buffer());
        let marked = format!(" {} beta", symbols.prompt_marker);
        assert!(
            text.contains(&marked),
            "the selected row carries no mark in the {} preset:\n{text}",
            preset.as_str()
        );
        assert!(
            !text.contains(&format!(" {} alpha", symbols.prompt_marker)),
            "an unselected row carries the mark in the {} preset",
            preset.as_str()
        );
    }
}

#[test]
fn long_and_wide_labels_stop_at_the_border() {
    let wide = format!("{}{}", "z".repeat(90), "日本語のモデル名".repeat(6));
    let mut picker = Picker::new(
        Kind::Models,
        "Select model",
        vec![Row::new(wide.clone(), "wide").with_detail("11-16 10:00".repeat(4))],
        "no models",
    );
    let area = Rect::new(0, 0, 40, 12);
    let buffer = screen(&mut picker, area.width, area.height, None);
    let panel = panel(area, picker.len());
    // The border rows carry the corners; every row between them has to show
    // the sides, which is what proves the label and the second column stopped
    // at the panel instead of painting over the frame.
    for y in panel.y + 1..panel.bottom() - 1 {
        let inside = buffer.cell((panel.x, y)).map(|cell| cell.symbol()).unwrap_or("");
        let outside = buffer
            .cell((panel.right() - 1, y))
            .map(|cell| cell.symbol())
            .unwrap_or("");
        assert_eq!(inside, "│", "the left border at row {y} was overwritten");
        assert_eq!(outside, "│", "the row at {y} painted past the right border");
    }
    assert!(buffer_text(&buffer).contains("zzz"));
}

#[test]
fn an_empty_list_says_why_it_is_empty() {
    let mut picker = Picker::new(
        Kind::Sessions,
        "Resume session",
        Vec::new(),
        "no sessions in this directory",
    );
    assert!(picker.is_empty());
    assert_eq!(picker.empty_text(), "no sessions in this directory");
    let buffer = screen(&mut picker, 60, 12, None);
    assert!(
        buffer_text(&buffer).contains("no sessions in this directory"),
        "the empty list drew no explanation"
    );
}

#[test]
fn a_list_that_is_still_loading_says_so_until_the_rows_arrive() {
    let mut picker = Picker::loading(Kind::Models, "Select model", "no models");
    assert!(picker.is_loading());
    assert_eq!(picker.empty_text(), "loading…");

    picker.fill(rows(&["alpha", "beta"]));
    assert!(!picker.is_loading());
    assert_eq!(labels(&picker), vec!["alpha", "beta"]);
}

#[test]
fn the_row_in_force_is_marked_and_still_selectable() {
    let mut picker = Picker::new(
        Kind::Models,
        "Select model",
        vec![Row::new("alpha", "alpha").marked(true), Row::new("beta", "beta")],
        "no models",
    );
    let buffer = screen(&mut picker, 60, 12, None);
    assert!(buffer_text(&buffer).contains("(current)"));
    assert_eq!(
        picker.on_key(key(KeyCode::Enter)),
        Outcome::Selected("alpha".to_string())
    );
}

#[test]
fn the_panel_stays_inside_a_small_screen() {
    let mut picker = list(&["alpha", "beta", "gamma"]);
    let area = Rect::new(0, 0, 20, 6);
    let rect = panel(area, picker.len());
    assert!(rect.right() <= area.right() && rect.bottom() <= area.bottom());
    // Too small for a list at all: the screen is left alone rather than drawn
    // over with a panel that cannot hold a border, an input and a footer.
    assert_eq!(panel(Rect::new(0, 0, 8, 4), 3), Rect::new(0, 0, 8, 4));
    let buffer = screen(&mut picker, 20, 6, None);
    assert!(buffer.cell((0, 0)).is_some());
}

#[test]
fn a_selected_row_is_drawn_with_the_selection_background() {
    let theme = Theme::default();
    let mut picker = list(&["alpha", "beta"]);
    picker.on_key(key(KeyCode::Down));
    let buffer = screen(&mut picker, 60, 12, None);
    let selected = theme.prompt.selected_bg;
    assert_ne!(selected, Color::Reset, "the theme has no selection background");
    let painted = (0..buffer.area.height)
        .any(|y| (0..buffer.area.width).any(|x| buffer.cell((x, y)).is_some_and(|cell| cell.bg == selected)));
    assert!(painted, "the selected row carries no selection background");
}

/// The text of one screen row, between two columns.
fn row_text(buffer: &Buffer, y: u16, left: u16, right: u16) -> String {
    let mut text = String::new();
    for x in left..right {
        if let Some(cell) = buffer.cell((x, y)) {
            text.push_str(cell.symbol());
        }
    }
    text
}

#[test]
fn the_second_column_sits_on_the_rows_right_edge() {
    let mut picker = Picker::new(
        Kind::Sessions,
        "Resume session",
        vec![
            Row::new("a session title long enough to need cutting", "one").with_detail("11-16 07:34"),
            Row::new("short", "two").with_detail("11-16 09:00"),
        ],
        "no sessions",
    );
    let area = Rect::new(0, 0, 40, 12);
    let buffer = screen(&mut picker, area.width, area.height, None);
    let panel = panel(area, picker.len());
    let inner = Rect {
        x: panel.x + 1,
        y: panel.y + 1,
        width: panel.width - 2,
        height: panel.height - 2,
    };
    let mut times = 0;
    for y in inner.y..inner.bottom() {
        let row = row_text(&buffer, y, inner.x, inner.right());
        if !row.contains("11-16") {
            continue;
        }
        times += 1;
        assert!(
            row.ends_with("07:34") || row.ends_with("09:00"),
            "the time is not on the right edge: {row:?}"
        );
        assert_eq!(
            UnicodeWidthStr::width(row.as_str()),
            usize::from(inner.width),
            "the second column did not reach the edge: {row:?}"
        );
    }
    assert_eq!(times, 2, "a row lost its time:\n{}", buffer_text(&buffer));
    // The long title was cut to leave the time its room, and says so.
    assert!(
        buffer_text(&buffer).contains('…'),
        "the title was not cut:\n{}",
        buffer_text(&buffer)
    );
}

#[test]
fn a_narrow_pane_keeps_the_name_and_drops_the_time() {
    let mut picker = Picker::new(
        Kind::Sessions,
        "Resume session",
        vec![Row::new("a session title", "one").with_detail("11-16 07:34")],
        "no sessions",
    );
    let area = Rect::new(0, 0, 20, 10);
    let buffer = screen(&mut picker, area.width, area.height, None);
    let text = buffer_text(&buffer);
    assert!(
        !text.contains("07:34"),
        "the time held on in a pane too narrow for it: {text}"
    );
    assert!(text.contains("a session"), "the name did not take the room: {text}");
}

#[test]
fn a_pane_too_small_for_the_list_says_so_instead_of_hiding() {
    let mut picker = list(&["alpha"]);
    let area = Rect::new(0, 0, 10, 4);
    let buffer = screen(&mut picker, area.width, area.height, Some(&"X".repeat(10)));
    let text = buffer_text(&buffer);
    assert!(text.contains("list hidd"), "the list hid itself: {text:?}");
    assert!(!text.contains('X'), "the conversation shows through: {text:?}");
}
