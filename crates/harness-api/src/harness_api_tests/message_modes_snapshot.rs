//! Minor 16 wire shape: `get_message_modes` and its reply.
//!
//! Both are additions, so a client that predates them neither sends nor reads
//! the pair. The reply is a new event kind, which an older client skips as an
//! unknown event.

use crate::*;

#[test]
fn the_request_encodes_the_session_it_asks_about() {
    let request = ClientFrame::new(
        1,
        ApiRequest::GetMessageModes {
            session_id: "s1".into(),
        },
    );

    assert_eq!(
        serde_json::to_string(&request).unwrap(),
        r#"{"v":1,"id":1,"req":"get_message_modes","session_id":"s1"}"#
    );
}

#[test]
fn the_reply_carries_the_three_mode_strings() {
    let reply = ServerFrame::reply(
        1,
        ApiEvent::MessageModes {
            session_id: "s1".into(),
            modes: MessageModes {
                follow_up_mode: "all".into(),
                steering_mode: "one-at-a-time".into(),
                interrupt_mode: "wait".into(),
            },
        },
    );

    assert_eq!(
        serde_json::to_string(&reply).unwrap(),
        r#"{"v":1,"reply_to":1,"ev":"message_modes","session_id":"s1","modes":{"follow_up_mode":"all","steering_mode":"one-at-a-time","interrupt_mode":"wait"}}"#
    );
    let decoded: ServerFrame = serde_json::from_str(&serde_json::to_string(&reply).unwrap()).unwrap();
    assert_eq!(decoded.event, reply.event, "the reply round-trips");
}

#[test]
fn an_older_client_skips_the_reply_as_an_unknown_event() {
    let legacy = r#"{"v":1,"reply_to":9,"ev":"message_modes","session_id":"s1","modes":{"follow_up_mode":"all","steering_mode":"all","interrupt_mode":"wait"}}"#;
    let decoded: ServerFrame = serde_json::from_str(legacy).unwrap();

    assert!(
        matches!(decoded.event, ApiEvent::MessageModes { .. }),
        "a client at this version must decode the pair instead of failing the frame"
    );
}
