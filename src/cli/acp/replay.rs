//! `session/load` replay: a stored conversation as the `session/update`
//! payloads a client saw while the turns ran live.
//!
//! The input is the daemon's rendered history (`render.rs`), one row per
//! visible message. Rows map onto the live mapper's shapes: user text, thought
//! and message chunks, a tool call with its final update, and the plan a todo
//! call showed. What history does not keep cannot be replayed: structured file
//! diffs (a tool result persists only its text, which carries the unified
//! diff), the failed status of a tool, and images a tool returned, which no live
//! update ever showed. A stored verdict is kept, so the final update carries it
//! under `_meta` as the live one did while `status` stays `completed`. System
//! rows are skipped; the compacted-prefix notice becomes a `compaction_update`
//! only for a client that advertised `session.compaction`.

use super::*;
use crate::protocol::HistoryMessage;
use crate::session::{RenderedImage, RenderedImageAnchor, is_compacted_history_notice, reasoning_line_content};

/// Every replay update of `messages`, in order. `images` are the history's
/// rendered images; those attached to a user prompt follow that prompt's text.
pub(super) fn replay_updates(
    messages: &[HistoryMessage],
    images: &[RenderedImage],
    options: &MapperOptions,
) -> Vec<Value> {
    let mut updates = Vec::new();
    let mut user_prompts = 0usize;
    let mut pending_a2ui: Vec<a2ui::PendingA2uiMessage> = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        match message.role.as_str() {
            "user" => {
                flush_pending_a2ui(&mut updates, &mut pending_a2ui, index, options.a2ui);
                let message_id = format!("replay-{index}");
                updates.push(chunk("user_message_chunk", &message_id, text_block(&message.content)));
                for image in images.iter().filter(|image| {
                    matches!(image.anchor, Some(RenderedImageAnchor::UserPrompt { ordinal }) if ordinal == user_prompts)
                }) {
                    let block = json!({ "type": "image", "mimeType": image.media_type, "data": image.data });
                    updates.push(chunk("user_message_chunk", &message_id, block));
                }
                user_prompts += 1;
            }
            "assistant" => {
                let message_id = format!("replay-{index}-msg");
                if pending_a2ui.is_empty() {
                    emit_assistant_text_parts(&mut updates, index, &message.content);
                } else if options.a2ui {
                    let (remaining_text, matched) =
                        a2ui::split_assistant_text_and_pending_a2ui(&message.content, &mut pending_a2ui);
                    if !remaining_text.is_empty() {
                        emit_assistant_text_parts(&mut updates, index, &remaining_text);
                    }
                    for resource in a2ui::embedded_resources_for_pending(&matched) {
                        updates.push(chunk("agent_message_chunk", &message_id, resource));
                    }
                } else {
                    let (_remaining, _matched) =
                        a2ui::split_assistant_text_and_pending_a2ui(&message.content, &mut pending_a2ui);
                    emit_assistant_text_parts(&mut updates, index, &message.content);
                }
            }
            "tool" => match &message.tool_data {
                Some(call) if a2ui::is_a2ui_tool_name(&call.name) => {
                    if let Some(entry) = a2ui::extract_replay_a2ui_message(&call.id, &call.input, &message.content) {
                        pending_a2ui.push(entry);
                    } else if !options.a2ui {
                        tool_updates(
                            &mut updates,
                            call,
                            &message.content,
                            message.is_error,
                            message.verdict,
                            options,
                        );
                    }
                }
                Some(call) => tool_updates(
                    &mut updates,
                    call,
                    &message.content,
                    message.is_error,
                    message.verdict,
                    options,
                ),
                None => updates.push(agent_text(index, &message.content)),
            },
            "system" => {
                if options.compaction && is_compacted_history_notice(&message.content) {
                    updates.push(json!({
                        "sessionUpdate": "compaction_update",
                        "compactionId": format!("replay-{index}-cmp"),
                        "status": "completed",
                    }));
                }
            }
            _ => updates.push(agent_text(index, &message.content)),
        }
    }
    flush_pending_a2ui(&mut updates, &mut pending_a2ui, messages.len(), options.a2ui);
    updates
}

fn emit_assistant_text_parts(updates: &mut Vec<Value>, index: usize, text: &str) {
    for part in split_reasoning(text) {
        let (update, kind, part_text) = match part {
            Part::Thought(part_text) => ("agent_thought_chunk", "thought", part_text),
            Part::Text(part_text) => ("agent_message_chunk", "msg", part_text),
        };
        updates.push(chunk(update, &format!("replay-{index}-{kind}"), text_block(&part_text)));
    }
}

