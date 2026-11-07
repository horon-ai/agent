//! Removed screen-panel surfaces must fail soft for old peers.
//!
//! The daemon no longer defines the panel and applet state events or requests.
//! Old session logs and old harness clients are still allowed to carry them:
//! stored event data must load, the harness must decode unknown frames, and the
//! bridge must answer a removed request with an error frame instead of a panic.

use horon_agent::protocol::ServerEvent;
use horon_agent_harness_api::{ApiEvent, ApiRequest, ServerFrame};
use horon_agent_harness_api_server::translate::{BridgeState, Outbound};
use serde_json::json;

#[test]
fn old_history_payload_with_removed_panel_fields_still_loads() {
    let old = json!({
        "type": "history",
        "id": 7,
        "session_id": "s1",
        "messages": [{"role": "user", "content": "hello"}],
        "side_panel": {"focused_page_id": "notes", "pages": []},
        "applets": {"instances": []},
    });
    let decoded: ServerEvent = serde_json::from_value(old).expect("old history must keep loading");
    match decoded {
        ServerEvent::History { messages, .. } => assert_eq!(messages.len(), 1),
        other => panic!("expected History, got {other:?}"),
    }
}

#[test]
fn removed_panel_event_types_are_rejected_without_panicking() {
    for removed in [
        json!({"type": "side_panel_state", "snapshot": {"focused_page_id": null, "pages": []}}),
        json!({"type": "applet_state", "session_id": "s1", "snapshot": {"instances": []}}),
    ] {
        assert!(
            serde_json::from_value::<ServerEvent>(removed.clone()).is_err(),
            "removed event {removed} must not decode as a live event"
        );
    }
}

#[test]
fn old_harness_clients_decode_removed_frames_as_unknown() {
    let event: ServerFrame = serde_json::from_value(json!({
        "v": 1,
        "ev": "side_panel_state",
        "session_id": "s1",
        "snapshot": {"focused_page_id": null, "pages": []},
    }))
    .expect("unknown events stay decodable for newer clients");
    assert!(matches!(event.event, ApiEvent::Unknown));

    let applet_state: ServerFrame = serde_json::from_value(json!({
        "v": 1,
        "ev": "applet_state",
        "session_id": "s1",
        "snapshot": {"instances": []},
    }))
    .expect("unknown events stay decodable for newer clients");
    assert!(matches!(applet_state.event, ApiEvent::Unknown));

    let request = serde_json::from_value::<ApiRequest>(json!({
        "v": 1,
        "id": 3,
        "req": "applet_action",
        "session_id": "s1",
        "instance": "chart-1",
        "action": {"action": "select"},
    }))
    .expect("unknown requests stay decodable so the bridge can answer");
    assert!(matches!(request, ApiRequest::Unknown));
}

#[test]
fn bridge_answers_removed_panel_requests_with_an_error_frame() {
    for request in [
        json!({"v": 1, "id": 4, "req": "applet_action", "session_id": "s1", "instance": "chart-1"}),
        json!({"v": 1, "id": 5, "req": "close_applet", "session_id": "s1", "instance": "chart-1"}),
    ] {
        let out = BridgeState::default().api_request_to_legacy(&request);
        let [Outbound::Reply(frame)] = out.as_slice() else {
            panic!("removed request {request} must be answered locally, got {out:?}");
        };
        let ApiEvent::Error { code, message, .. } = &frame.event else {
            panic!("removed request {request} must answer an error frame, got {frame:?}");
        };
        assert_eq!(*code, horon_agent_harness_api::ErrorCode::UnknownRequest);
        assert!(message.contains("unknown request"), "{message}");
    }
}
