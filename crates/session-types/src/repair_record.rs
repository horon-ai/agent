//! Handoff of the arguments a schema-directed repair rewrote.
//!
//! The dispatcher repairs a call's arguments before the tool runs, but the
//! record that ends up in the session is built by the session layer, which only
//! ever sees the repaired value. This module is the join between the two: the
//! dispatcher leaves the record under the session and tool call it belongs to,
//! and the session takes it when it stores that call's result.
//!
//! Nothing here is persisted and nothing here is a cache: only a call whose
//! arguments were actually rewritten ever puts an entry in, and the entry is
//! removed as soon as its result is stored. An entry whose result never gets
//! stored is dropped when the map fills, and a session dropped without storing
//! its calls is cleared outright by [`forget_session`], so an abandoned call
//! cannot grow the map without bound.

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

static PENDING: LazyLock<Mutex<HashMap<(String, String), ArgRepairRecord>>> = LazyLock::new(Mutex::default);

/// Leave `record` for the call `tool_call_id` of `session_id`, replacing an
/// earlier one so a retried call cannot stack duplicates.
///
/// The session is part of the key because one process serves many sessions and
/// a provider is free to reuse a call id such as `call_0` across them: without
/// it, one session's append could claim another session's record.
pub fn remember(session_id: &str, tool_call_id: &str, record: ArgRepairRecord) {
    let mut pending = PENDING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let key = (session_id.to_string(), tool_call_id.to_string());
    if pending.len() >= PENDING_CAP && !pending.contains_key(&key) {
        pending.clear();
    }
    pending.insert(key, record);
}

/// Take the record for `tool_call_id` of `session_id`, if that call was
/// repaired.
pub fn take(session_id: &str, tool_call_id: &str) -> Option<ArgRepairRecord> {
    PENDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&(session_id.to_string(), tool_call_id.to_string()))
}

/// A copy of the record for `tool_call_id` of `session_id`, for a reader that
/// runs before the result is stored and must leave it for the session to take.
pub fn peek(session_id: &str, tool_call_id: &str) -> Option<ArgRepairRecord> {
    PENDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&(session_id.to_string(), tool_call_id.to_string()))
        .cloned()
}

/// Take the record for the first tool result among `blocks` that belongs to
/// this session, if that call was repaired.
///
/// A stored tool result may bundle several results; only a repaired call has a
/// record, so the first block that has one answers for the message.
pub fn take_in(session_id: &str, blocks: &[ContentBlock]) -> Option<ArgRepairRecord> {
    blocks.iter().find_map(|block| match block {
        ContentBlock::ToolResult { tool_use_id, .. } => take(session_id, tool_use_id),
        _ => None,
    })
}

/// Drop every record left for `session_id`, for a session that ended or was
/// torn down before its calls were stored.
pub fn forget_session(session_id: &str) {
    let mut pending = PENDING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    pending.retain(|(session, _), _| session != session_id);
}
