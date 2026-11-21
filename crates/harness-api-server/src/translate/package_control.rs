//! Package management: one request the daemon owns, one reply it publishes.
//!
//! The daemon behind this bridge is the only writer of the package catalog, so
//! the bridge forwards the request instead of answering it. An older daemon
//! cannot serve it and would answer an unknown frame with an uncorrelated
//! error, so the request is refused here when the daemon's capability probe did
//! not report `package_control`.
//!
//! Neither direction stores session state: the catalog belongs to the daemon,
//! and the receipt carries the generation it published.

use horon_agent_harness_api::{
    ApiEvent, CAPABILITY_PACKAGE_CONTROL_V1, ErrorCode, PackageControlReceipt, PackageControlRequest, ServerFrame,
    unsupported_reply,
};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{BridgeState, Outbound, SimpleKind};

/// Whether the daemon behind this bridge answered its probe with
/// `package_control`. One bridge process serves one daemon, so this is a
/// property of the process rather than of a connection.
static SUPPORTED: AtomicBool = AtomicBool::new(false);

/// Record the daemon's answer to the capability probe.
pub(crate) fn set_supported(supported: bool) {
    SUPPORTED.store(supported, Ordering::Release);
}

fn supported() -> bool {
    SUPPORTED.load(Ordering::Acquire)
}

/// Translate `package_control`, or `None` for any other request.
pub(super) fn request(state: &mut BridgeState, request: &Value) -> Option<Vec<Outbound>> {
    if request["req"].as_str() != Some("package_control") {
        return None;
    }
    let api_id = request["id"].as_u64().unwrap_or(0);
    if !supported() {
        return Some(vec![Outbound::Reply(unsupported_reply(
            api_id,
            CAPABILITY_PACKAGE_CONTROL_V1,
            "package_control",
        ))]);
    }
    let typed = match serde_json::from_value::<PackageControlRequest>(request.clone()) {
        Ok(typed) => typed,
        Err(error) => {
            return Some(state.error_reply(
                api_id,
                ErrorCode::InvalidRequest,
                &format!("invalid package_control request: {error}"),
            ));
        }
    };
    let operation = match serde_json::to_value(&typed.operation) {
        Ok(operation) => operation,
        Err(error) => {
            return Some(state.error_reply(
                api_id,
                ErrorCode::Internal,
                &format!("could not encode the package operation: {error}"),
            ));
        }
    };
    let legacy_id = state.legacy_id();
    state
        .pending_simple
        .push((legacy_id, api_id, SimpleKind::PackageControl));
    Some(vec![Outbound::Legacy(serde_json::json!({
        "type": "package_control",
        "id": legacy_id,
        "operation_id": typed.operation_id,
        "operation": operation,
    }))])
}

/// Translate a `package_control_result`, or `None` for any other event.
pub(super) fn event(state: &mut BridgeState, event: &Value) -> Option<Vec<ServerFrame>> {
    if event["type"].as_str() != Some("package_control_result") {
        return None;
    }
    let legacy_id = event["id"].as_u64().unwrap_or(0);
    let Some(api_id) = state.take_simple(legacy_id, SimpleKind::PackageControl) else {
        // A reply nobody is waiting for: the connection that asked left. Do not
        // let it fall through to the generic arms, which do not know this type.
        return Some(Vec::new());
    };
    let receipt = match serde_json::from_value::<PackageControlReceipt>(event["receipt"].clone()) {
        Ok(receipt) => receipt,
        Err(error) => {
            return Some(vec![ServerFrame::reply(
                api_id,
                ApiEvent::Error {
                    code: ErrorCode::Internal,
                    message: format!("invalid package_control receipt: {error}"),
                    details: None,
                },
            )]);
        }
    };
    Some(vec![ServerFrame::reply(api_id, ApiEvent::PackageControl { receipt })])
}
