//! Translation of the handoff compaction extension to `compact`.
//!
//! The legacy daemon request keeps its shape: `compact` with an optional
//! `method`, answered by one `compact_result` frame. A handoff result is a
//! result, not a scheduled acknowledgement, so it is relayed as a
//! `compacted_ev` with the outcome attached even when the daemon says
//! `success: false` — the client asked which of the four outcomes happened, and
//! an error frame cannot answer that.

use super::*;
use serde_json::{Value, json};

/// Build the legacy `compact` request, forwarding `method` only when the client
/// named one so an older daemon still sees the frame it always saw.
pub(super) fn request(id: u64, request: &Value) -> Value {
    let mut compact = json!({"type": "compact", "id": id});
    if let Some(method) = request["method"].as_str() {
        compact["method"] = json!(method);
    }
    compact
}

/// Translate one `compact_result` frame into the API reply.
///
/// Compaction is scheduled, not performed inline, and the daemon reports a
/// refusal via `success: false` with a reason rather than an error frame. That
/// is relayed as a real error so a client is not told "done" about work that
/// never happened. A handoff request is the exception: its frame carries the
/// outcome, and `success: false` there means the boundary was not written,
/// which the outcome already says.
pub(super) fn result_reply(state: &mut BridgeState, event: &Value) -> Vec<ServerFrame> {
    let Some(api_id) = state.take_simple(event["id"].as_u64().unwrap_or(0), SimpleKind::Compact) else {
        return vec![];
    };
    let message = event["message"].as_str().unwrap_or("").to_string();
    let handoff = parse_outcome(&event["handoff"]);
    if handoff.is_none() && event["success"].as_bool() == Some(false) {
        return vec![ServerFrame::reply(
            api_id,
            ApiEvent::Error {
                code: ErrorCode::InvalidRequest,
                message,
                details: None,
            },
        )];
    }
    vec![ServerFrame::reply(
        api_id,
        ApiEvent::Compacted {
            session_id: state.session_id.clone().unwrap_or_else(String::new),
            message,
            handoff,
        },
    )]
}

/// Read the daemon's outcome into the API type. An unreadable value stays
/// `None`, which leaves the client on the plain `compacted` reply rather than
/// inventing an outcome the daemon did not report.
fn parse_outcome(value: &Value) -> Option<horon_agent_harness_api::HandoffCompaction> {
    if value.is_null() {
        return None;
    }
    // A newer daemon can report an outcome this build does not know. The client
    // still gets the plain reply rather than an invented outcome.
    serde_json::from_value(value.clone()).map_or(None, Some)
}
