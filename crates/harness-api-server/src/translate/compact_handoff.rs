//! Translation of the handoff compaction extension to `compact`.
//!
//! The legacy daemon request keeps its shape: `compact` with an optional
//! `method`, answered by one `compact_result` frame. A handoff result is a
//! result, not a scheduled acknowledgement, so it is relayed as a
//! `compacted_ev` with the outcome attached even when the daemon says
//! `success: false` — the client asked which of the four outcomes happened, and
//! an error frame cannot answer that.

use super::*;
use horon_agent_harness_api::HandoffCompaction;
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
            session_id: state.session_id.as_deref().unwrap_or("").to_string(),
            message,
            handoff,
        },
    )]
}

/// Read the daemon's outcome into the API type.
///
/// The four outcomes are read field by field rather than through a `serde`
/// round trip: the value is either one the daemon wrote from this same enum or
/// one from a newer daemon, and only the four documented shapes count as an
/// outcome. Anything else stays `None`, which leaves the client on the plain
/// `compacted` reply rather than inventing an outcome nobody reported.
fn parse_outcome(value: &Value) -> Option<HandoffCompaction> {
    match value["outcome"].as_str()? {
        // A count wider than this target's `usize` saturates: it is still the
        // committed outcome, only a smaller platform cannot hold the exact count.
        "committed" => Some(HandoffCompaction::Committed {
            covered: usize::try_from(value["covered"].as_u64()?).unwrap_or(usize::MAX),
        }),
        "nothing_to_cover" => Some(HandoffCompaction::NothingToCover),
        "provider_error" => {
            let detail = value["detail"].as_str()?;
            Some(HandoffCompaction::ProviderError {
                detail: detail.to_string(),
            })
        }
        "cancelled" => Some(HandoffCompaction::Cancelled),
        _ => None,
    }
}
