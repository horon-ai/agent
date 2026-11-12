//! A record that lost nothing keeps its bytes; one that lost something says so,
//! and an older record still loads.

use horon_agent_session_types::{DegradationKind, DegradationReceipt, StoredMessage};

#[test]
fn a_record_without_a_loss_encodes_exactly_as_before() {
    // Given a record stored before receipts existed.
    let before = r#"{"id":"m-1","role":"assistant","content":[{"type":"text","text":"hi"}]}"#;
    let message: StoredMessage = serde_json::from_str(before).expect("old record loads");
    // Then the field is absent, not an empty list.
    assert!(message.degradations.is_none());
    // And re-encoding it produces the same bytes, so an untouched session is
    // not rewritten by a build that knows about receipts.
    assert_eq!(serde_json::to_string(&message).expect("encodes"), before);
}

#[test]
fn a_narrowed_record_carries_every_loss_and_round_trips() {
    // Given a request that lost a keyword and its strict guarantee.
    let receipts = vec![
        DegradationReceipt::new(DegradationKind::SchemaKeywordDropped, "search", "uniqueItems"),
        DegradationReceipt::new(DegradationKind::StrictDisabled, "search", "strict"),
    ];
    let mut message: StoredMessage =
        serde_json::from_str(r#"{"id":"m-2","role":"assistant","content":[],"degradations":null}"#)
            .expect("record loads");
    message.degradations = Some(receipts.clone());
    // When it is written and read back.
    let encoded = serde_json::to_string(&message).expect("encodes");
    let decoded: StoredMessage = serde_json::from_str(&encoded).expect("decodes");
    // Then both losses survive, with their stable wire names.
    assert_eq!(decoded.degradations, Some(receipts));
    assert!(encoded.contains(r#""kind":"schema_keyword_dropped""#), "{encoded}");
    assert!(encoded.contains(r#""target":"strict""#), "{encoded}");
}
