//! Tests for the direct Chat Completions provider.
//!
//! Every network test drives a real loopback HTTP server and runs under a
//! deadline, so a stalled stream fails the test instead of hanging the suite.

use super::*;
use crate::chat::request::{
    build_chat_completions_request, build_chat_completions_request_reporting, responses_input_to_chat_messages,
};
use crate::chat::stream::{ChatStreamState, parse_chat_chunk};
use crate::chat::transport::{HttpErrorKind, classify_http_status, http_response_error};
use crate::test_support::{DEADLINE, MockServer, Reply, TEST_KEY, collect, sse_frame};
use horon_agent_message_types::degradation::{DegradationKind, DegradationReceipt};
use horon_agent_message_types::{ContentBlock, Message, Role, StreamEvent, ToolDefinition};
use horon_agent_provider_core::Provider;
use serde_json::{Value, json};
use std::collections::VecDeque;

// ---------------------------------------------------------------------------
// Helpers

pub(super) fn provider(url: &str) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(url, ChatAuth::Bearer, Some(TEST_KEY.to_string()), "test-model", vec![])
        .expect("provider builds")
}

fn parse_all(chunks: &[&str], api_key: Option<&str>) -> Vec<StreamEvent> {
    let mut state = ChatStreamState::default();
    let mut pending = VecDeque::new();
    let mut events = Vec::new();
    for chunk in chunks {
        if let Some(event) = parse_chat_chunk(chunk, &mut state, &mut pending, api_key) {
            events.push(event);
        }
        events.extend(pending.drain(..));
    }
    events
}

fn tool_use_message() -> Message {
    Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Text {
                text: "Checking the file.".to_string(),
                cache_control: None,
            },
            ContentBlock::ToolUse {
                id: "call_1".to_string(),
                name: "read".to_string(),
                input: json!({ "file_path": "a.txt" }),
                thought_signature: None,
            },
        ],
        timestamp: None,
        tool_duration_ms: None,
    }
}

// ---------------------------------------------------------------------------
// (a) Responses input -> chat messages

#[test]
fn responses_messages_become_chat_messages() {
    let messages = vec![
        Message::user("hello"),
        tool_use_message(),
        Message::tool_result("call_1", "file body", false),
        Message::user("thanks"),
    ];

    let body = build_chat_completions_request("test-model", &messages, &[], "You are helpful.", Some("high"));
    let chat = body["messages"].as_array().expect("messages array");

    assert_eq!(chat[0], json!({ "role": "system", "content": "You are helpful." }));
    assert_eq!(chat[1], json!({ "role": "user", "content": "hello" }));
    assert_eq!(chat[2]["role"], json!("assistant"));
    assert_eq!(chat[2]["content"], json!("Checking the file."));
    assert_eq!(chat[2]["tool_calls"][0]["id"], json!("call_1"));
    assert_eq!(chat[2]["tool_calls"][0]["type"], json!("function"));
    assert_eq!(chat[2]["tool_calls"][0]["function"]["name"], json!("read"));
    assert_eq!(
        chat[2]["tool_calls"][0]["function"]["arguments"],
        json!("{\"file_path\":\"a.txt\"}")
    );
    assert_eq!(
        chat[3],
        json!({ "role": "tool", "tool_call_id": "call_1", "content": "file body" })
    );
    assert_eq!(chat[4], json!({ "role": "user", "content": "thanks" }));

    assert_eq!(body["stream"], json!(true));
    assert_eq!(body["stream_options"]["include_usage"], json!(true));
    assert_eq!(body["reasoning_effort"], json!("high"));
    assert!(body.get("tools").is_none(), "no tools were declared");
}

