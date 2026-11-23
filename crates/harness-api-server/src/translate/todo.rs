//! The todo requests and replies the API serves.
//!
//! Reads come from the stored snapshot; a status edit is forwarded to the
//! daemon, which owns the write, the shared lock and the `TodoChanged` push.
//! The reply carries the state and revision from one read so a client never
//! pairs a list with a revision of a different write.

use super::simple_kind::SimpleKind;
use super::{BridgeState, Outbound};
use horon_agent_harness_api::{ApiEvent, ErrorCode, ServerFrame};
use serde_json::{Value, json};

/// Resolve the home the session's todo files live under.
pub(super) fn home(session_id: &str) -> Result<std::path::PathBuf, (ErrorCode, String)> {
    if !horon_agent_todo_policy::store::is_safe_session_id(session_id) {
        return Err((ErrorCode::InvalidRequest, "invalid session_id".to_string()));
    }
    BridgeState::horon_agent_home().ok_or_else(|| (ErrorCode::Internal, "no horon-agent home".to_string()))
}

/// Answer `get_todo_state` from the stored snapshot.
pub(super) fn get(api_id: u64, request: &Value) -> Vec<Outbound> {
    let session_id = request["session_id"].as_str().unwrap_or("");
    match home(session_id) {
        Ok(home) => {
            let (state, revision) = horon_agent_todo_policy::store::load_snapshot_with_revision(&home, session_id);
            vec![Outbound::Reply(ServerFrame::reply(
                api_id,
                ApiEvent::TodoState {
                    session_id: session_id.to_string(),
                    state,
                    revision,
                },
            ))]
        }
        Err((code, message)) => BridgeState::error_reply(api_id, code, &message),
    }
}

/// Forward `set_todo_status` to the daemon.
///
/// The daemon serializes the write with the todo tool on the same lock and
/// publishes the `TodoUpdated` event every attached client reads as
/// `TodoChanged`. A direct file write here would bypass both.
pub(super) fn set(state: &mut BridgeState, api_id: u64, request: &Value) -> Vec<Outbound> {
    let expected_revision = request["expected_revision"].as_str().unwrap_or("");
    let todo_id = request["todo_id"].as_str().unwrap_or("");
    let status = request["status"].as_str().unwrap_or("");
    if expected_revision.trim().is_empty() || todo_id.trim().is_empty() || status.trim().is_empty() {
        return BridgeState::error_reply(
            api_id,
            ErrorCode::InvalidRequest,
            "set_todo_status needs a non-empty expected_revision, id and status",
        );
    }
    let id = state.legacy_id();
    state.pending_simple.push((id, api_id, SimpleKind::TodoEdit));
    vec![Outbound::Legacy(json!({
        "type": "set_todo_status",
        "id": id,
        "expected_revision": expected_revision,
        "todo_id": todo_id,
        "status": status,
    }))]
}

/// Apply the persistent effects of one follow-up decision.
pub(super) fn ack(api_id: u64, request: &Value) -> Vec<Outbound> {
    let session_id = request["session_id"].as_str().unwrap_or("");
    let effects: horon_agent_todo_policy::TodoEffects = match serde_json::from_value(request["effects"].clone()) {
        Ok(effects) => effects,
        Err(error) => {
            return BridgeState::error_reply(api_id, ErrorCode::InvalidRequest, &format!("invalid effects: {error}"));
        }
    };
    let result = home(session_id).and_then(|home| {
        horon_agent_todo_policy::store::apply_effects(&home, session_id, effects)
            .map_err(|error| (ErrorCode::Internal, error.to_string()))
    });
    match result {
        Ok(()) => vec![Outbound::Reply(ServerFrame::reply(api_id, ApiEvent::Ok))],
        Err((code, message)) => BridgeState::error_reply(api_id, code, &message),
    }
}

/// Turn the daemon's `todo_edit_result` into the API reply.
///
/// The state and revision are read back as one pair; the daemon's own revision
/// is not trusted as a name for bytes this process has not read.
pub(super) fn edit_result(state: &mut BridgeState, event: &Value, attached: &str) -> Vec<ServerFrame> {
    let id = event["id"].as_u64().unwrap_or(0);
    let Some(api_id) = state.take_simple(id, SimpleKind::TodoEdit) else {
        return vec![];
    };
    let refusal = event["refusal"]
        .as_str()
        .and_then(horon_agent_todo_policy::TodoEditRefusal::from_wire_name);
    match home(attached) {
        Ok(home) => {
            let (state, read_revision) = horon_agent_todo_policy::store::load_snapshot_with_revision(&home, attached);
            // If this read cannot name the list, the reply carries no revision:
            // the client refreshes and stays read-only instead of editing a
            // store this process could not read.
            let revision = read_revision.unwrap_or_else(|| "".into());
            vec![ServerFrame::reply(
                api_id,
                ApiEvent::TodoEditResult {
                    session_id: attached.to_string(),
                    applied: event["applied"].as_bool().unwrap_or(false),
                    refusal,
                    state,
                    revision,
                },
            )]
        }
        Err((code, message)) => vec![ServerFrame::reply(
            api_id,
            ApiEvent::Error {
                code,
                message,
                details: None,
            },
        )],
    }
}
