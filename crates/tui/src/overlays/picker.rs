//! A filterable list drawn over the conversation.
//!
//! `/model`, `/effort`, `/resume`, `/skills`, `/links` and the history search
//! all ask the user to pick one row out of a list, so this screen knows nothing
//! about models, efforts, sessions, skills, links or drafts: it holds rows, the
//! filter the user typed, and the row the selection is on. `/queue` uses the
//! same screen for rows that are changed instead of chosen; its keys are
//! answered before this module sees them.
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
    /// The reasoning-effort levels the client offers.
    Efforts,
    /// The sessions stored for this working directory.
    Sessions,
    /// The recorded message boundaries this session can fork from.
    Forks,
    /// The swarm children of the attached session, and the parent it returns to.
    ///
    /// Its rows are chosen like a session's: `enter` names a session id for the
    /// attach request the session list already uses.
    Swarm,
    /// Archived sessions available to restore for this working directory.
    ArchivedSessions,
    /// The skills the harness reports for this session.
    Skills,
    /// The drafts sent before this one.
    History,
    /// The text this session can copy: the draft, answers, code blocks.
    Copies,
    /// The http(s) links in the newest assistant answers.
    Links,
    /// The background work the session is running.
    ///
    /// Its rows describe work rather than offer a value, so `delete` cancels
    /// the selected terminal and `enter` changes nothing; the keys are answered
    /// before the generic list sees them.
    Jobs,
    /// The messages waiting for the running turn to end.
    ///
    /// This list is not a choice among values: each row is a message the user
    /// can take back, delete or move, and its keys are answered before the
    /// generic list sees them.
    Queue,
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
    /// Lines the filter matches besides the name and the second column.
    ///
    /// Kept apart from the preview because a preview's first line is context
    /// the rows of one list share — a directory, a time — and matching it
    /// would make a filter on that context keep every row. Only the caller
    /// knows which of a row's lines say something about the row itself.
    search: Vec<String>,
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
            search: Vec::new(),
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

    /// The same row with lines the filter matches besides its name.
    ///
    /// A row whose lines are already searchable leaves this empty: the list
    /// then matches the name and the second column only.
    pub fn with_search(mut self, lines: Vec<String>) -> Self {
        self.search = lines;
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

    /// A list of the given rows, opened with `selected` under the cursor.
    ///
    /// A list whose row in force is not its first row — the effort ladder is
    /// the one today — opens on that row, so `enter` confirms what the reader
    /// sees marked instead of moving to the top. A position past the end of
    /// the list opens on the last row it has.
    pub fn at(kind: Kind, title: impl Into<String>, rows: Vec<Row>, empty: impl Into<String>, selected: usize) -> Self {
        let mut picker = Self::new(kind, title, rows, empty);
        picker.selected = selected.min(picker.visible.len().saturating_sub(1));
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
    pub(crate) fn title(&self) -> &str {
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

    /// The index, in the full list, of the row under the cursor.
    ///
    /// A caller that keeps its own list beside the rows — the queue does —
    /// reads the row this way: its position in `rows` is its position there.
    pub fn selected_index(&self) -> Option<usize> {
        self.visible.get(self.selected).copied()
    }

    /// The full-list index of the row `steps` away in the drawn order.
    ///
    /// Used to move a row past its neighbour: the neighbour is the row drawn
    /// next to this one, which is not the next index when a filter hides rows
    /// between them.
    pub fn neighbour_index(&self, steps: isize) -> Option<usize> {
        let at = self.selected.checked_add_signed(steps)?;
        self.visible.get(at).copied()
    }

    /// Replace the rows, keeping the row at full-list index `keep` under the
    /// cursor.
    ///
    /// [`fill`](Self::fill) sends the cursor back to the top, which is right
    /// for rows the harness just answered with and wrong for a list being
    /// edited: after a move or a delete the user is still looking at the same
    /// row, and it has to stay selected even when a filter hides it — then the
    /// top of the result is the closest thing to it.
    pub fn replace_rows(&mut self, rows: Vec<Row>, keep: usize) {
        self.rows = rows;
        self.selected = 0;
        self.refilter();
        if let Some(position) = self.visible.iter().position(|index| *index == keep) {
            self.selected = position;
        }
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
    /// A row matches when the filter is a subsequence of its name, of its
    /// second column or of one of the lines it offers for searching, so `/mdl`
    /// finds `model`, a bare `10:3` still finds the time, and a word a session
    /// said finds that session even when its title says something else. The
    /// drawn preview is not searched: its first line is context the rows share,
    /// and matching it would keep the whole list. The scores order the hits;
    /// equal scores keep the row order the caller gave, which is why an empty
    /// filter shows the list in its own order.
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
            let search = row
                .search
                .iter()
                .filter_map(|line| fuzzy::find(needle, line).map(|found| found.score))
                .max();
            let score = match (label, detail) {
                (Some(label), Some(detail)) => Some(label.max(detail)),
                (label, detail) => label.or(detail),
            };
            let score = match (score, search) {
                (Some(found), Some(search)) => Some(found.max(search)),
                (found, search) => found.or(search),
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
