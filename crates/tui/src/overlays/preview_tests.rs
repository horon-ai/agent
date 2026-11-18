//! Tests for the picker's preview pane and its fuzzy filter.
//!
//! The keys and the windowing are covered by `picker_tests`; what is checked
//! here is what the panel does with a row that carries a body: where the pane
//! goes, which row it follows, what it does with a body that does not fit, and
//! that a filter which matches nothing leaves neither a row nor a body behind.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use super::picker::{Kind, Picker, Row, draw, panel, panel_with_preview};
use crate::theme::{SymbolPreset, Symbols, Theme};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn typed(picker: &mut Picker, text: &str) {
    for character in text.chars() {
        picker.on_key(key(KeyCode::Char(character)));
    }
}

fn labels(picker: &Picker) -> Vec<String> {
    picker.visible().iter().map(|row| row.label.clone()).collect()
}

fn lines(text: &str) -> Vec<String> {
    vec![text.to_string()]
}

/// A model list whose rows carry the bodies named beside them.
fn previews(rows: &[(&str, &str)]) -> Picker {
    let rows = rows
        .iter()
        .map(|row| Row::new(row.0, row.0).with_preview(lines(row.1)))
        .collect();
    Picker::new(Kind::Models, "Select model", rows, "no models")
}

/// Draw the picker over a blank terminal of this size.
fn screen(picker: &mut Picker, width: u16, height: u16) -> Buffer {
    screen_area(picker, width, height, Rect::new(0, 0, width, height))
}

/// Draw the picker inside `area` of a larger, otherwise blank, terminal.
fn screen_area(picker: &mut Picker, width: u16, height: u16, area: Rect) -> Buffer {
    draw_with(picker, width, height, area, Symbols::default())
}

/// Draw the picker with one glyph preset, over a blank terminal of this size.
fn draw_with(picker: &mut Picker, width: u16, height: u16, area: Rect, symbols: Symbols) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => panic!("test backend failed: {error}"),
    };
    let theme = Theme::default();
    if let Err(error) = terminal.draw(|frame| {
        draw(frame, area, picker, &theme, symbols);
    }) {
        panic!("draw failed: {error}");
    }
    terminal.backend().buffer().clone()
}

fn text(buffer: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                out.push_str(cell.symbol());
            }
        }
        out.push('\n');
    }
    out
}

#[test]
fn the_preview_shows_the_selected_rows_body() {
    let mut picker = previews(&[("alpha", "alpha body"), ("beta", "beta body")]);
    let drawn = text(&screen(&mut picker, 60, 20));
    assert!(
        drawn.contains("alpha body"),
        "the selected row's body was not drawn:\n{drawn}"
    );
    assert!(
        !drawn.contains("beta body"),
        "the body of a row the selection is not on was drawn:\n{drawn}"
    );
}

#[test]
fn the_preview_follows_the_selection() {
    let mut picker = previews(&[("alpha", "alpha body"), ("beta", "beta body")]);
    picker.on_key(key(KeyCode::Down));
    let drawn = text(&screen(&mut picker, 60, 20));
    assert!(drawn.contains("beta body"), "the new row's body is missing:\n{drawn}");
    assert!(
        !drawn.contains("alpha body"),
        "the body of the row the selection left stayed on screen:\n{drawn}"
    );

    picker.on_key(key(KeyCode::Up));
    let drawn = text(&screen(&mut picker, 60, 20));
    assert!(drawn.contains("alpha body"), "the body did not come back:\n{drawn}");
    assert!(
        !drawn.contains("beta body"),
        "the body of the row left behind stayed:\n{drawn}"
    );
}

