//! Optional fields on `HistoryMessage` and `RenderedImage` stay out of the
//! encoded form until a caller sets them, so an older payload round-trips
//! byte-identically.

use crate::{HistoryMessage, RenderedImage};

#[test]
fn image_history_boundary_is_optional_and_round_trips() {
    let legacy = serde_json::json!({"media_type": "image/png", "data": "bytes", "label": null,
        "source": {"kind": "tool_result", "tool_name": "read"}, "anchor": {"kind": "tool_call", "id": "read-1"}});
    let mut image: RenderedImage = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(image.history_message_index, None);
    assert_eq!(serde_json::to_value(&image).unwrap(), legacy);
    for boundary in [0, 3] {
        image.history_message_index = Some(boundary);
        let encoded = serde_json::to_value(&image).unwrap();
        assert_eq!(encoded["history_message_index"], boundary);
        assert_eq!(serde_json::from_value::<RenderedImage>(encoded).unwrap(), image);
    }
}

#[test]
fn history_response_stats_are_backward_compatible_and_optional() {
    let old = serde_json::json!({"role":"assistant","content":"answer"});
    let message: HistoryMessage = serde_json::from_value(old.clone()).unwrap();
    assert!(message.response_stats.is_none());
    assert_eq!(serde_json::to_value(message).unwrap(), old);
    let new = serde_json::json!({"role":"assistant","content":"answer",
        "response_stats":{"input_tokens":0,"output_tokens":12,"cache_read_tokens":0}});
    let message: HistoryMessage = serde_json::from_value(new.clone()).unwrap();
    assert_eq!(message.response_stats.as_ref().unwrap().duration_secs, None);
    assert_eq!(serde_json::to_value(message).unwrap(), new);
}
