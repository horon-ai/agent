//! Drawing for the filterable list, its preview pane included.
//!
//! The state and the keys live in `picker`, which re-exports `draw` and the two
//! panel functions, so the call sites read the same as before the split. What
//! is here is geometry: how tall the panel is, where the list stops and the
//! preview starts, and how one row and one preview body read at a width.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::picker::{Picker, Row};
use crate::fuzzy::{self, Match};
use crate::theme::{Symbols, Theme};
use crate::tool_line::truncate_cells;
use crate::wrap::wrap_line;

/// Cells between a row's name and its second column.
const COLUMN_GAP: usize = 2;

/// Cells a name keeps before the second column gives way to it.
///
/// A row whose name would be cut shorter than this says nothing about itself,
/// so the time is dropped and the name takes the whole row instead.
const MIN_LABEL: usize = 10;

/// Preview rows the pane shows before it says there is more.
const PREVIEW_MAX: usize = 8;

/// List rows the panel keeps when the preview wants the same space.
///
/// The panel rule clamps the row count to `1..=MIN_LIST_ROWS`, so this must
/// stay above 1: `clamp` panics when its maximum is below its minimum.
const MIN_LIST_ROWS: usize = 3;

/// Where the list is drawn inside `area`, for `rows` visible rows.
///
/// The panel is sized to its rows rather than to the screen, so a short list
/// leaves the conversation readable around it, and the frame that clears the
/// cells behind the list is this exact rectangle.
pub fn panel(area: Rect, rows: usize) -> Rect {
    panel_sized(area, rows.saturating_add(4))
}

/// The same panel, grown by the preview the visible rows carry.
///
/// `preview_lines` is the longest preview among the visible rows, already
/// capped by the caller; the rule line above the preview is added here. The
/// panel never spends the list's last three rows on a preview: a body is only
/// worth showing beside a list that can still be moved through, so a pane that
/// short keeps the list and drops the preview.
pub fn panel_with_preview(area: Rect, rows: usize, preview_lines: usize) -> Rect {
    panel_shape(area, rows, preview_lines).0
}

/// The panel rectangle and the rows the preview gets inside it.
fn panel_shape(area: Rect, rows: usize, preview_lines: usize) -> (Rect, usize) {
    let block = preview_block(preview_lines);
    let plain = panel_sized(area, rows.saturating_add(4));
    if block == 0 || area.width < 12 || area.height < 5 {
        return (plain, 0);
    }
    let rect = panel_sized(area, rows.saturating_add(4).saturating_add(block));
    // Inside the border the filter line and the footer take one row each, so
    // the list and the preview share what is left. The list keeps up to three
    // rows — all of them, when it has fewer — before the preview gets space.
    let shared = usize::from(rect.height).saturating_sub(4);
    if shared < rows.clamp(1, MIN_LIST_ROWS) + block {
        return (plain, 0);
    }
    (rect, block)
}

/// Rows the preview needs: its rule line plus its lines, or nothing at all.
fn preview_block(lines: usize) -> usize {
    if lines == 0 {
        return 0;
    }
    lines.min(PREVIEW_MAX) + 1
}

