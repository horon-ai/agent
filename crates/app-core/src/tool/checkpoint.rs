//! `checkpoint` and `rewind`: the model marks a point of its own session and
//! later folds everything after it back into a short report it writes itself.
//!
//! An autonomous agent that explores a dead end otherwise keeps the whole
//! attempt in its context until compaction. With this pair the model marks a
//! point, explores, and replaces the exploration with its conclusion.
//!
//! Both tools only state the intent. A tool round cannot rebuild its own
//! transcript: the assistant message that asked for it, and the results still
//! to come, are part of the same turn. So a call records a pending request for
//! the session and `crate::agent::rewind_summary` applies it when the turn
//! settles, through the same history path a client `Rewind` request uses.
//!
//! The checkpoint is a hidden `<system-reminder>` record in the transcript, so
//! it survives a daemon restart and is rebuilt from the session history rather
//! than from memory. Files, background tasks and process state are never
//! restored; only conversation history moves.
//!
//! Registered only when `[tools] checkpoint` is on: a checkpoint nothing can
//! rewind to, or a rewind with no checkpoint, is useless alone.

use super::{Tool, ToolContext, ToolEffects, ToolOutput};
use crate::message::{ContentBlock, Role};
use crate::session::{Session, StoredDisplayRole, StoredMessage};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};

/// First line of every checkpoint record.
///
/// A stored message whose first text block starts with `<system-reminder>` is
/// an internal reminder: the transcript view hides it and the rewind target
/// list skips it, while the provider still receives it as background. The same
/// shape is what keeps the report of a rewind out of the transcript.
pub(crate) const MARKER_TAG: &str = "<system-reminder>\nhoron checkpoint";

/// The record a checkpoint leaves in the transcript.
fn marker_text() -> String {
    format!(
        "{MARKER_TAG}\nA checkpoint was recorded here. Call `rewind` with a report to fold the work \
         after this point into that report. Files, background tasks and process state are not \
         restored."
    )
}

/// Whether `message` is a checkpoint record.
pub(crate) fn is_marker(message: &StoredMessage) -> bool {
    message
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.trim_start()),
            _ => None,
        })
        .is_some_and(|text| text.starts_with(MARKER_TAG))
}

/// The stored index of the newest checkpoint record in `messages`, if any.
pub(crate) fn newest_marker_index(messages: &[StoredMessage]) -> Option<usize> {
    messages.iter().rposition(is_marker)
}

/// How many visible transcript messages precede `marker_index`.
///
/// This is the 1-based index [`crate::agent::Agent::rewind_to_message`] expects
/// for the last message before the checkpoint; `0` means the checkpoint is the
/// first thing in the session and there is nothing to rewind to.
pub(crate) fn messages_before(session: &Session, marker_index: usize) -> usize {
    session
        .rewind_target_stored_indices()
        .into_iter()
        .filter(|stored| *stored < marker_index)
        .count()
}

/// What a session asked for and the turn end has not applied yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pending {
    /// Append the checkpoint record when the turn settles.
    Checkpoint,
    /// Rewind to the active checkpoint and keep `report` in place of the branch.
    Rewind { report: String },
}

/// One session's unapplied request, if any.
///
/// Process-lifetime and keyed by session, like the context rollover request: a
/// tool sees only the session id, and a session can have at most one pending
/// request no matter how many times either tool is called in a turn.
static PENDING: LazyLock<Mutex<HashMap<String, Pending>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn pending() -> MutexGuard<'static, HashMap<String, Pending>> {
    PENDING.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The request `session_id` has queued, if any.
pub(crate) fn pending_request(session_id: &str) -> Option<Pending> {
    pending().get(session_id).cloned()
}

/// Take the request so the turn end applies it exactly once.
pub(crate) fn take_pending(session_id: &str) -> Option<Pending> {
    pending().remove(session_id)
}

/// Drop the request once it is applied, or once it cannot be used.
pub(crate) fn clear_pending(session_id: &str) {
    pending().remove(session_id);
}

/// Forget a closed session's request, like every other per-session cache.
pub(crate) fn forget_session(session_id: &str) {
    pending().remove(session_id);
}

pub(crate) fn request_checkpoint(session_id: &str) {
    pending().insert(session_id.to_string(), Pending::Checkpoint);
}

pub(crate) fn request_rewind(session_id: &str, report: String) {
    pending().insert(session_id.to_string(), Pending::Rewind { report });
}

/// Whether both tools are registered for this session.
pub(crate) fn enabled() -> bool {
    crate::config::config().tools.checkpoint
}