fn flush_pending_a2ui(
    updates: &mut Vec<Value>,
    pending_a2ui: &mut Vec<a2ui::PendingA2uiMessage>,
    index: usize,
    a2ui_enabled: bool,
) {
    if pending_a2ui.is_empty() {
        return;
    }
    let drained = std::mem::take(pending_a2ui);
    let message_id = format!("replay-{index}-a2ui-msg");
    if a2ui_enabled {
        for resource in a2ui::embedded_resources_for_pending(&drained) {
            updates.push(chunk("agent_message_chunk", &message_id, resource));
        }
    } else {
        for entry in drained {
            if !entry.expected_card.is_empty() {
                updates.push(chunk(
                    "agent_message_chunk",
                    &message_id,
                    text_block(&entry.expected_card),
                ));
            }
        }
    }
}

/// A completed (or failed) call: the call, the plan a todo write showed, then
/// the final update with the stored result text and the stored verdict, when
/// there is one.
fn tool_updates(
    updates: &mut Vec<Value>,
    call: &crate::message::ToolCall,
    output: &str,
    is_error: Option<bool>,
    verdict: Option<crate::tool::ToolVerdict>,
    options: &MapperOptions,
) {
    let failed = is_error == Some(true);
    let mut started = json!({
        "sessionUpdate": "tool_call",
        "toolCallId": call.id,
        "name": call.name,
        "title": tool_title(&call.name),
        "kind": tool_kind(&call.name),
        "status": if failed { "pending" } else { "completed" },
    });
    if call.input.is_object()
        && let Some(object) = started.as_object_mut()
    {
        if let Some(path) = tool_location(&call.input, options.cwd.as_deref()) {
            object.insert("locations".to_string(), json!([{ "path": path }]));
        }
        object.insert("rawInput".to_string(), call.input.clone());
    }
    updates.push(started);
    if call.name == "todo"
        && let Some(entries) = crate::tool::todo_call_plan(&call.input, output)
    {
        updates.push(plan_update(&entries));
    }
    let mut done = json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": call.id,
        "title": tool_title(&call.name),
        "kind": tool_kind(&call.name),
        "status": if failed { "failed" } else { "completed" },
        "content": [{ "type": "content", "content": text_block(output) }],
        "rawOutput": { "output": output },
    });
    if let Some(meta) = verdict_meta(verdict) {
        done["_meta"] = meta;
    }
    updates.push(done);
}

fn agent_text(index: usize, text: &str) -> Value {
    chunk("agent_message_chunk", &format!("replay-{index}-msg"), text_block(text))
}

fn chunk(update: &str, message_id: &str, content: Value) -> Value {
    json!({ "sessionUpdate": update, "messageId": message_id, "content": content })
}

fn text_block(text: &str) -> Value {
    json!({ "type": "text", "text": text })
}

#[derive(Debug, PartialEq)]
pub(super) enum Part {
    Thought(String),
    Text(String),
}

/// Split an assistant row into its reasoning and answer runs.
///
/// The renderer writes each reasoning line with `reasoning_line_markup`
/// (an empty line as a bare newline) and ends the block with one blank line,
/// before the answer text. `reasoning_line_content` reverses the line markup
/// and its escaping. Parts that are empty or only whitespace are dropped.
pub(super) fn split_reasoning(content: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut thought: Vec<String> = Vec::new();
    let mut in_thought = false;
    // Bare newlines after a reasoning line: empty reasoning lines, or the
    // block's terminator when the run ends.
    let mut blanks = 0usize;
    let mut text = String::new();
    for line in content.split_inclusive('\n') {
        if let Some(body) = reasoning_line_content(line) {
            if !in_thought {
                push_part(&mut parts, Part::Text(std::mem::take(&mut text)));
                in_thought = true;
            }
            thought.extend(std::iter::repeat_n(String::new(), blanks));
            blanks = 0;
            thought.push(body);
        } else if in_thought && line == "\n" {
            blanks += 1;
        } else {
            if in_thought {
                close_thought(&mut parts, &mut thought, blanks);
                in_thought = false;
                blanks = 0;
            }
            text.push_str(line);
        }
    }
    if in_thought {
        close_thought(&mut parts, &mut thought, blanks);
    }
    push_part(&mut parts, Part::Text(text));
    parts
}

/// End a reasoning run: the first trailing blank is the terminator, the rest
/// were empty reasoning lines.
fn close_thought(parts: &mut Vec<Part>, thought: &mut Vec<String>, blanks: usize) {
    thought.extend(std::iter::repeat_n(String::new(), blanks.saturating_sub(1)));
    push_part(parts, Part::Thought(std::mem::take(thought).join("\n")));
}

fn push_part(parts: &mut Vec<Part>, part: Part) {
    let (Part::Thought(text) | Part::Text(text)) = &part;
    if !text.trim().is_empty() {
        parts.push(part);
    }
}
