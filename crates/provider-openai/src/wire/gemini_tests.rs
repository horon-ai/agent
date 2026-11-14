//! The Gemini request body and the chunk shapes a `generateContent` stream can
//! arrive in.
//!
//! Every stream test asserts the full ordered event list rather than looking for
//! one event, because the order is what the turn loop relies on: thinking closes
//! before text, text closes before a call, and `MessageEnd` comes last.

use super::{GeminiParser, SYNTHETIC_CALL_ID_PREFIX, build_generate_content_request};
use crate::chat::transport::REDACTED;
use crate::test_support::{TEST_KEY, sse_frame};
use crate::wire::sse::{WireParser, WireSseStream};
use horon_agent_message_types::StreamEvent;
use horon_agent_message_types::degradation::{DegradationKind, DegradationReceipt};
use horon_agent_message_types::{ContentBlock, Message, Role, ToolDefinition};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Builders

fn user(blocks: Vec<ContentBlock>) -> Message {
    Message {
        role: Role::User,
        content: blocks,
        timestamp: None,
        tool_duration_ms: None,
    }
}

fn assistant(blocks: Vec<ContentBlock>) -> Message {
    Message {
        role: Role::Assistant,
        content: blocks,
        timestamp: None,
        tool_duration_ms: None,
    }
}

fn text(text: &str) -> ContentBlock {
    ContentBlock::Text {
        text: text.to_string(),
        cache_control: None,
    }
}

fn call(id: &str, name: &str, input: Value) -> ContentBlock {
    ContentBlock::ToolUse {
        id: id.to_string(),
        name: name.to_string(),
        input,
        thought_signature: None,
    }
}

fn result(id: &str, content: &str, is_error: Option<bool>) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: id.to_string(),
        content: content.to_string(),
        is_error,
        verdict: None,
    }
}

fn tool(name: &str, schema: Value) -> ToolDefinition {
    ToolDefinition::new(name, format!("the {name} tool"), schema)
}

fn object_schema(properties: Value) -> Value {
    json!({ "type": "object", "properties": properties })
}

fn body(model: &str, messages: &[Message], tools: &[ToolDefinition], system: &str, effort: Option<&str>) -> Value {
    build_generate_content_request(model, messages, tools, system, effort, None)
}