/// A panel `wanted` rows tall, kept inside `area` and centred in it.
fn panel_sized(area: Rect, wanted: usize) -> Rect {
    if area.width < 12 || area.height < 5 {
        return area;
    }
    let width = area.width.saturating_sub(4).min(78);
    let height = rows_cells(wanted)
        .min(area.height.saturating_sub(2))
        .max(5)
        .min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// A row count as a layout length, saturating at the largest one a layout takes.
fn rows_cells(rows: usize) -> u16 {
    u16::try_from(rows).unwrap_or(u16::MAX)
}

/// Draw the list over `area`.
pub fn draw(frame: &mut Frame<'_>, area: Rect, picker: &mut Picker, theme: &Theme, symbols: Symbols) {
    if area.width < 12 || area.height < 5 {
        // No room for a border and one row. The list still owns every key, so
        // it says it is there instead of disappearing and swallowing them.
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " list hidden: window too small, esc closes",
                Style::default().fg(theme.core.muted),
            ))),
            area,
        );
        return;
    }
    // The list is modal, so the conversation behind it is not being read: a
    // character of it beside the panel (the panel is narrower than the pane)
    // reads as part of the list. The whole pane is cleared, then the panel is
    // drawn on the blank area.
    frame.render_widget(Clear, area);
    let rows = picker.visible_indices().len();
    let (rect, preview) = panel_shape(area, rows, wanted_preview(picker));
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.core.border))
        .title(Span::styled(
            format!(" {} {} ", symbols.bullet, picker.title()),
            Style::default().fg(theme.core.accent),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.width == 0 || inner.height < 3 {
        return;
    }
    // A pane that came up shorter than the preview was sized for gives the
    // preview up before it gives up a list row.
    let preview = preview.min(usize::from(inner.height).saturating_sub(2 + rows.clamp(1, MIN_LIST_ROWS)));
    let list_rows = usize::from(inner.height).saturating_sub(2 + preview);
    let [filter_area, list_area, preview_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(rows_cells(list_rows)),
        Constraint::Length(rows_cells(preview)),
        Constraint::Length(1),
    ])
    .areas(inner);

    let (filter, style) = if picker.filter().is_empty() {
        ("type to filter".to_string(), Style::default().fg(theme.core.muted))
    } else {
        (picker.filter().to_string(), Style::default().fg(theme.core.text))
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" filter: ", Style::default().fg(theme.core.muted)),
            Span::styled(filter, style),
        ])),
        filter_area,
    );

    picker.set_page(list_area.height as usize);
    if picker.visible_indices().is_empty() {
        // An empty filter result says why it is empty; the preview is not drawn
        // at all, so no row that the filter removed can leave its words behind.
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {}", picker.empty_text()),
                Style::default().fg(theme.core.muted),
            ))),
            list_area,
        );
    } else {
        let height = list_area.height as usize;
        let start = picker.window_start(height);
        let mut lines: Vec<Line<'static>> = Vec::new();
        for offset in 0..height {
            let Some(index) = picker.visible_indices().get(start + offset).copied() else {
                break;
            };
            let Some(row) = picker.row_at(index) else {
                break;
            };
            lines.push(row_line(
                row,
                start + offset == picker.selection(),
                picker.filter(),
                theme,
                symbols,
                list_area.width,
            ));
        }
        frame.render_widget(Paragraph::new(lines), list_area);
    }

    if preview > 0 {
        render_preview(frame, preview_area, picker, theme, symbols);
    }

    let footer = if picker.visible_indices().is_empty() {
        " esc close".to_string()
    } else {
        format!(
            " {}/{} · enter select · esc close · ↑↓ move · type to filter",
            picker.selection() + 1,
            picker.visible_indices().len()
        )
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(footer, Style::default().fg(theme.core.muted)))),
        footer_area,
    );
}

/// The longest preview the visible rows carry, capped at what the pane shows.
///
/// The largest one, not the selected row's, sizes the panel: a pane that grew
/// and shrank as the selection moved would make the list jump under the arrow
/// keys.
fn wanted_preview(picker: &Picker) -> usize {
    picker
        .visible_indices()
        .iter()
        .filter_map(|index| picker.row_at(*index))
        .map(|row| row.preview.len())
        .max()
        .unwrap_or(0)
        .min(PREVIEW_MAX)
}

/// The rule line and the selected row's body, wrapped to the pane's width.
fn render_preview(frame: &mut Frame<'_>, area: Rect, picker: &Picker, theme: &Theme, symbols: Symbols) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let rule = symbols
        .rule
        .repeat(usize::from(area.width) / UnicodeWidthStr::width(symbols.rule).max(1));
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(rule, Style::default().fg(theme.core.border)))),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let Some(row) = picker.selected_row() else {
        return;
    };
    if row.preview.is_empty() || area.height < 2 {
        return;
    }
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    let style = Style::default().fg(theme.core.muted);
    let width = usize::from(body.width);
    let mut rows: Vec<Line<'static>> = Vec::new();
    for line in &row.preview {
        rows.extend(wrap_line(&Line::from(Span::styled(line.clone(), style)), width));
    }
    // The body keeps more rows than the pane has only when the preview is
    // longer than the panel: the last kept row says so rather than stopping
    // mid-sentence as if that were the end.
    let cut = rows.len() > usize::from(body.height);
    rows.truncate(usize::from(body.height));
    if cut && let Some(last) = rows.pop() {
        rows.push(cut_row(last, width));
    }
    frame.render_widget(Paragraph::new(rows), body);
}

