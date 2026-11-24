//! The whole output of a tool call whose preview was cut.
//!
//! A transcript line keeps only the head of a finished call's output, because a
//! tool may return megabytes and the line outlives the call. The rest of a cut
//! output is kept here, beside the items rather than inside them, so that a key
//! can show the whole text of the selected cut call and put the preview back on
//! the next press. The selected call follows the view: scrolling to an earlier
//! result makes that result the one the key acts on, and a call that arrives
//! while the view follows the end becomes the selection again. Keeping the text
//! apart is what leaves `ToolCall`, `OutputHead` and the drawing look as they
//! are: a line stays a plain description of a call.
//!
//! The memory is bounded on purpose. One call keeps at most
//! [`MAX_KEPT_BYTES`], and a cut copy ends with a line saying it was cut, so a
//! truncated view never reads as the tool's own last line. Only the
//! [`MAX_KEPT_CALLS`] most recent cut calls hold anything: a long turn must not
//! grow with the outputs of its first calls, and the line of a call whose text
//! was dropped stops naming the key rather than promising a view that is gone.
//!
//! A call the daemon gave no id keeps nothing, because a line without an id
//! cannot be found again once a later call starts.

use crate::keys::{self, Bindings, Key};
use crate::tool_line::{OutputHead, Retained};

/// Bytes of one call's output kept for expanding.
pub(crate) const MAX_KEPT_BYTES: usize = 64 * 1024;

/// Cut calls whose output is kept; older ones drop their text.
pub(crate) const MAX_KEPT_CALLS: usize = 16;

/// The last line of a kept copy that was itself cut.
pub(crate) const TRUNCATED: &str = "… output truncated";

/// What the expand key says when no call has kept output.
pub(crate) const NOTHING_TO_EXPAND: &str = "no tool output to expand";

/// Whether the preview `head` hid any of `output`.
///
/// Two things are hidden: the rows below the preview, and the cells cut off the
/// right of a row the preview keeps. Either one makes the whole text worth
/// keeping, and neither makes a call's line promise a view of text that is
/// already complete on screen.
pub(crate) fn cut(output: &str, head: &OutputHead) -> bool {
    if head.hidden_rows() > 0 {
        return true;
    }
    // A kept row that differs from the text it came from was cut at its right
    // edge, which is the other way the preview hides output.
    let mut rows = output.lines();
    head.rows.iter().any(|kept| rows.next() != Some(kept.as_str()))
}

/// One call's kept output, and whether it is drawn in full.
#[derive(Debug)]
struct Kept {
    call_id: String,
    text: String,
    expanded: bool,
}

/// The kept outputs, oldest first.
#[derive(Debug)]
pub(crate) struct ExpandState {
    kept: Vec<Kept>,
    /// The call the key acts on, if one is selected.
    selected: Option<String>,
    /// Whether a result arriving now becomes the selection.
    ///
    /// True while the view follows the end, which is when a new result is the
    /// one being read. Scrolling to a result stops the following, so a call
    /// that lands below a held view does not take the selection away from the
    /// result on screen.
    follow: bool,
    /// Whether finished calls are hidden, as `transcript.show_tool_activity`
    /// says. A call in progress or waiting for approval is always drawn.
    hide_activity: bool,
}

impl Default for ExpandState {
    fn default() -> Self {
        Self {
            kept: Vec::new(),
            selected: None,
            // A client starts at the end of the conversation, where a new
            // result is the one being read.
            follow: true,
            hide_activity: false,
        }
    }
}