fn parts_of(contents: &Value, index: usize) -> Vec<Value> {
    contents
        .as_array()
        .and_then(|entries| entries.get(index))
        .and_then(|entry| entry.get("parts"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Request body

#[test]
fn a_plain_question_becomes_one_user_turn() {
    let request = body("gemini-3-pro", &[user(vec![text("hello")])], &[], "be brief", None);

    assert_eq!(
        request["systemInstruction"],
        json!({ "parts": [{ "text": "be brief" }] })
    );
    assert_eq!(
        request["contents"],
        json!([{ "role": "user", "parts": [{ "text": "hello" }] }])
    );
    assert!(request.get("model").is_none(), "the model lives in the URL: {request}");
    assert!(request.get("stream").is_none(), "streaming lives in the URL: {request}");
    assert!(request.get("tools").is_none(), "{request}");
    assert!(request.get("generationConfig").is_none(), "{request}");
}

#[test]
fn an_empty_system_prompt_adds_no_system_instruction() {
    let request = body("gemini-3-pro", &[user(vec![text("hello")])], &[], "", None);

    assert!(request.get("systemInstruction").is_none(), "{request}");
    assert_eq!(request["contents"][0]["parts"][0]["text"], json!("hello"));
}

#[test]
fn an_assistant_turn_is_a_model_turn_and_an_image_is_inline_data() {
    let messages = [
        user(vec![
            text("look"),
            ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "QUJD".to_string(),
            },
        ]),
        assistant(vec![text("seen")]),
    ];
    let request = body("gemini-3-pro", &messages, &[], "", None);

    assert_eq!(
        request["contents"],
        json!([
            {
                "role": "user",
                "parts": [
                    { "text": "look" },
                    { "inlineData": { "mimeType": "image/png", "data": "QUJD" } }
                ]
            },
            { "role": "model", "parts": [{ "text": "seen" }] }
        ])
    );
}

#[test]
fn a_server_call_id_travels_back_on_the_call_and_its_response() {
    let messages = [
        user(vec![text("read it")]),
        assistant(vec![call("fc_1", "read", json!({ "path": "a.txt" }))]),
        user(vec![result("fc_1", "contents", None)]),
    ];
    let request = body("gemini-3-pro", &messages, &[], "", None);

    assert_eq!(
        request["contents"][1],
        json!({ "role": "model", "parts": [{ "functionCall": {
            "name": "read",
            "args": { "path": "a.txt" },
            "id": "fc_1",
        } }] })
    );
    assert_eq!(
        request["contents"][2],
        json!({ "role": "user", "parts": [{ "functionResponse": {
            "name": "read",
            "response": { "output": "contents" },
            "id": "fc_1",
        } }] })
    );
}

#[test]
fn a_synthesized_call_id_is_never_sent_back() {
    let messages = [
        user(vec![text("read it")]),
        assistant(vec![call("hgc_1a_0", "read", json!({ "path": "a.txt" }))]),
        user(vec![result("hgc_1a_0", "contents", None)]),
    ];
    let request = body("gemini-3-pro", &messages, &[], "", None);

    let call_part = &request["contents"][1]["parts"][0]["functionCall"];
    assert!(
        call_part.get("id").is_none(),
        "a local id is not the server's: {call_part}"
    );
    assert_eq!(call_part["name"], json!("read"));
    let response = &request["contents"][2]["parts"][0]["functionResponse"];
    assert!(response.get("id").is_none(), "{response}");
    // The response is still addressed: the name comes from the history lookup.
    assert_eq!(response["name"], json!("read"));
}

#[test]
fn a_failed_call_answers_with_error_and_a_bodyless_call_sends_no_args() {
    let messages = [
        user(vec![text("go")]),
        assistant(vec![call("fc_9", "boom", json!("not an object"))]),
        user(vec![
            result("fc_9", "it broke", Some(true)),
            result("fc_missing", "orphan", None),
        ]),
    ];
    let request = body("gemini-3-pro", &messages, &[], "", None);

    assert_eq!(
        request["contents"][1]["parts"][0]["functionCall"],
        json!({ "name": "boom", "args": {}, "id": "fc_9" })
    );
    let responses = parts_of(&request["contents"], 2);
    assert_eq!(
        responses.len(),
        1,
        "a result for no known call is skipped: {responses:?}"
    );
    assert_eq!(
        responses[0],
        json!({ "functionResponse": {
            "name": "boom",
            "response": { "error": "it broke" },
            "id": "fc_9",
        } })
    );
}

#[test]
fn a_thought_signature_rides_on_the_part_not_inside_the_call() {
    let signed = ContentBlock::ToolUse {
        id: "fc_2".to_string(),
        name: "read".to_string(),
        input: json!({ "path": "a.txt" }),
        thought_signature: Some("sig".to_string()),
    };
    let messages = [
        user(vec![text("go")]),
        assistant(vec![signed]),
        user(vec![result("fc_2", "done", None)]),
    ];
    let request = body("gemini-3-pro", &messages, &[], "", None);

    let part = &request["contents"][1]["parts"][0];
    assert_eq!(part["thoughtSignature"], json!("sig"));
    assert!(
        part["functionCall"].get("thoughtSignature").is_none(),
        "the signature belongs to the part: {part}"
    );
    assert_eq!(part["functionCall"]["id"], json!("fc_2"));
}

#[test]
fn an_empty_or_absent_signature_adds_no_key() {
    for signature in [None, Some(String::new())] {
        let unsigned = ContentBlock::ToolUse {
            id: "fc_3".to_string(),
            name: "read".to_string(),
            input: json!({}),
            thought_signature: signature,
        };
        let messages = [
            user(vec![text("go")]),
            assistant(vec![unsigned]),
            user(vec![result("fc_3", "done", None)]),
        ];
        let request = body("gemini-3-pro", &messages, &[], "", None);
        let part = &request["contents"][1]["parts"][0];
        assert!(part.get("thoughtSignature").is_none(), "{part}");
    }
}

#[test]
fn history_only_blocks_are_dropped_and_the_turns_around_them_merge() {
    let messages = [
        user(vec![text("one")]),
        user(vec![ContentBlock::ReasoningTrace {
            text: "a trace".to_string(),
        }]),
        user(vec![text("two")]),
        assistant(vec![
            ContentBlock::AnthropicThinking {
                thinking: "hmm".to_string(),
                signature: "s".to_string(),
            },
            ContentBlock::ProviderNative {
                provider: "anthropic-compatible".to_string(),
                item: json!({ "type": "server_tool_use" }),
            },
        ]),
    ];
    let request = body("gemini-3-pro", &messages, &[], "", None);

    assert_eq!(
        request["contents"],
        json!([{ "role": "user", "parts": [{ "text": "one" }, { "text": "two" }] }]),
        "the assistant turn had no carryable part, so it vanished and the users merged"
    );
}

#[test]
fn tools_become_one_entry_holding_every_declaration() {
    let tools = [
        tool("read", object_schema(json!({ "path": { "type": "string" } }))),
        tool("write", object_schema(json!({ "body": { "type": "string" } }))),
    ];
    let request = body("gemini-3-pro", &[user(vec![text("hi")])], &tools, "", None);

    let entries = request["tools"].as_array().expect("one tools entry");
    assert_eq!(entries.len(), 1, "{entries:?}");
    let declarations = entries[0]["functionDeclarations"]
        .as_array()
        .expect("a declaration list");
    assert_eq!(declarations.len(), 2);
    assert_eq!(declarations[0]["name"], json!("read"));
    assert_eq!(declarations[0]["description"], json!("the read tool"));
    assert_eq!(
        declarations[0]["parameters"]["properties"]["path"]["type"],
        json!("string")
    );
    assert_eq!(declarations[1]["name"], json!("write"));
    assert!(!request.to_string().contains("parametersJsonSchema"));
}

#[test]
fn a_tool_with_no_properties_sends_no_parameters() {
    let tools = [
        tool("empty", json!({ "type": "object", "properties": {} })),
        tool("none", json!({ "type": "object" })),
    ];
    let request = body("gemini-3-pro", &[user(vec![text("hi")])], &tools, "", None);

    let declarations = request["tools"][0]["functionDeclarations"]
        .as_array()
        .expect("a declaration list");
    assert_eq!(declarations.len(), 2);
    for declaration in declarations {
        assert!(
            declaration.get("parameters").is_none(),
            "Gemini rejects an object schema with no properties: {declaration}"
        );
    }
}

#[test]
fn a_duplicate_name_is_dropped_and_reported() {
    let tools = [
        tool("read", object_schema(json!({ "a": { "type": "string" } }))),
        tool("read", object_schema(json!({ "b": { "type": "string" } }))),
    ];
    let mut receipts: Vec<DegradationReceipt> = Vec::new();
    let request = build_generate_content_request(
        "gemini-3-pro",
        &[user(vec![text("hi")])],
        &tools,
        "",
        None,
        Some(&mut receipts),
    );

    let declarations = request["tools"][0]["functionDeclarations"]
        .as_array()
        .expect("a declaration list");
    assert_eq!(declarations.len(), 1, "{declarations:?}");
    assert!(
        receipts
            .iter()
            .any(|receipt| receipt.kind == DegradationKind::DuplicateToolDropped
                && receipt.tool == "read"
                && receipt.target == "read"),
        "{receipts:?}"
    );
}

#[test]
fn a_keyword_the_gemini_dialect_drops_is_reported() {
    let tools = [tool(
        "read",
        json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "additionalProperties": false,
        }),
    )];
    let mut receipts: Vec<DegradationReceipt> = Vec::new();
    let request = build_generate_content_request(
        "gemini-3-pro",
        &[user(vec![text("hi")])],
        &tools,
        "",
        None,
        Some(&mut receipts),
    );

    let parameters = &request["tools"][0]["functionDeclarations"][0]["parameters"];
    assert!(parameters.get("additionalProperties").is_none(), "{parameters}");
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| receipt.kind == DegradationKind::SchemaKeywordDropped)
            .map(|receipt| (receipt.tool.as_str(), receipt.target.as_str()))
            .collect::<Vec<_>>(),
        vec![("read", "additionalProperties")]
    );
}