#[test]
fn parallel_tool_calls_stay_one_assistant_message() {
    let call = |id: &str, command: &str| ContentBlock::ToolUse {
        id: id.to_string(),
        name: "bash".to_string(),
        input: json!({ "command": command }),
        thought_signature: None,
    };
    let messages = vec![
        Message::user("run both"),
        Message {
            role: Role::Assistant,
            content: vec![call("call_a", "echo one"), call("call_b", "echo two")],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message::tool_result("call_a", "one", false),
        Message::tool_result("call_b", "two", false),
    ];

    let body = build_chat_completions_request("m", &messages, &[], "", None);
    let chat = body["messages"].as_array().expect("messages array");

    let roles: Vec<&str> = chat.iter().filter_map(|message| message["role"].as_str()).collect();
    assert_eq!(roles, ["user", "assistant", "tool", "tool"], "{body}");
    let ids: Vec<&str> = chat[1]["tool_calls"]
        .as_array()
        .expect("tool calls")
        .iter()
        .filter_map(|call| call["id"].as_str())
        .collect();
    assert_eq!(ids, ["call_a", "call_b"]);
    assert_eq!(chat[2]["tool_call_id"], json!("call_a"));
    assert_eq!(chat[3]["tool_call_id"], json!("call_b"));
}

#[test]
fn parallel_tool_calls_stay_one_message_when_a_result_is_missing() {
    let call = |id: &str| ContentBlock::ToolUse {
        id: id.to_string(),
        name: "bash".to_string(),
        input: json!({ "command": "true" }),
        thought_signature: None,
    };
    let messages = vec![
        Message::user("run both"),
        Message {
            role: Role::Assistant,
            content: vec![call("call_a"), call("call_b")],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message::tool_result("call_b", "two", false),
    ];

    let body = build_chat_completions_request("m", &messages, &[], "", None);
    let chat = body["messages"].as_array().expect("messages array");

    let roles: Vec<&str> = chat.iter().filter_map(|message| message["role"].as_str()).collect();
    assert_eq!(roles, ["user", "assistant", "tool", "tool"], "{body}");
    assert_eq!(chat[1]["tool_calls"].as_array().map(Vec::len), Some(2));
    assert_eq!(chat[2]["tool_call_id"], json!("call_a"));
    assert_eq!(chat[3]["tool_call_id"], json!("call_b"));
    assert_eq!(chat[3]["content"], json!("two"));
}

#[test]
fn failed_tool_result_keeps_its_error_marker() {
    let messages = vec![tool_use_message(), Message::tool_result("call_1", "boom", true)];
    let body = build_chat_completions_request("m", &messages, &[], "", None);
    let chat = body["messages"].as_array().expect("messages array");
    assert_eq!(chat[1]["role"], json!("tool"));
    assert_eq!(chat[1]["content"], json!("[Error] boom"));
}

#[test]
fn assistant_text_and_tool_calls_share_one_message() {
    let input = vec![
        json!({ "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "working" }] }),
        json!({ "type": "function_call", "name": "read", "arguments": "{}", "call_id": "call_9" }),
    ];
    let chat = responses_input_to_chat_messages(&input);
    assert_eq!(chat.len(), 1);
    assert_eq!(chat[0]["content"], json!("working"));
    assert_eq!(chat[0]["tool_calls"][0]["id"], json!("call_9"));
}

#[test]
fn user_images_become_chat_image_parts() {
    let messages = vec![Message::user_with_images(
        "look",
        vec![("image/png".to_string(), "AAAA".to_string())],
    )];
    let body = build_chat_completions_request("m", &messages, &[], "", None);
    let content = &body["messages"][0]["content"];
    assert_eq!(content[0]["type"], json!("image_url"));
    assert_eq!(content[0]["image_url"]["url"], json!("data:image/png;base64,AAAA"));
    assert_eq!(content[1]["type"], json!("text"));
    assert_eq!(content[1]["text"], json!("look"));
}

#[test]
fn user_audio_becomes_input_audio_parts() {
    let mut message = Message::user("listen");
    message.content.insert(
        0,
        ContentBlock::Audio {
            format: "wav".to_string(),
            data: "AAAA".to_string(),
        },
    );
    let messages = vec![message];

    let chat = build_chat_completions_request("m", &messages, &[], "", None);
    let content = &chat["messages"][0]["content"];
    assert_eq!(
        content[0],
        json!({ "type": "input_audio", "input_audio": { "data": "AAAA", "format": "wav" } })
    );
    assert_eq!(content[1], json!({ "type": "text", "text": "listen" }));

    let responses = crate::request::build_responses_input(&messages);
    assert_eq!(
        responses[0]["content"][0],
        json!({ "type": "input_audio", "input_audio": { "data": "AAAA", "format": "wav" } })
    );
}

// ---------------------------------------------------------------------------
// (b) tools -> chat tools[].function

#[test]
fn tools_become_chat_function_definitions() {
    let tools = vec![ToolDefinition::new(
        "read",
        "Read a file",
        json!({
            "type": "object",
            "properties": { "file_path": { "type": "string" } },
            "required": ["file_path"]
        }),
    )];

    let body = build_chat_completions_request("test-model", &[], &tools, "", None);
    let definitions = body["tools"].as_array().expect("tools array");
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0]["type"], json!("function"));
    assert_eq!(definitions[0]["function"]["name"], json!("read"));
    assert_eq!(definitions[0]["function"]["description"], json!("Read a file"));
    assert_eq!(
        definitions[0]["function"]["parameters"]["properties"]["file_path"]["type"],
        json!("string")
    );
    assert_eq!(body["tool_choice"], json!("auto"));
}

#[test]
fn deferred_tool_definitions_are_not_sent_to_a_chat_endpoint() {
    let tools = vec![
        ToolDefinition::new("read", "Read", json!({ "type": "object", "properties": {} })),
        ToolDefinition::new("later", "Deferred", json!({ "type": "object", "properties": {} })).deferred(),
    ];
    let body = build_chat_completions_request("m", &[], &tools, "", None);
    let names: Vec<&str> = body["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|tool| tool["function"]["name"].as_str().expect("name"))
        .collect();
    assert_eq!(names, vec!["read"]);
}

// ---------------------------------------------------------------------------
// (b') one function-parameters root for a multi-action tool (#596)

/// The schema shape a multi-action tool such as `swarm` declares: a root object
/// whose per-action requirement ("`label` only for `spawn`") lives in a root
/// `anyOf`, because the root `required` array applies to every action.
///
/// It is otherwise strict-eligible — closed object, every property typed,
/// `additionalProperties: false` — so a `strict: false` below can only come from
/// collapsing the alternatives.
fn multi_action_tool() -> ToolDefinition {
    ToolDefinition::new(
        "swarm",
        "Run one swarm action",
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["spawn", "list", "message"],
                    "description": "what to do"
                },
                "to_session": { "type": "string", "description": "who to address" },
                "label": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Required for spawn"
                },
                "filter": { "anyOf": [{ "type": "string" }, { "type": "integer" }] }
            },
            "required": ["action"],
            "anyOf": [
                {
                    "type": "object",
                    "required": ["action", "label"],
                    "properties": {
                        "action": { "type": "string", "enum": ["spawn"] },
                        "label": { "type": "string", "minLength": 1 },
                        "mode": { "type": "string", "enum": ["fast"] }
                    }
                },
                {
                    "type": "object",
                    "required": ["action"],
                    "properties": {
                        "action": { "type": "string", "enum": ["list", "message"] },
                        "mode": { "type": "string", "enum": ["safe"] }
                    },
                    "anyOf": [
                        {"properties": {"query": {"type": "string"}}},
                        {"properties": {"limit": {"type": "integer"}}}
                    ]
                }
            ]
        }),
    )
}

