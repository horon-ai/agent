//! A filterable list drawn over the conversation.
//!
//! `/model` and `/resume` both ask the user to pick one row out of a list the
//! daemon owns, so this screen knows nothing about models or sessions: it holds
//! rows, the filter the user typed, and the row the selection is on. The caller
//! decides what a chosen row means, which is what keeps the keys and the drawing
//! identical for both lists.
//!
//! Every key that arrives while the list is open is consumed here, including a
//! plain letter: a filter that let characters through would type into the
//! message being drafted underneath. `up`/`down`/`pageup`/`pagedown` move,
//! typing narrows, `enter` reports the selected row and `esc` closes.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::theme::{Symbols, Theme};
use crate::tool_line::truncate_cells;

/// Rows one page covers before the first frame has measured the list.
///
/// A page key pressed before anything was drawn still has to move somewhere;
/// the first draw replaces this with the rows the list actually got.
const FIRST_PAGE: usize = 10;

/// Cells between a row's name and its second column.
const COLUMN_GAP: usize = 2;

/// Cells a name keeps before the second column gives way to it.
///
/// A row whose name would be cut shorter than this says nothing about itself,
/// so the time is dropped and the name takes the whole row instead.
const MIN_LABEL: usize = 10;

/// What the list is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The models this session can switch to.
    Models,
    /// The sessions stored for this working directory.
    Sessions,
}

/// One row of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// What the filter matches and what the user reads.
    pub label: String,
    /// A short second column: a time, a status, or nothing.
    pub detail: String,
    /// What selecting the row reports.
    pub value: String,
    /// Whether this row is the one in force.
    pub current: bool,
}

impl Row {
    /// A row that carries a value and nothing else on it.
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: String::new(),
            value: value.into(),
            current: false,
        }
    }

    /// The same row with a second column.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    /// The same row, marked as the one in force.
    pub fn marked(mut self, current: bool) -> Self {
        self.current = current;
        self
    }
}

/// What one keystroke asked the caller to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The list consumed the key.
    Handled,
    /// Esc: close the list and change nothing.
    Closed,
    /// Enter on a row: the value that row carries.
    Selected(String),
}

/// The state of the list screen.
pub struct Picker {
    kind: Kind,
    title: String,
    rows: Vec<Row>,
    /// Indices into `rows` that match the filter, in display order.
    visible: Vec<usize>,
    /// Position inside `visible`.
    selected: usize,
    filter: String,
    /// What the list says when it has no rows at all.
    empty: String,
    /// Whether the rows are still on their way from the harness.
    loading: bool,
    /// Rows one page key covers, measured by the last draw.
    page: usize,
}

impl Picker {
    /// A list of the given rows, ready to be filtered.
    pub fn new(kind: Kind, title: impl Into<String>, rows: Vec<Row>, empty: impl Into<String>) -> Self {
        let mut picker = Self {
            kind,
            title: title.into(),
            rows,
            visible: Vec::new(),
            selected: 0,
            filter: String::new(),
            empty: empty.into(),
            loading: false,
            page: FIRST_PAGE,
        };
        picker.refilter();
        picker
    }

    /// A list with no rows yet, asking for the ones on their way.
    pub fn loading(kind: Kind, title: impl Into<String>, empty: impl Into<String>) -> Self {
        let mut picker = Self::new(kind, title, Vec::new(), empty);
        picker.loading = true;
        picker
    }

    /// What this list is for, which decides what a chosen row means.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The filter as typed.
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Whether the rows are still being fetched.
    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// The rows the filter leaves, in display order.
    pub fn visible(&self) -> Vec<&Row> {
        self.visible.iter().filter_map(|index| self.rows.get(*index)).collect()
    }

    /// How many rows the filter leaves.
    pub fn len(&self) -> usize {
        self.visible.len()
    }