#[test]
fn a_deferred_tool_is_not_declared_on_this_turn() {
    let tools = [
        tool("read", object_schema(json!({ "path": { "type": "string" } }))),
        tool("write", object_schema(json!({ "body": { "type": "string" } }))).deferred(),
    ];
    let request = body("gemini-3-pro", &[user(vec![text("hi")])], &tools, "", None);

    let declarations = request["tools"][0]["functionDeclarations"]
        .as_array()
        .expect("a declaration list");
    assert_eq!(declarations.len(), 1, "{declarations:?}");
    assert_eq!(declarations[0]["name"], json!("read"));
}

#[test]
fn a_gemini_25_model_takes_a_token_budget() {
    for (effort, budget) in [
        ("minimal", 512_u64),
        ("low", 1024),
        ("medium", 8192),
        ("high", 24_576),
        ("xhigh", 24_576),
        ("max", 24_576),
    ] {
        let request = body("gemini-2.5-pro", &[user(vec![text("hi")])], &[], "", Some(effort));
        assert_eq!(
            request["generationConfig"]["thinkingConfig"],
            json!({ "thinkingBudget": budget, "includeThoughts": true }),
            "effort {effort}"
        );
    }
}

#[test]
fn a_models_prefix_on_the_model_name_is_ignored() {
    let request = body(
        "models/gemini-2.5-flash",
        &[user(vec![text("hi")])],
        &[],
        "",
        Some("minimal"),
    );

    assert_eq!(
        request["generationConfig"]["thinkingConfig"],
        json!({ "thinkingBudget": 512, "includeThoughts": true })
    );
}