#[test]
fn a_narrowing_filter_keeps_the_preview_on_its_own_row() {
    let mut picker = previews(&[("model-alpha", "alpha body"), ("model-beta", "beta body")]);
    picker.on_key(key(KeyCode::Down));
    typed(&mut picker, "alpha");
    let drawn = text(&screen(&mut picker, 60, 20));
    assert!(
        drawn.contains("alpha body"),
        "the selection fell back to the wrong row's body:\n{drawn}"
    );
    assert!(
        !drawn.contains("beta body"),
        "the preview kept the body of the row the filter removed:\n{drawn}"
    );
}

#[test]
fn the_panel_grows_by_the_preview_it_carries() {
    let area = Rect::new(0, 0, 60, 24);
    let plain = panel(area, 3);
    assert_eq!(
        panel_with_preview(area, 3, 0),
        plain,
        "a list with no preview changed shape"
    );
    let grown = panel_with_preview(area, 3, 4);
    assert_eq!(
        grown.height,
        plain.height + 5,
        "the panel did not make room for four lines and their rule"
    );
    assert_eq!(grown.width, plain.width, "the preview changed the panel's width");
}

#[test]
fn the_preview_panel_stays_inside_its_area() {
    let area = Rect::new(0, 0, 40, 10);
    let rect = panel_with_preview(area, 30, 8);
    assert!(
        rect.right() <= area.right() && rect.bottom() <= area.bottom(),
        "the panel was drawn outside the pane: {rect:?}"
    );
}

#[test]
fn a_short_pane_keeps_every_list_row_and_drops_the_preview() {
    let rows = vec![
        Row::new("alpha", "alpha").with_preview(lines("alpha body")),
        Row::new("beta", "beta").with_preview(lines("beta body")),
        Row::new("gamma", "gamma").with_preview(lines("gamma body")),
        Row::new("delta", "delta").with_preview(lines("delta body")),
    ];
    let mut picker = Picker::new(Kind::Sessions, "Resume session", rows, "no sessions");
    let area = Rect::new(0, 0, 60, 10);
    assert_eq!(
        panel_with_preview(area, picker.len(), 1),
        panel(area, picker.len()),
        "a pane too short for both kept the preview's rows"
    );
    let drawn = text(&screen_area(&mut picker, 60, 10, area));
    assert!(
        drawn.contains("delta"),
        "the preview took a list row in a pane too short for both:\n{drawn}"
    );
    assert!(
        !drawn.contains("alpha body"),
        "the preview was drawn in a pane too short for it:\n{drawn}"
    );
}

#[test]
fn a_preview_longer_than_its_pane_ends_with_a_cut_mark() {
    let body: Vec<String> = (0..12).map(|index| format!("body line {index:02}")).collect();
    let mut picker = Picker::new(
        Kind::Sessions,
        "Resume session",
        vec![Row::new("alpha", "alpha").with_preview(body)],
        "no sessions",
    );
    let drawn = text(&screen(&mut picker, 60, 30));
    assert!(
        drawn.contains("body line 07"),
        "the pane stopped before its eight lines:\n{drawn}"
    );
    assert!(
        !drawn.contains("body line 08"),
        "a ninth preview line was drawn:\n{drawn}"
    );
    assert!(
        drawn.contains('…'),
        "the cut body said nothing about the rest:\n{drawn}"
    );
}

#[test]
fn a_narrow_preview_wraps_its_body_and_marks_the_cut() {
    let long = "a preview line long enough to need more than one row of this narrow pane";
    let mut picker = previews(&[("alpha", long)]);
    let drawn = text(&screen(&mut picker, 24, 20));
    assert!(drawn.contains("a preview"), "the body was not drawn:\n{drawn}");
    assert!(
        drawn.contains('…'),
        "the wrapped body was not marked as continuing:\n{drawn}"
    );
}

