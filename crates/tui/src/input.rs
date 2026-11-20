//! The multi-line editor under the transcript.
//!
//! It edits by character rather than byte so a cursor never lands inside a
//! multi-byte code point, and it keeps the terminal concerns (which rows exist,
//! where the caret sits, how much of a line fits) out of the app.
//!
//! The cursor stops are whole grapheme clusters as the Unicode text
//! segmentation rules define them (UAX #29), so a Hangul syllable typed as
//! conjoining jamo, an Indic conjunct, a flag built from two regional
//! indicators and a letter carrying a combining accent are one stop: moving,
//! deleting or clicking never cuts a character in half. A cluster is measured
//! in the cells the terminal gives it, and the renderer records where it drew
//! the pane so a mouse click is read against the same cells the frame used.
//!
//! The buffer is a flat run of characters in which `'\n'` separates the lines,
//! and [`Input::line_starts`] indexes the first character of each line. Keeping
//! that index up to date on every edit is what lets a frame draw only the rows
//! it shows: a draft of five thousand lines costs the same per frame as one
//! line, because no frame walks the buffer. The segmenter reads a string, so a
//! line is composed once into a cached buffer that keeps its capacity: an edit
//! rebuilds that one line, and a movement or a frame reads the cached
//! boundaries instead of collecting the line again.

use std::cell::{RefCell, RefMut};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Cells one tab becomes when a paste carries it.
///
/// A terminal draws a tab wherever its own tab stops put it, which the renderer
/// cannot know; expanding the character on the way in keeps the caret and the
/// text in the same place.
const TAB_CELLS: usize = 4;

/// Where the last frame drew the draft, so a mouse click can be read against
/// the same cells the frame used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Placement {
    /// The pane's first column.
    pub x: u16,
    /// The pane's first row.
    pub y: u16,
    /// The cells every row spends before its text: the prompt marker and the
    /// gap after it.
    pub lead: u16,
}

/// The pane and row starts a frame drew the draft with.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Drawn {
    placement: Placement,
    /// The index of the first line shown.
    first_line: usize,
    /// The index of the first character drawn on each row.
    row_starts: Vec<usize>,
}

/// The cluster boundaries and cells of one line, kept between calls.
///
/// The buffer is characters rather than a string, so a line is composed here
/// once for the segmenter and the character, byte and cell offset of every
/// cluster are kept together. The cursor, the renderer and a mouse click read
/// the same boundaries, and the buffers keep their capacity so an edit rebuilds
/// one line without allocating again.
#[derive(Debug, Clone, Default)]
struct Boundaries {
    /// The buffer revision these boundaries describe.
    revision: u64,
    /// The line they describe.
    line: usize,
    /// The index of the line's first character in the draft.
    line_start: usize,
    /// The line's text, the segmenter's view of it.
    text: String,
    /// The character index of each cluster start, relative to the line, ending
    /// with the line's character count.
    chars: Vec<usize>,
    /// The byte offset of each cluster start in `text`, ending with its length.
    bytes: Vec<usize>,
    /// The cells before each cluster, ending with the line's total cells.
    cells: Vec<usize>,
}

impl Boundaries {
    /// Which entry of `chars` is the cluster that contains `offset`.
    ///
    /// An offset that is not a boundary (a click between two cells, or an index
    /// left over from an older frame) answers with the cluster around it.
    fn cluster(&self, offset: usize) -> usize {
        self.chars.partition_point(|start| *start <= offset).saturating_sub(1)
    }
}

/// An editable draft with a cursor.
#[derive(Debug, Clone)]
pub struct Input {
    characters: Vec<char>,
    /// The index of the first character of each line, ascending, starting at 0.
    line_starts: Vec<usize>,
    cursor: usize,
    /// Bumped by every edit; the cached boundaries of an older revision are
    /// rebuilt before they are read.
    revision: u64,
    /// The boundaries of the line last measured or drawn.
    boundaries: RefCell<Boundaries>,
    /// The pane the last frame drew with, cleared by every edit so a click can
    /// never be read against text the frame did not show.
    drawn: Option<Drawn>,
}

/// Two drafts are equal when they hold the same text with the cursor in the
/// same place; the cached line and the last pane a frame drew are not part of
/// the draft.
impl PartialEq for Input {
    fn eq(&self, other: &Self) -> bool {
        self.characters == other.characters && self.line_starts == other.line_starts && self.cursor == other.cursor
    }
}

