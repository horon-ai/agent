//! IS-6 and IS-13: `session/resume` with `replayFrom: {type: "start"}` on v2.
//!
//! The stored conversation is replayed as whole-message upserts, never as
//! chunks or `tool_call`: user rows become `user_message` with the id the
//! daemon stored for them (the id the prompt response returned), assistant rows
//! become `agent_thought` and `agent_message`, and tool rows become one
//! completed `tool_call_update`. System rows are not messages and are skipped.
//!
//! Agent and thought ids are recomputed exactly as the live mapper derives
//! them: from [`message_ids`] of the stored id of the row that started the
//! turn, advanced once before the first text after any tool activity. The
//! daemon marks that row (`turn_start`), so a user message the running turn
//! took in between its rounds (a soft interrupt) or a reminder stored inside
//! the turn continues the turn's ids instead of starting new ones. Such a user
//! row is still replayed as its own `user_message`: it is conversation the user
//! wrote, and the plan replays every user row. A History with no marked row (an
//! older daemon, or turns stored before the marker existed) falls back to
//! treating every user row and every stored system row as a start.
//! [`resume_position`] hands the same walk's end state to the mapper of a run
//! attached part-way, so the ids it streams next continue the ones replay
//! reported.

use super::super::mapper::tool_location;
use super::super::replay::{Part, split_reasoning};
use super::super::rpc::{JSONRPC_INVALID_PARAMS, RpcError};
use super::super::util::{tool_kind, tool_title, verdict_meta};
use super::shapes::{
    MessageIds, MessageKind, ToolCallPatch, message_ids, tool_call_update, user_message, whole_message,
};
use crate::protocol::HistoryMessage;
use crate::session::{RenderedImage, RenderedImageAnchor};
use serde_json::{Value, json};
use std::path::Path;

/// IS-13: whether `replayFrom` asks for the whole conversation: `false` when
/// it is absent or `null`, `true` for `{type: "start"}`. Every other cursor,
/// and any malformed value, is invalid params, so nothing is ever replayed from
/// a cursor this agent does not understand.
pub(crate) fn replay_from(params: &Value) -> Result<bool, RpcError> {
    let cursor = match params.get("replayFrom") {
        None | Some(Value::Null) => return Ok(false),
        Some(Value::Object(cursor)) => cursor,
        Some(_) => return Err(invalid("replayFrom must be an object with a string type")),
    };
    match cursor.get("type").and_then(Value::as_str) {
        Some("start") => Ok(true),
        Some(other) => Err(invalid(format!("Unsupported replayFrom cursor type {other:?}"))),
        None => Err(invalid("replayFrom must be an object with a string type")),
    }
}

fn invalid(message: impl Into<String>) -> RpcError {
    RpcError::new(JSONRPC_INVALID_PARAMS, message)
}

/// Where the message ids of a turn stand after its stored rows: the state the
/// live mapper of that turn is in when it has mapped the same events.
#[derive(Clone, Debug)]
pub(crate) struct Position {
    /// The stored id of the row that started the turn.
    pub(crate) start: String,
    pub(crate) ids: MessageIds,
    /// A tool round followed the last text: the next text opens a new message.
    pub(crate) round_pending: bool,
}

/// The id walk over history rows, shared by replay and [`resume_position`].
#[derive(Default)]
struct Walk {
    position: Option<Position>,
    /// The content of the current round's agent message and thought, which
    /// every upsert replaces whole.
    agent: Vec<Value>,
    thought: Vec<Value>,
}

impl Walk {
    fn start(&mut self, start: &str) {
        self.position = Some(Position {
            start: start.to_string(),
            ids: message_ids(start),
            round_pending: false,
        });
        self.agent.clear();
        self.thought.clear();
    }

    fn tool_round(&mut self) {
        if let Some(position) = self.position.as_mut() {
            position.round_pending = true;
        }
    }

    /// The ids of the message the next text belongs to. A row of agent text
    /// before any start row (its turn was compacted away) keys its own turn.
    fn open_message(&mut self, fallback_start: &str) -> MessageIds {
        if self.position.is_none() {
            self.start(fallback_start);
        }
        let position = self.position.as_mut().expect("a turn was just started");
        if position.round_pending {
            position.round_pending = false;
            position.ids.advance_after_tool_round();
            self.agent.clear();
            self.thought.clear();
        }
        position.ids.clone()
    }
}

