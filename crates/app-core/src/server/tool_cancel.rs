//! Daemon handlers for the per-call cancellations of an ACP v2 editor.
//!
//! A `cancel_tool_call` names one tool call and stops only that call: the turn
//! keeps running and the cancelled call reports an ordinary `tool_done` whose
//! text the model sees, so the transcript stays valid and the model can adapt.
//! That is why this lives beside the turn instead of going through the
//! turn-level cancel in `client_lifecycle`: nothing here touches the turn's
//! stop signal, the agent lock, or the processing task.
//!
//! A `cancel_turn` names the run the caller saw. It reuses the turn-level
//! cancel path, but only when the named run is still the one running, so a
//! cancel that lost the race with a finished turn cannot stop its successor.
//!
//! The background-work requests live in [`super::background_work`]: they share
//! the background manager's lifetime broadcast, not this module's per-call
//! signal registry.

use super::*;

/// Apply a `cancel_tool_call` and answer the caller.
///
/// `accepted: true` means the registered call was signalled, and its own
/// `tool_done` (with the cancellation text) follows on the connection that owns
/// the turn. Anything else answers `accepted: false, reason: "unknown"`: no
/// call by that id is running in this session, which includes a call that
/// already finished, one that was moved to a background task, and one nobody
/// ever started.
pub(super) fn handle_cancel_tool_call(
    id: u64,
    call_id: &str,
    session_id: &str,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let accepted = crate::tool_cancel_registry::cancel(session_id, call_id);
    crate::logging::info(&format!(
        "SERVER_TOOL_CALL_CANCEL_REQUEST id={} session={} call_id={} accepted={}",
        id, session_id, call_id, accepted
    ));
    reply(
        client_event_tx,
        ServerEvent::ToolCallCancelResult {
            id,
            accepted,
            reason: (!accepted).then(|| "unknown".to_string()),
        },
        "tool_call_cancel",
    );
}

/// Queue one reply, reporting a connection that closed before it was read.
fn reply(client_event_tx: &mpsc::UnboundedSender<ServerEvent>, event: ServerEvent, request: &'static str) {
    if client_event_tx.send(event).is_err() {
        crate::logging::warn(&format!("SERVER_REQUEST_REPLY_DROPPED request={request}"));
    }
}
