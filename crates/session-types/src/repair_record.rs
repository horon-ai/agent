//! Handoff of the arguments a schema-directed repair rewrote.
//!
//! The dispatcher repairs a call's arguments before the tool runs, but the
//! record that ends up in the session is built by the session layer, which only
//! ever sees the repaired value. This module is the join between the two: the
//! dispatcher leaves the record under the tool call id, and the session takes it
//! when it stores that call's result.
//!
//! Nothing here is persisted and nothing here is a cache: only a call whose
//! arguments were actually rewritten ever puts an entry in, and the entry is
//! removed as soon as its result is stored. An entry whose result never gets
//! stored is dropped when the map fills, so an abandoned call cannot grow it
//! without bound.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use horon_agent_message_types::ContentBlock;

/// What a repair changed for one call.
#[derive(Debug, Clone, PartialEq)]
pub struct ArgRepairRecord {
    /// The arguments exactly as the model sent them.
    pub original_input: serde_json::Value,
    /// One note per repair applied, in the order they were applied.
    pub changes: Vec<String>,
}

/// Entries kept at once. A repaired call's record lives here only for the time
/// between dispatch and the append of its result, so this bound is reached only
/// if many calls are repaired and never stored.
const PENDING_CAP: usize = 1_024;

static PENDING: LazyLock<Mutex<HashMap<String, ArgRepairRecord>>> = LazyLock::new(Mutex::default);

/// Leave `record` for the call `tool_call_id`, replacing an earlier one so a
/// retried call cannot stack duplicates.
pub fn remember(tool_call_id: &str, record: ArgRepairRecord) {
    let mut pending = PENDING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if pending.len() >= PENDING_CAP && !pending.contains_key(tool_call_id) {
        pending.clear();
    }
    pending.insert(tool_call_id.to_string(), record);
}

/// Take the record for `tool_call_id`, if this call was repaired.
pub fn take(tool_call_id: &str) -> Option<ArgRepairRecord> {
    PENDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(tool_call_id)
}

/// Take the record for the first tool result among `blocks`, if that call was
/// repaired.
///
/// A stored tool result may bundle several results; only a repaired call has a
/// record, so the first block that has one answers for the message.
pub fn take_in(blocks: &[ContentBlock]) -> Option<ArgRepairRecord> {
    blocks.iter().find_map(|block| match block {
        ContentBlock::ToolResult { tool_use_id, .. } => take(tool_use_id),
        _ => None,
    })
}