#[test]
fn a_newer_model_takes_a_named_thinking_level() {
    for (effort, level) in [
        ("minimal", "MINIMAL"),
        ("low", "LOW"),
        ("medium", "MEDIUM"),
        ("high", "HIGH"),
        ("xhigh", "XHIGH"),
        ("max", "XHIGH"),
    ] {
        let request = body("gemini-3-pro", &[user(vec![text("hi")])], &[], "", Some(effort));
        assert_eq!(
            request["generationConfig"]["thinkingConfig"],
            json!({ "thinkingLevel": level, "includeThoughts": true }),
            "effort {effort}"
        );
    }
}

#[test]
fn an_effort_the_wire_cannot_express_adds_no_generation_config() {
    for effort in [None, Some(""), Some("none"), Some("swarm")] {
        let request = body("gemini-3-pro", &[user(vec![text("hi")])], &[], "", effort);
        assert!(
            request.get("generationConfig").is_none(),
            "effort {effort:?} must not send a thinking config: {request}"
        );
    }
}

// ---------------------------------------------------------------------------
// Parser helpers

fn parse_payloads(payloads: &[&str]) -> Vec<StreamEvent> {
    let mut parser = GeminiParser::new();
    let mut events = VecDeque::new();
    for payload in payloads {
        parser.parse(payload, &mut events);
    }
    parser.finish(&mut events);
    events.into_iter().collect()
}

/// One payload's events, without the EOF step, so a mid-stream ordering is
/// visible on its own.
fn parse_one(parser: &mut GeminiParser, payload: &str) -> Vec<StreamEvent> {
    let mut events = VecDeque::new();
    parser.parse(payload, &mut events);
    events.into_iter().collect()
}

