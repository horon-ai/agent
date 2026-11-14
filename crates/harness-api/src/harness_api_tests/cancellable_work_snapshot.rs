//! Forward compatibility of the minor 22 additions: older frames still
//! decode, and a newer event never fails a client that predates it.

use crate::*;

/// The minor 22 additions are additive: every pre-existing frame keeps its
/// exact bytes, and a new event frame deserializes on a client that predates it.
#[test]
fn minor_twenty_two_additions_are_additive() {
    // A request frame that existed before minor 22 must still serialize
    // byte-identically and still decode after the enum grew.
    let legacy = r#"{"v":1,"id":3,"req":"get_snapshot","session_id":"s1","tail":5}"#;
    let decoded: ClientFrame = serde_json::from_str(legacy).unwrap();
    assert_eq!(serde_json::to_string(&decoded).unwrap(), legacy);

    // A frame from this version carrying a feature the peer does not know must
    // decode into the forward-compatibility catch-all rather than fail.
    let unknown_event: ServerFrame =
        serde_json::from_str(r#"{"v":1,"ev":"some_future_event","reply_to":4,"session_id":"s1"}"#)
            .expect("a newer event must decode");
    assert!(matches!(unknown_event.event, ApiEvent::Unknown));

    // The three new events carry no unknown keys for an old decoder once the
    // struct exists, and their optional field stays absent when unset.
    let cancel_turn = ApiEvent::CancelTurnResult {
        session_id: "s1".into(),
        run_id: "run-1".into(),
        accepted: true,
        reason: None,
    };
    let wire = serde_json::to_value(&cancel_turn).unwrap();
    assert_eq!(wire["ev"], "cancel_turn_result");
    assert!(
        wire.get("reason").is_none(),
        "an unset reason must not serialize: {wire}"
    );

    let list = ApiEvent::BackgroundWorkList {
        session_id: "s1".into(),
        items: vec![BackgroundWorkItem {
            call_id: "call-1".into(),
            task_id: "bg_1".into(),
            kind: "terminal".into(),
            command: None,
            cwd: None,
            output: None,
        }],
    };
    let wire = serde_json::to_value(&list).unwrap();
    let item = &wire["items"][0];
    assert!(item.get("command").is_none() && item.get("output").is_none(), "{wire}");
    assert_eq!(serde_json::from_value::<ApiEvent>(wire).unwrap(), list);
}

/// Stored sessions, transcripts, and every event a client knew before minor 22
/// still decode: the new variants only add names, they never narrow one.
#[test]
fn older_event_frames_still_decode_after_the_enum_grew() {
    for legacy in [
        r#"{"v":1,"ev":"ok","reply_to":1}"#,
        r#"{"v":1,"ev":"pong","reply_to":2}"#,
        r#"{"v":1,"ev":"text_delta","reply_to":3,"session_id":"s1","text":"hi","message_id":"m1"}"#,
    ] {
        let frame: ServerFrame = serde_json::from_str(legacy).unwrap_or_else(|error| panic!("{legacy}: {error}"));
        assert_ne!(
            frame.event,
            ApiEvent::Unknown,
            "{legacy} must not collapse into the catch-all"
        );
    }
}
