//! The drafts submitted before this one, newest last.
//!
//! The walk is a cursor over the entries plus the draft it started from, so
//! stepping back past the newest entry returns what the user was typing. That
//! draft is kept in one place: a caller that had to remember it would lose it
//! on the first submitted message, which is exactly when `up` is used.

/// How many drafts are kept; older ones fall off the front.
const CAPACITY: usize = 100;

/// The drafts sent from this editor, and where a walk through them stands.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct History {
    entries: Vec<String>,
    /// Where the walk is; `None` while the user is on their own draft.
    index: Option<usize>,
    /// What the user was typing when the walk started.
    draft: Option<String>,
}

impl History {
    /// No drafts yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Remember a submitted draft.
    ///
    /// The walk ends here: the next `up` starts from the newest entry rather
    /// than from a position inside a draft that is no longer on screen.
    pub fn record(&mut self, text: &str) {
        self.reset();
        if text.trim().is_empty() {
            return;
        }
        // Repeating a message is common and answering it again with `up` twice
        // is not; one entry per distinct message in a row.
        if self.entries.last().map(String::as_str) == Some(text) {
            return;
        }
        self.entries.push(text.to_string());
        if self.entries.len() > CAPACITY {
            self.entries.remove(0);
        }
    }

    /// The drafts sent before this one, newest first.
    ///
    /// The history list reads the entries through this walk rather than a copy
    /// of them: what `record` drops off the front must stop being offered.
    pub fn drafts(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().rev().map(String::as_str)
    }

    /// The draft that `position` names when walking [`History::drafts`].
    ///
    /// A draft is named by its place, newest first, rather than by its text: the
    /// same message can be sent more than once, and a multi-line draft is one
    /// entry whose text is not one row.
    pub fn draft_at(&self, position: usize) -> Option<&str> {
        self.drafts().nth(position)
    }

    /// Step to the draft before the one on screen, remembering `current`
    /// first, or `None` at the oldest entry.
    pub fn previous(&mut self, current: &str) -> Option<String> {
        match self.index {
            None => {
                let last = self.entries.len().checked_sub(1)?;
                self.draft = Some(current.to_string());
                self.index = Some(last);
                Some(self.entries[last].clone())
            }
            Some(0) => None,
            Some(index) => {
                self.index = Some(index - 1);
                Some(self.entries[index - 1].clone())
            }
        }
    }

    /// Step forward, and past the newest entry back to the draft that was
    /// being typed.
    pub fn forward(&mut self) -> Option<String> {
        let index = self.index?;
        if index + 1 < self.entries.len() {
            self.index = Some(index + 1);
            return Some(self.entries[index + 1].clone());
        }
        self.index = None;
        self.draft.take()
    }

    /// End the walk, keeping the entries.
    pub fn reset(&mut self) {
        self.index = None;
        self.draft = None;
    }
}