#[test]
fn a_root_alternative_becomes_one_object_schema_for_chat_tools() {
    let tools = [multi_action_tool()];

    let body = build_chat_completions_request("m", &[], &tools, "", None);
    let function = &body["tools"][0]["function"];
    let parameters = &function["parameters"];

    // A root combiner is not part of the function-parameters contract.
    assert!(parameters.get("anyOf").is_none(), "{parameters}");
    assert!(parameters.get("oneOf").is_none(), "{parameters}");
    assert_eq!(parameters["type"], json!("object"));
    // The requirement every alternative shares stays; the spawn-only one is not
    // forced onto the other actions.
    assert_eq!(parameters["required"], json!(["action"]));

    // Every alternative's parameters are still advertised, and a shared name
    // keeps the root description.
    for name in ["action", "to_session", "label", "mode", "query", "limit"] {
        assert!(
            parameters["properties"].get(name).is_some(),
            "`{name}` was dropped: {parameters}"
        );
    }
    assert_eq!(
        parameters["properties"]["label"]["description"],
        json!("Required for spawn")
    );
    assert_eq!(parameters["properties"]["label"]["minLength"], json!(1));

    // The same property declared differently by different alternatives keeps
    // each alternative's own schema instead of picking one.
    let alternatives = parameters["properties"]["mode"]["anyOf"]
        .as_array()
        .expect("mode alternatives");
    let mut declared_values: Vec<Value> = Vec::new();
    for alternative in alternatives {
        declared_values.extend(alternative["enum"].as_array().cloned().unwrap_or_default());
    }
    for value in ["fast", "safe"] {
        assert!(
            declared_values.contains(&json!(value)),
            "`{value}` was dropped from the alternatives: {parameters}"
        );
    }

    // Only the root is widened; a nested alternative stays as declared.
    assert_eq!(
        parameters["properties"]["filter"]["anyOf"],
        json!([{ "type": "string" }, { "type": "integer" }])
    );

    // Collapsing alternatives widens what a call may omit, so structured output
    // is not guaranteed for this tool.
    assert_eq!(function["strict"], json!(false), "{function}");
}

#[test]
fn a_widened_root_alternative_is_reported_as_a_dropped_combiner() {
    let tools = [multi_action_tool()];
    let mut receipts: Vec<DegradationReceipt> = Vec::new();

    let reported = build_chat_completions_request_reporting("m", &[], &tools, "", None, Some(&mut receipts));

    assert!(
        receipts.contains(&DegradationReceipt::new(
            DegradationKind::SchemaKeywordDropped,
            "swarm",
            "anyOf"
        )),
        "{receipts:?}"
    );
    assert!(
        receipts.contains(&DegradationReceipt::new(
            DegradationKind::StrictDisabled,
            "swarm",
            "strict"
        )),
        "{receipts:?}"
    );
    // Reporting never changes what is sent.
    assert_eq!(reported, build_chat_completions_request("m", &[], &tools, "", None));
}

#[test]
fn a_tool_without_root_alternatives_keeps_its_declaration() {
    // The boundary the collapse must not cross: a schema with local `$ref`/`allOf`
    // and no root combiner is sent exactly as the dialect normalized it.
    let tools = [ToolDefinition::new(
        "lookup",
        "Look something up",
        json!({
            "type": "object",
            "$defs": { "name": { "type": "string" } },
            "properties": { "name": { "$ref": "#/$defs/name" } },
            "required": ["name"]
        }),
    )];

    let body = build_chat_completions_request("m", &[], &tools, "", None);
    let function = &body["tools"][0]["function"];
    let parameters = &function["parameters"];

    assert_eq!(function["strict"], json!(true), "{function}");
    assert_eq!(parameters["properties"]["name"]["type"], json!("string"));
    assert_eq!(parameters["required"], json!(["name"]));
}

