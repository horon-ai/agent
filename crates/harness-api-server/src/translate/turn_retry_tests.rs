//! Answering a `send_message` that continues a failed run.
//!
//! The daemon decides whether the named run may be continued, so the request's
//! reply is that decision and not a receipt: `ok` once a new run exists, an error
//! on the same id when the run may not be continued. An ordinary message keeps
//! the `message_accepted` it has always produced.

use super::*;

/// A continuation is answered `ok` on its own request id only when the daemon
/// has begun the new run, and a refusal comes back as an error on that same id.
/// An ordinary message keeps the `message_accepted` it has always produced.
#[test]
fn a_continuation_is_answered_by_the_daemon_decision() {
    let mut state = BridgeState {
        session_id: Some("s1".into()),
        ..Default::default()
    };
    let out = state.api_request_to_legacy(&json!({
        "req": "send_message", "id": 7, "session_id": "s1",
        "content": "", "system_reminder": "continue", "expected_run_id": "run-a"
    }));
    let Outbound::Legacy(message) = &out[0] else {
        panic!("expected legacy outbound");
    };
    assert_eq!(message["expected_run_id"], "run-a", "the run id must reach the daemon");
    let legacy_id = message["id"].as_u64().unwrap();

    // The decision arrives as the daemon's ack of that request.
    let accepted = state.legacy_event_to_api(&json!({"type": "ack", "id": legacy_id}));
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0].reply_to, Some(7));
    assert!(
        matches!(&accepted[0].event, ApiEvent::Ok),
        "a started continuation is answered `ok` on its request, not as a stream event"
    );
    // Its turn is still reported: the ack answered the request, not the turn.
    let done = state.legacy_event_to_api(&json!({"type": "done", "id": legacy_id}));
    assert!(
        matches!(&done[0].event, ApiEvent::TurnDone { .. }),
        "the continuation's own turn still ends with a turn boundary"
    );

    // A second continuation is refused by the daemon; the refusal names the
    // request so the caller's `request()` returns it as an error.
    let out = state.api_request_to_legacy(&json!({
        "req": "send_message", "id": 8, "session_id": "s1",
        "content": "", "system_reminder": "continue", "expected_run_id": "run-a"
    }));
    let Outbound::Legacy(message) = &out[0] else {
        panic!("expected legacy outbound");
    };
    let refused_id = message["id"].as_u64().unwrap();
    let refused = state.legacy_event_to_api(&json!({
        "type": "error", "id": refused_id, "message": "the run named is no longer current"
    }));
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].reply_to, Some(8));
    assert!(
        matches!(&refused[0].event, ApiEvent::Error { message, .. } if message.contains("no longer current")),
        "a refusal is a correlated error reply, not a turn event"
    );
    let unrelated_done = state.legacy_event_to_api(&json!({"type": "done", "id": 999}));
    assert!(
        unrelated_done
            .iter()
            .all(|frame| !matches!(frame.event, ApiEvent::TurnDone { .. }))
    );
}
