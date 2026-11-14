//! Conversation rewriting shared by the wires that require strict turn shape.
//!
//! The Anthropic Messages and Gemini `generateContent` APIs reject a request
//! whose turns do not alternate, whose tool call has no result, and whose tool
//! result answers no call. The agent loop, however, produces whatever the round
//! trips produced: several assistant messages in a row while a turn is repaired,
//! an interrupted call with no output, a retried call whose output arrived in a
//! later turn, a result stored before the call it answers. [`normalize`] rewrites
//! such a history into the shape those wires accept without changing what the
//! model reads: a stored result answers its call wherever the history keeps it,
//! and only a call with no stored result at all gets the stand-in error text.

use horon_agent_message_types::{ContentBlock, Message, Role, TOOL_OUTPUT_MISSING_TEXT};
use std::collections::{HashMap, VecDeque};

/// The text a call with no stored output gets, so the wire always sees a result.
fn missing_output() -> String {
    format!("[Error] {TOOL_OUTPUT_MISSING_TEXT}")
}

/// Rewrite `messages` into an alternating history with paired tool calls.
///
/// Four steps, in this order:
///
/// 1. a message with no content is dropped first, then adjacent messages of the
///    same role are merged with their block order kept, so an empty message can
///    neither separate two turns nor split one;
/// 2. every tool result stored in any user turn is collected into a pool keyed
///    by `tool_use_id`, in history order;
/// 3. the merged history is walked. An assistant turn is kept as it is, and its
///    tool calls become the pending calls, in call order. A user turn keeps only
///    its non-result blocks, and the reply it opens is built from the pending
///    calls: for each call in call order, the first unused pooled result with
///    that id, or a result carrying [`TOOL_OUTPUT_MISSING_TEXT`] when the pool
///    holds none. So the answers follow the call order, each stored result
///    answers exactly one call, and a result answers its call in the turn right
///    after it even when the history stored the result elsewhere (a later turn,
///    another user turn, or before the call);
/// 4. a history that ends on a call gets one appended user turn with the same
///    answers.
///
/// Pooled results no call claims are dropped, so a history that ends on such a
/// result loses its last turn and can end on an assistant turn. A duplicate call
/// id is answered once per call, which puts the same id on two results; ids are
/// unique in practice, and a strict wire rejects duplicate call ids anyway.
///
/// Pure: no I/O and no global state.
pub fn normalize(messages: &[Message]) -> Vec<Message> {
    let merged = merge_consecutive(messages);
    let mut pool = result_pool(&merged);
    let mut output: Vec<Message> = Vec::with_capacity(merged.len());
    // The calls of the latest assistant turn, in the order they were issued.
    let mut pending: Vec<String> = Vec::new();

    for message in merged {
        match message.role {
            Role::Assistant => {
                pending = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                        _ => None,
                    })
                    .collect();
                push(&mut output, message);
            }
            Role::User => {
                // Results are delivered from the pool, so the copy in this turn
                // must not be sent again.
                let mut kept: Vec<ContentBlock> = Vec::with_capacity(message.content.len());
                for block in message.content {
                    if !matches!(block, ContentBlock::ToolResult { .. }) {
                        kept.push(block);
                    }
                }
                let mut content = answers(&std::mem::take(&mut pending), &mut pool);
                content.extend(kept);
                push(
                    &mut output,
                    Message {
                        role: Role::User,
                        content,
                        timestamp: message.timestamp,
                        tool_duration_ms: message.tool_duration_ms,
                    },
                );
            }
        }
    }

    // The history ended on a call, so the reply that answers it is the one the
    // wire is waiting for.
    if !pending.is_empty() {
        let content = answers(&pending, &mut pool);
        push(
            &mut output,
            Message {
                role: Role::User,
                content,
                timestamp: None,
                tool_duration_ms: None,
            },
        );
    }

    output
}

/// Answer each call with the first unused stored result for its id.
///
/// A call the pool cannot answer gets a stand-in, which is the only case that
/// invents output.
fn answers(ids: &[String], pool: &mut HashMap<String, VecDeque<ContentBlock>>) -> Vec<ContentBlock> {
    ids.iter()
        .map(|id| match pool.get_mut(id).and_then(VecDeque::pop_front) {
            Some(block) => block,
            None => missing_result(id.clone()),
        })
        .collect()
}

/// Every stored tool result, in history order, grouped by the call it answers.
fn result_pool(messages: &[Message]) -> HashMap<String, VecDeque<ContentBlock>> {
    let mut pool: HashMap<String, VecDeque<ContentBlock>> = HashMap::new();
    for message in messages.iter().filter(|message| message.role == Role::User) {
        for block in &message.content {
            if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                pool.entry(tool_use_id.clone()).or_default().push_back(block.clone());
            }
        }
    }
    pool
}

/// A result standing in for a call whose output was never stored.
fn missing_result(tool_use_id: String) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id,
        content: missing_output(),
        is_error: Some(true),
        verdict: None,
    }
}

/// Append `message`, folding it into the previous turn when the role repeats.
///
/// A turn can lose all of its content when the results it held are delivered
/// from the pool, which is what can make the next turn repeat the role, so the
/// fold happens here rather than only in [`merge_consecutive`].
fn push(output: &mut Vec<Message>, message: Message) {
    if message.content.is_empty() {
        return;
    }
    match output.last_mut() {
        Some(last) if last.role == message.role => last.content.extend(message.content),
        _ => output.push(message),
    }
}

/// Merge adjacent messages that share a role, keeping their block order.
///
/// Empty messages are skipped before the merge, so they never break the run of
/// same-role turns on either side of them.
fn merge_consecutive(messages: &[Message]) -> Vec<Message> {
    let mut merged: Vec<Message> = Vec::with_capacity(messages.len());
    for message in messages.iter().filter(|message| !message.content.is_empty()) {
        match merged.last_mut() {
            Some(last) if last.role == message.role => last.content.extend(message.content.iter().cloned()),
            _ => merged.push(message.clone()),
        }
    }
    merged
}

#[cfg(test)]
#[path = "turns_tests.rs"]
mod turns_tests;
