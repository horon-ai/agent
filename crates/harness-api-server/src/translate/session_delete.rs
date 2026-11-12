//! `delete_session`: forward a permanent delete to the daemon.
//!
//! The bridge answers nothing itself. The daemon acks the request before it
//! touches the store, so only its `done` proves the transcript is gone; see
//! `SimpleKind::Delete`.

use super::{BridgeState, ErrorCode, Outbound, SimpleKind};
use serde_json::json;

/// Refuse early, or hand the delete to the daemon.
pub(crate) fn handle(state: &mut BridgeState, api_id: u64, request: &serde_json::Value) -> Vec<Outbound> {
    let Some(session_id) = BridgeState::request_session(request, api_id) else {
        return BridgeState::error_reply(api_id, ErrorCode::InvalidRequest, "session_id must not be empty");
    };
    // The daemon validates the id again; this check only keeps a delete from
    // being forwarded for a session that is already gone, which the client
    // should hear as unknown rather than as a successful round trip.
    let Some(path) = BridgeState::session_original_path(&session_id) else {
        return BridgeState::error_reply(api_id, ErrorCode::UnknownSession, "session does not exist");
    };
    if !path.is_file() {
        return BridgeState::error_reply(api_id, ErrorCode::UnknownSession, "session does not exist");
    }
    // A session this connection is attached to cannot be deleted: the daemon
    // owns that writer and would refuse, but saying so here leaves this
    // connection's attachment untouched instead of racing a round trip.
    if state.session_id.as_deref() == Some(session_id.as_str()) {
        return BridgeState::error_reply(
            api_id,
            ErrorCode::InvalidRequest,
            "session is attached to this connection; detach before deleting it",
        );
    }
    let id = state.legacy_id();
    // `confirm` is always true: an API client sends the request only after its
    // own confirmation, and the field exists to stop an archive-shaped call
    // from erasing a transcript.
    state.pending_simple.push((id, api_id, SimpleKind::Delete));
    vec![Outbound::Legacy(json!({
        "type": "delete_session",
        "id": id,
        "session_id": session_id,
        "confirm": true,
    }))]
}