// ---------------------------------------------------------------------------
// (c) chunk sequence -> StreamEvent sequence

#[test]
fn streams_text_reasoning_tool_calls_and_usage_in_order() {
    let chunks = [
        r#"{"id":"c1","choices":[{"index":0,"delta":{"role":"assistant"}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"reasoning_content":"Need a tool. "}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":"Let me check."}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read","arguments":"{\"file"}}]}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"_path\":\"a.txt\"}"}}]}}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15,"prompt_tokens_details":{"cached_tokens":4}}}"#,
        "[DONE]",
    ];
    let events = parse_all(&chunks, None);

    let mut index = 0;
    let mut next = |expected: &str| {
        let event = events
            .get(index)
            .unwrap_or_else(|| panic!("missing {expected}; got {events:?}"));
        index += 1;
        event
    };
    assert!(matches!(next("ThinkingStart"), StreamEvent::ThinkingStart));
    assert!(matches!(next("ThinkingDelta"), StreamEvent::ThinkingDelta(text) if text == "Need a tool. "));
    assert!(matches!(next("ThinkingEnd"), StreamEvent::ThinkingEnd));
    assert!(matches!(next("ThinkingDone"), StreamEvent::ThinkingDone { .. }));
    assert!(matches!(next("TextDelta"), StreamEvent::TextDelta(text) if text == "Let me check."));
    assert!(matches!(next("TextDone"), StreamEvent::TextDone));
    assert!(matches!(next("ToolUseStart"), StreamEvent::ToolUseStart { id, name } if id == "call_1" && name == "read"));
    assert!(
        matches!(next("ToolInputDeltaFor"), StreamEvent::ToolInputDeltaFor { id, delta }
            if id == "call_1" && delta == "{\"file")
    );
    assert!(
        matches!(next("ToolInputDeltaFor"), StreamEvent::ToolInputDeltaFor { id, delta }
            if id == "call_1" && delta == "_path\":\"a.txt\"}")
    );
    assert!(matches!(next("ToolUseEndFor"), StreamEvent::ToolUseEndFor { id } if id == "call_1"));
    assert!(matches!(
        next("TokenUsage"),
        StreamEvent::TokenUsage {
            input_tokens: Some(10),
            output_tokens: Some(5),
            cache_read_input_tokens: Some(4),
            cache_creation_input_tokens: None,
        }
    ));
    assert!(
        matches!(next("MessageEnd"), StreamEvent::MessageEnd { stop_reason } if stop_reason.as_deref() == Some("tool_calls"))
    );
    assert_eq!(index, events.len(), "no extra events: {events:?}");
}

// ---------------------------------------------------------------------------
// (d) HTTP failures are classified

#[test]
fn failed_statuses_are_classified() {
    assert_eq!(classify_http_status(401), HttpErrorKind::Authentication);
    assert_eq!(classify_http_status(403), HttpErrorKind::Authentication);
    assert_eq!(classify_http_status(429), HttpErrorKind::RateLimited);
    assert_eq!(classify_http_status(500), HttpErrorKind::Server);
    assert_eq!(classify_http_status(404), HttpErrorKind::Client);

    let unauthorized = http_response_error("http://x/v1/chat/completions", 401, None, "bad key", Some(TEST_KEY));
    assert!(unauthorized.to_string().contains("HTTP 401"));
    assert!(unauthorized.to_string().contains("authentication failed"));
    let limited = http_response_error("http://x/v1/chat/completions", 429, None, "slow down", Some(TEST_KEY));
    assert!(limited.to_string().contains("rate limit"));
    let server = http_response_error("http://x/v1/chat/completions", 500, None, "boom", Some(TEST_KEY));
    assert!(server.to_string().contains("HTTP 500"));
    assert!(server.to_string().contains("server error"));
}

#[tokio::test]
async fn http_failures_surface_as_errors_without_the_key() {
    tokio::time::timeout(DEADLINE, async {
        for (status, marker) in [
            (401, "authentication failed"),
            (429, "rate limit"),
            (500, "server error"),
        ] {
            let server = MockServer::start(Reply::status(
                status,
                &format!("upstream rejected the request with the key {TEST_KEY}"),
            ));
            let provider = provider(&format!("{}/v1", server.url));
            let error = provider
                .complete(&[], &[], "system", None)
                .await
                .err()
                .expect("an HTTP failure is an error");
            let message = error.to_string();
            assert!(message.contains(&format!("HTTP {status}")), "{message}");
            assert!(message.contains(marker), "{message}");
            assert!(!message.contains(TEST_KEY), "the credential leaked into {message}");
            assert!(message.contains("[redacted]"), "{message}");
        }
    })
    .await
    .expect("HTTP failure checks finish within the deadline");
}

