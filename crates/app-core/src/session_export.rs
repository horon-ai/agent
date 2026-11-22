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
//!
//! Both transformations happen once, in [`project`], so the HTML report
//! ([`crate::session_export_html`]) shows exactly the text the Markdown export
//! does, escaped for its own format instead of turned into Markdown.

use crate::message::{ContentBlock, Role};
use crate::session::{Session, StoredMessage};
use anyhow::Result;

/// Longest tool result kept verbatim in an exported transcript.
pub const MAX_TOOL_RESULT_BYTES: usize = 4096;

/// One block of a redacted transcript, in the shape every renderer consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProjectedBlock {
    /// Visible text: an answer, or the body of a user turn.
    Text(String),
    /// A reasoning item, kept beside the answer it produced.
    Thinking(String),
    /// A tool call, with its arguments already redacted and pretty-printed.
    ToolCall { name: String, input: String },
    /// A tool result, labelled `Result` or `Error`.
    ToolResult { label: &'static str, content: String },
    /// A block a transcript shows as a bracketed placeholder.
    Placeholder(&'static str),
}

/// One message of a redacted transcript: the heading text and its blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectedMessage {
    /// The heading a renderer prints for this turn: `User` or `Assistant`.
    pub role: &'static str,
    pub blocks: Vec<ProjectedBlock>,
}

/// Render visible history as a Markdown transcript.
///
/// Each message becomes a `### User` or `### Assistant` heading followed by
/// its content blocks. Text is redacted, a tool call is shown as a fenced JSON
/// block, and a tool result is shown as a fenced block truncated at
/// [`MAX_TOOL_RESULT_BYTES`].
pub fn render_markdown(messages: &[StoredMessage]) -> String {
    let mut md = String::new();
    for message in project(messages) {
        md.push_str(&format!("### {}\n\n", message.role));
        for block in message.blocks {
            match block {
                ProjectedBlock::Text(text) => {
                    md.push_str(&text);
                    md.push_str("\n\n");
                }
                ProjectedBlock::Thinking(text) => md.push_str(&format!("*Thinking:* {text}\n\n")),
                ProjectedBlock::ToolCall { name, input } => {
                    md.push_str(&format!("**Tool: `{name}`**\n```json\n{input}\n```\n\n"));
                }
                ProjectedBlock::ToolResult { label, content } => {
                    md.push_str(&format!("**{label}:**\n```\n{content}\n```\n\n"));
                }
                ProjectedBlock::Placeholder(label) => md.push_str(&format!("[{label}]\n\n")),
            }
        }
    }
    md
}

/// Render a session's full history as a Markdown transcript.
pub fn export_session_markdown(session: &Session) -> Result<String> {
    Ok(render_markdown(&session.history_messages()?))
}

/// The redacted, truncated projection both export formats render.
///
/// Redaction and truncation happen here, once, so a second format cannot
/// forget a rule the Markdown transcript follows.
pub(crate) fn project(messages: &[StoredMessage]) -> Vec<ProjectedMessage> {
    messages
        .iter()
        .map(|message| ProjectedMessage {
            role: match message.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
            },
            blocks: message.content.iter().filter_map(projected_block).collect(),
        })
        .collect()
}

fn projected_block(block: &ContentBlock) -> Option<ProjectedBlock> {
    match block {
        ContentBlock::Text { text, .. } => Some(ProjectedBlock::Text(crate::message::redact_secrets(text))),
        ContentBlock::Reasoning { text } | ContentBlock::ReasoningTrace { text } => {
            Some(ProjectedBlock::Text(crate::message::redact_secrets(text)))
        }
        ContentBlock::AnthropicThinking { thinking, .. } => {
            Some(ProjectedBlock::Thinking(crate::message::redact_secrets(thinking)))
        }
        ContentBlock::OpenAIReasoning { summary, .. } => {
            let joined = summary
                .iter()
                .map(|item| crate::message::redact_secrets(item))
                .collect::<Vec<_>>()
                .join("\n");
            (!joined.is_empty()).then_some(ProjectedBlock::Thinking(joined))
        }
        ContentBlock::ToolUse { name, input, .. } => {
            let redacted = redacted_json(input);
            let input = serde_json::to_string_pretty(&redacted).unwrap_or_else(|_| redacted.to_string());
            Some(ProjectedBlock::ToolCall {
                name: name.clone(),
                input,
            })
        }
        ContentBlock::ToolResult { content, .. } => {
            let label = if block.tool_result_failed() { "Error" } else { "Result" };
            let content = crate::message::redact_secrets(content);
            let content = if content.len() > MAX_TOOL_RESULT_BYTES {
                format!(
                    "{}\n… (truncated, {} bytes total)",
                    crate::util::truncate_str(&content, MAX_TOOL_RESULT_BYTES),
                    content.len()
                )
            } else {
                content
            };
            Some(ProjectedBlock::ToolResult { label, content })
        }
        ContentBlock::Image { .. } => Some(ProjectedBlock::Placeholder("Image")),
        ContentBlock::Audio { .. } => Some(ProjectedBlock::Placeholder("audio")),
        ContentBlock::OpenAICompaction { .. } => Some(ProjectedBlock::Placeholder("OpenAI native compaction")),
        ContentBlock::ToolReference { .. } | ContentBlock::ProviderNative { .. } => None,
    }
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
