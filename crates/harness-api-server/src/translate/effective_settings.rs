//! `get_effective_settings`: the bridge forwards it to the daemon.
//!
//! The rows describe the configuration the daemon loaded, so only the daemon
//! can answer: the bridge's own environment and its own read of the config file
//! need not match the session it serves. This module holds the forward, the
//! reply translation, and the one piece of state a forward needs — whether the
//! daemon behind this bridge advertises the request.

use std::sync::atomic::{AtomicBool, Ordering};

use super::{BridgeState, ErrorCode, Outbound, SimpleKind};
use horon_agent_harness_api::{ApiEvent, CAPABILITY_EFFECTIVE_SETTINGS_V1, EffectiveSetting, ServerFrame};
use serde_json::{Value, json};

/// Whether the daemon behind this bridge advertises `effective_settings_api`.
///
/// A bridge process dials one legacy socket, so the daemon, and therefore this
/// capability, is a property of the process rather than of one connection.
/// Recording it here rather than in [`BridgeState`] keeps the per-connection
/// state free of a flag every construction site would have to name.
static SUPPORTED: AtomicBool = AtomicBool::new(false);

/// Record what the daemon's ping advertised for this bridge.
pub(crate) fn set_supported(supported: bool) {
    SUPPORTED.store(supported, Ordering::Relaxed);
}

/// Translate `get_effective_settings` into the daemon's request, or refuse it.
///
/// A daemon that does not advertise the request is refused with the structured
/// `unsupported` reply naming the API capability rather than forwarded: an
/// older daemon answers an unknown request with a frame carrying no id, so a
/// forwarded request would park the client until its own timeout.
pub(super) fn request(state: &mut BridgeState, request: &Value, api_id: u64, req: &str) -> Vec<Outbound> {
    request_with(state, request, api_id, req, SUPPORTED.load(Ordering::Relaxed))
}

/// [`request`] with the daemon's support stated outright, so a test can prove
/// both directions without a process-wide flag it would have to restore.
fn request_with(state: &mut BridgeState, request: &Value, api_id: u64, req: &str, supported: bool) -> Vec<Outbound> {
    if !supported {
        return vec![Outbound::Reply(horon_agent_harness_api::unsupported_reply(
            api_id,
            CAPABILITY_EFFECTIVE_SETTINGS_V1,
            req,
        ))];
    }
    // The API names the session the question is about; the daemon answers for
    // itself, so the id stays here rather than becoming a legacy field the
    // daemon would ignore.
    if BridgeState::request_session(request, api_id).is_none() {
        return BridgeState::error_reply(api_id, ErrorCode::InvalidRequest, "session_id must not be empty");
    }
    let id = state.legacy_id();
    state.pending_simple.push((id, api_id, SimpleKind::EffectiveSettings));
    vec![Outbound::Legacy(json!({
        "type": "get_effective_settings",
        "id": id,
    }))]
}

/// Turn the daemon's reply into the API's answer for the request it belongs to.
///
/// The daemon serializes the rows itself, so a row it adds later reaches an
/// older bridge as data rather than as a parse failure. A reply this bridge
/// cannot read is reported as an error instead of being passed on
/// half-decoded, and a reply no in-flight request names is dropped.
pub(super) fn reply(state: &mut BridgeState, event: &Value) -> Vec<ServerFrame> {
    let id = event["id"].as_u64().unwrap_or(0);
    let Some(api_id) = state.take_simple(id, SimpleKind::EffectiveSettings) else {
        return vec![];
    };
    let Some(session_id) = state.session_id.clone() else {
        return vec![ServerFrame::reply(
            api_id,
            ApiEvent::Error {
                code: ErrorCode::Internal,
                message: "the settings reply has no attached session".into(),
                details: None,
            },
        )];
    };
    match <Vec<EffectiveSetting> as serde::Deserialize>::deserialize(&event["settings"]) {
        Ok(settings) => vec![ServerFrame::reply(
            api_id,
            ApiEvent::EffectiveSettings { session_id, settings },
        )],
        Err(error) => vec![ServerFrame::reply(
            api_id,
            ApiEvent::Error {
                code: ErrorCode::Internal,
                message: format!("the daemon's effective settings could not be read: {error}"),
                details: None,
            },
        )],
    }
}

#[cfg(test)]
#[path = "effective_settings_tests.rs"]
mod tests;
