//! The Alt+B handler: hand the tool call this session is awaiting to the
//! background manager.
//!
//! Split out of `client_lifecycle` to keep that file flat. The handler reads
//! `running_tool_registry` (one entry per session, because the Alt+B signal
//! carries no target identity) and is reachable from the agent-side tests that
//! hold a concurrent group in flight.

use super::*;

pub(crate) fn move_tool_to_background(
    id: u64,
    session_control: &SessionControlHandle,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    crate::logging::info(&format!(
        "SERVER_BACKGROUND_TOOL_REQUEST id={} session={}",
        id, session_control.session_id
    ));
    let running = crate::running_tool_registry::current(&session_control.session_id);
    // Fire regardless: the agent resets the signal before every tool wait, so
    // an idle fire is harmless, and this keeps the pre-#1778 behavior.
    let signalled = session_control.request_background_current_tool();
    let ack_queued = client_event_tx.send(ServerEvent::Ack { id }).is_ok();
    let running = running.filter(|_| signalled);
    let moved = running.is_some();
    let (tool_call_id, tool_name) = match running {
        Some(tool) => (Some(tool.call_id), Some(tool.name)),
        None => (None, None),
    };
    if client_event_tx
        .send(ServerEvent::BackgroundToolResult {
            id,
            moved,
            tool_call_id,
            tool_name,
        })
        .is_err()
    {
        crate::logging::warn(&format!(
            "SERVER_BACKGROUND_TOOL_REPLY_DROPPED id={} session={}",
            id, session_control.session_id
        ));
    }
    crate::logging::info(&format!(
        "SERVER_BACKGROUND_TOOL_RESULT id={} session={} signalled={} ack_queued={}",
        id, session_control.session_id, signalled, ack_queued
    ));
}
