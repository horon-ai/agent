//! The `degradation_receipts` event payload.
//!
//! `history_message_index` and `response_stats` optionality is covered by the
//! tests beside this module; the payload itself lives in its own file so the
//! event enum, already at its size budget, grows by one arm rather than by the
//! receipt shape too.

use horon_agent_session_types::DegradationReceipt;
use serde::{Deserialize, Serialize};

/// What one request lost to provider schema constraints, anchored to the stored
/// assistant message it produced.
///
/// Additive: a client that does not know the event ignores it, and the message
/// keeps its usual content either way.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DegradationReceiptsEvent {
    pub session_id: String,
    /// The stored assistant message this request produced.
    pub message_id: String,
    /// One entry per distinct loss in the session, not per request.
    pub receipts: Vec<DegradationReceipt>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use horon_agent_session_types::DegradationKind;

    #[test]
    fn a_receipt_round_trips_with_its_kind_and_target() {
        // Given one recorded loss.
        let event = DegradationReceiptsEvent {
            session_id: "s-1".to_string(),
            message_id: "m-1".to_string(),
            receipts: vec![DegradationReceipt::new(
                DegradationKind::SchemaKeywordDropped,
                "bash",
                "uniqueItems",
            )],
        };
        // When it crosses the wire.
        let encoded = serde_json::to_value(&event).expect("serialises");
        // Then the kind is the stable wire name and the target survives verbatim.
        assert_eq!(
            encoded["receipts"][0]["kind"],
            serde_json::json!("schema_keyword_dropped")
        );
        assert_eq!(encoded["receipts"][0]["tool"], serde_json::json!("bash"));
        assert_eq!(encoded["receipts"][0]["target"], serde_json::json!("uniqueItems"));
        assert_eq!(
            serde_json::from_value::<DegradationReceiptsEvent>(encoded).expect("decodes"),
            event
        );
    }
}