/// Cut one wrapped row to leave room for the mark that says it continues.
fn cut_row(row: Line<'static>, width: usize) -> Line<'static> {
    let budget = width.saturating_sub(1).max(1);
    let mut used = 0;
    let mut full = false;
    let mut last = Style::default();
    let mut spans: Vec<Span<'static>> = Vec::new();
    for span in &row.spans {
        last = span.style;
        let mut text = String::new();
        for character in span.content.chars() {
            let cells = UnicodeWidthChar::width(character).unwrap_or(0);
            if used + cells > budget {
                full = true;
                break;
            }
            used += cells;
            text.push(character);
        }
        if !text.is_empty() {
            spans.push(Span::styled(text, span.style));
        }
        if full {
            break;
        }
    }
    spans.push(Span::styled("…", last));
    Line::from(spans)
}

/// One row as it is drawn: the label, the marker for the row in force, and the
/// second column.
///
/// The second column is flush with the row's right edge, so the times read down
/// the list instead of starting wherever the name before them ended; the label
/// is cut to leave it that room. A pane too narrow for both keeps the name and
/// drops the second column, because the name is what a row is chosen by, and a
/// clipped time says less than a clipped name.
fn row_line(row: &Row, chosen: bool, filter: &str, theme: &Theme, symbols: Symbols, width: u16) -> Line<'static> {
    let base = if chosen {
        Style::default()
            .fg(theme.core.text)
            .bg(theme.prompt.selected_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.core.text)
    };
    // The mark is the input line's own marker, repeated here so the selection
    // is the row with the mark even where the theme's selection background is
    // not drawn: a terminal without colour or with the `ascii` symbols still
    // shows which row `enter` would take.
    let mark = if chosen {
        symbols.prompt_marker.to_string()
    } else {
        " ".repeat(symbols.prompt_marker.chars().count())
    };
    let lead = format!(" {mark} ");
    let lead_width = UnicodeWidthStr::width(lead.as_str());
    let current = if row.current { " (current)" } else { "" };
    let current_width = UnicodeWidthStr::width(current);
    let available = usize::from(width).saturating_sub(lead_width);
    let name_room = available.saturating_sub(current_width);
    let detail_width = UnicodeWidthStr::width(row.detail.as_str());
    let column = if detail_width > 0 && name_room > detail_width + COLUMN_GAP + MIN_LABEL {
        Some(detail_width + COLUMN_GAP)
    } else {
        None
    };
    let label_room = column.map_or(name_room, |column| name_room - column);
    let label = truncate_cells(&row.label, label_room);
    // The emphasis is what makes a fuzzy hit legible: `mdl` and `model` both
    // land on `model`, and the underline says which characters the filter used.
    let matched = fuzzy::find(filter, &row.label);
    let mut spans = vec![Span::styled(lead, Style::default().fg(theme.core.accent))];
    spans.extend(label_spans(&label, matched.as_ref(), base));
    if row.current {
        spans.push(Span::styled(current, Style::default().fg(theme.core.accent)));
    }
    if column.is_some() {
        // The pad is what puts the second column on the right edge; it
        // carries the row's own style so a selected row stays one block.
        let used = lead_width + UnicodeWidthStr::width(label.as_str()) + current_width + detail_width;
        let pad = usize::from(width).saturating_sub(used);
        spans.push(Span::styled(" ".repeat(pad), base));
        let detail = if chosen {
            base
        } else {
            Style::default().fg(theme.core.muted)
        };
        spans.push(Span::styled(row.detail.clone(), detail));
    }
    Line::from(spans)
}

/// The label split at the characters the filter matched.
///
/// The matched characters carry the row's style plus bold and underline, so
/// the mark survives a terminal that renders no colour. `label` is the label
/// already cut to the pane, and the positions are character indices into the
/// whole label; cutting keeps a prefix, so the indices still line up.
fn label_spans(label: &str, matched: Option<&Match>, base: Style) -> Vec<Span<'static>> {
    let positions: &[usize] = match matched {
        Some(found) => &found.positions,
        None => &[],
    };
    if positions.is_empty() {
        return vec![Span::styled(label.to_string(), base)];
    }
    let emphasis = base.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_matched: Option<bool> = None;
    for (index, character) in label.chars().enumerate() {
        let hit = positions.binary_search(&index).is_ok();
        if run_matched != Some(hit) {
            if let Some(previous) = run_matched {
                let style = if previous { emphasis } else { base };
                spans.push(Span::styled(std::mem::take(&mut run), style));
            }
            run_matched = Some(hit);
        }
        run.push(character);
    }
    if let Some(previous) = run_matched {
        let style = if previous { emphasis } else { base };
        spans.push(Span::styled(run, style));
    }
    spans
}
