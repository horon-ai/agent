//! The multi-line editor under the transcript.
//!
//! It edits by character rather than byte so a cursor never lands inside a
//! multi-byte code point, and it keeps the terminal concerns (which rows exist,
//! where the caret sits, how much of a line fits) out of the app.
//!
//! The buffer is a flat run of characters in which `'\n'` separates the lines,
//! and [`Input::line_starts`] indexes the first character of each line. Keeping
//! that index up to date on every edit is what lets a frame draw only the rows
//! it shows: a draft of five thousand lines costs the same per frame as one
//! line, because no frame walks the buffer.

use unicode_width::UnicodeWidthChar;

/// Cells one tab becomes when a paste carries it.
///
/// A terminal draws a tab wherever its own tab stops put it, which the renderer
/// cannot know; expanding the character on the way in keeps the caret and the
/// text in the same place.
const TAB_CELLS: usize = 4;

/// An editable draft with a cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    characters: Vec<char>,
    /// The index of the first character of each line, ascending, starting at 0.
    line_starts: Vec<usize>,
    cursor: usize,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            characters: Vec::new(),
            line_starts: vec![0],
            cursor: 0,
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
        fingerprint(self.characters.iter().copied())
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
        let start = self.line_start(self.line_of_cursor());
        self.cells(start, self.cursor)
    }

    /// The cells `characters[from..to]` occupy.
    fn cells(&self, from: usize, to: usize) -> u16 {
        let cells: usize = self.characters[from..to]
            .iter()
            .map(|character| char_width(*character))
            .sum();
        u16::try_from(cells).unwrap_or(u16::MAX)
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
        let mut cursor_row = 0;
        let mut cursor_cell = 0;
        for (row, line) in (first..self.line_count()).take(room).enumerate() {
            let caret = line == cursor_line;
            let (text, cell) = self.row(line, width, caret);
            if caret {
                cursor_row = row;
                cursor_cell = cell;
            }
            rows.push(text);
        }
        View {
            rows,
            cursor_row,
            cursor_cell,
        }
    }

    /// One row of the pane: the part of `line` that fits `width` cells, and
    /// where the caret sits inside it when it is on this line.
    ///
    /// A line wider than the pane scrolls under the caret, which keeps the
    /// rightmost cell free for itself; a line the caret is not on is drawn from
    /// its start.
    fn row(&self, line: usize, width: u16, caret: bool) -> (String, u16) {
        let start = self.line_start(line);
        let end = self.line_end(line);
        let room = usize::from(width);
        if room == 0 {
            return (String::new(), 0);
        }
        let limit = room.saturating_sub(1);
        let cursor_cells = if caret { usize::from(self.cursor_column()) } else { 0 };
        let mut first = start;
        let mut dropped = 0;
        while caret && cursor_cells.saturating_sub(dropped) > limit && first < self.cursor {
            dropped += char_width(self.characters[first]);
            first += 1;
        }
        let mut text = String::new();
        let mut used = 0;
        for character in &self.characters[first..end] {
            let cells = char_width(*character);
            if used + cells > room {
                break;
            }
            text.push(*character);
            used += cells;
        }
        let cell = u16::try_from(cursor_cells.saturating_sub(dropped))
            .unwrap_or(u16::MAX)
            .min(u16::try_from(limit).unwrap_or(u16::MAX));
        (text, cell)
    }

    /// Replace `characters[at..at + removed]` with `inserted` and rebuild the
    /// line index around the edit.
    ///
    /// Every edit lands here so the index cannot drift from the buffer.
    fn splice(&mut self, at: usize, removed: usize, inserted: &[char]) {
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
                self.splice(self.cursor, 0, &[character]);
                self.cursor += 1;
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
        self.cursor = at + normalized.len();
    }

    /// Start a new line at the cursor.
    pub fn newline(&mut self) {
        self.splice(self.cursor, 0, &['\n']);
        self.cursor += 1;
    }

    /// Delete the character before the cursor, joining lines at a line start.
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.splice(self.cursor - 1, 1, &[]);
        self.cursor -= 1;
    }

    /// Delete the character at the cursor, joining lines at a line end.
    pub fn delete(&mut self) {
        if self.cursor < self.characters.len() {
            self.splice(self.cursor, 1, &[]);
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
            self.splice(from, self.cursor - from, &[]);
            self.cursor = from;
        }
    }

    /// Move the cursor one character left.
    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Move the cursor one character right.
    pub fn right(&mut self) {
        if self.cursor < self.characters.len() {
            self.cursor += 1;
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

    /// The character index in `line` closest to `column` cells from its start.
    fn at_column(&self, line: usize, column: u16) -> usize {
        let start = self.line_start(line);
        let end = self.line_end(line);
        let target = usize::from(column);
        let mut cells = 0;
        for index in start..end {
            let width = char_width(self.characters[index]);
            if cells + width > target {
                return index;
            }
            cells += width;
        }
        end
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
        self.cursor = at;
    }

    /// Empty the draft and reset the cursor.
    pub fn clear(&mut self) {
        self.characters.clear();
        self.line_starts.clear();
        self.line_starts.push(0);
        self.cursor = 0;
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

/// The cells one character occupies, matching what the terminal draws.
fn char_width(character: char) -> usize {
    UnicodeWidthChar::width(character).unwrap_or(0)
}

/// Fingerprint of a draft, read one character at a time.
///
/// One definition for both sides of the comparison: the editor hashes the
/// characters it holds, the popup hashes the text it was handed, and neither
/// builds a string to find out that nothing changed.
pub fn fingerprint(characters: impl Iterator<Item = char>) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for character in characters {
        hash ^= u64::from(character as u32);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
