//! `fork_session`, `fork_session_at` and `transfer_session`: the bridge's three
//! session-cloning requests.
//!
//! The daemon owns the transcripts, so each request is forwarded as the frame
//! it names and the daemon's `split_response` answers. A fork from a recorded
//! boundary additionally needs the daemon capability probe: an older daemon
//! answers the unknown frame with an uncorrelated error, so the bridge refuses
//! it here instead of leaving the client to time out.

use super::{BridgeState, ErrorCode, Outbound};
use serde_json::{Value, json};

/// Forward one clone request, or refuse it before writing a frame.
pub(super) fn request(state: &mut BridgeState, request: &Value, api_id: u64, req: &str) -> Vec<Outbound> {
    if req == "fork_session_at" {
        if !state.session_fork_at_supported {
            let capability = horon_agent_harness_api::CAPABILITY_SESSION_FORK_AT_V1;
            return vec![Outbound::Reply(horon_agent_harness_api::unsupported_reply(
                api_id, capability, req,
            ))];
        }
        let Some(boundary) = request["boundary_message_id"]
            .as_str()
            .map(str::trim)
            .filter(|boundary| !boundary.is_empty())
        else {
            return BridgeState::error_reply(
                api_id,
                ErrorCode::InvalidRequest,
                "boundary_message_id must not be empty",
            );
        };
        let id = state.legacy_id();
        state.pending_fork_id = Some((id, api_id));
        return vec![Outbound::Legacy(json!({
            "type": "split_at",
            "id": id,
            "boundary_message_id": boundary,
        }))];
    }
    let id = state.legacy_id();
    state.pending_fork_id = Some((id, api_id));
    let kind = if req == "fork_session" { "split" } else { "transfer" };
    vec![Outbound::Legacy(json!({"type": kind, "id": id}))]
}
