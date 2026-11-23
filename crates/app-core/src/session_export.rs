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
//! Both transformations happen once, in [`stream_export`], so the HTML report
//! ([`crate::session_export_html`]) shows exactly the text the Markdown export
//! does, escaped for its own format instead of turned into Markdown.

use std::io::Write as _;

use crate::message::{ContentBlock, Role};
use crate::session::{Session, StoredMessage};
use anyhow::Result;

/// Longest tool result kept verbatim in an exported transcript.
pub const MAX_TOOL_RESULT_BYTES: usize = 4096;

/// Streaming target shared by the Markdown and HTML transcript exporters.
///
/// Redaction and truncation happen once in [`stream_export`], so both formats
/// always emit identical text without allocating intermediate projected trees.
pub(crate) trait ExportSink {
    fn begin_message(&mut self, role: &'static str);
    fn end_message(&mut self);
    fn text(&mut self, text: &str);
    fn thinking(&mut self, text: &str);
    fn tool_call(&mut self, name: &str, input: &serde_json::Value);
    fn tool_result(&mut self, label: &'static str, content: &str, truncated_total: Option<usize>);
    fn placeholder(&mut self, label: &'static str);
}

/// Serialize a JSON value while redacting every string leaf in place.
struct RedactedJson<'a>(&'a serde_json::Value);

impl serde::Serialize for RedactedJson<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        match self.0 {
            serde_json::Value::Null => serializer.serialize_unit(),
            serde_json::Value::Bool(value) => serializer.serialize_bool(*value),
            serde_json::Value::Number(value) => value.serialize(serializer),
            serde_json::Value::String(text) => serializer.serialize_str(&crate::message::redact_secrets(text)),
            serde_json::Value::Array(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(&RedactedJson(item))?;
                }
                seq.end()
            }
            serde_json::Value::Object(fields) => {
                let mut map = serializer.serialize_map(Some(fields.len()))?;
                for (key, value) in fields {
                    map.serialize_entry(key, &RedactedJson(value))?;
                }
                map.end()
            }
        }
    }
}

pub(crate) fn write_redacted_pretty_json<W: std::io::Write>(writer: &mut W, value: &serde_json::Value) {
    if serde_json::to_writer_pretty(&mut *writer, &RedactedJson(value)).is_err() {
        let _fallback = serde_json::to_writer(&mut *writer, &RedactedJson(value));
    }
}

struct MarkdownSink(Vec<u8>);

impl ExportSink for MarkdownSink {
    fn begin_message(&mut self, role: &'static str) {
        self.0.extend_from_slice(b"### ");
        self.0.extend_from_slice(role.as_bytes());
        self.0.extend_from_slice(b"\n\n");
    }

    fn end_message(&mut self) {}

    fn text(&mut self, text: &str) {
        self.0.extend_from_slice(text.as_bytes());
        self.0.extend_from_slice(b"\n\n");
    }

    fn thinking(&mut self, text: &str) {
        self.0.extend_from_slice(b"*Thinking:* ");
        self.0.extend_from_slice(text.as_bytes());
        self.0.extend_from_slice(b"\n\n");
    }

    fn tool_call(&mut self, name: &str, input: &serde_json::Value) {
        self.0.extend_from_slice(b"**Tool: `");
        self.0.extend_from_slice(name.as_bytes());
        self.0.extend_from_slice(b"`**\n```json\n");
        write_redacted_pretty_json(&mut self.0, input);
        self.0.extend_from_slice(b"\n```\n\n");
    }

    fn tool_result(&mut self, label: &'static str, content: &str, truncated_total: Option<usize>) {
        self.0.extend_from_slice(b"**");
        self.0.extend_from_slice(label.as_bytes());
        self.0.extend_from_slice(b":**\n```\n");
        self.0.extend_from_slice(content.as_bytes());
        if let Some(total) = truncated_total {
            let _write_res = write!(self.0, "\n… (truncated, {total} bytes total)");
        }
        self.0.extend_from_slice(b"\n```\n\n");
    }

    fn placeholder(&mut self, label: &'static str) {
        self.0.push(b'[');
        self.0.extend_from_slice(label.as_bytes());
        self.0.extend_from_slice(b"]\n\n");
    }
}

/// Render visible history as a Markdown transcript.
///
/// Each message becomes a `### User` or `### Assistant` heading followed by
/// its content blocks. Text is redacted, a tool call is shown as a fenced JSON
/// block, and a tool result is shown as a fenced block truncated at
/// [`MAX_TOOL_RESULT_BYTES`].
pub fn render_markdown(messages: &[StoredMessage]) -> String {
    let mut sink = MarkdownSink(Vec::with_capacity(messages.len().saturating_mul(256)));
    stream_export(messages, &mut sink);
    String::from_utf8(sink.0).unwrap_or_else(|err| String::from_utf8_lossy(err.as_bytes()).into_owned())
}

/// Render a session's full history as a Markdown transcript.
pub fn export_session_markdown(session: &Session) -> Result<String> {
    Ok(render_markdown(&session.history_messages()?))
}

/// Stream the redacted, truncated transcript directly into `sink`.
pub(crate) fn stream_export(messages: &[StoredMessage], sink: &mut impl ExportSink) {
    let mut summary_scratch = String::new();
    for message in messages {
        let role = match message.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
        };
        sink.begin_message(role);
        for block in &message.content {
            match block {
                ContentBlock::Text { text, .. }
                | ContentBlock::Reasoning { text }
                | ContentBlock::ReasoningTrace { text } => {
                    let redacted = crate::message::redact_secrets(text);
                    sink.text(&redacted);
                }
                ContentBlock::AnthropicThinking { thinking, .. } => {
                    let redacted = crate::message::redact_secrets(thinking);
                    sink.thinking(&redacted);
                }
                ContentBlock::OpenAIReasoning { summary, .. } => {
                    summary_scratch.clear();
                    for (index, item) in summary.iter().enumerate() {
                        if index > 0 {
                            summary_scratch.push('\n');
                        }
                        summary_scratch.push_str(&crate::message::redact_secrets(item));
                    }
                    if !summary_scratch.is_empty() {
                        sink.thinking(&summary_scratch);
                    }
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    sink.tool_call(name, input);
                }
                ContentBlock::ToolResult { content, .. } => {
                    let label = if block.tool_result_failed() { "Error" } else { "Result" };
                    let redacted = crate::message::redact_secrets(content);
                    if redacted.len() > MAX_TOOL_RESULT_BYTES {
                        sink.tool_result(
                            label,
                            crate::util::truncate_str(&redacted, MAX_TOOL_RESULT_BYTES),
                            Some(redacted.len()),
                        );
                    } else {
                        sink.tool_result(label, &redacted, None);
                    }
                }
                ContentBlock::Image { .. } => sink.placeholder("Image"),
                ContentBlock::Audio { .. } => sink.placeholder("audio"),
                ContentBlock::OpenAICompaction { .. } => sink.placeholder("OpenAI native compaction"),
                ContentBlock::ToolReference { .. } | ContentBlock::ProviderNative { .. } => {}
            }
        }
        sink.end_message();
    }
}

#[cfg(test)]
#[path = "session_export_tests.rs"]
mod tests;
