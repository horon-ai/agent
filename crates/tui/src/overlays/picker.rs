//! A filterable list drawn over the conversation.
//!
//! `/model`, `/resume` and the history search all ask the user to pick one row
//! out of a list, so this screen knows nothing about models, sessions or drafts:
//! it holds rows, the filter the user typed, and the row the selection is on.
//! The caller decides what a chosen row means, which is what keeps the keys and
//! the drawing identical for every list.
//!
//! Every key that arrives while the list is open is consumed here, including a
//! plain letter: a filter that let characters through would type into the
//! message being drafted underneath. `up`/`down`/`pageup`/`pagedown` move,
//! typing narrows, `enter` reports the selected row and `esc` closes.
//!
//! The panel itself, its preview pane and the way a row reads once it has a
//! width live in `picker_draw`; this module owns the state and the keys.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::fuzzy;

pub use super::picker_draw::{draw, panel, panel_with_preview};

/// Rows one page covers before the first frame has measured the list.
///
/// A page key pressed before anything was drawn still has to move somewhere;
/// the first draw replaces this with the rows the list actually got.
const FIRST_PAGE: usize = 10;

/// What the list is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The models this session can switch to.
    Models,
    /// The sessions stored for this working directory.
    Sessions,
    /// The drafts sent before this one.
    History,
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
    /// Lines the preview pane shows while this row is selected.
    pub preview: Vec<String>,
}

impl Row {
    /// A row that carries a value and nothing else on it.
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: String::new(),
            value: value.into(),
            current: false,
            preview: Vec::new(),
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

    /// The same row with the lines its preview pane shows.
    ///
    /// A row with no lines keeps the pane off and the list takes the whole
    /// panel, so a list whose rows say nothing extra still reads as one list.
    pub fn with_preview(mut self, lines: Vec<String>) -> Self {
        self.preview = lines;
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

    /// The title the panel is drawn with.
    pub(super) fn title(&self) -> &str {
        &self.title
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

    /// Attach preview lines to the row that carries `value`.
    ///
    /// The body of a row often arrives after the list does, so a caller fills
    /// it here once it knows what the row holds. A value the list no longer
    /// holds is ignored: attaching a late answer to whatever row moved into its
    /// place would show one row's body under another row's name.
    pub fn set_preview(&mut self, value: &str, lines: Vec<String>) {
        let Some(row) = self.rows.iter_mut().find(|row| row.value == value) else {
            return;
        };
        row.preview = lines;
    }

    /// Indices into the rows the filter leaves, in display order.
    pub(super) fn visible_indices(&self) -> &[usize] {
        &self.visible
    }

    /// Position of the selection inside the rows the filter leaves.
    pub(super) fn selection(&self) -> usize {
        self.selected
    }

    /// One row by its index in the full list.
    pub(super) fn row_at(&self, index: usize) -> Option<&Row> {
        self.rows.get(index)
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

    /// Recompute which rows the filter leaves, best match first.
    ///
    /// A row matches when the filter is a subsequence of its name or of its
    /// second column, so `/mdl` finds `model` and a bare `10:3` still finds the
    /// time; the scores order the hits. Equal scores keep the row order the
    /// caller gave, which is why the row in force stays on top while the filter
    /// is empty.
    fn refilter(&mut self) {
        let needle = self.filter.as_str();
        let mut ranked: Vec<(i32, usize)> = Vec::with_capacity(self.rows.len());
        for (index, row) in self.rows.iter().enumerate() {
            let label = fuzzy::find(needle, &row.label).map(|found| found.score);
            let detail = if row.detail.is_empty() {
                None
            } else {
                fuzzy::find(needle, &row.detail).map(|found| found.score)
            };
            let score = match (label, detail) {
                (Some(label), Some(detail)) => Some(label.max(detail)),
                (label, detail) => label.or(detail),
            };
            if let Some(score) = score {
                ranked.push((score, index));
            }
        }
        // A stable sort keeps the rows that score the same in the order the
        // caller listed them.
        ranked.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        self.visible = ranked.into_iter().map(|(_, index)| index).collect();
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