/// A row's stored id, or a replay-local one for a row the daemon stored none
/// for, unique per row.
fn row_id(message: &HistoryMessage, index: usize) -> String {
    message.message_id.clone().unwrap_or_else(|| format!("replay-{index}"))
}

/// Every replay update of `messages`, in order. `images` are the history's
/// rendered images; those attached to a user prompt follow its text.
#[cfg(test)]
pub(crate) fn replay_updates(messages: &[HistoryMessage], images: &[RenderedImage], cwd: &Path) -> Vec<Value> {
    replay_updates_with_a2ui(messages, images, cwd, false)
}

/// Replay stored history with A2UI negotiation flag governing whether A2UI payloads
/// render as `EmbeddedResource` (`a2ui_enabled = true`) or readable text cards.
pub(crate) fn replay_updates_with_a2ui(
    messages: &[HistoryMessage],
    images: &[RenderedImage],
    cwd: &Path,
    a2ui_enabled: bool,
) -> Vec<Value> {
    walk_history(messages, images, cwd, a2ui_enabled).0
}

/// The id state the last turn of `messages` ends in; `None` for a history
/// with no turn. The same walk as [`replay_updates`], so the two never differ.
#[cfg(test)]
pub(crate) fn resume_position(messages: &[HistoryMessage]) -> Option<Position> {
    resume_position_with_a2ui(messages, false)
}

/// Position counterpart to [`replay_updates_with_a2ui`].
pub(crate) fn resume_position_with_a2ui(messages: &[HistoryMessage], a2ui_enabled: bool) -> Option<Position> {
    walk_history(messages, &[], Path::new("/"), a2ui_enabled).1.position
}

/// Whether row `index` started a turn. From the first marked row on, every
/// start is marked, since the build that wrote it marks every start; rows
/// before it were stored without the marker, where every user row and every
/// stored system row is taken as a start.
fn starts_turn(message: &HistoryMessage, index: usize, first_marked: Option<usize>) -> bool {
    match first_marked {
        Some(first) if index >= first => message.turn_start,
        _ => message.role == "user" || message.message_id.is_some(),
    }
}

