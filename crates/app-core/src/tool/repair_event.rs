//! Emitting the repair event an API client sees.
//!
//! The dispatcher knows a call was repaired but has no event sender, and the
//! turn loop has the sender but only the call id. The record left under that id
//! bridges them, so this stays a lookup rather than a parameter threaded through
//! every dispatch path.

use horon_agent_protocol::ServerEvent;
use horon_agent_session_types::repair_record::peek;

/// Send `tool_args_repaired` for `call_id` if its arguments were repaired.
///
/// Called next to the append of the call's result, so it must not consume the
/// record: the session takes it a moment later.
pub(crate) fn send(event_tx: &tokio::sync::mpsc::UnboundedSender<ServerEvent>, call_id: &str, name: &str) {
    let Some(record) = peek(call_id) else {
        return;
    };
    let _ = event_tx.send(ServerEvent::ToolArgsRepaired {
        id: call_id.to_string(),
        name: name.to_string(),
        original_input: record.original_input,
        changes: record.changes,
    });
}