/// The stored history a checkpoint decision is made against.
///
/// The live session object is not reachable from a tool, and only the stored
/// transcript can answer "is a checkpoint already active": the record is
/// written when a turn settles, so everything an earlier turn marked is on disk.
fn stored_history(session_id: &str) -> Result<Vec<StoredMessage>> {
    let session = Session::load(session_id).map_err(|error| {
        anyhow::anyhow!("No readable stored history for this session yet, so nothing can be marked: {error}")
    })?;
    session.history_messages()
}

pub struct CheckpointTool;

impl CheckpointTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CheckpointTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for CheckpointTool {
    fn name(&self) -> &str {
        "checkpoint"
    }

    fn description(&self) -> &str {
        "Mark this point of the conversation so `rewind` can later fold everything after it into a \
         report you write. One checkpoint per session; it applies when this turn ends. Files, \
         background tasks and process state are not restored."
    }

    /// The record is appended to the session transcript at turn end.
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
        if let Some(pending) = pending_request(&ctx.session_id) {
            let what = match pending {
                Pending::Checkpoint => "a checkpoint",
                Pending::Rewind { .. } => "a rewind",
            };
            anyhow::bail!(
                "This turn already queued {what}. Only one checkpoint or rewind applies per turn, \
                 and the current one is applied when the turn ends."
            );
        }
        let history = stored_history(&ctx.session_id)?;
        if newest_marker_index(&history).is_some() {
            anyhow::bail!(
                "A checkpoint is already active. Call `rewind` with a report to use it before \
                 marking another."
            );
        }
        request_checkpoint(&ctx.session_id);
        Ok(ToolOutput::new(
            "Checkpoint queued. It is recorded when this turn ends; call `rewind` in a later turn \
             with a report to fold the work after it into that report. Files, background tasks and \
             process state are not restored.",
        )
        .with_title("checkpoint"))
    }
}

pub struct RewindTool;

impl RewindTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RewindTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, serde::Deserialize)]
struct RewindInput {
    report: String,
}

#[async_trait]
impl Tool for RewindTool {
    fn name(&self) -> &str {
        "rewind"
    }

    fn description(&self) -> &str {
        "Rewind to the active checkpoint: every message after it leaves this session's context and \
         `report` takes its place as background for the work ahead. Files, background tasks and \
         process state are not restored."
    }

    /// The rewind truncates the session transcript at turn end.
    fn effects(&self) -> ToolEffects {
        ToolEffects::write()
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["report"],
            "properties": {
                "intent": super::intent_schema_property(),
                "report": {
                    "type": "string",
                    "minLength": 1,
                    "description": "What the abandoned branch established, why it was abandoned, \
                                    and what the next attempt should do differently. This is the \
                                    only record of that work left in context."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: RewindInput = serde_json::from_value(input)?;
        let report = params.report.trim();
        if report.is_empty() {
            anyhow::bail!(
                "rewind needs a non-empty report: it is the only record of the discarded work that \
                 stays in context."
            );
        }
        if let Some(Pending::Checkpoint) = pending_request(&ctx.session_id) {
            anyhow::bail!(
                "This turn queued a checkpoint that is not recorded yet. Nothing after it exists to \
                 rewind away, so call `rewind` in a later turn."
            );
        }
        let session = Session::load(&ctx.session_id).map_err(|error| {
            anyhow::anyhow!("No readable stored history for this session yet, so no checkpoint is active: {error}")
        })?;
        let history = session.history_messages()?;
        let Some(marker) = newest_marker_index(&history) else {
            anyhow::bail!("No checkpoint is active. Call `checkpoint` first, in an earlier turn.");
        };
        if messages_before(&session, marker) == 0 {
            anyhow::bail!(
                "The active checkpoint has no earlier conversation to rewind to, so there is nothing \
                 to fold into a report."
            );
        }
        request_rewind(&ctx.session_id, report.to_string());
        Ok(ToolOutput::new(
            "Rewind queued. When this turn ends the conversation is cut back to the checkpoint and \
             your report takes the place of everything after it. Files, background tasks and process \
             state are not restored.",
        )
        .with_title("rewind"))
    }
}

/// A checkpoint record as a stored message, ready to be appended at turn end.
pub(crate) fn marker_message() -> (Role, Vec<ContentBlock>, Option<StoredDisplayRole>) {
    (
        Role::User,
        vec![ContentBlock::Text {
            text: marker_text(),
            cache_control: None,
        }],
        Some(StoredDisplayRole::System),
    )
}

#[cfg(test)]
#[path = "checkpoint_tests.rs"]
mod checkpoint_tests;
