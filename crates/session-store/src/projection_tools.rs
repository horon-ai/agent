//! A narrow typed view of StoredMessage serialization for tool-chain checks.
//! The full StoredMessage is retained; this adapter avoids a new production dependency.
use crate::projection_payload::decode;
use crate::*;
use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum MessageRole {
    User,
    Assistant,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Block {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
    },
    ToolResult {
        tool_use_id: String,
    },
    ToolReference {
        tool_use_id: String,
    },
    Image {},
    Audio {},
    #[serde(other)]
    Inert,
}

#[derive(Deserialize)]
struct MessageView {
    role: MessageRole,
    content: Vec<Block>,
}

/// Returns the last stored user-content position. Legacy import owns assigning
/// display roles to old synthetic prompts; rendered-index policy stays outside.
pub(crate) fn validate_tools(messages: &[ProjectedMessage], cut: Option<usize>) -> Result<Option<usize>, StoreError> {
    let mut pending = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut latest_user = None;
    for (index, item) in messages.iter().enumerate() {
        if cut == Some(index) && !pending.is_empty() {
            return Err(broken("cut splits an outstanding tool group"));
        }
        let value = serde_json::to_value(&item.message)
            .map_err(|e| StoreError::internal(StoreErrorCode::Internal, e.to_string()))?;
        let view: MessageView = decode(&value)?;
        let hidden = item.message.display_role.is_some()
            || view
                .content
                .iter()
                .find_map(|b| match b {
                    Block::Text { text } => Some(text.trim_start().starts_with("<system-reminder>")),
                    Block::ToolUse { .. }
                    | Block::ToolResult { .. }
                    | Block::ToolReference { .. }
                    | Block::Image {}
                    | Block::Audio {}
                    | Block::Inert => None,
                })
                .unwrap_or(false);
        let user = match view.role {
            MessageRole::User => true,
            MessageRole::Assistant => false,
        };
        let tool_images = view.content.iter().any(|b| matches!(b, Block::ToolResult { .. }));
        for block in view.content {
            match block {
                Block::ToolUse { id } => {
                    if id.is_empty() || !seen.insert(id.clone()) {
                        return Err(broken("empty or duplicate tool call identity"));
                    }
                    pending.insert(id);
                }
                Block::ToolResult { tool_use_id } => {
                    if !pending.remove(&tool_use_id) {
                        return Err(broken("orphan, reordered or duplicate tool result"));
                    }
                }
                Block::ToolReference { tool_use_id } => {
                    if !seen.contains(&tool_use_id) {
                        return Err(broken("tool reference has no preceding call"));
                    }
                }
                Block::Text { text } => {
                    if user
                        && !hidden
                        && !text.is_empty()
                        && !text
                            .trim_start()
                            .starts_with("[Attached image associated with the preceding tool result:")
                    {
                        latest_user = Some(index);
                    }
                }
                Block::Image {} | Block::Audio {} => {
                    if user && !hidden && !tool_images {
                        latest_user = Some(index);
                    }
                }
                Block::Inert => {}
            }
        }
    }
    if cut == Some(messages.len()) && !pending.is_empty() {
        return Err(broken("cut drops active tool calls"));
    }
    Ok(latest_user)
}

fn broken(message: &str) -> StoreError {
    StoreError::invalid_request(StoreErrorCode::BrokenToolChain, message)
}