fn labels(events: &[StreamEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| match event {
            StreamEvent::TextDelta(text) => format!("TextDelta({text})"),
            StreamEvent::TextDone => "TextDone".to_string(),
            StreamEvent::ToolUseStart { id, name } => format!("ToolUseStart({id},{name})"),
            StreamEvent::ToolInputDeltaFor { id, delta } => format!("ToolInputDeltaFor({id},{delta})"),
            StreamEvent::ToolUseEndFor { id } => format!("ToolUseEndFor({id})"),
            StreamEvent::ToolUseSignatureFor { id, signature } => {
                format!("ToolUseSignatureFor({id},{signature})")
            }
            StreamEvent::ThinkingStart => "ThinkingStart".to_string(),
            StreamEvent::ThinkingDelta(text) => format!("ThinkingDelta({text})"),
            StreamEvent::ThinkingEnd => "ThinkingEnd".to_string(),
            StreamEvent::ThinkingDone { .. } => "ThinkingDone".to_string(),
            StreamEvent::MessageEnd { stop_reason } => format!("MessageEnd({stop_reason:?})"),
            StreamEvent::TokenUsage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
            } => format!(
                "TokenUsage({input_tokens:?},{output_tokens:?},{cache_read_input_tokens:?},{cache_creation_input_tokens:?})"
            ),
            StreamEvent::Error {
                message,
                retry_after_secs,
            } => format!("Error({message},{retry_after_secs:?})"),
            other => format!("other({other:?})"),
        })
        .collect()
}

fn summary(events: &[StreamEvent]) -> Vec<String> {
    // The thinking duration is wall-clock and is never part of the contract.
    labels(events)
}

// ---------------------------------------------------------------------------
// Parser: the happy path

const TWO_TEXTS: &str = r#"{"candidates":[{"content":{"parts":[{"text":"Hello"}]}}]}"#;
const TEXT_AND_FINISH: &str = r#"{"candidates":[{"content":{"parts":[{"text":" world"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5,"thoughtsTokenCount":2,"cachedContentTokenCount":4}}"#;

#[test]
fn text_chunks_accumulate_and_the_last_one_closes_the_turn() {
    let events = parse_payloads(&[TWO_TEXTS, TEXT_AND_FINISH]);

    assert_eq!(
        summary(&events),
        vec![
            "TextDelta(Hello)",
            "TokenUsage(Some(10),Some(7),Some(4),None)",
            "TextDelta( world)",
            "TextDone",
            "MessageEnd(Some(\"STOP\"))",
        ]
    );
}

#[test]
fn a_thought_part_is_a_thinking_run_closed_before_the_text() {
    let thought = r#"{"candidates":[{"content":{"parts":[{"text":"pondering","thought":true}]}}]}"#;
    let answer = r#"{"candidates":[{"content":{"parts":[{"text":"answer"}]},"finishReason":"STOP"}]}"#;
    let events = parse_payloads(&[thought, answer]);

    assert_eq!(
        summary(&events),
        vec![
            "ThinkingStart",
            "ThinkingDelta(pondering)",
            "ThinkingEnd",
            "ThinkingDone",
            "TextDelta(answer)",
            "TextDone",
            "MessageEnd(Some(\"STOP\"))",
        ]
    );
}

#[test]
fn a_call_reports_its_id_signature_arguments_and_end_in_order() {
    let chunk = r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read","args":{"path":"a.txt"}},"thoughtSignature":"sig"}]},"finishReason":"STOP"}]}"#;
    let events = parse_payloads(&[chunk]);
    let summary = summary(&events);

    assert_eq!(summary.len(), 5, "{summary:?}");
    let id = match &events[0] {
        StreamEvent::ToolUseStart { id, name } => {
            assert_eq!(name, "read");
            assert!(id.starts_with(SYNTHETIC_CALL_ID_PREFIX), "{id}");
            id.clone()
        }
        other => panic!("expected a tool start, got {other:?}"),
    };
    assert_eq!(
        summary,
        vec![
            format!("ToolUseStart({id},read)"),
            format!("ToolUseSignatureFor({id},sig)"),
            format!("ToolInputDeltaFor({id},{{\"path\":\"a.txt\"}})"),
            format!("ToolUseEndFor({id})"),
            "MessageEnd(Some(\"STOP\"))".to_string(),
        ]
    );
}