#[test]
fn the_preview_is_drawn_in_every_symbol_preset() {
    for preset in [SymbolPreset::Unicode, SymbolPreset::Nerd, SymbolPreset::Ascii] {
        let symbols = preset.symbols();
        let mut picker = previews(&[("alpha", "alpha body")]);
        let drawn = text(&draw_with(&mut picker, 60, 20, Rect::new(0, 0, 60, 20), symbols));
        assert!(
            drawn.contains("alpha body"),
            "the body is missing in the {} preset:\n{drawn}",
            preset.as_str()
        );
        let rule = symbols.rule.repeat(20);
        assert!(
            drawn.contains(&rule),
            "the rule line is missing in the {} preset:\n{drawn}",
            preset.as_str()
        );
    }
}

#[test]
fn a_preview_set_after_the_rows_arrived_is_drawn() {
    let mut picker = Picker::new(
        Kind::Sessions,
        "Resume session",
        vec![Row::new("alpha", "alpha"), Row::new("beta", "beta")],
        "no sessions",
    );
    picker.set_preview("beta", lines("late body"));
    picker.on_key(key(KeyCode::Down));
    let drawn = text(&screen(&mut picker, 60, 20));
    assert!(
        drawn.contains("late body"),
        "the body attached after the list arrived was not drawn:\n{drawn}"
    );
}

#[test]
fn a_preview_for_a_row_the_list_dropped_is_not_kept() {
    let mut picker = previews(&[("alpha", "alpha body")]);
    picker.fill(vec![Row::new("gamma", "gamma")]);
    picker.set_preview("alpha", lines("stale body"));
    let drawn = text(&screen(&mut picker, 60, 20));
    assert!(
        !drawn.contains("stale body"),
        "a body outlived the row it belonged to:\n{drawn}"
    );
}

#[test]
fn a_filter_that_matches_nothing_leaves_no_preview_behind() {
    let mut picker = previews(&[("model-alpha", "alpha body"), ("model-beta", "beta body")]);
    typed(&mut picker, "zzz");
    let drawn = text(&screen(&mut picker, 60, 20));
    assert!(
        drawn.contains("nothing matches that filter"),
        "the empty result did not say why it was empty:\n{drawn}"
    );
    assert!(
        !drawn.contains("alpha body"),
        "the body of a row the filter removed stayed on screen:\n{drawn}"
    );
}

#[test]
fn the_filter_orders_a_prefix_hit_over_a_loose_one() {
    let mut picker = Picker::new(
        Kind::Models,
        "Select model",
        vec![
            Row::new("smooth-dealer", "a"),
            Row::new("annual-model", "b"),
            Row::new("model-large", "c"),
        ],
        "no models",
    );
    typed(&mut picker, "model");
    assert_eq!(labels(&picker), vec!["model-large", "annual-model", "smooth-dealer"]);
}

#[test]
fn the_label_underlines_the_characters_the_filter_matched() {
    let mut picker = Picker::new(
        Kind::Models,
        "Select model",
        vec![Row::new("model-large", "model-large")],
        "no models",
    );
    typed(&mut picker, "ml");
    let buffer = screen(&mut picker, 60, 12);
    let mut underlined: Vec<String> = Vec::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let Some(cell) = buffer.cell((x, y)) else {
                continue;
            };
            if cell.modifier.contains(Modifier::UNDERLINED) {
                underlined.push(cell.symbol().to_string());
            }
        }
    }
    assert_eq!(
        underlined,
        vec!["m".to_string(), "l".to_string()],
        "the fuzzy hit is not marked on the row:\n{}",
        text(&buffer)
    );
}

#[test]
fn a_pane_with_no_room_draws_nothing_and_does_not_panic() {
    let mut picker = previews(&[("alpha", "alpha body"), ("beta", "beta body")]);
    let drawn = text(&screen_area(&mut picker, 60, 12, Rect::new(0, 0, 60, 0)));
    assert!(
        !drawn.contains("alpha body"),
        "a zero-height pane drew a body:\n{drawn}"
    );
    let drawn = text(&screen_area(&mut picker, 60, 12, Rect::new(0, 0, 0, 12)));
    assert!(!drawn.contains("alpha body"), "a zero-width pane drew a body:\n{drawn}");
}
