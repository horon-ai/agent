//! Incremental identity-only tool validation; never retains message bodies.
use super::{ContentBlock, StoredMessage};
use anyhow::Result;
use horon_agent_session_store::{StoreError, StoreErrorCode};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct ToolChain {
    seen: BTreeSet<String>,
    pending: BTreeSet<String>,
}
impl ToolChain {
    pub fn append(&mut self, message: &StoredMessage) -> Result<()> {
        let broken = || StoreError::invalid_request(StoreErrorCode::BrokenToolChain, "invalid tool identity order");
        for block in &message.content {
            match block {
                ContentBlock::ToolUse { id, .. } => {
                    if id.is_empty() || !self.seen.insert(id.clone()) {
                        return Err(broken().into());
                    }
                    self.pending.insert(id.clone());
                }
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    if !self.pending.remove(tool_use_id) {
                        return Err(broken().into());
                    }
                }
                ContentBlock::ToolReference { tool_use_id, .. } => {
                    if !self.seen.contains(tool_use_id) {
                        return Err(broken().into());
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}