// ---------------------------------------------------------------------------
// (e) malformed input becomes an error event, never a panic

#[test]
fn malformed_chunk_yields_error_event() {
    let malformed = format!("{{\"choices\":[{{\"delta\":{{\"content\":\"{TEST_KEY}\"}}]");
    let chunks = [malformed.as_str(), "[DONE]"];
    let events = parse_all(&chunks, Some(TEST_KEY));

    assert_eq!(events.len(), 2, "{events:?}");
    match &events[0] {
        StreamEvent::Error {
            message,
            retry_after_secs,
        } => {
            assert!(message.contains("malformed JSON chunk"), "{message}");
            assert!(!message.contains(TEST_KEY), "the credential leaked into {message}");
            assert!(message.contains("[redacted]"), "{message}");
            assert_eq!(*retry_after_secs, None);
        }
        other => panic!("expected one error event, got {other:?}"),
    }
    assert!(
        matches!(&events[1], StreamEvent::MessageEnd { stop_reason: None }),
        "the stream keeps going after a bad frame: {events:?}"
    );
}

#[tokio::test]
async fn malformed_chunk_from_http_stream_yields_error_event() {
    tokio::time::timeout(DEADLINE, async {
        let broken = sse_frame(json!({ "choices": [{ "delta": { "content": "ok" } }] }));
        let malformed = "data: {\"choices\": [\n\n";
        let done = "data: [DONE]\n\n";
        let server = MockServer::start(Reply::sse(&[&broken, malformed, done]));
        let provider = provider(&format!("{}/v1", server.url));
        let mut stream = provider.complete(&[], &[], "system", None).await.expect("stream opens");
        let events = collect(&mut stream).await;

        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, StreamEvent::Error { .. }))
                .count(),
            1,
            "one malformed frame produces exactly one error event: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, StreamEvent::TextDelta(text) if text == "ok")),
            "valid frames before the bad one still arrive: {events:?}"
        );
        assert!(
            matches!(events.last(), Some(StreamEvent::MessageEnd { .. })),
            "the stream still completes: {events:?}"
        );
    })
    .await
    .expect("malformed stream checks finish within the deadline");
}

#[tokio::test]
async fn empty_body_yields_error_event() {
    tokio::time::timeout(DEADLINE, async {
        let server = MockServer::start(Reply::status(200, ""));
        let provider = provider(&format!("{}/v1", server.url));
        let mut stream = provider.complete(&[], &[], "system", None).await.expect("stream opens");
        let events = collect(&mut stream).await;
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(
            matches!(&events[0], StreamEvent::Error { message, .. } if message.contains("ended before the response finished")),
            "{events:?}"
        );
    })
    .await
    .expect("empty body checks finish within the deadline");
}

#[tokio::test]
async fn truncated_stream_yields_error_event() {
    tokio::time::timeout(DEADLINE, async {
        let partial = sse_frame(json!({ "choices": [{ "delta": { "content": "half" } }] }));
        let server = MockServer::start(Reply::sse(&[&partial]));
        let provider = provider(&format!("{}/v1", server.url));
        let mut stream = provider.complete(&[], &[], "system", None).await.expect("stream opens");
        let events = collect(&mut stream).await;

        assert!(
            events
                .iter()
                .any(|event| matches!(event, StreamEvent::TextDelta(text) if text == "half")),
            "{events:?}"
        );
        assert!(
            matches!(events.last(), Some(StreamEvent::Error { message, .. })
                if message.contains("ended before the response finished")),
            "a stream that stops before finish_reason is truncated, not complete: {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, StreamEvent::MessageEnd { .. })),
            "a truncated stream must not look complete: {events:?}"
        );
    })
    .await
    .expect("truncated stream checks finish within the deadline");
}

#[tokio::test]
async fn truncated_final_frame_without_terminator_yields_error() {
    tokio::time::timeout(DEADLINE, async {
        // The body ends inside the response: the last SSE frame has no trailing
        // blank line, carries a tool-call delta, and no finish_reason ever
        // arrives. The final frame must still be parsed, its tool call closed,
        // and the stream must end with the terminal truncation error.
        let unterminated = format!(
            "data: {}",
            json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_cut",
                            "type": "function",
                            "function": { "name": "read", "arguments": "{\"file" }
                        }]
                    }
                }]
            })
        );
        let server = MockServer::start(Reply::sse(&[&unterminated]));
        let provider = provider(&format!("{}/v1", server.url));
        let mut stream = provider.complete(&[], &[], "system", None).await.expect("stream opens");
        let events = collect(&mut stream).await;

        assert!(
            events.iter().any(
                |event| matches!(event, StreamEvent::ToolUseStart { id, name } if id == "call_cut" && name == "read")
            ),
            "the unterminated frame is still parsed: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, StreamEvent::ToolInputDeltaFor { id, delta }
                if id == "call_cut" && delta == "{\"file")),
            "{events:?}"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, StreamEvent::ToolUseEndFor { id } if id == "call_cut")),
            "the started tool call is closed before the terminal error: {events:?}"
        );
        assert!(
            matches!(events.last(), Some(StreamEvent::Error { message, .. })
                if message.contains("ended before the response finished")),
            "a body that ends inside the response must report truncation: {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, StreamEvent::MessageEnd { .. })),
            "a truncated stream must not look complete: {events:?}"
        );
    })
    .await
    .expect("truncated final-frame checks finish within the deadline");
}

