//! The four cancellable-work requests and their three replies, end to end
//! through the bridge.

use super::*;
use crate::translate::tests::{legacy_out, only_reply_event};

fn cancellable_state() -> BridgeState {
    BridgeState {
        session_id: Some("s1".into()),
        cancellable_work_api_supported: true,
        ..Default::default()
    }
}

/// Each of the four cancellable-work requests forwards as the daemon frame it
/// names, and its reply comes back correlated to the API request.
#[test]
fn cancellable_work_requests_forward_and_their_replies_correlate() {
    let mut state = cancellable_state();
    let out = state.api_request_to_legacy(&json!({
        "req": "cancel_turn", "id": 10, "session_id": "s1", "run_id": "run-7"
    }));
    let legacy = legacy_out(&out);
    assert_eq!(legacy["type"], "cancel_turn");
    assert_eq!(legacy["run_id"], "run-7");
    let legacy_id = legacy["id"].as_u64().unwrap();
    let reply = state.legacy_event_to_api(&json!({
        "type": "cancel_turn_result", "id": legacy_id, "run_id": "run-7", "accepted": true
    }));
    assert_eq!(reply.len(), 1);
    assert_eq!(reply[0].reply_to, Some(10));
    assert!(matches!(
        &reply[0].event,
        ApiEvent::CancelTurnResult { run_id, accepted: true, reason: None, .. } if run_id == "run-7"
    ));

    let out = state.api_request_to_legacy(&json!({
        "req": "cancel_tool_call", "id": 11, "session_id": "s1", "call_id": "call-3"
    }));
    let legacy = legacy_out(&out);
    assert_eq!(legacy["type"], "cancel_tool_call");
    assert_eq!(legacy["call_id"], "call-3");
    let legacy_id = legacy["id"].as_u64().unwrap();
    let reply = state.legacy_event_to_api(&json!({
        "type": "tool_call_cancel_result", "id": legacy_id, "call_id": "call-3",
        "accepted": false, "reason": "unknown"
    }));
    assert!(matches!(
        &reply[0].event,
        ApiEvent::ToolCallCancelResult { call_id, accepted: false, reason: Some(reason), .. }
            if call_id == "call-3" && reason == "unknown"
    ));

    let out = state.api_request_to_legacy(&json!({
        "req": "cancel_background_terminal", "id": 12, "session_id": "s1", "call_id": "call-4"
    }));
    let legacy = legacy_out(&out);
    assert_eq!(legacy["type"], "cancel_background_terminal");
    let legacy_id = legacy["id"].as_u64().unwrap();
    let reply = state.legacy_event_to_api(&json!({
        "type": "tool_call_cancel_result", "id": legacy_id, "call_id": "call-4", "accepted": true
    }));
    assert_eq!(reply[0].reply_to, Some(12));

    let out = state.api_request_to_legacy(&json!({
        "req": "list_background_work", "id": 13, "session_id": "s1"
    }));
    let legacy = legacy_out(&out);
    assert_eq!(legacy["type"], "list_background_work");
    assert!(legacy.get("call_id").is_none());
    let legacy_id = legacy["id"].as_u64().unwrap();
    let reply = state.legacy_event_to_api(&json!({
        "type": "background_work_list",
        "id": legacy_id,
        "items": [{
            "call_id": "call-5",
            "task_id": "bg_9",
            "kind": "terminal",
            "command": "sleep 30",
            "cwd": "/tmp",
            "output": "aGk="
        }]
    }));
    assert_eq!(reply[0].reply_to, Some(13));
    match &reply[0].event {
        ApiEvent::BackgroundWorkList { items, .. } => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].call_id, "call-5");
            assert_eq!(items[0].kind, "terminal");
            assert_eq!(items[0].command.as_deref(), Some("sleep 30"));
            assert_eq!(items[0].output.as_deref(), Some("aGk="));
        }
        other => panic!("expected background work list, got {other:?}"),
    }
}

/// A daemon that never advertised the capability refuses all four requests with
/// a structured, named error instead of forwarding into silence.
#[test]
fn cancellable_work_requests_are_refused_by_a_daemon_without_the_capability() {
    for request in [
        json!({"req": "cancel_turn", "id": 20, "session_id": "s1", "run_id": "run-7"}),
        json!({"req": "cancel_tool_call", "id": 21, "session_id": "s1", "call_id": "call-3"}),
        json!({"req": "cancel_background_terminal", "id": 22, "session_id": "s1", "call_id": "call-3"}),
        json!({"req": "list_background_work", "id": 23, "session_id": "s1"}),
    ] {
        // No capability: the bridge must refuse rather than forward a frame the
        // daemon would drop silently.
        let mut state = BridgeState {
            session_id: Some("s1".into()),
            ..Default::default()
        };
        let out = state.api_request_to_legacy(&request);
        let event = only_reply_event(out);
        match event {
            ApiEvent::Error { code, message, .. } => {
                assert_eq!(code, ErrorCode::Unsupported, "{request}");
                assert!(
                    message.contains("cancellable_work_v1"),
                    "the error names the capability the caller needs: {message}"
                );
                assert!(
                    message.contains(request["req"].as_str().unwrap()),
                    "the error names the surface it refused: {message}"
                );
            }
            other => panic!("{request} must be refused, got {other:?}"),
        }
    }
}

/// A malformed request is refused before the capability check, so a client with
/// a typo learns what is wrong rather than being told to upgrade.
#[test]
fn cancellable_work_requests_validate_their_arguments() {
    let mut state = cancellable_state();
    for request in [
        json!({"req": "cancel_turn", "id": 30, "session_id": "s1", "run_id": ""}),
        json!({"req": "cancel_turn", "id": 31, "session_id": "s1"}),
        json!({"req": "cancel_tool_call", "id": 32, "session_id": "s1", "call_id": ""}),
        json!({"req": "cancel_background_terminal", "id": 33, "session_id": "s1"}),
    ] {
        let event = only_reply_event(state.api_request_to_legacy(&request));
        assert!(
            matches!(
                &event,
                ApiEvent::Error {
                    code: ErrorCode::InvalidRequest,
                    ..
                }
            ),
            "{request} must be refused as invalid, got {event:?}"
        );
    }
}

/// A daemon that predates the echoed subject answers a cancel without one.
/// The bridge cannot tell which request that frame belongs to, so it answers
/// nothing rather than answering the wrong one: the client sees its own
/// timeout, which is the failure it can reason about.
#[test]
fn a_cancel_reply_without_its_subject_answers_no_request() {
    for (request, reply) in [
        (
            json!({"req": "cancel_turn", "id": 40, "session_id": "s1", "run_id": "run-7"}),
            json!({"type": "cancel_turn_result", "accepted": true}),
        ),
        (
            json!({"req": "cancel_tool_call", "id": 41, "session_id": "s1", "call_id": "call-7"}),
            json!({"type": "tool_call_cancel_result", "accepted": true}),
        ),
    ] {
        let mut state = cancellable_state();
        let out = state.api_request_to_legacy(&request);
        let legacy_id = legacy_out(&out)["id"].as_u64().unwrap();
        let mut older = reply;
        // The id still matches, so the only missing part is the subject.
        older["id"] = Value::from(legacy_id);
        let frames = state.legacy_event_to_api(&older);
        assert!(
            frames.is_empty(),
            "{request}: a reply with no subject must answer no request, got {frames:?}"
        );
    }
}