#[test]
fn two_calls_in_one_chunk_get_two_different_ids() {
    let chunk = r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read","args":{}}},{"functionCall":{"name":"write","args":{}}}]},"finishReason":"STOP"}]}"#;
    let events = parse_payloads(&[chunk]);

    let ids: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ToolUseStart { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), 2, "{events:?}");
    assert_ne!(ids[0], ids[1]);
    assert!(ids.iter().all(|id| id.starts_with(SYNTHETIC_CALL_ID_PREFIX)), "{ids:?}");
}

#[test]
fn text_already_open_is_closed_before_a_call_starts() {
    let chunk = r#"{"candidates":[{"content":{"parts":[{"text":"calling"},{"functionCall":{"name":"read","args":{}}}]},"finishReason":"STOP"}]}"#;
    let events = parse_payloads(&[chunk]);
    let summary = summary(&events);

    assert_eq!(summary.len(), 6, "{summary:?}");
    assert_eq!(summary[0], "TextDelta(calling)");
    assert_eq!(summary[1], "TextDone");
    assert!(summary[2].starts_with("ToolUseStart("), "{summary:?}");
}

#[test]
fn a_server_id_is_sanitized_and_a_missing_args_object_becomes_empty() {
    let chunk = r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read","id":"fc.1"}}]},"finishReason":"STOP"}]}"#;
    let events = parse_payloads(&[chunk]);
    let summary = summary(&events);

    assert_eq!(
        summary,
        vec![
            "ToolUseStart(fc_1,read)".to_string(),
            "ToolInputDeltaFor(fc_1,{})".to_string(),
            "ToolUseEndFor(fc_1)".to_string(),
            "MessageEnd(Some(\"STOP\"))".to_string(),
        ]
    );
    assert!(!summary.iter().any(|label| label.contains(SYNTHETIC_CALL_ID_PREFIX)));
}

