//! The projection block a handoff boundary becomes in every later request.
//!
//! A structured summary is introduced as `## Previous Conversation Summary`,
//! which reads as third-party notes: the successor treats it as something it was
//! told about, not something it did. A handoff document is the opposite — the
//! same agent wrote it a moment ago and the successor is its continuation — so it
//! needs its own introduction. The wording is load-bearing: it tells the model to
//! resume the work instead of summarizing the notes, and it points at the verbatim
//! tail that follows as the part it should trust most when the two disagree.

/// Introduction for a handoff boundary in the request projection.
///
/// Kept as one constant so the rendered prompt and the tests agree on the exact
/// text. A change here is a change to what every successor is told.
pub const HANDOFF_FRAME: &str = "\
## Handoff From Your Earlier Self

You wrote this before your context was compacted. Continue the work it describes;
do not summarize it back. Where it disagrees with the messages that follow, the
messages are newer and win.";

/// The boundary block for either kind of compaction summary.
///
/// One call site picks the introduction, so a new boundary kind cannot be added
/// with the summary framing by omission: the caller has to say which it is.
pub fn boundary_text_block(document: &str, handoff: bool) -> String {
    if handoff {
        handoff_text_block(document)
    } else {
        crate::compacted_summary_text_block(document)
    }
}

/// Wrap a handoff document in its successor framing.
///
/// The separator matches [`crate::compacted_summary_text_block`] so the rest of
/// the projection reads the same regardless of which boundary produced it.
pub fn handoff_text_block(document: &str) -> String {
    format!("{HANDOFF_FRAME}\n\n{}\n\n---\n\n", document.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_addresses_the_successor_as_the_continuation() {
        let block = handoff_text_block("## Goal\n\nship it\n");
        assert!(block.starts_with(HANDOFF_FRAME));
        assert!(block.contains("ship it"));
        assert!(block.ends_with("---\n\n"));
    }

    #[test]
    fn trailing_document_whitespace_does_not_double_the_separator() {
        let block = handoff_text_block("## Goal\n\nship it\n\n\n");
        assert!(block.contains("ship it\n\n---\n\n"));
        assert!(!block.contains("ship it\n\n\n"));
    }
}
