//! Markdown transcript export.
//!
//! One renderer serves the harness API request, the CLI `export` command and
//! the ambient runner, so the three can never disagree about the text a user
//! sees. Two transformations are applied while rendering rather than before:
//!
//! - Secrets are redacted through [`crate::message::redact_secrets`], the same
//!   pattern table the disk path uses.
//! - A tool result longer than [`MAX_TOOL_RESULT_BYTES`] is cut at a UTF-8
//!   boundary and replaced with a marker naming the original size, so one
//!   runaway command cannot make a transcript unreadable.

use crate::message::{ContentBlock, Role};
use crate::session::{Session, StoredMessage};
use anyhow::Result;

/// Longest tool result kept verbatim in an exported transcript.
pub const MAX_TOOL_RESULT_BYTES: usize = 4096;

/// Render visible history as a Markdown transcript.
///
/// Each message becomes a `### User` or `### Assistant` heading followed by
/// its content blocks. Text is redacted, a tool call is shown as a fenced JSON
/// block, and a tool result is shown as a fenced block truncated at
/// [`MAX_TOOL_RESULT_BYTES`].
pub fn render_markdown(messages: &[StoredMessage]) -> String {
    let mut md = String::new();
    for msg in messages {
        let role_label = match msg.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
        };
        md.push_str(&format!("### {role_label}\n\n"));
        for block in &msg.content {
            match block {
                ContentBlock::Text { text, .. }
                | ContentBlock::Reasoning { text }
                | ContentBlock::ReasoningTrace { text } => {
                    let text = crate::message::redact_secrets(text);
                    md.push_str(&text);
                    md.push_str("\n\n");
                }
                ContentBlock::AnthropicThinking { thinking, .. } => {
                    let thinking = crate::message::redact_secrets(thinking);
                    md.push_str(&format!("*Thinking:* {thinking}\n\n"));
                }
                ContentBlock::OpenAIReasoning { summary, .. } => {
                    let joined = summary
                        .iter()
                        .map(|item| crate::message::redact_secrets(item))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !joined.is_empty() {
                        md.push_str(&format!("*Thinking:* {joined}\n\n"));
                    }
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    let redacted = redacted_json(input);
                    let input_str = serde_json::to_string_pretty(&redacted).unwrap_or_else(|_| redacted.to_string());
                    md.push_str(&format!("**Tool: `{name}`**\n```json\n{input_str}\n```\n\n"));
                }
                ContentBlock::ToolResult { content, is_error, .. } => {
                    let label = if is_error == &Some(true) { "Error" } else { "Result" };
                    let content = crate::message::redact_secrets(content);
                    let display = if content.len() > MAX_TOOL_RESULT_BYTES {
                        format!(
                            "{}\n… (truncated, {} bytes total)",
                            crate::util::truncate_str(&content, MAX_TOOL_RESULT_BYTES),
                            content.len()
                        )
                    } else {
                        content
                    };
                    md.push_str(&format!("**{label}:**\n```\n{display}\n```\n\n"));
                }
                ContentBlock::Image { .. } => md.push_str("[Image]\n\n"),
                ContentBlock::Audio { .. } => md.push_str("[audio]\n\n"),
                ContentBlock::OpenAICompaction { .. } => md.push_str("[OpenAI native compaction]\n\n"),
                ContentBlock::ToolReference { .. } | ContentBlock::ProviderNative { .. } => {}
            }
        }
    }
    md
}

/// Render a session's full history as a Markdown transcript.
pub fn export_session_markdown(session: &Session) -> Result<String> {
    Ok(render_markdown(&session.history_messages()?))
}

/// Redact every string value of a tool call's input.
fn redacted_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => serde_json::Value::String(crate::message::redact_secrets(text)),
        serde_json::Value::Array(items) => serde_json::Value::Array(items.iter().map(redacted_json).collect()),
        serde_json::Value::Object(fields) => serde_json::Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), redacted_json(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
#[path = "session_export_tests.rs"]
mod tests;