impl ExpandState {
    /// Keep `output` for the call `call_id` names, opened shut.
    ///
    /// A call already in the table is left alone: a result can reach the client
    /// twice when a connection is replayed, and the second copy is the same
    /// text. A call the window dropped is recorded again when its result is
    /// replayed, and re-enters as the newest entry, because nothing remembers
    /// what was dropped.
    ///
    /// Answers whether a line the pane has already counted changes its rows:
    /// only an entry that was open folds back to its preview, which shrinks a
    /// block nothing else re-measures. A shut line keeps its rows when a newer
    /// call arrives: the marker is always one row, and the key label only takes
    /// its place there when it still fits on that row (see `tool_line`), so the
    /// hand-off changes what the marker says and not how much of the pane it
    /// takes.
    pub(crate) fn record(&mut self, call_id: &str, output: &str) -> bool {
        if call_id.is_empty() || self.kept.iter().any(|kept| kept.call_id == call_id) {
            return false;
        }
        let redraw = self.kept.iter().any(|kept| kept.expanded);
        // Only one line is drawn in full at a time, so a new call closes the
        // one before it.
        for kept in &mut self.kept {
            kept.expanded = false;
        }
        self.kept.push(Kept {
            call_id: call_id.to_string(),
            text: bounded(output),
            expanded: false,
        });
        let excess = self.kept.len().saturating_sub(MAX_KEPT_CALLS);
        if excess > 0 {
            self.kept.drain(..excess);
        }
        // The selection follows new results while the view follows the end,
        // and falls back to nothing when a held view's selection was dropped,
        // so the key never acts on a line the reader cannot see.
        if self.follow {
            self.selected = self.kept.last().map(|kept| kept.call_id.clone());
        } else if self
            .selected
            .as_deref()
            .is_none_or(|id| !self.kept.iter().any(|kept| kept.call_id == id))
        {
            self.selected = None;
        }
        redraw
    }

    /// Show, or hide again, the whole text of the selected kept call.
    ///
    /// Answers whether there was anything to toggle: with nothing kept, or with
    /// the view showing no call at all, the key has no work to do, and the
    /// caller says so instead of doing nothing silently.
    pub(crate) fn toggle(&mut self) -> bool {
        let Some(index) = self
            .selected
            .as_deref()
            .and_then(|id| self.kept.iter().position(|kept| kept.call_id == id))
        else {
            return false;
        };
        // The selected entry decides the direction: whatever the table held,
        // one press opens it and the next one shuts it again.
        let expand = self.kept.get(index).is_none_or(|kept| !kept.expanded);
        for (position, kept) in self.kept.iter_mut().enumerate() {
            kept.expanded = expand && position == index;
        }
        true
    }

    /// Make `call_id` the call the key acts on.
    ///
    /// Answers whether the call holds kept text. The caller passes the call the
    /// view shows, so this also stops a result that arrives later from taking
    /// the selection away from the line on screen.
    pub(crate) fn select(&mut self, call_id: &str) -> bool {
        if self.selected.as_deref() == Some(call_id) {
            self.follow = false;
            return true;
        }
        if !self.holds(call_id) {
            return false;
        }
        match &mut self.selected {
            Some(selected) => {
                selected.clear();
                selected.push_str(call_id);
            }
            None => self.selected = Some(call_id.to_string()),
        }
        self.follow = false;
        true
    }

    /// Make the newest kept call the selection and follow new results again.
    pub(crate) fn follow_newest(&mut self) {
        self.selected = self.kept.last().map(|kept| kept.call_id.clone());
        self.follow = true;
    }

    /// Leave no call selected, as the view showing no result does.
    pub(crate) fn select_none(&mut self) {
        self.selected = None;
        self.follow = false;
    }

    /// The call the key acts on.
    pub(crate) fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Whether `call_id` is one of the calls holding kept text.
    pub(crate) fn holds(&self, call_id: &str) -> bool {
        self.kept.iter().any(|kept| kept.call_id == call_id)
    }

    /// Draw, or stop drawing, the calls that finished.
    pub(crate) fn set_activity(&mut self, show: bool) {
        self.hide_activity = !show;
    }

    /// Whether finished calls are drawn.
    pub(crate) fn shows_activity(&self) -> bool {
        !self.hide_activity
    }