fn walk_history(
    messages: &[HistoryMessage],
    images: &[RenderedImage],
    cwd: &Path,
    a2ui_enabled: bool,
) -> (Vec<Value>, Walk) {
    let first_marked = messages.iter().position(|message| message.turn_start);
    let mut walk = Walk::default();
    let mut updates = Vec::new();
    let mut user_prompts = 0usize;
    let mut pending_a2ui: Vec<super::super::a2ui::PendingA2uiMessage> = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        let starts = starts_turn(message, index, first_marked);
        match message.role.as_str() {
            "user" => {
                flush_pending_a2ui_v2(
                    &mut updates,
                    &mut walk,
                    &mut pending_a2ui,
                    &format!("replay-a2ui-{index}"),
                    a2ui_enabled,
                );
                let id = row_id(message, index);
                if starts {
                    walk.start(&id);
                }
                let mut content = vec![json!({ "type": "text", "text": message.content })];
                content.extend(
                    images
                        .iter()
                        .filter(|image| {
                            matches!(image.anchor, Some(RenderedImageAnchor::UserPrompt { ordinal }) if ordinal == user_prompts)
                        })
                        .map(|image| json!({ "type": "image", "mimeType": image.media_type, "data": image.data })),
                );
                user_prompts += 1;
                updates.push(user_message(&id, &content));
            }
            "assistant" => {
                let ids = walk.open_message(&row_id(message, index));
                let (mut thought, mut agent) = (false, false);
                let (text_to_split, matched_a2ui) = if pending_a2ui.is_empty() {
                    (message.content.clone(), Vec::new())
                } else {
                    let (remaining, matched) =
                        super::super::a2ui::split_assistant_text_and_pending_a2ui(&message.content, &mut pending_a2ui);
                    if a2ui_enabled {
                        (remaining, matched)
                    } else {
                        (message.content.clone(), Vec::new())
                    }
                };
                if !text_to_split.is_empty() {
                    for part in split_reasoning(&text_to_split) {
                        match part {
                            Part::Thought(text) => {
                                walk.thought.push(json!({ "type": "text", "text": text }));
                                thought = true;
                            }
                            Part::Text(text) => {
                                walk.agent.push(json!({ "type": "text", "text": text }));
                                agent = true;
                            }
                        }
                    }
                }
                if !matched_a2ui.is_empty() {
                    walk.agent
                        .extend(super::super::a2ui::embedded_resources_for_pending(&matched_a2ui));
                    agent = true;
                }
                if thought {
                    updates.push(whole_message(MessageKind::AgentThought, &ids.thought(), &walk.thought));
                }
                if agent {
                    updates.push(whole_message(MessageKind::AgentMessage, &ids.agent(), &walk.agent));
                }
            }
            "tool" => match &message.tool_data {
                Some(call) if super::super::a2ui::is_a2ui_tool_name(&call.name) => {
                    if let Some(entry) =
                        super::super::a2ui::extract_replay_a2ui_message(&call.id, &call.input, &message.content)
                    {
                        pending_a2ui.push(entry);
                    } else if !a2ui_enabled {
                        walk.tool_round();
                        updates.push(tool_update(call, &message.content, message.verdict, cwd));
                    }
                }
                Some(call) => {
                    walk.tool_round();
                    updates.push(tool_update(call, &message.content, message.verdict, cwd));
                }
                None => {
                    walk.tool_round();
                }
            },
            // A system row is never a message. One that started a turn (an
            // `[auto]` or system turn) keys that turn's ids; a reminder inside
            // a turn and the compacted-history notice (no stored id) start
            // nothing. Background task notes arrive inside a running turn.
            "system" => {
                if let (true, Some(id)) = (starts, &message.message_id) {
                    flush_pending_a2ui_v2(
                        &mut updates,
                        &mut walk,
                        &mut pending_a2ui,
                        &format!("replay-a2ui-{index}"),
                        a2ui_enabled,
                    );
                    walk.start(id);
                }
            }
            _ => {}
        }
    }
    flush_pending_a2ui_v2(
        &mut updates,
        &mut walk,
        &mut pending_a2ui,
        &format!("replay-a2ui-{}", messages.len()),
        a2ui_enabled,
    );
    (updates, walk)
}

fn flush_pending_a2ui_v2(
    updates: &mut Vec<Value>,
    walk: &mut Walk,
    pending_a2ui: &mut Vec<super::super::a2ui::PendingA2uiMessage>,
    fallback_id: &str,
    a2ui_enabled: bool,
) {
    if pending_a2ui.is_empty() {
        return;
    }
    let drained = std::mem::take(pending_a2ui);
    let ids = walk.open_message(fallback_id);
    if a2ui_enabled {
        walk.agent
            .extend(super::super::a2ui::embedded_resources_for_pending(&drained));
    } else {
        for entry in drained {
            if !entry.expected_card.is_empty() {
                walk.agent.push(json!({ "type": "text", "text": entry.expected_card }));
            }
        }
    }
    if !walk.agent.is_empty() {
        updates.push(whole_message(MessageKind::AgentMessage, &ids.agent(), &walk.agent));
    }
}

/// One stored tool call as a completed upsert that creates it on a client that
/// never saw it: name, title, kind, input, location, the stored result text and
/// the stored verdict, when the result carries one.
fn tool_update(
    call: &crate::message::ToolCall,
    output: &str,
    verdict: Option<crate::tool::ToolVerdict>,
    cwd: &Path,
) -> Value {
    let raw_input = call.input.is_object().then(|| call.input.clone());
    let locations = raw_input
        .as_ref()
        .and_then(|input| tool_location(input, Some(cwd)))
        .map(|path| vec![json!({ "path": path })]);
    let patch = ToolCallPatch {
        name: Some(call.name.clone()),
        title: Some(tool_title(&call.name)),
        kind: Some(tool_kind(&call.name).to_string()),
        status: Some("completed".to_string()),
        content: Some(vec![json!({
            "type": "content",
            "content": { "type": "text", "text": output },
        })]),
        locations,
        raw_input,
        raw_output: None,
        // A stored call is already over: it advertises no client controls.
        capabilities: None,
        meta: verdict_meta(verdict),
    };
    tool_call_update(&call.id, &patch)
}
