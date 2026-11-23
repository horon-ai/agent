use super::{ContentBlock, Message, Role, cache_relevant_message_value};
use serde_json::json;

#[test]
fn a2ui_block_serializes_with_a2ui_tag_and_round_trips() {
    let block = ContentBlock::A2ui {
        envelopes: vec![json!({
            "version": "1.0",
            "createSurface": { "surfaceId": "panel_1" }
        })],
        fallback_text: Some("Panel opened".to_string()),
    };

    let raw = serde_json::to_value(&block).expect("serialize");
    assert_eq!(raw["type"], "a2ui");
    assert_eq!(raw["envelopes"][0]["createSurface"]["surfaceId"], "panel_1");
    assert_eq!(raw["fallback_text"], "Panel opened");

    let decoded: ContentBlock = serde_json::from_value(raw).expect("deserialize");
    assert_eq!(
        decoded.a2ui_text_card().as_deref(),
        Some("[A2UI surface: panel_1]\nPanel opened")
    );
}

#[test]
fn cache_relevant_message_value_preserves_a2ui_blocks_deterministically() {
    let msg = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::A2ui {
            envelopes: vec![json!({"version": "1.0", "deleteSurface": {"surfaceId": "panel_1"}})],
            fallback_text: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    };
    let projected = cache_relevant_message_value(&msg);
    assert_eq!(projected["content"][0]["type"], "a2ui");
    assert!(projected["content"][0].get("fallback_text").is_none());
}