    /// Drop the kept text of one call whose line stopped drawing its output.
    ///
    /// A line the gate parks or decides shows neither the preview nor the whole
    /// text, so keeping the text any longer would leave the key expanding
    /// nothing the reader can see.
    pub(crate) fn forget(&mut self, call_id: &str) {
        self.kept.retain(|kept| kept.call_id != call_id);
        let stale = self
            .selected
            .as_deref()
            .is_none_or(|id| !self.kept.iter().any(|kept| kept.call_id == id));
        if stale {
            if self.follow {
                self.follow_newest();
            } else {
                self.selected = None;
            }
        }
    }

    /// Forget every kept output, as leaving the conversation does.
    ///
    /// The display preference is not conversation state, so it stays.
    pub(crate) fn clear(&mut self) {
        self.kept.clear();
        self.selected = None;
        self.follow = true;
    }

    /// The calls holding kept text, oldest first.
    #[cfg(test)]
    pub(crate) fn calls(&self) -> impl Iterator<Item = &str> {
        self.kept.iter().map(|kept| kept.call_id.as_str())
    }

    /// Whether `call_id`'s whole output is being drawn.
    #[cfg(test)]
    pub(crate) fn is_expanded(&self, call_id: &str) -> bool {
        self.kept.iter().any(|kept| kept.call_id == call_id && kept.expanded)
    }

    /// What the line of `call_id` draws beyond the preview.
    ///
    /// The key is named only on the selected call, which is the one the key
    /// expands, and only while that call is not already open: a marker on a call
    /// the press would not touch is a promise the press does not keep. The
    /// marker is also what shows which call scrolling has selected.
    fn retained(&self, call_id: &str, key: Option<Key>) -> Retained<'_> {
        let Some(kept) = self.kept.iter().find(|kept| kept.call_id == call_id) else {
            return Retained::default();
        };
        let selected = self.selected.as_deref() == Some(call_id);
        if kept.expanded {
            return Retained {
                key: None,
                text: Some(kept.text.as_str()),
                kept: true,
                selected,
            };
        }
        Retained {
            key: if selected { key } else { None },
            text: None,
            kept: true,
            selected,
        }
    }
}

/// `output` kept whole, or cut on a character boundary with a closing note.
///
/// The cut lands before a character whose bytes cross the budget, so the kept
/// copy stays valid text rather than half a character. The note is a line of its
/// own, which is what keeps a cut view from reading as the tool's own last line.
fn bounded(output: &str) -> String {
    if output.len() <= MAX_KEPT_BYTES {
        return output.to_string();
    }
    let slice = &output[..output.floor_char_boundary(MAX_KEPT_BYTES)];
    let needs_newline = !slice.ends_with('\n');
    let mut text = String::with_capacity(slice.len() + usize::from(needs_newline) + TRUNCATED.len());
    text.push_str(slice);
    if needs_newline {
        text.push('\n');
    }
    text.push_str(TRUNCATED);
    text
}

/// What one frame needs to draw the calls whose output was kept.
///
/// Built once per frame so the key is read once rather than per line, and read
/// from the bindings in force: a key rebound on the settings screen has to be
/// the one the marker under a cut preview names.
pub(crate) struct Frame<'a> {
    kept: &'a ExpandState,
    key: Option<Key>,
}

impl<'a> Frame<'a> {
    /// The frame for `kept`, expanding on the key the bindings now name.
    pub(crate) fn new(kept: &'a ExpandState, bindings: &Bindings) -> Self {
        Self {
            kept,
            key: bindings.first_key(keys::ids::APP_EXPAND_TOOL),
        }
    }

    /// What the line of `call_id` draws beyond the preview.
    ///
    /// The result borrows the kept text rather than this frame, so a caller may
    /// build a frame for one draw and hold what it returns.
    pub(crate) fn retained(&self, call_id: &str) -> Retained<'a> {
        self.kept.retained(call_id, self.key)
    }

    /// Whether a call that finished is left out of the pane.
    ///
    /// A call in progress and a call waiting for approval are always drawn: the
    /// setting hides the record of what ran, not the work the reader has to see
    /// or answer.
    pub(crate) fn hides_finished(&self) -> bool {
        !self.kept.shows_activity()
    }
}