#[test]
fn a_synthesized_id_has_the_prefix_hex_stamp_and_a_counter() {
    let mut parser = GeminiParser::new();
    let events = parse_one(
        &mut parser,
        r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read","args":{}}},{"functionCall":{"name":"write","args":{}}}]}}]}"#,
    );
    let ids: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ToolUseStart { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(ids.len(), 2, "{events:?}");
    for id in &ids {
        let rest = id.strip_prefix(SYNTHETIC_CALL_ID_PREFIX).expect("the prefix");
        let (stamp, counter) = rest.rsplit_once('_').expect("a stamp and a counter");
        assert!(
            !stamp.is_empty() && stamp.chars().all(|c| c.is_ascii_hexdigit()),
            "{id}"
        );
        assert!(counter.parse::<u32>().is_ok(), "{id}");
    }
    assert!(ids[0].ends_with("_0"), "{ids:?}");
    assert!(ids[1].ends_with("_1"), "{ids:?}");

    // A second parser counts from zero again, so only the stamp separates the
    // two streams. Two creations inside one clock tick may share a stamp and are
    // then identical, which is why the format is what this asserts; a tie is not
    // a failure the streams can observe, but a differently shaped id would be.
    let mut second = GeminiParser::new();
    let other = parse_one(
        &mut second,
        r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read","args":{}}}]}}]}"#,
    );
    let other_id = match other.first() {
        Some(StreamEvent::ToolUseStart { id, .. }) => id.clone(),
        other => panic!("expected a tool start, got {other:?}"),
    };
    let rest = other_id.strip_prefix(SYNTHETIC_CALL_ID_PREFIX).expect("the prefix");
    let (stamp, counter) = rest.rsplit_once('_').expect("a stamp and a counter");
    assert!(
        !stamp.is_empty() && stamp.chars().all(|c| c.is_ascii_hexdigit()),
        "{other_id}"
    );
    assert_eq!(counter, "0", "{other_id}");
    assert!(other_id.starts_with(SYNTHETIC_CALL_ID_PREFIX), "{other_id}");
}

#[test]
fn the_events_of_a_call_round_trip_into_the_next_request() {
    let chunk = r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read","args":{"path":"a.txt"}},"thoughtSignature":"sig"}]},"finishReason":"STOP"}]}"#;
    let events = parse_payloads(&[chunk]);

    let mut id = String::new();
    let mut name = String::new();
    let mut input = Value::Null;
    let mut signature = None;
    for event in &events {
        match event {
            StreamEvent::ToolUseStart {
                id: started,
                name: started_name,
            } => {
                id = started.clone();
                name = started_name.clone();
            }
            StreamEvent::ToolInputDeltaFor { delta, .. } => {
                input = serde_json::from_str(delta).expect("the delta is the whole argument object");
            }
            StreamEvent::ToolUseSignatureFor { signature: found, .. } => signature = Some(found.clone()),
            _ => {}
        }
    }
    assert!(id.starts_with(SYNTHETIC_CALL_ID_PREFIX), "{id}");
    assert_eq!(signature.as_deref(), Some("sig"));

    let messages = [
        user(vec![text("read it")]),
        assistant(vec![ContentBlock::ToolUse {
            id: id.clone(),
            name,
            input,
            thought_signature: signature,
        }]),
        user(vec![result(&id, "contents", None)]),
    ];
    let request = body("gemini-3-pro", &messages, &[], "", None);
    let part = &request["contents"][1]["parts"][0];

    assert_eq!(part["thoughtSignature"], json!("sig"));
    assert!(
        part["functionCall"].get("id").is_none(),
        "an id this wire invented is not the server's to match: {part}"
    );
}

#[test]
fn usage_arrives_field_by_field_and_only_when_present() {
    let only_prompt = r#"{"usageMetadata":{"promptTokenCount":7},"candidates":[{"content":{"parts":[]}}]}"#;
    let nothing = r#"{"candidates":[{"content":{"parts":[{"text":"x"}]},"finishReason":"STOP"}]}"#;
    let events = parse_payloads(&[only_prompt, nothing]);

    assert_eq!(
        summary(&events),
        vec![
            "TokenUsage(Some(7),None,None,None)",
            "TextDelta(x)",
            "TextDone",
            "MessageEnd(Some(\"STOP\"))",
        ]
    );
}

// ---------------------------------------------------------------------------
// Parser: failures and guardrails

#[test]
fn a_server_error_is_terminal_and_later_payloads_are_ignored() {
    let error = r#"{"error":{"code":429,"message":"quota","status":"RESOURCE_EXHAUSTED"}}"#;
    let events = parse_payloads(&[error, TWO_TEXTS]);

    assert_eq!(
        summary(&events),
        vec!["Error(Gemini-compatible stream returned an error: RESOURCE_EXHAUSTED: quota,None)".to_string()]
    );
}

#[test]
fn a_body_that_stops_before_a_finish_reason_is_truncated() {
    let events = parse_payloads(&[TWO_TEXTS]);

    assert_eq!(
        summary(&events),
        vec![
            "TextDelta(Hello)",
            "TextDone",
            "Error(Gemini-compatible stream ended before the response finished (no finishReason),None)",
        ]
    );
}

#[test]
fn a_safety_finish_becomes_the_stop_reason_without_an_error() {
    for reason in ["SAFETY", "MAX_TOKENS"] {
        let chunk = format!(r#"{{"candidates":[{{"finishReason":"{reason}"}}]}}"#);
        let events = parse_payloads(&[&chunk]);
        assert_eq!(
            summary(&events),
            vec![format!("MessageEnd(Some(\"{reason}\"))")],
            "reason {reason}"
        );
    }
}

#[test]
fn a_blocked_prompt_reports_its_block_reason_as_the_stop_reason() {
    let blocked = r#"{"promptFeedback":{"blockReason":"SAFETY"}}"#;
    let events = parse_payloads(&[blocked]);

    assert_eq!(summary(&events), vec!["MessageEnd(Some(\"SAFETY\"))".to_string()]);
}

#[test]
fn a_malformed_chunk_reports_once_and_the_stream_continues() {
    let events = parse_payloads(&["{\"candidates\": [", TEXT_AND_FINISH]);

    let summary = summary(&events);
    assert!(
        summary[0].starts_with("Error(Gemini-compatible stream sent a malformed JSON chunk:"),
        "{summary:?}"
    );
    assert_eq!(
        summary[1..].to_vec(),
        vec![
            "TokenUsage(Some(10),Some(7),Some(4),None)",
            "TextDelta( world)",
            "TextDone",
            "MessageEnd(Some(\"STOP\"))",
        ]
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, StreamEvent::Error { .. }))
            .count(),
        1,
        "{summary:?}"
    );
}

