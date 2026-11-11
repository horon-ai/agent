//! A request that narrowed its catalog reports what it lost; one that did not
//! sends byte-identical bytes either way.

use crate::chat::request::{build_chat_completions_request, build_chat_completions_request_reporting};
use horon_agent_message_types::degradation::{DegradationKind, DegradationReceipt};
use horon_agent_message_types::{Message, ToolDefinition};
use serde_json::json;

fn tool(name: &str, schema: serde_json::Value) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: "test tool".to_string(),
        input_schema: schema,
        defer_loading: false,
    }
}

fn tools() -> Vec<ToolDefinition> {
    vec![
        tool(
            "search",
            json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        ),
        // A duplicate name: the chat endpoint can only carry the first one.
        tool(
            "search",
            json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        ),
        // A keyword outside the dialect: dropped from the sent schema.
        tool(
            "tags",
            json!({"type": "object", "properties": {"t": {"type": "array", "items": {"type": "string"}, "uniqueItems": true}}}),
        ),
    ]
}

fn messages() -> Vec<Message> {
    vec![Message::user("hello")]
}

#[test]
fn a_narrowed_request_reports_each_loss_once_and_sends_the_same_body() {
    // Given a catalog with a duplicate definition and a keyword OpenAI rejects.
    let tools = tools();
    // When it is built with a receipt sink.
    let mut receipts: Vec<DegradationReceipt> = Vec::new();
    let reported =
        build_chat_completions_request_reporting("gpt-5", &messages(), &tools, "system", None, Some(&mut receipts));
    // Then every loss is named, keyed to the tool it belongs to.
    assert!(
        receipts.contains(&DegradationReceipt::new(
            DegradationKind::DuplicateToolDropped,
            "search",
            "search"
        )),
        "{receipts:?}"
    );
    assert!(
        receipts.contains(&DegradationReceipt::new(
            DegradationKind::SchemaKeywordDropped,
            "tags",
            "uniqueItems"
        )),
        "{receipts:?}"
    );
    // And the request itself is what the non-reporting builder sends.
    assert_eq!(
        reported,
        build_chat_completions_request("gpt-5", &messages(), &tools, "system", None)
    );
}

#[test]
fn a_clean_catalog_reports_nothing_and_keeps_its_bytes() {
    // Given a catalog nothing needs to narrow.
    let tools = vec![tool(
        "search",
        json!({"type": "object", "properties": {"q": {"type": "string"}}}),
    )];
    // When it is built with a sink.
    let mut receipts: Vec<DegradationReceipt> = Vec::new();
    let reported =
        build_chat_completions_request_reporting("gpt-5", &messages(), &tools, "system", None, Some(&mut receipts));
    // Then nothing is claimed and the body matches the ordinary builder exactly.
    let unexpected: Vec<_> = receipts
        .iter()
        .filter(|r| r.kind != DegradationKind::StrictDisabled)
        .collect();
    assert!(unexpected.is_empty(), "{receipts:?}");
    assert_eq!(
        reported,
        build_chat_completions_request("gpt-5", &messages(), &tools, "system", None)
    );
    assert_eq!(
        serde_json::to_string(&reported).expect("encodes"),
        serde_json::to_string(&build_chat_completions_request(
            "gpt-5",
            &messages(),
            &tools,
            "system",
            None
        ))
        .expect("encodes")
    );
}