impl Eq for Input {}

impl Default for Input {
    fn default() -> Self {
        Self {
            characters: Vec::new(),
            line_starts: vec![0],
            cursor: 0,
            revision: 0,
            boundaries: RefCell::new(Boundaries::default()),
            drawn: None,
        }
    }
}

/// The rows a pane of a given size shows, and where the caret sits in them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    /// The visible rows, top to bottom, already cut to the pane's width.
    pub rows: Vec<String>,
    /// Which entry of `rows` holds the caret.
    pub cursor_row: usize,
    /// The caret's cell inside that row, counted from its first cell.
    pub cursor_cell: u16,
    /// The line the first entry of `rows` stands for.
    pub first_line: usize,
    /// The character each entry of `rows` starts at, for a click's mapping.
    pub row_starts: Vec<usize>,
}

impl Input {
    /// An empty draft.
    pub fn new() -> Self {
        Self::default()
    }

    /// The draft as text, lines joined by `'\n'`.
    pub fn text(&self) -> String {
        self.characters.iter().collect()
    }

    /// A cheap fingerprint of the draft, for callers that only need to know
    /// whether it changed.
    ///
    /// The popup compares the whole draft on every keystroke and every frame;
    /// hashing it costs one pass and no allocation, where building the string
    /// costs an allocation of the whole message.
    pub fn fingerprint(&self) -> u64 {
        // `len()` is the constant-time half: the per-frame check reads at most
        // the first word of the draft.
        fingerprint(self.characters.len(), self.characters.iter().copied())
    }

    /// Whether the draft starts with the command marker.
    pub fn starts_with_slash(&self) -> bool {
        matches!(self.characters.first(), Some('/'))
    }

    /// Replace the whole draft, leaving the cursor at its end.
    pub fn set_text(&mut self, text: &str) {
        self.clear();
        self.insert_text(text);
    }

    /// Whether nothing has been typed.
    pub fn is_empty(&self) -> bool {
        self.characters.is_empty()
    }

    /// Where the cursor sits, counted in characters.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// How many lines the draft has; an empty draft has one.
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Which line the cursor is on, counted from zero.
    pub fn line_of_cursor(&self) -> usize {
        // `line_starts` always begins with 0, so this is at least 1.
        self.line_starts.partition_point(|start| *start <= self.cursor) - 1
    }

    /// The index of the first character of a line.
    fn line_start(&self, line: usize) -> usize {
        self.line_starts[line]
    }

    /// The index just past the last character of a line, before its newline.
    fn line_end(&self, line: usize) -> usize {
        match self.line_starts.get(line + 1) {
            Some(next) => next - 1,
            None => self.characters.len(),
        }
    }

    /// The cursor column, counted in cells from the start of its line.
    ///
    /// The renderer offsets it by the prompt marker, so a wide character moves
    /// the cursor by its two cells instead of one.
    pub fn cursor_column(&self) -> u16 {
        let line = self.line_of_cursor();
        let boundaries = self.boundaries(line);
        let offset = self.cursor - boundaries.line_start;
        cells_u16(boundaries.cells[boundaries.cluster(offset)])
    }

