//! The message record a session stores, plus the schema-repair record a
//! repaired tool call carries.

use chrono::{DateTime, Utc};
use horon_agent_message_types::degradation::DegradationReceipt;
use horon_agent_message_types::{ContentBlock, Message, Role};
use serde::{Deserialize, Serialize};

use crate::{StoredDisplayRole, StoredTokenUsage};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: String,
    pub role: Role,
    pub content: Vec<ContentBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_role: Option<StoredDisplayRole>,
    /// This message started a turn: it is the `start_message_id` that turn
    /// reports. A message stored inside a running turn (a soft interrupt, a
    /// reminder, a tool result) leaves it unset, and so does every message an
    /// older build stored.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub turn_start: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<StoredTokenUsage>,
    /// Arguments the model sent for this tool call before the schema-directed
    /// repair rewrote them. Only a repaired call carries it, so every other
    /// record keeps its previous bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_input: Option<serde_json::Value>,
    /// One entry per repair applied to produce the arguments that ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg_repairs: Option<Vec<String>>,
    /// What this turn's request lost to provider schema constraints: dropped
    /// schema keywords or string formats, a tool refused strict mode, and a
    /// duplicate definition that was not sent. Only a narrowed request carries
    /// it, so every other record keeps its previous bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degradations: Option<Vec<DegradationReceipt>>,
}

impl Default for StoredMessage {
    /// A blank record, meant as the base of a struct-update literal for callers
    /// that set every field they care about. Nothing appends one as it stands:
    /// the id is empty and there is no content.
    fn default() -> Self {
        Self {
            id: String::new(),
            role: Role::User,
            content: Vec::new(),
            display_role: None,
            turn_start: false,
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
            original_input: None,
            arg_repairs: None,
            degradations: None,
        }
    }
}

impl StoredMessage {
    pub fn to_message(&self) -> Message {
        Message {
            role: self.role.clone(),
            content: self.content.clone(),
            timestamp: self.timestamp,
            tool_duration_ms: self.tool_duration_ms,
        }
    }

    /// Get a text preview of the message content
    pub fn content_preview(&self) -> String {
        for block in &self.content {
            match block {
                ContentBlock::Text { text, .. } => {
                    // Return first non-empty text block
                    let text = text.trim();
                    if !text.is_empty() {
                        return text.replace('\n', " ");
                    }
                }
                ContentBlock::ToolUse { name, .. } => {
                    return format!("[tool: {}]", name);
                }
                ContentBlock::ToolResult { content, .. } => {
                    let preview = content.trim().replace('\n', " ");
                    if !preview.is_empty() {
                        return format!("[result: {}]", preview);
                    }
                }
                _ => {}
            }
        }
        "(empty)".to_string()
    }
}
