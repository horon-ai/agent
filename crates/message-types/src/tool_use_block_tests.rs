use super::*;

/// A Gemini-3 tool call's `thought_signature` must survive the trip from the
/// live stream into the stored assistant message.
///
/// Regression: the turn loops each hand-built `ContentBlock::ToolUse` and
/// three of the four copies hardcoded `thought_signature: None`. The stream
/// captured the signature correctly, but it was dropped the moment the
/// assistant message was persisted, so the *next* request replayed a
/// fully-unsigned history and the backend rejected it with HTTP 400
/// `Function call is missing a thought_signature in functionCall parts`.
/// Reported as a failure on the second message of a Gemini session.
#[test]
fn to_tool_use_block_preserves_thought_signature() {
    let call = ToolCall {
        id: "toolu_1".to_string(),
        name: "websearch".to_string(),
        input: serde_json::json!({"query": "water testing"}),
        intent: None,
        thought_signature: Some("SIG_ABC".to_string()),
    };

    match call.to_tool_use_block() {
        ContentBlock::ToolUse {
            id,
            name,
            thought_signature,
            ..
        } => {
            assert_eq!(id, "toolu_1");
            assert_eq!(name, "websearch");
            assert_eq!(
                thought_signature.as_deref(),
                Some("SIG_ABC"),
                "signature must be carried onto the stored block"
            );
        }
        other => panic!("expected ToolUse, got {other:?}"),
    }
}

/// The signature must also survive session serialization, otherwise a
/// resumed session replays unsigned calls and hits the same 400.
#[test]
fn thought_signature_round_trips_through_session_json() {
    let block = ToolCall {
        id: "toolu_2".to_string(),
        name: "bash".to_string(),
        input: serde_json::json!({"command": "ls"}),
        intent: None,
        thought_signature: Some("SIG_PERSIST".to_string()),
    }
    .to_tool_use_block();

    let json = serde_json::to_string(&block).expect("serialize");
    let restored: ContentBlock = serde_json::from_str(&json).expect("deserialize");

    match restored {
        ContentBlock::ToolUse { thought_signature, .. } => {
            assert_eq!(thought_signature.as_deref(), Some("SIG_PERSIST"))
        }
        other => panic!("expected ToolUse, got {other:?}"),
    }
}

/// Providers that do not use signatures must stay clean: no empty-string
/// signature, and the field omitted from serialized sessions.
#[test]
fn absent_signature_stays_absent() {
    let block = ToolCall {
        id: "toolu_3".to_string(),
        name: "read".to_string(),
        input: serde_json::json!({}),
        intent: None,
        thought_signature: None,
    }
    .to_tool_use_block();

    let json = serde_json::to_string(&block).expect("serialize");
    assert!(
        !json.contains("thought_signature"),
        "absent signature must be omitted, got {json}"
    );
}
