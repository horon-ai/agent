//! The harness API `tool_done` carries the stored verdict (protocol 1.19).
//!
//! The field is additive: a call no layer judged serializes exactly as it did
//! before the field existed, and a client that predates it decodes the frame.

use crate::*;

#[test]
fn tool_done_without_a_verdict_keeps_the_eighteen_frame() {
    let event = ApiEvent::tool_done("s1", "call-1", "bash", "ok", None);
    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        r#"{"ev":"tool_done","session_id":"s1","call_id":"call-1","name":"bash","output":"ok"}"#
    );
}

#[test]
fn tool_done_with_a_verdict_round_trips_the_wire_name() {
    let event = ApiEvent::ToolDone {
        session_id: "s1".into(),
        call_id: "call-1".into(),
        name: "bash".into(),
        output: "denied".into(),
        error: Some("denied".into()),
        verdict: Some(horon_agent_tool_types::ToolVerdict::Rejected),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert_eq!(
        json,
        r#"{"ev":"tool_done","session_id":"s1","call_id":"call-1","name":"bash","output":"denied","error":"denied","verdict":"rejected"}"#
    );
    let ApiEvent::ToolDone { verdict, .. } = serde_json::from_str(&json).unwrap() else {
        panic!("expected tool_done");
    };
    assert_eq!(verdict, Some(horon_agent_tool_types::ToolVerdict::Rejected));
}

#[test]
fn tool_done_decodes_a_frame_that_predates_the_verdict() {
    let older = r#"{"ev":"tool_done","session_id":"s1","call_id":"call-1","name":"bash","output":"ok"}"#;
    let ApiEvent::ToolDone { verdict, .. } = serde_json::from_str(older).unwrap() else {
        panic!("expected tool_done");
    };
    assert_eq!(verdict, None, "an absent verdict is not a judgment");
}
