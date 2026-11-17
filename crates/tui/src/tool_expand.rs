//! The whole output of a tool call whose preview was cut.
//!
//! A transcript line keeps only the head of a finished call's output, because a
//! tool may return megabytes and the line outlives the call. The rest of a cut
//! output is kept here, beside the items rather than inside them, so that a key
//! can show the whole text of the newest cut call and put the preview back on
//! the next press. Keeping it apart is what leaves `ToolCall`, `OutputHead` and
//! the drawing look as they are: a line stays a plain description of a call.
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
#[derive(Debug, Default)]
pub(crate) struct ExpandState {
    kept: Vec<Kept>,
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
        // Only one line is drawn in full at a time, and the newest call is the
        // one the key is about, so a new call closes the one before it.
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
        redraw
    }

    /// Show, or hide again, the whole text of the newest kept call.
    ///
    /// Answers whether there was anything to toggle: with nothing kept the key
    /// has no work to do, and the caller says so instead of doing nothing
    /// silently.
    pub(crate) fn toggle(&mut self) -> bool {
        let Some(newest) = self.kept.len().checked_sub(1) else {
            return false;
        };
        // The newest entry decides the direction: whatever the table held, one
        // press opens the newest call and the next one shuts it again.
        let expand = self.kept.get(newest).is_none_or(|kept| !kept.expanded);
        for (position, kept) in self.kept.iter_mut().enumerate() {
            kept.expanded = expand && position == newest;
        }
        true
    }

    /// Drop the kept text of one call whose line stopped drawing its output.
    ///
    /// A line the gate parks or decides shows neither the preview nor the whole
    /// text, so keeping the text any longer would leave the key expanding
    /// nothing the reader can see.
    pub(crate) fn forget(&mut self, call_id: &str) {
        self.kept.retain(|kept| kept.call_id != call_id);
    }

    /// Forget every kept output, as leaving the conversation does.
    pub(crate) fn clear(&mut self) {
        self.kept.clear();
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
    /// The key is named only on the newest kept call, which is the one the key
    /// expands, and only while that call is not already open: a marker on a call
    /// the press would not touch is a promise the press does not keep.
    fn retained(&self, call_id: &str, key: Option<Key>) -> Retained<'_> {
        let Some(index) = self.kept.iter().position(|kept| kept.call_id == call_id) else {
            return Retained::default();
        };
        let Some(kept) = self.kept.get(index) else {
            return Retained::default();
        };
        if kept.expanded {
            return Retained {
                key: None,
                text: Some(kept.text.as_str()),
                kept: true,
            };
        }
        let newest = index + 1 == self.kept.len();
        Retained {
            key: if newest { key } else { None },
            text: None,
            kept: true,
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
    // Built a character at a time up to the budget: a character whose bytes
    // would cross it starts nothing, so the copy stops on a boundary by
    // construction and needs neither a byte index nor a fallback for a broken one.
    let mut text = String::with_capacity(MAX_KEPT_BYTES + TRUNCATED.len() + 2);
    for character in output.chars() {
        if text.len() + character.len_utf8() > MAX_KEPT_BYTES {
            break;
        }
        text.push(character);
    }
    if !text.ends_with('\n') {
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
}
