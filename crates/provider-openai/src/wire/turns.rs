//! Conversation rewriting shared by the wires that require strict turn shape.
//!
//! The Anthropic Messages and Gemini `generateContent` APIs reject a request
//! whose turns do not alternate, whose tool call has no result, and whose tool
//! result answers no call. The agent loop, however, produces whatever the round
//! trips produced: several assistant messages in a row while a turn is repaired,
//! an interrupted call with no output, a retried call whose earlier output
//! arrived alone. [`normalize`] rewrites such a history into the shape those
//! wires accept, without changing what the model reads.

use horon_agent_message_types::{ContentBlock, Message, Role, TOOL_OUTPUT_MISSING_TEXT};

/// The text a call with no stored output gets, so the wire always sees a result.
fn missing_output() -> String {
    format!("[Error] {TOOL_OUTPUT_MISSING_TEXT}")
}

/// Rewrite `messages` into an alternating history with paired tool calls.
///
/// Three rewrites, in this order:
///
/// 1. consecutive messages of the same role are merged, so the turns alternate;
/// 2. a tool result is kept only when a tool call with that id precedes it, and
///    each such call is answered at most once;
/// 3. a tool call with no result gets one carrying [`TOOL_OUTPUT_MISSING_TEXT`],
///    placed in the reply that follows it (a reply is appended when the history
///    ends on the call).
///
/// Pure: no I/O, no global state, and the input is not modified.
pub fn normalize(messages: &[Message]) -> Vec<Message> {
    let merged = merge_consecutive(messages);
    let mut output: Vec<Message> = Vec::with_capacity(merged.len());
    // Calls awaiting a result, in the order they were issued.
    let mut open_calls: Vec<String> = Vec::new();

    for message in merged {
        match message.role {
            Role::Assistant => {
                for block in &message.content {
                    if let ContentBlock::ToolUse { id, .. } = block {
                        open_calls.push(id.clone());
                    }
                }
                push(&mut output, message);
            }
            Role::User => {
                let (results, others) = split_results(message.content);
                // A result is only paired when a call precedes it; everything
                // else answers nothing and would be rejected.
                let mut answers: Vec<ContentBlock> = Vec::new();
                for block in results {
                    let ContentBlock::ToolResult { tool_use_id, .. } = &block else {
                        continue;
                    };
                    if let Some(position) = open_calls.iter().position(|id| id == tool_use_id) {
                        open_calls.remove(position);
                        answers.push(block);
                    }
                }
                // Calls this reply left unanswered get a synthesized result
                // placed before the real ones, in the order the calls were made.
                if !open_calls.is_empty() {
                    let mut synthesized: Vec<ContentBlock> = std::mem::take(&mut open_calls)
                        .into_iter()
                        .map(missing_result)
                        .collect();
                    synthesized.extend(answers);
                    answers = synthesized;
                }
                let mut content = answers;
                content.extend(others);
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
    if !open_calls.is_empty() {
        let content = open_calls.into_iter().map(missing_result).collect();
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
/// Empty messages are dropped, which is what can make the next turn repeat the
/// role, so the fold happens here rather than only in [`merge_consecutive`].
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
fn merge_consecutive(messages: &[Message]) -> Vec<Message> {
    let mut merged: Vec<Message> = Vec::with_capacity(messages.len());
    for message in messages {
        match merged.last_mut() {
            Some(last) if last.role == message.role => last.content.extend(message.content.iter().cloned()),
            _ => merged.push(message.clone()),
        }
    }
    merged
}

/// Split a user message into its tool results and everything else, keeping the
/// order within each group.
fn split_results(content: Vec<ContentBlock>) -> (Vec<ContentBlock>, Vec<ContentBlock>) {
    let mut results = Vec::new();
    let mut others = Vec::new();
    for block in content {
        if matches!(block, ContentBlock::ToolResult { .. }) {
            results.push(block);
        } else {
            others.push(block);
        }
    }
    (results, others)
}

#[cfg(test)]
#[path = "turns_tests.rs"]
mod turns_tests;
