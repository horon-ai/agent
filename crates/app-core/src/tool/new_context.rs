//! `new_context`: the model decides that its working notes are enough and
//! starts the next turn from a clean active context.
//!
//! The tool only states the intent. A tool round cannot rebuild its own
//! transcript: the assistant message that asked for it, and the results still
//! to come, are part of the same turn. So the call records a pending request
//! for the session and `crate::agent::compaction` rebuilds the context at the
//! end of the turn — at that point the transcript is final and no result can be
//! orphaned. Calling the tool twice in one turn records one request, because the
//! pending set is keyed by session, not by call.
//!
//! Registered only when `[tools] context_notes` is on: the rollover keeps the
//! notes and the latest user message, and those notes are the state the next
//! turn continues from. `history://current` keeps the full transcript readable
//! beside them.

use super::{Tool, ToolContext, ToolEffects, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex, MutexGuard};

/// Sessions that asked for a rollover the turn end has not applied yet.
///
/// Process-lifetime and keyed by session, like the undo journal: the tool sees
/// only the session id, and one session can have at most one pending rollover
/// no matter how many times the tool is called in a turn.
static PENDING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

fn pending() -> MutexGuard<'static, HashSet<String>> {
    PENDING.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Record that `session_id` asked to roll its context over at turn end.
pub(crate) fn request_rollover(session_id: &str) {
    pending().insert(session_id.to_string());
}

/// Whether `session_id` has a rollover request the turn end has not applied yet.
pub(crate) fn rollover_pending(session_id: &str) -> bool {
    pending().contains(session_id)
}

/// Drop the request once the boundary is committed, or once it cannot be used.
pub(crate) fn clear_rollover(session_id: &str) {
    pending().remove(session_id);
}

pub struct NewContextTool;

impl NewContextTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for NewContextTool {
    fn name(&self) -> &str {
        "new_context"
    }

    fn description(&self) -> &str {
        "Roll this session's context over at the end of this turn: keep the working notes and the latest user message, drop the rest, and leave the full transcript at history://current."
    }

    /// The rollover moves the session's compaction boundary, so plan mode and
    /// approval must treat it as a mutation rather than a read.
    fn effects(&self) -> ToolEffects {
        ToolEffects::write()
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(&self, _input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        request_rollover(&ctx.session_id);
        Ok(ToolOutput::new(
            "Context rollover queued. When this turn ends the active context becomes your working \
             notes and the latest user message; every earlier message stays readable at \
             history://current.",
        )
        .with_title("new_context"))
    }
}

#[cfg(test)]
#[path = "new_context_tests.rs"]
mod tests;
