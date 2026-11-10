//! The single-line editor under the transcript.
//!
//! It edits by character rather than byte so a cursor never lands inside a
//! multi-byte code point, and it keeps the terminal concerns (cursor column)
//! out of the app.

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

    /// The cursor column for a prompt that starts at column zero, in cells.
    ///
    /// TUI text is overwhelmingly single-width; a wide character means the
    /// cursor lands one cell early on that line rather than not at all.
    pub fn cursor_column(&self) -> u16 {
        u16::try_from(self.cursor).unwrap_or(u16::MAX)
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
}