    /// The boundaries of `line`, rebuilt after an edit changed the buffer.
    ///
    /// One cached line is enough for the ways the editor reads it: a movement
    /// stays on its line, and a frame draws one line at a time. The string and
    /// the offset tables keep their capacity, so an edit costs one pass over
    /// the edited line and no new allocation once the buffers have grown.
    fn boundaries(&self, line: usize) -> RefMut<'_, Boundaries> {
        let mut boundaries = self.boundaries.borrow_mut();
        if boundaries.revision == self.revision && boundaries.line == line && !boundaries.chars.is_empty() {
            return boundaries;
        }
        Self::fill(&mut boundaries, self, line);
        boundaries
    }

    /// Rebuild `boundaries` for `line` of `input`, reusing their capacity.
    ///
    /// The line is composed once and segmented once; the grapheme, byte and
    /// cell offsets are kept together so the cursor, the row text and a click
    /// all read the same boundaries.
    fn fill(boundaries: &mut Boundaries, input: &Input, line: usize) {
        let line_start = input.line_starts[line];
        let end = input.line_end(line);
        boundaries.revision = input.revision;
        boundaries.line = line;
        boundaries.line_start = line_start;
        boundaries.text.clear();
        boundaries
            .text
            .extend(input.characters[line_start..end].iter().copied());
        boundaries.chars.clear();
        boundaries.bytes.clear();
        boundaries.cells.clear();
        boundaries.chars.push(0);
        boundaries.bytes.push(0);
        boundaries.cells.push(0);
        let text = &boundaries.text;
        let mut characters = 0usize;
        let mut cells = 0usize;
        for (byte, grapheme) in text.grapheme_indices(true) {
            characters += grapheme.chars().count();
            cells += UnicodeWidthStr::width(grapheme);
            boundaries.chars.push(characters);
            boundaries.bytes.push(byte + grapheme.len());
            boundaries.cells.push(cells);
        }
    }

    /// Which line a character index belongs to.
    ///
    /// A newline belongs to the line it ends, so the index just past it is the
    /// first character of the next line and this one is still inside the line
    /// above.
    fn line_index(&self, at: usize) -> usize {
        self.line_starts.partition_point(|start| *start <= at) - 1
    }

    /// The first character of the cluster that contains `at`.
    ///
    /// A cluster never crosses a line break. The position just before one is
    /// itself a stop, so moving left from a line start lands there instead of
    /// on the previous line's last cluster.
    fn cluster_start(&self, at: usize) -> usize {
        let line = self.line_index(at);
        if at == self.line_end(line) {
            return at;
        }
        let boundaries = self.boundaries(line);
        let offset = at - boundaries.line_start;
        boundaries.line_start + boundaries.chars[boundaries.cluster(offset)]
    }

    /// The index just past the cluster that starts at `at`.
    ///
    /// A line break stands alone: deleting one joins lines without taking the
    /// cluster that opens the next line with it.
    fn cluster_end(&self, at: usize) -> usize {
        if at >= self.characters.len() {
            return at;
        }
        let line = self.line_index(at);
        if at == self.line_end(line) {
            return at + 1;
        }
        let boundaries = self.boundaries(line);
        let offset = at - boundaries.line_start;
        let index = boundaries.cluster(offset);
        boundaries.line_start + boundaries.chars[index + 1]
    }

    /// The rows a pane of `width` cells and at most `max_rows` lines shows.
    ///
    /// The pane follows the caret: once the draft is taller than the pane, the
    /// caret's line is the last one drawn, and the rows above it are the lines
    /// that lead to it. `max_rows` is at least one even when the caller passes
    /// zero, since the draft always shows one row.
    pub fn view(&self, width: u16, max_rows: u16) -> View {
        let room = usize::from(max_rows.max(1));
        let cursor_line = self.line_of_cursor();
        let first = cursor_line.saturating_sub(room - 1);
        let mut rows = Vec::with_capacity(room.min(self.line_count() - first));
        let mut row_starts = Vec::with_capacity(rows.capacity());
        let mut cursor_row = 0;
        let mut cursor_cell = 0;
        for (row, line) in (first..self.line_count()).take(room).enumerate() {
            let caret = line == cursor_line;
            let boundaries = self.boundaries(line);
            let offset = caret.then(|| self.cursor - boundaries.line_start);
            let (text, cell, start) = Self::row(&boundaries, offset, width);
            if caret {
                cursor_row = row;
                cursor_cell = cell;
            }
            rows.push(text);
            row_starts.push(start);
        }
        View {
            rows,
            cursor_row,
            cursor_cell,
            first_line: first,
            row_starts,
        }
    }

    /// Record where a frame drew the draft, so a later click lands on the
    /// character whose cells it covered.
    ///
    /// The renderer calls this after it has computed the layout, and every
    /// edit drops it: a click between an edit and the next frame is ignored
    /// rather than mapped through cells that no longer describe the draft.
    pub fn note_view(&mut self, placement: Placement, view: &View) {
        self.drawn = Some(Drawn {
            placement,
            first_line: view.first_line,
            row_starts: view.row_starts.clone(),
        });
    }

    /// Move the caret to a cell of a mouse click, and say whether the click
    /// landed on a row of the draft.
    ///
    /// `column` and `row` are terminal cells, the same ones the frame drew
    /// with. A click left of the text, on the prompt marker, starts the line;
    /// a click past the end of the text ends it.
    pub fn place_caret(&mut self, column: u16, row: u16) -> bool {
        let Some(drawn) = &self.drawn else {
            return false;
        };
        let Some(offset) = row.checked_sub(drawn.placement.y) else {
            return false;
        };
        let row_index = usize::from(offset);
        let Some(&start) = drawn.row_starts.get(row_index) else {
            return false;
        };
        let line = drawn.first_line + row_index;
        if line >= self.line_count() {
            return false;
        }
        let end = self.line_end(line);
        // A click cannot reach outside the row it clicked; the recorded start
        // came from the frame that drew it, and a cell inside a cluster rounds
        // to that cluster's start.
        let start = start.clamp(self.line_start(line), end);
        let cell = column.saturating_sub(drawn.placement.x.saturating_add(drawn.placement.lead));
        self.cursor = self.column_at(start, end, cell);
        true
    }

    /// One row of the pane: the part of `line` that fits `width` cells, where
    /// the caret sits inside it when it is on this line, and the character the
    /// row starts at.
    ///
    /// A line wider than the pane scrolls under the caret, which keeps the
    /// rightmost cell free for itself; a line the caret is not on is drawn from
    /// its start. Whole clusters move out of view, so a combining mark is never
    /// drawn without the letter it belongs to. `caret` is the cursor's character
    /// offset inside the line, when the caret is on it.
    fn row(boundaries: &Boundaries, caret: Option<usize>, width: u16) -> (String, u16, usize) {
        let start = boundaries.line_start;
        let room = usize::from(width);
        if room == 0 {
            return (String::new(), 0, start);
        }
        let limit = room.saturating_sub(1);
        let count = boundaries.chars.len() - 1;
        let mut first = 0;
        let mut dropped = 0;
        if let Some(offset) = caret {
            let cursor_cells = boundaries.cells[boundaries.cluster(offset)];
            while cursor_cells.saturating_sub(dropped) > limit && first < count && boundaries.chars[first] < offset {
                first += 1;
                dropped = boundaries.cells[first];
            }
        }
        let mut text = String::new();
        let mut used = 0;
        let mut at = first;
        while at < count {
            let cells = boundaries.cells[at + 1] - boundaries.cells[at];
            if used + cells > room {
                break;
            }
            text.push_str(&boundaries.text[boundaries.bytes[at]..boundaries.bytes[at + 1]]);
            used += cells;
            at += 1;
        }
        let cell = caret.map_or(0, |offset| {
            let cursor_cells = boundaries.cells[boundaries.cluster(offset)];
            cells_u16(cursor_cells.saturating_sub(dropped)).min(u16::try_from(limit).unwrap_or(u16::MAX))
        });
        (text, cell, start + boundaries.chars[first])
    }

    /// Replace `characters[at..at + removed]` with `inserted` and rebuild the
    /// line index around the edit.
    ///
    /// Every edit lands here so the index cannot drift from the buffer.
    fn splice(&mut self, at: usize, removed: usize, inserted: &[char]) {
        // A click is mapped through the cells of the last frame; the text this
        // edit produces has not been drawn yet, so those cells are dropped.
        self.drawn = None;
        // The cached boundaries describe the line before this edit.
        self.revision = self.revision.wrapping_add(1);
        let end = at + removed;
        self.characters.splice(at..end, inserted.iter().copied());
        // Starts up to and including `at` are unaffected; the ones inside the
        // replaced range belong to text that is gone; the rest move by the
        // difference in length.
        let kept = self.line_starts.partition_point(|start| *start <= at);
        let tail: Vec<usize> = self.line_starts[kept..]
            .iter()
            .copied()
            .filter(|start| *start > end)
            .map(|start| start - removed + inserted.len())
            .collect();
        self.line_starts.truncate(kept);
        for (offset, character) in inserted.iter().enumerate() {
            if *character == '\n' {
                self.line_starts.push(at + offset + 1);
            }
        }
        self.line_starts.extend(tail);
    }

    /// Insert one character at the cursor.
    ///
    /// A control character is dropped rather than stored: a terminal has no way
    /// to draw one, and a cursor column counted through it would drift. A tab
    /// becomes spaces for the same reason.
    pub fn insert(&mut self, character: char) {
        match character {
            '\n' | '\r' => self.newline(),
            '\t' => self.insert_text("    "),
            character if character.is_control() => {}
            character => {
                let at = self.cursor;
                self.splice(at, 0, &[character]);
                // The inserted character may join marks that already followed
                // the cursor (a line that opens with a combining mark), and the
                // caret belongs after the whole cluster.
                self.cursor = self.cluster_end(at);
            }
        }
    }

    /// Insert a whole run of text, as one edit, leaving the cursor after it.
    ///
    /// A pasted newline arrives as `\r\n` from some terminals and as `\r` from
    /// others, and either one is a single line break here. Control characters
    /// are dropped and tabs become spaces, so a paste cannot move the caret
    /// somewhere the renderer does not measure.
    pub fn insert_text(&mut self, text: &str) {
        let mut normalized: Vec<char> = Vec::new();
        let mut characters = text.chars().peekable();
        while let Some(character) = characters.next() {
            match character {
                '\r' => {
                    if characters.peek() == Some(&'\n') {
                        characters.next();
                    }
                    normalized.push('\n');
                }
                // A line break is a control character too, and it is the one
                // that must survive: it is what makes the paste a draft with
                // lines instead of one long row.
                '\n' => normalized.push('\n'),
                '\t' => normalized.extend(std::iter::repeat_n(' ', TAB_CELLS)),
                character if character.is_control() => {}
                character => normalized.push(character),
            }
        }
        if normalized.is_empty() {
            return;
        }
        let at = self.cursor;
        self.splice(at, 0, &normalized);
        // The last pasted character may compose with marks already after the
        // cursor, so the caret goes after that whole cluster, not between it.
        self.cursor = self.cluster_end(at + normalized.len() - 1);
    }

    /// Start a new line at the cursor.
    pub fn newline(&mut self) {
        self.splice(self.cursor, 0, &['\n']);
        self.cursor += 1;
    }

    /// Delete the character before the cursor, joining lines at a line start.
    ///
    /// A cluster is removed whole: one press takes a letter and its combining
    /// marks, never half of it.
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let from = self.cluster_start(self.cursor - 1);
        self.splice(from, self.cursor - from, &[]);
        self.cursor = from;
    }

    /// Delete the character at the cursor, joining lines at a line end.
    pub fn delete(&mut self) {
        if self.cursor < self.characters.len() {
            let to = self.cluster_end(self.cursor);
            self.splice(self.cursor, to - self.cursor, &[]);
        }
    }

    /// Delete from the start of the cursor's line to the cursor.
    pub fn kill_to_start(&mut self) {
        let start = self.line_start(self.line_of_cursor());
        if self.cursor > start {
            self.splice(start, self.cursor - start, &[]);
            self.cursor = start;
        }
    }

    /// Delete from the cursor to the end of its line.
    ///
    /// On an empty tail the line break goes instead, which is how a draft made
    /// of two lines becomes one again.
    pub fn kill_to_end(&mut self) {
        let end = self.line_end(self.line_of_cursor());
        if self.cursor < end {
            self.splice(self.cursor, end - self.cursor, &[]);
            return;
        }
        if end < self.characters.len() {
            self.splice(self.cursor, 1, &[]);
        }
    }

    /// Delete the word before the cursor, staying on its line.
    pub fn delete_word(&mut self) {
        let start = self.line_start(self.line_of_cursor());
        let mut from = self.cursor;
        while from > start && is_space(self.characters[from - 1]) {
            from -= 1;
        }
        while from > start && !is_space(self.characters[from - 1]) {
            from -= 1;
        }
        if from < self.cursor {
            // A word boundary inside a cluster belongs to the cluster, so the
            // deletion starts before its base character.
            let from = self.cluster_start(from);
            self.splice(from, self.cursor - from, &[]);
            self.cursor = from;
        }
    }

    /// Move the cursor one cluster left.
    pub fn left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.cluster_start(self.cursor - 1);
        }
    }

    /// Move the cursor one cluster right.
    pub fn right(&mut self) {
        if self.cursor < self.characters.len() {
            self.cursor = self.cluster_end(self.cursor);
        }
    }

    /// Move the cursor to the start of its line.
    pub fn home(&mut self) {
        self.cursor = self.line_start(self.line_of_cursor());
    }

    /// Move the cursor to the end of its line.
    pub fn end(&mut self) {
        self.cursor = self.line_end(self.line_of_cursor());
    }

    /// Move the cursor to the line above, keeping its column, and say whether
    /// there was one.
    ///
    /// The answer is what lets the caller pass an `up` at the first line on to
    /// the history instead of swallowing it.
    pub fn up(&mut self) -> bool {
        let line = self.line_of_cursor();
        if line == 0 {
            return false;
        }
        let column = self.cursor_column();
        self.cursor = self.at_column(line - 1, column);
        true
    }

    /// Move the cursor to the line below, keeping its column, and say whether
    /// there was one.
    pub fn down(&mut self) -> bool {
        let line = self.line_of_cursor();
        if line + 1 >= self.line_count() {
            return false;
        }
        let column = self.cursor_column();
        self.cursor = self.at_column(line + 1, column);
        true
    }

    /// The cluster boundary in `line` closest to `column` cells from its start.
    fn at_column(&self, line: usize, column: u16) -> usize {
        self.column_at(self.line_start(line), self.line_end(line), column)
    }

    /// The first cluster boundary at or before `column` cells past `from`, not
    /// past `to`.
    ///
    /// A cell inside a wide character or a cluster of several cells answers
    /// with the boundary before it, which is a stop the cursor can occupy. Both
    /// ends sit on one line.
    fn column_at(&self, from: usize, to: usize, column: u16) -> usize {
        if from >= to {
            return to;
        }
        let line = self.line_index(from);
        let boundaries = self.boundaries(line);
        let line_start = boundaries.line_start;
        let first = boundaries.cluster(from - line_start);
        let last = boundaries.cluster(to - line_start);
        let base = boundaries.cells[first];
        let target = usize::from(column);
        for index in first..last {
            let before = boundaries.cells[index] - base;
            let width = boundaries.cells[index + 1] - boundaries.cells[index];
            if before + width > target {
                return line_start + boundaries.chars[index];
            }
        }
        to
    }

    /// Move to the start of the word before the cursor.
    pub fn word_left(&mut self) {
        let mut at = self.cursor;
        while at > 0 && is_space(self.characters[at - 1]) {
            at -= 1;
        }
        while at > 0 && !is_space(self.characters[at - 1]) {
            at -= 1;
        }
        // The walk stops at a non-space character or the start of the draft; a
        // boundary inside a cluster is not a place the cursor can stand.
        if at < self.characters.len() {
            at = self.cluster_start(at);
        }
        self.cursor = at;
    }

    /// Move past the word after the cursor.
    pub fn word_right(&mut self) {
        let mut at = self.cursor;
        while at < self.characters.len() && !is_space(self.characters[at]) {
            at += 1;
        }
        while at < self.characters.len() && is_space(self.characters[at]) {
            at += 1;
        }
        // The walk stops after the spaces; a position inside a cluster is not a
        // place the cursor can stand, so it steps past that whole cluster.
        if at < self.characters.len() {
            let start = self.cluster_start(at);
            if start < at {
                at = self.cluster_end(start);
            }
        }
        self.cursor = at;
    }

    /// Empty the draft and reset the cursor.
    pub fn clear(&mut self) {
        self.characters.clear();
        self.line_starts.clear();
        self.line_starts.push(0);
        self.cursor = 0;
        self.revision = self.revision.wrapping_add(1);
        self.drawn = None;
    }

    /// Take the draft out, leaving the editor empty.
    pub fn take(&mut self) -> String {
        let text = self.text();
        self.clear();
        text
    }
}

