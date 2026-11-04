//! Writer-local tool identities only; no message text or checkpoint history copy.
use crate::*;
use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Default)]
pub(crate) struct ToolHistory {
    seen: BTreeSet<String>,
    pending: BTreeSet<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Role {
    User,
    Assistant,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Block {
    ToolUse {
        id: String,
    },
    ToolResult {
        tool_use_id: String,
    },
    ToolReference {
        tool_use_id: String,
    },
    #[serde(other)]
    Inert,
}

#[derive(Deserialize)]
struct MessageTools {
    role: Role,
    content: Vec<Block>,
}

impl ToolHistory {
    pub fn from_messages(messages: &[ProjectedMessage]) -> Result<Self, StoreError> {
        let mut state = Self::default();
        for message in messages {
            state.append(&message.message)?;
        }
        Ok(state)
    }

    pub fn append(&mut self, message: &horon_agent_session_types::StoredMessage) -> Result<(), StoreError> {
        let value = serde_json::to_value(message).map_err(crate::cache_io::json_error)?;
        let view: MessageTools = serde_json::from_value(value).map_err(crate::cache_io::json_error)?;
        match view.role {
            Role::User | Role::Assistant => {}
        }
        for block in view.content {
            match block {
                Block::ToolUse { id } => {
                    if id.is_empty() || !self.seen.insert(id.clone()) {
                        return Err(broken("empty or duplicate tool call identity"));
                    }
                    self.pending.insert(id);
                }
                Block::ToolResult { tool_use_id } => {
                    if !self.pending.remove(&tool_use_id) {
                        return Err(broken("orphan, reordered or duplicate tool result"));
                    }
                }
                Block::ToolReference { tool_use_id } => {
                    if !self.seen.contains(&tool_use_id) {
                        return Err(broken("tool reference has no preceding call"));
                    }
                }
                Block::Inert => {}
            }
        }
        Ok(())
    }
}

fn broken(message: &str) -> StoreError {
    StoreError::invalid_request(StoreErrorCode::BrokenToolChain, message)
}
