//! Transcript rows and outcomes retained by local recordings.

use super::{Item, ToolCall, ToolState, Transcript};

/// What a replayed failure says instead of the output it never kept.
///
/// A recording leaves tool bodies out, so the row cannot repeat the error text.
/// Saying that the output was not recorded is true and reads as a boundary
/// rather than as a failure reason the tool never gave.
pub const OUTPUT_NOT_RECORDED: &str = "the output was not recorded";

/// What a replayed warning says instead of the text it never kept.
///
/// A recording keeps that a warning was shown, not what it said: a warning can
/// name a private path or echo a harness message, and a recording is read on its
/// own. The row says so rather than inventing the text.
pub const WARNING_NOT_RECORDED: &str = "a warning was shown; its text was not recorded";

impl Transcript {
    /// How many times this conversation was replaced by a clear.
    ///
    /// The number itself means nothing; a reader compares it with the one it
    /// last saw to learn that the items it counted are gone.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Append the line a recording kept for one tool call.
    ///
    /// Only the name and whether the call failed survived the recording, so the
    /// line has neither arguments nor output. A failure says why it has no
    /// error text rather than inventing one.
    pub fn push_recorded_tool(&mut self, name: &str, failed: bool) {
        self.finish_stream();
        let mut call = ToolCall::new("", name);
        call.state = if failed {
            ToolState::Failed(OUTPUT_NOT_RECORDED.to_string())
        } else {
            ToolState::Done
        };
        self.push_item(Item::Tool(call), None);
    }

    /// Append the line a recording kept for one warning.
    ///
    /// The recording keeps that a warning was shown, not what it said, so the
    /// row states the boundary instead of repeating text the file never held.
    pub fn push_recorded_warning(&mut self) {
        self.finish_stream();
        self.push_item(Item::Warning(WARNING_NOT_RECORDED.to_string()), None);
    }

    /// How the call `call_id` names stands, without its body.
    ///
    /// A caller that only needs the visible outcome (a recording keeps the name
    /// and whether the call failed) does not get the output this way. An empty
    /// id names the most recent call of that name, which is the line the result
    /// that carried no id was just written onto.
    pub fn tool_state(&self, call_id: &str, name: &str) -> Option<ToolState> {
        self.items.iter().rev().find_map(|item| {
            let Item::Tool(call) = item else {
                return None;
            };
            let named = if call_id.is_empty() {
                call.name == name
            } else {
                call.is_call(call_id)
            };
            named.then(|| call.state.clone())
        })
    }
}