// ---------------------------------------------------------------------------
// Happy path over the real transport

#[tokio::test]
async fn streams_tool_call_split_across_chunks() {
    tokio::time::timeout(DEADLINE, async {
        let first = sse_frame(json!({
            "id": "chatcmpl-1",
            "choices": [{
                "index": 0,
                "delta": {
                    "role": "assistant",
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_abc",
                        "type": "function",
                        "function": { "name": "read", "arguments": "{\"file_" }
                    }]
                }
            }]
        }));
        let second = sse_frame(json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{ "index": 0, "function": { "arguments": "path\":\"notes.md\"}" } }]
                }
            }]
        }));
        let finishing = sse_frame(json!({
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }]
        }));
        let usage = sse_frame(json!({
            "choices": [],
            "usage": { "prompt_tokens": 12, "completion_tokens": 4, "total_tokens": 16 }
        }));
        let done = "data: [DONE]\n\n";
        let server = MockServer::start(Reply::sse(&[&first, &second, &finishing, &usage, done]));

        let provider = provider(&format!("{}/v1", server.url));
        let mut stream = provider.complete(&[], &[], "system", None).await.expect("stream opens");
        let events = collect(&mut stream).await;

        let start = events
            .iter()
            .position(|event| matches!(event, StreamEvent::ToolUseStart { .. }))
            .expect("tool start");
        assert!(
            matches!(&events[start], StreamEvent::ToolUseStart { id, name } if id == "call_abc" && name == "read"),
            "{events:?}"
        );

        let fragments: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ToolInputDeltaFor { id, delta } if id == "call_abc" => Some(delta.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(fragments, vec!["{\"file_", "path\":\"notes.md\"}"], "{events:?}");
        assert_eq!(fragments.concat(), "{\"file_path\":\"notes.md\"}");

        assert!(
            events
                .iter()
                .any(|event| matches!(event, StreamEvent::ToolUseEndFor { id } if id == "call_abc")),
            "{events:?}"
        );
        assert!(
            events.iter().any(|event| matches!(
                event,
                StreamEvent::TokenUsage {
                    input_tokens: Some(12),
                    output_tokens: Some(4),
                    ..
                }
            )),
            "{events:?}"
        );
        assert!(
            matches!(events.last(), Some(StreamEvent::MessageEnd { stop_reason })
                if stop_reason.as_deref() == Some("tool_calls")),
            "{events:?}"
        );

        let requests = server.requests();
        assert_eq!(requests.len(), 1, "one streaming request");
        assert_eq!(requests[0].path, "/v1/chat/completions");
        let body: Value = serde_json::from_str(&requests[0].body).expect("request body is JSON");
        assert_eq!(body["model"], json!("test-model"));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["messages"][0], json!({ "role": "system", "content": "system" }));
    })
    .await
    .expect("tool-call streaming finishes within the deadline");
}

// ---------------------------------------------------------------------------
// Transport and provider state

#[tokio::test]
async fn auth_modes_send_the_expected_credential_header() {
    tokio::time::timeout(DEADLINE, async {
        for (auth, expected_header) in [
            (ChatAuth::Bearer, "authorization"),
            (
                ChatAuth::Header {
                    header: "api-key".to_string(),
                },
                "api-key",
            ),
            (ChatAuth::None, ""),
        ] {
            let server = MockServer::start(Reply::sse(&["data: [DONE]\n\n"]));
            let url = format!("{}/v1", server.url);
            let provider =
                OpenAiCompatibleProvider::new(url, auth, Some(TEST_KEY.to_string()), "m", vec![]).expect("provider");
            let mut stream = provider.complete(&[], &[], "system", None).await.expect("stream opens");
            let _ = collect(&mut stream).await;

            let requests = server.requests();
            let request = requests.first().expect("one request");
            match expected_header {
                "" => {
                    assert!(request.header("authorization").is_none(), "{request:?}");
                    assert!(request.header("api-key").is_none(), "{request:?}");
                }
                "authorization" => {
                    let expected = format!("Bearer {TEST_KEY}");
                    assert_eq!(request.header("authorization"), Some(expected.as_str()));
                }
                "api-key" => assert_eq!(request.header("api-key"), Some(TEST_KEY)),
                other => panic!("unexpected header {other}"),
            }
        }
    })
    .await
    .expect("auth header checks finish within the deadline");
}