/// Whether a character separates words.
fn is_space(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n')
}

/// Cells as a terminal column, saturating rather than wrapping.
fn cells_u16(cells: usize) -> u16 {
    u16::try_from(cells).unwrap_or(u16::MAX)
}

/// Characters hashed before the scan stops.
///
/// The popup's matches depend on the first word of a one-line draft, so a
/// prefix this long is more than any command name, and a pasted paragraph must
/// not cost a pass per frame.
const FINGERPRINT_CELLS: usize = 64;

/// Fingerprint of a draft, from its length and its first word.
///
/// One definition for both sides of the comparison: the editor hashes the
/// characters it holds, the popup hashes the text it was handed, and neither
/// builds a string to find out that nothing changed.
///
/// The length is a parameter so the per-frame caller can read it without a pass
/// (the editor knows `characters.len()`); the scan stops at the first whitespace
/// and after [`FINGERPRINT_CELLS`] characters, so a pasted paragraph costs a
/// bounded read. Two drafts of the same length whose first words match hash
/// alike, which is what the popup needs: only the first word changes what it
/// offers.
pub fn fingerprint(length: usize, characters: impl Iterator<Item = char>) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for character in characters
        .take_while(|character| !character.is_whitespace())
        .take(FINGERPRINT_CELLS)
    {
        hash ^= u64::from(character as u32);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // Folded in after the word, so a draft that grew or shrank differs even
    // when its first word is the same one.
    hash ^= (length as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    hash
}
