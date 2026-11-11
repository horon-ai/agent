//! A repaired tool call keeps the arguments the model sent and the repairs that
//! were applied, and a record written before those fields existed still loads.

use horon_agent_session_store::*;
use horon_agent_session_types::StoredMessage;
use serde_json::{Value, json};

/// The bytes an untouched record has always had: the two repair fields are
/// absent, and so is every other field that is empty.
fn untouched_bytes() -> Value {
    json!({
        "message": {
            "id": "m-1",
            "role": "user",
            "content": [{"type": "text", "text": "hello"}]
        }
    })
}

/// The same record spelled with an explicit null for every field that is empty,
/// which is the other shape a writer can produce for it.
fn spelled_with_nulls() -> Value {
    json!({
        "message": {
            "id": "m-1",
            "role": "user",
            "content": [{"type": "text", "text": "hello"}],
            "display_role": null,
            "turn_start": false,
            "timestamp": null,
            "tool_duration_ms": null,
            "token_usage": null,
            "original_input": null,
            "arg_repairs": null
        }
    })
}

#[test]
fn a_record_written_before_the_fields_existed_still_loads() {
    // Given the bytes of a session written by an earlier version.
    let earlier = untouched_bytes();
    // When it is read back through the payload the store decodes.
    let payload: MessageAppendedPayload =
        serde_json::from_value(earlier.clone()).expect("an earlier record still decodes");
    // Then the repair fields read as absent, and re-encoding produces the same
    // bytes, because a record that was never repaired omits both fields.
    assert!(payload.message.original_input.is_none());
    assert!(payload.message.arg_repairs.is_none());
    assert_eq!(serde_json::to_value(&payload).unwrap(), earlier);
}

#[test]
fn a_repaired_record_holds_the_original_arguments_and_the_change_list() {
    // Given a call whose arguments were rewritten before they ran.
    let original = json!({"command": "ls", "timeout": "5000"});
    let changes = vec!["coerced `timeout` to a number".to_string()];
    let payload = MessageAppendedPayload {
        message: StoredMessage {
            original_input: Some(original.clone()),
            arg_repairs: Some(changes.clone()),
            degradations: None,
            ..StoredMessage::default()
        },
    };
    // When the payload is written and read back the way the store does it.
    let bytes = serde_json::to_vec(&payload).unwrap();
    let decoded: MessageAppendedPayload = serde_json::from_slice(&bytes).unwrap();
    // Then both values survive exactly, and the bytes carry them.
    assert_eq!(decoded.message.original_input, Some(original));
    assert_eq!(decoded.message.arg_repairs, Some(changes));
    assert!(serde_json::to_value(&payload).unwrap()["message"]["original_input"].is_object());
}

#[test]
fn an_unrepaired_record_omits_both_fields() {
    // Given a record that no repair touched.
    let payload = MessageAppendedPayload {
        message: StoredMessage::default(),
    };
    // When it is serialised.
    let value = serde_json::to_value(&payload).unwrap();
    // Then neither key appears, so the record keeps its previous bytes.
    assert!(value["message"].get("original_input").is_none());
    assert!(value["message"].get("arg_repairs").is_none());
}

#[test]
fn a_payload_with_the_fields_at_null_matches_one_that_omits_them() {
    // Given two spellings of the same untouched record: explicit nulls, and the
    // absent keys an earlier writer produced.
    let with_nulls = spelled_with_nulls();
    let without = untouched_bytes();
    // When both are decoded and re-encoded.
    let a: MessageAppendedPayload = serde_json::from_value(with_nulls).unwrap();
    let b: MessageAppendedPayload = serde_json::from_value(without.clone()).unwrap();
    // Then they decode to the same record and re-encode to the same bytes, so a
    // record that was never repaired keeps the bytes it already had.
    assert_eq!(a.message.id, b.message.id);
    assert_eq!(a.message.role, b.message.role);
    assert_eq!(
        serde_json::to_value(&a.message.content).unwrap(),
        serde_json::to_value(&b.message.content).unwrap()
    );
    assert_eq!(serde_json::to_value(&a).unwrap(), without);
}
