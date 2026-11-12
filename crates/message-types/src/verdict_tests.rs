//! Verdict storage tests.
//!
//! These live apart from the message module's own tests so that adding a case
//! does not push `lib.rs` over its size cap.

use super::*;

#[test]
fn a_stored_tool_result_without_a_verdict_keeps_its_old_bytes() {
    let json = r#"{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"done"}]}"#;
    let message: Message = serde_json::from_str(json).expect("a result stored before verdicts still reads");
    match &message.content[0] {
        ContentBlock::ToolResult { verdict, is_error, .. } => {
            assert_eq!(*verdict, None, "an old result has no verdict to report");
            assert_eq!(*is_error, None);
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
    assert_eq!(serde_json::to_string(&message).unwrap(), json);
}

#[test]
fn a_stated_verdict_round_trips_through_stored_json() {
    let json = r#"{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_2","content":"boom","is_error":true,"verdict":"failed"}]}"#;
    let message: Message = serde_json::from_str(json).expect("a verdict deserializes");
    assert!(matches!(
        &message.content[0],
        ContentBlock::ToolResult {
            verdict: Some(ToolVerdict::Failed),
            ..
        }
    ));
    assert_eq!(serde_json::to_string(&message).unwrap(), json);
}

#[test]
fn both_constructors_store_the_bytes_the_literal_would() {
    let plain = ContentBlock::tool_result("toolu_3", "ok", None);
    let literal = ContentBlock::ToolResult {
        tool_use_id: "toolu_3".to_string(),
        content: "ok".to_string(),
        is_error: None,
        verdict: None,
    };
    assert_eq!(
        serde_json::to_string(&plain).unwrap(),
        serde_json::to_string(&literal).unwrap(),
        "a result built without a verdict must serialize exactly as the literal it replaces"
    );

    let stated = ContentBlock::tool_result_with_verdict("toolu_4", "cut", Some(false), Some(ToolVerdict::Truncated));
    let json = serde_json::to_string(&stated).unwrap();
    assert!(json.contains(r#""verdict":"truncated""#), "got {json}");
}

#[test]
fn an_unknown_verdict_name_is_refused() {
    let json =
        r#"{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_5","content":"x","verdict":"done"}]}"#;
    assert!(
        serde_json::from_str::<Message>(json).is_err(),
        "a verdict nobody defined is an error, not a guess"
    );
}