#[tokio::test]
async fn extra_body_fields_override_generated_request_fields() {
    tokio::time::timeout(DEADLINE, async {
        let server = MockServer::start(Reply::sse(&["data: [DONE]\n\n"]));
        let provider = OpenAiCompatibleProvider::new(format!("{}/v1", server.url), ChatAuth::None, None, "m", vec![])
            .expect("provider")
            .with_extra_body(json!({ "chat_template_kwargs": { "thinking": true } }))
            .with_headers(vec![("x-profile".to_string(), "qa".to_string())]);

        let mut stream = provider.complete(&[], &[], "system", None).await.expect("stream opens");
        let _ = collect(&mut stream).await;

        let requests = server.requests();
        let request = requests.first().expect("one request");
        let body: Value = serde_json::from_str(&request.body).expect("request body is JSON");
        assert_eq!(body["chat_template_kwargs"]["thinking"], json!(true));
        assert_eq!(request.header("x-profile"), Some("qa"));
        assert!(request.header("authorization").is_none());
    })
    .await
    .expect("extra body checks finish within the deadline");
}

#[tokio::test]
async fn session_header_carries_each_request_session_id() {
    tokio::time::timeout(DEADLINE, async {
        let server = MockServer::start(Reply::sse(&["data: [DONE]\n\n"]));
        let provider = OpenAiCompatibleProvider::new(format!("{}/v1", server.url), ChatAuth::None, None, "m", vec![])
            .expect("provider")
            .with_session_header(Some(reqwest::header::HeaderName::from_static("x-session-affinity")));
        let forked = provider.fork();
        let context = |session: &str| ProviderRequestContext {
            session_id: Some(session.to_string()),
            cache_root_id: Some("shared-root".to_string()),
            ..Default::default()
        };

        for (runtime, session) in [
            (
                &provider as &dyn Provider,
                "session_otter_1795696332403_9a252fd9aabc0a1f",
            ),
            (forked.as_ref(), "session_badger_1795695929227_2cbdae737a3f6c2c"),
        ] {
            let mut stream = runtime
                .complete_with_context(&[Message::user("hi")], &[], "system", None, &context(session))
                .await
                .expect("stream opens");
            let _ = collect(&mut stream).await;
        }
        let mut plain = provider
            .complete(&[Message::user("hi")], &[], "system", None)
            .await
            .expect("stream opens");
        let _ = collect(&mut plain).await;

        // A UUIDv7 like Claude Code's and omp's session ids: the session's
        // creation time leads, and the same id always maps to the same UUID.
        let requests = server.requests();
        assert_eq!(
            requests[0].header("x-session-affinity"),
            Some("01a217d7-8e73-7b04-88b4-ad9122fa3265")
        );
        assert_eq!(
            requests[1].header("x-session-affinity"),
            Some("01a217d1-678b-7b5a-8e49-ec6ef495d79d")
        );
        assert!(requests[2].header("x-session-affinity").is_none());
    })
    .await
    .expect("session header checks finish within the deadline");
}

#[test]
fn endpoint_handles_both_base_url_shapes() {
    let base =
        OpenAiCompatibleProvider::new("http://127.0.0.1:9/v1/", ChatAuth::None, None, "m", vec![]).expect("provider");
    assert_eq!(base.endpoint(), "http://127.0.0.1:9/v1/chat/completions");

    let full = OpenAiCompatibleProvider::new(
        "https://api.example.com/v1/chat/completions",
        ChatAuth::None,
        None,
        "m",
        vec![],
    )
    .expect("provider");
    assert_eq!(full.endpoint(), "https://api.example.com/v1/chat/completions");
}

#[test]
fn construction_rejects_an_unusable_endpoint() {
    let missing_scheme = OpenAiCompatibleProvider::new("api.example.com/v1", ChatAuth::None, None, "m", vec![]);
    assert!(missing_scheme.is_err());
    let empty = OpenAiCompatibleProvider::new("   ", ChatAuth::None, None, "m", vec![]);
    assert!(empty.is_err());
}

#[test]
fn fork_keeps_model_and_settings_independent() {
    let parent = OpenAiCompatibleProvider::new(
        "http://127.0.0.1:9/v1",
        ChatAuth::Bearer,
        Some(TEST_KEY.to_string()),
        "alpha",
        vec!["alpha".to_string(), "beta".to_string()],
    )
    .expect("provider");
    let fork = parent.fork();

    fork.set_model("beta").expect("fork switches model");
    fork.set_reasoning_effort("high").expect("fork sets effort");

    assert_eq!(parent.model(), "alpha");
    assert_eq!(fork.model(), "beta");
    assert_eq!(parent.reasoning_effort(), None);
    assert_eq!(fork.reasoning_effort(), Some("high".to_string()));
    assert!(parent.set_model("  ").is_err(), "an empty model is refused");
    assert_eq!(fork.available_models_for_switching(), vec!["alpha", "beta"]);
}

