//! Which finished tool calls the pane draws, and which call the expand key
//! acts on.
//!
//! The selection follows the view the reader holds rather than the newest call,
//! so a held frame expands the line under it. The activity setting hides calls
//! that already finished; the call in progress and one waiting for approval are
//! the work being followed and stay on screen.

use super::{Item, Transcript};

impl Transcript {
    /// Show, or hide again, the whole output of the selected cut call.
    ///
    /// Answers whether there was anything to toggle: the caller says so when
    /// there was not, rather than leaving the key silently doing nothing. A
    /// selection the activity setting hides is not expanded: the press would
    /// change text no frame draws.
    pub(crate) fn toggle_expand(&mut self) -> bool {
        let Some(call_id) = self.expand.selected().map(str::to_string) else {
            return false;
        };
        if self.tool_hidden(&call_id) {
            return false;
        }
        self.expand.toggle()
    }

    /// Make the call the view shows the one the expand key acts on.
    ///
    /// `heights` are the wrapped rows of each item as the last held frame
    /// measured them, and `top` is the first row of the content the pane shows.
    /// The first call whose rows cross that window is selected; a window that
    /// shows no call leaves nothing selected, so the key does not open a line
    /// the reader cannot see.
    pub(crate) fn select_tool_at(&mut self, heights: &[usize], top: usize, view: usize) {
        if heights.len() != self.items.len() {
            return;
        }
        let bottom = top.saturating_add(view);
        let mut row = 0usize;
        let mut chosen: Option<String> = None;
        for (item, height) in self.items.iter().zip(heights) {
            let start = row;
            row = row.saturating_add(*height);
            if row <= top {
                continue;
            }
            if start >= bottom {
                break;
            }
            if let Item::Tool(call) = item
                && self.expand.holds(&call.call_id)
                && !self.tool_hidden(&call.call_id)
            {
                chosen = Some(call.call_id.clone());
                break;
            }
        }
        match chosen {
            Some(call_id) => {
                self.expand.select(&call_id);
            }
            None => self.expand.select_none(),
        }
    }

    /// Make the newest call the selection again, as following the end does.
    pub(crate) fn select_newest_tool(&mut self) {
        self.expand.follow_newest();
    }

    /// Whether the activity setting hides `call_id`'s line.
    fn tool_hidden(&self, call_id: &str) -> bool {
        self.items
            .iter()
            .any(|item| matches!(item, Item::Tool(call) if call.call_id == call_id && call.finished_activity()))
            && !self.expand.shows_activity()
    }

    /// Draw, or stop drawing, the calls that finished.
    ///
    /// A call in progress and one waiting for approval stay on screen: the
    /// setting is about the record of what ran, not about the work itself.
    pub(crate) fn set_tool_activity(&mut self, show: bool) {
        self.expand.set_activity(show);
    }

    /// Whether finished calls are drawn.
    pub(crate) fn show_tool_activity(&self) -> bool {
        self.expand.shows_activity()
    }
}