#[test]
fn an_array_payload_is_handled_like_its_elements() {
    let array = format!("[{TWO_TEXTS},{TEXT_AND_FINISH}]");
    let joined = parse_payloads(&[&array]);
    let separately = parse_payloads(&[TWO_TEXTS, TEXT_AND_FINISH]);

    assert_eq!(summary(&joined), summary(&separately));
    assert_eq!(summary(&joined).first().map(String::as_str), Some("TextDelta(Hello)"));
}

#[test]
fn the_unspecified_finish_reason_does_not_count_as_finished() {
    let chunk = r#"{"candidates":[{"finishReason":"FINISH_REASON_UNSPECIFIED"}]}"#;
    let events = parse_payloads(&[chunk]);

    assert_eq!(
        summary(&events),
        vec!["Error(Gemini-compatible stream ended before the response finished (no finishReason),None)"]
    );
}

// ---------------------------------------------------------------------------
// Parser: through the shared SSE stream

type Chunks = Vec<Result<bytes::Bytes, reqwest::Error>>;

fn ok(text: &str) -> Chunks {
    vec![Ok(bytes::Bytes::from(text.to_string()))]
}

#[tokio::test]
async fn a_key_quoted_in_a_server_error_is_redacted() {
    let error = json!({
        "error": { "code": 401, "message": format!("bad key {TEST_KEY}"), "status": "UNAUTHENTICATED" }
    });
    let server = WireSseStream::new(
        futures::stream::iter(ok(&sse_frame(error))),
        GeminiParser::new(),
        "Gemini-compatible",
        Some(Arc::from(TEST_KEY)),
    );
    let body = Box::pin(server);
    let mut stream: horon_agent_provider_core::EventStream = body;
    let events = crate::test_support::collect(&mut stream).await;

    assert_eq!(events.len(), 1, "{events:?}");
    match &events[0] {
        StreamEvent::Error { message, .. } => {
            assert!(message.contains(REDACTED), "{message}");
            assert!(!message.contains(TEST_KEY), "the credential leaked into {message}");
            assert!(message.contains("UNAUTHENTICATED"), "{message}");
        }
        other => panic!("expected one error event, got {other:?}"),
    }
}

#[tokio::test]
async fn a_complete_body_through_the_shared_stream_closes_with_a_message_end() {
    let body = format!(
        "{}{}",
        sse_frame(json_from(TWO_TEXTS)),
        sse_frame(json_from(TEXT_AND_FINISH))
    );
    let stream = WireSseStream::new(
        futures::stream::iter(ok(&body)),
        GeminiParser::new(),
        "Gemini-compatible",
        None,
    );
    let mut stream: horon_agent_provider_core::EventStream = Box::pin(stream);
    let events = crate::test_support::collect(&mut stream).await;

    assert_eq!(
        summary(&events),
        vec![
            "TextDelta(Hello)",
            "TokenUsage(Some(10),Some(7),Some(4),None)",
            "TextDelta( world)",
            "TextDone",
            "MessageEnd(Some(\"STOP\"))",
        ]
    );
}

/// The literal payload as a JSON value, so a frame can be written with the same
/// escaping the server uses.
fn json_from(payload: &str) -> Value {
    serde_json::from_str(payload).expect("the fixture is valid JSON")
}