#[test]
fn provider_identity_and_routes_describe_the_profile() {
    let provider = OpenAiCompatibleProvider::new(
        "http://127.0.0.1:9/v1",
        ChatAuth::None,
        None,
        "alpha",
        vec!["alpha".to_string()],
    )
    .expect("provider")
    .with_profile_id("my-gw")
    .with_display_name("My gateway");

    assert_eq!(provider.name(), "openai-compatible");
    assert_eq!(provider.display_name(), "My gateway");
    assert_eq!(provider.usage_is_inclusive(), Some(true));
    let routes = provider.model_routes();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].model, "alpha");
    assert_eq!(routes[0].detail, "my-gw");
    assert_eq!(routes[0].api_method, "chat_completions");
}

/// horon-agent owns summary compaction for this provider: it asks the same
/// Chat Completions model for a summary, so manual and proactive compaction
/// are available even though no native compaction artifact exists.
#[test]
fn summary_compaction_is_available_for_the_chat_provider() {
    let provider = OpenAiCompatibleProvider::new(
        "http://127.0.0.1:9/v1",
        ChatAuth::None,
        None,
        "alpha",
        vec!["alpha".to_string()],
    )
    .expect("provider");
    assert!(provider.supports_compaction());
    assert!(provider.uses_horon_agent_compaction());
}

#[tokio::test]
async fn split_completion_with_context_sends_the_key_and_plain_completion_does_not() {
    tokio::time::timeout(DEADLINE, async {
        let server = MockServer::start(Reply::sse(&["data: [DONE]\n\n"]));
        let provider = OpenAiCompatibleProvider::new(
            format!("{}/v1", server.url),
            ChatAuth::None,
            None,
            "m",
            vec!["m".to_string()],
        )
        .expect("provider")
        .with_prompt_cache_key_policy(Some(true));

        let mut stream = provider
            .complete_split_with_context(
                &[Message::user("hi")],
                &[super::prompt_cache_tests::prompt_tool()],
                "static",
                "dynamic",
                None,
                &super::prompt_cache_tests::cache_context("root-a"),
            )
            .await
            .expect("stream opens");
        let _ = collect(&mut stream).await;

        let mut plain = provider
            .complete_split(&[Message::user("hi")], &[], "static", "dynamic", None)
            .await
            .expect("stream opens");
        let _ = collect(&mut plain).await;

        let requests = server.requests();
        let keyed: Value = serde_json::from_str(&requests[0].body).expect("body is JSON");
        let unkeyed: Value = serde_json::from_str(&requests[1].body).expect("body is JSON");
        assert!(
            super::prompt_cache_tests::is_prompt_cache_key(&keyed["prompt_cache_key"]),
            "{keyed}"
        );
        assert!(unkeyed.get("prompt_cache_key").is_none(), "{unkeyed}");
        let folded = keyed["messages"].as_array().expect("messages").iter().any(|message| {
            message["content"]
                .as_str()
                .is_some_and(|content| content.contains("<system-reminder>\ndynamic\n</system-reminder>"))
        });
        assert!(
            folded,
            "the dynamic system part is folded into the conversation: {keyed}"
        );
    })
    .await
    .expect("prompt cache request checks finish within the deadline");
}

// ---------------------------------------------------------------------------
// Per-turn effort keyword override

/// A context override wins over the stored effort for exactly that request,
/// and the request body leaves the stored effort untouched.
#[test]
fn effort_keywords_context_override_wins_and_leaves_stored_effort() {
    let provider = openai_provider_for_effort("gpt-5.1");
    provider.set_reasoning_effort("medium").expect("set stored effort");
    assert_eq!(
        provider.reasoning_effort().as_deref(),
        Some("medium"),
        "stored effort before the request"
    );

    let context = horon_agent_provider_core::ProviderRequestContext {
        reasoning_effort: Some("high".to_string()),
        ..Default::default()
    };
    let body: Value =
        serde_json::from_slice(&provider.build_body(&[Message::user("ultrathink")], &[], "system", Some(&context)))
            .expect("body is JSON");

    assert_eq!(
        body.get("reasoning_effort").and_then(Value::as_str),
        Some("high"),
        "the per-call override reaches the wire"
    );
    assert_eq!(
        provider.reasoning_effort().as_deref(),
        Some("medium"),
        "the override never mutates the stored effort"
    );
}

/// With no context override the body keeps using the stored effort.
#[test]
fn effort_keywords_absent_context_override_uses_stored_effort() {
    let provider = openai_provider_for_effort("gpt-5.1");
    provider.set_reasoning_effort("low").expect("set stored effort");

    let context = horon_agent_provider_core::ProviderRequestContext {
        reasoning_effort: None,
        ..Default::default()
    };
    let body: Value =
        serde_json::from_slice(&provider.build_body(&[Message::user("hello")], &[], "system", Some(&context)))
            .expect("body is JSON");

    assert_eq!(body.get("reasoning_effort").and_then(Value::as_str), Some("low"));
}

fn openai_provider_for_effort(model: &str) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(
        "https://api.openai.com/v1",
        ChatAuth::Bearer,
        Some("sk-test-secret".to_string()),
        model,
        vec![model.to_string()],
    )
    .expect("provider")
}
