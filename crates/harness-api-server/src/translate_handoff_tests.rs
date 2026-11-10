//! Handoff compaction through the bridge.
//!
//! The request keeps the `compact` shape with an optional `method`, and the
//! reply carries what happened instead of the legacy "scheduled" message, so a
//! caller learns whether the boundary was written.

use super::*;
use horon_agent_harness_api::HandoffCompaction;
use serde_json::{Value, json};

fn state_with_session() -> BridgeState {
    let mut state = BridgeState::default();
    state.session_id = Some("s1".into());
    state
}

/// Send one `compact` request and return the legacy id the daemon sees.
fn compact_request(state: &mut BridgeState, method: Option<&str>) -> (u64, Value) {
    let mut request = json!({"id": 6, "req": "compact", "session_id": "s1"});
    if let Some(method) = method {
        request["method"] = json!(method);
    }
    let out = state.api_request_to_legacy(&request);
    match &out[0] {
        Outbound::Legacy(value) => (value["id"].as_u64().unwrap_or(0), value.clone()),
        other => panic!("expected a legacy compact request, got {other:?}"),
    }
}

#[test]
fn compact_handoff_forwards_the_method_to_the_daemon() {
    let mut state = state_with_session();
    let (_, legacy) = compact_request(&mut state, Some("handoff"));
    assert_eq!(legacy["type"], "compact");
    assert_eq!(legacy["method"], "handoff");
}

/// A client that never heard of the handoff method must send the exact frame it
/// always sent, so an old bridge and an old daemon stay interchangeable.
#[test]
fn compact_without_a_method_sends_the_legacy_request() {
    let mut state = state_with_session();
    let (_, legacy) = compact_request(&mut state, None);
    assert_eq!(legacy, json!({"type": "compact", "id": legacy["id"]}));
}

#[test]
fn compact_handoff_result_reports_the_covered_prefix() {
    let mut state = state_with_session();
    let (legacy_id, _) = compact_request(&mut state, Some("handoff"));
    let frames = state.legacy_event_to_api(&json!({
        "type": "compact_result",
        "id": legacy_id,
        "message": "Committed a handoff document covering the first 4 messages.",
        "success": true,
        "handoff": {"outcome": "committed", "covered": 4},
    }));
    assert_eq!(frames.len(), 1);
    match &frames[0].event {
        ApiEvent::Compacted {
            handoff: Some(HandoffCompaction::Committed { covered }),
            ..
        } => assert_eq!(*covered, 4),
        other => panic!("expected a committed handoff reply, got {other:?}"),
    }
}

/// A provider error is the answer to "which outcome happened", not a transport
/// failure: the transcript is untouched and a client that saw only an error
/// could not tell that apart from a boundary that was written.
#[test]
fn compact_handoff_provider_error_is_reported_as_the_outcome() {
    let mut state = state_with_session();
    let (legacy_id, _) = compact_request(&mut state, Some("handoff"));
    let frames = state.legacy_event_to_api(&json!({
        "type": "compact_result",
        "id": legacy_id,
        "message": "Handoff compaction failed: quota exceeded",
        "success": false,
        "handoff": {"outcome": "provider_error", "detail": "quota exceeded"},
    }));
    assert_eq!(frames.len(), 1);
    match &frames[0].event {
        ApiEvent::Compacted {
            handoff: Some(HandoffCompaction::ProviderError { detail }),
            ..
        } => assert_eq!(detail, "quota exceeded"),
        other => panic!("expected a provider-error outcome, got {other:?}"),
    }
}