    /// Whether the filter leaves no row at all.
    pub fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }

    /// The row the selection is on, if any.
    pub fn selected_row(&self) -> Option<&Row> {
        self.visible.get(self.selected).and_then(|index| self.rows.get(*index))
    }

    /// The row drawn for the selection, as one line of text.
    pub fn selected_label(&self) -> Option<&str> {
        self.selected_row().map(|row| row.label.as_str())
    }

    /// Replace the rows with the ones the harness reported.
    pub fn fill(&mut self, rows: Vec<Row>) {
        self.rows = rows;
        self.loading = false;
        self.selected = 0;
        self.refilter();
    }

    /// The line drawn where the rows would be when there are none.
    pub fn empty_text(&self) -> &str {
        if self.loading {
            return "loading…";
        }
        if self.rows.is_empty() {
            return &self.empty;
        }
        "nothing matches that filter"
    }

    /// Rows one page key covers.
    pub fn page(&self) -> usize {
        self.page.max(1)
    }

    /// Set the page size the next page key uses.
    pub fn set_page(&mut self, rows: usize) {
        self.page = rows.max(1);
    }

    /// Translate one keystroke.
    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Esc => return Outcome::Closed,
            KeyCode::Enter => {
                return match self.selected_row() {
                    Some(row) => Outcome::Selected(row.value.clone()),
                    None => Outcome::Handled,
                };
            }
            KeyCode::Up => self.step(-1),
            KeyCode::Down => self.step(1),
            KeyCode::PageUp => self.step(-(self.page() as isize)),
            KeyCode::PageDown => self.step(self.page() as isize),
            KeyCode::Backspace => {
                self.filter.pop();
                self.reset_selection();
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter.push(character);
                self.reset_selection();
            }
            _ => {}
        }
        Outcome::Handled
    }

    /// Move the selection, keeping it inside the rows the filter leaves.
    fn step(&mut self, delta: isize) {
        if self.visible.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.visible.len() - 1;
        self.selected = if delta < 0 {
            self.selected.saturating_sub(delta.unsigned_abs())
        } else {
            (self.selected + delta as usize).min(last)
        };
    }

    /// A new filter starts reading from the top of its own result.
    fn reset_selection(&mut self) {
        self.selected = 0;
        self.refilter();
    }

    /// Recompute which rows the filter leaves.
    fn refilter(&mut self) {
        let needle = self.filter.to_lowercase();
        self.visible = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                needle.is_empty()
                    || row.label.to_lowercase().contains(&needle)
                    || row.detail.to_lowercase().contains(&needle)
            })
            .map(|(index, _)| index)
            .collect();
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
    }

    /// The first row of the window the selection has to be visible in.
    pub fn window_start(&self, height: usize) -> usize {
        if height == 0 || self.selected < height {
            return 0;
        }
        (self.selected + 1 - height).min(self.visible.len().saturating_sub(height))
    }
}

/// Where the list is drawn inside `area`, for `rows` visible rows.
///
/// The panel is sized to its rows rather than to the screen, so a short list
/// leaves the conversation readable around it, and the frame that clears the
/// cells behind the list is this exact rectangle.
pub fn panel(area: Rect, rows: usize) -> Rect {
    if area.width < 12 || area.height < 5 {
        return area;
    }
    let width = area.width.saturating_sub(4).min(78);
    let wanted = u16::try_from(rows).unwrap_or(u16::MAX).saturating_add(4);
    let height = wanted.min(area.height.saturating_sub(2)).max(5).min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
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
    let rect = panel(area, picker.visible.len());
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.core.border))
        .title(Span::styled(
            format!(" {} {} ", symbols.bullet, picker.title),
            Style::default().fg(theme.core.accent),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.width == 0 || inner.height < 3 {
        return;
    }
    let [filter_area, list_area, footer_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)]).areas(inner);

    let (filter, style) = if picker.filter.is_empty() {
        ("type to filter".to_string(), Style::default().fg(theme.core.muted))
    } else {
        (picker.filter.clone(), Style::default().fg(theme.core.text))
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" filter: ", Style::default().fg(theme.core.muted)),
            Span::styled(filter, style),
        ])),
        filter_area,
    );

    picker.set_page(list_area.height as usize);
    if picker.visible.is_empty() {
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
            let Some(index) = picker.visible.get(start + offset).copied() else {
                break;
            };
            lines.push(picker.row_line(
                index,
                start + offset == picker.selected,
                theme,
                symbols,
                list_area.width,
            ));
        }
        frame.render_widget(Paragraph::new(lines), list_area);
    }

    let footer = if picker.visible.is_empty() {
        " esc close".to_string()
    } else {
        format!(
            " {}/{} · enter select · esc close · ↑↓ move · type to filter",
            picker.selected + 1,
            picker.visible.len()
        )
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(footer, Style::default().fg(theme.core.muted)))),
        footer_area,
    );
}

impl Picker {
    /// One row as it is drawn: the label, the marker for the row in force, and
    /// the second column.
    ///
    /// The second column is flush with the row's right edge, so the times read
    /// down the list instead of starting wherever the name before them ended;
    /// the label is cut to leave it that room. A pane too narrow for both keeps
    /// the name and drops the second column, because the name is what a row is
    /// chosen by, and a clipped time says less than a clipped name.
    fn row_line(&self, index: usize, chosen: bool, theme: &Theme, symbols: Symbols, width: u16) -> Line<'static> {
        let Some(row) = self.rows.get(index) else {
            return Line::default();
        };
        let base = if chosen {
            Style::default()
                .fg(theme.core.text)
                .bg(theme.prompt.selected_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.core.text)
        };
        // The mark is the input line's own marker, repeated here so the
        // selection is the row with the mark even where the theme's selection
        // background is not drawn: a terminal without colour or with the
        // `ascii` symbols still shows which row `enter` would take.
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
        let mut spans = vec![
            Span::styled(lead, Style::default().fg(theme.core.accent)),
            Span::styled(label.clone(), base),
        ];
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
}
