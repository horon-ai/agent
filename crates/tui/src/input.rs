//! The single-line editor under the transcript.
//!
//! It edits by character rather than byte so a cursor never lands inside a
//! multi-byte code point, and it keeps the terminal concerns (cursor column,
//! how much of the line fits) out of the app.

use unicode_width::UnicodeWidthChar;

/// An editable line with a cursor.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Input {
    characters: Vec<char>,
    cursor: usize,
}

impl Input {
    /// An empty line.
    pub fn new() -> Self {
        Self::default()
    }

    /// The line as text.
    pub fn text(&self) -> String {
        self.characters.iter().collect()
    }

    /// Whether nothing has been typed.
    pub fn is_empty(&self) -> bool {
        self.characters.is_empty()
    }

    /// Where the cursor sits, counted in characters.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The cursor column, counted in cells from the start of the line.
    ///
    /// The renderer offsets it by the prompt marker, so a wide character moves
    /// the cursor by its two cells instead of one.
    pub fn cursor_column(&self) -> u16 {
        let cells: usize = self.characters[..self.cursor]
            .iter()
            .map(|character| char_width(*character))
            .sum();
        u16::try_from(cells).unwrap_or(u16::MAX)
    }

    /// The part of the line that fits `width` cells, and where the cursor sits
    /// inside it.
    ///
    /// The window follows the cursor: text before it is dropped from the left
    /// once the line is wider than the pane, so the caret never leaves the
    /// screen. The cursor keeps the rightmost cell free for itself.
    pub fn window(&self, width: u16) -> (String, u16) {
        let room = usize::from(width);
        if room == 0 {
            return (String::new(), 0);
        }
        let cursor_cells = usize::from(self.cursor_column());
        // Leaving one cell free keeps the cursor visible at the right edge.
        let limit = room.saturating_sub(1);
        let mut first = 0;
        let mut dropped = 0;
        while cursor_cells.saturating_sub(dropped) > limit && first < self.cursor {
            dropped += char_width(self.characters[first]);
            first += 1;
        }
        let mut text = String::new();
        let mut used = 0;
        for character in &self.characters[first..] {
            let cells = char_width(*character);
            if used + cells > room {
                break;
            }
            text.push(*character);
            used += cells;
        }
        let column = u16::try_from(cursor_cells.saturating_sub(dropped)).unwrap_or(u16::MAX);
        (text, column.min(u16::try_from(limit).unwrap_or(u16::MAX)))
    }

    /// Insert a character at the cursor.
    pub fn insert(&mut self, character: char) {
        self.characters.insert(self.cursor, character);
        self.cursor += 1;
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor -= 1;
        self.characters.remove(self.cursor);
    }

    /// Delete the character at the cursor.
    pub fn delete(&mut self) {
        if self.cursor < self.characters.len() {
            self.characters.remove(self.cursor);
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

    /// Move the cursor to the start.
    pub fn home(&mut self) {
        self.cursor = 0;
    }

    /// Move the cursor to the end.
    pub fn end(&mut self) {
        self.cursor = self.characters.len();
    }

    /// Empty the line and reset the cursor.
    pub fn clear(&mut self) {
        self.characters.clear();
        self.cursor = 0;
    }

    /// Take the line out, leaving the editor empty.
    pub fn take(&mut self) -> String {
        let text = self.text();
        self.clear();
        text
    }
}

/// The cells one character occupies, matching what the terminal draws.
fn char_width(character: char) -> usize {
    UnicodeWidthChar::width(character).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_and_editing_follow_the_cursor() {
        let mut input = Input::new();
        for character in "helo".chars() {
            input.insert(character);
        }
        assert_eq!(input.text(), "helo");
        input.left();
        input.insert('l');
        assert_eq!(input.text(), "hello");
        assert_eq!(input.cursor(), 4);
        input.backspace();
        assert_eq!(input.text(), "helo");
        input.home();
        input.delete();
        assert_eq!(input.text(), "elo");
        input.end();
        assert_eq!(input.cursor(), 3);
    }

    #[test]
    fn take_empties_the_editor() {
        let mut input = Input::new();
        input.insert('a');
        assert_eq!(input.take(), "a");
        assert!(input.is_empty());
        assert_eq!(input.cursor_column(), 0);
    }

    #[test]
    fn edits_at_the_boundaries_do_nothing() {
        let mut input = Input::new();
        input.backspace();
        input.delete();
        input.left();
        input.right();
        assert!(input.is_empty());
    }

    fn typed(text: &str) -> Input {
        let mut input = Input::new();
        for character in text.chars() {
            input.insert(character);
        }
        input
    }

    #[test]
    fn short_line_is_not_windowed() {
        let mut input = typed("hi");
        assert_eq!(input.window(20), ("hi".to_string(), 2));
        input.home();
        assert_eq!(input.window(20), ("hi".to_string(), 0));
    }

    #[test]
    fn long_line_scrolls_so_the_cursor_stays_inside() {
        // Ten characters in a six-cell pane: the last five fit before the
        // cursor, which sits in the last cell.
        let input = typed("abcdefghij");
        assert_eq!(input.window(6), ("fghij".to_string(), 5));
        assert!(
            usize::from(input.window(6).1) < 6,
            "cursor escaped the input area: {:?}",
            input.window(6)
        );
    }

    #[test]
    fn moving_left_scrolls_the_window_back() {
        let mut input = typed("abcdefghij");
        for _ in 0..5 {
            input.left();
        }
        // Cursor on 'f' (column 5 in the full line) with a six-cell pane keeps
        // the start visible and the caret at cell five.
        assert_eq!(input.window(6), ("abcdef".to_string(), 5));
        input.home();
        assert_eq!(input.window(6), ("abcdef".to_string(), 0));
    }

    #[test]
    fn wide_characters_move_the_cursor_by_their_cells() {
        let input = typed("한글");
        assert_eq!(input.cursor_column(), 4);
        // Three cells hold one two-cell syllable plus the cursor.
        assert_eq!(input.window(3), ("글".to_string(), 2));
    }

    #[test]
    fn zero_width_window_shows_nothing() {
        let input = typed("text");
        assert_eq!(input.window(0), (String::new(), 0));
    }
}
