//! The `degradation_receipts` frame.
//!
//! Kept out of [`translate`](crate::translate)'s match so that file, already at
//! its size budget, does not grow for an event most turns never send.

use horon_agent_harness_api::degradation::DegradationReceiptsEvent;
use horon_agent_harness_api::{ApiEvent, ServerFrame};
use horon_agent_session_types::DegradationReceipt;
use serde_json::Value;

/// Frame for an assistant message whose request was narrowed to fit the
/// provider.
pub(crate) fn frames(session_id: String, event: &Value) -> Vec<ServerFrame> {
    // A malformed receipt list still sends the event with the anchors intact, so
    // a client can at least tell that this request was narrowed. Only entries
    // that decode are carried; nothing is invented for the rest.
    let receipts: Vec<DegradationReceipt> = match serde_json::from_value(event["receipts"].clone()) {
        Ok(receipts) => receipts,
        Err(_) => Vec::new(),
    };
    vec![ServerFrame::event(ApiEvent::DegradationReceipts(
        DegradationReceiptsEvent {
            session_id,
            message_id: event["id"].as_str().unwrap_or("").to_string(),
            receipts,
        },
    ))]
}

#[cfg(test)]
mod tests {
    use super::*;
    use horon_agent_session_types::DegradationKind;
    use serde_json::json;

    #[test]
    fn the_frame_carries_every_receipt_and_the_message_it_belongs_to() {
        // Given the wire event the turn loop emits.
        let event = json!({
            "id": "message-7",
            "receipts": [
                {"kind": "schema_keyword_dropped", "tool": "bash", "target": "uniqueItems"},
                {"kind": "strict_disabled", "tool": "bash", "target": "strict"},
            ],
        });
        // When it is translated for a client.
        let value = serde_json::to_value(&frames("s-1".to_string(), &event)[0]).expect("frame");
        // Then one event goes out with both receipts intact.
        assert_eq!(value["ev"], json!("degradation_receipts"));
        assert_eq!(value["session_id"], json!("s-1"));
        assert_eq!(value["message_id"], json!("message-7"));
        assert_eq!(value["receipts"][0]["kind"], json!("schema_keyword_dropped"));
        assert_eq!(value["receipts"][0]["target"], json!("uniqueItems"));
        assert_eq!(value["receipts"][1]["kind"], json!("strict_disabled"));
        assert_eq!(
            serde_json::from_value::<DegradationReceipt>(value["receipts"][0].clone()).expect("decodes"),
            DegradationReceipt::new(DegradationKind::SchemaKeywordDropped, "bash", "uniqueItems")
        );
    }

    #[test]
    fn a_malformed_receipt_list_still_sends_the_event() {
        // Given an event whose receipts are not decodable.
        let event = json!({"id": "message-8", "receipts": "not a list"});
        // When it is translated.
        let value = serde_json::to_value(&frames("s-1".to_string(), &event)[0]).expect("frame");
        // Then the anchor survives and no loss is claimed.
        assert_eq!(value["ev"], json!("degradation_receipts"));
        assert_eq!(value["message_id"], json!("message-8"));
        assert_eq!(value["receipts"], json!([]));
    }
}
