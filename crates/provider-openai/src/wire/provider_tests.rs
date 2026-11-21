//! End-to-end routing tests for the four wire shapes one provider speaks.
//!
//! Each test drives the real provider against a loopback HTTP fake, so the path
//! the request reached, the credential header it carried, the body it built, the
//! stream it parsed and the metadata it reports are all observed on the wire
//! rather than read back from a builder call.

use super::WireProtocol;
use crate::chat::OpenAiCompatibleProvider;
use crate::chat::transport::ChatAuth;
use crate::test_support::{DEADLINE, MockServer, Reply, TEST_KEY, collect, sse_frame};
use horon_agent_message_types::degradation::DegradationKind;
use horon_agent_message_types::{ContentBlock, Message, Role, StreamEvent, ToolDefinition};
use horon_agent_provider_core::anthropic_reasoning_caps;
use horon_agent_provider_core::{Provider, ProviderRequestContext};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Helpers

const ALL_WIRES: [WireProtocol; 4] = [
    WireProtocol::ChatCompletions,
    WireProtocol::Responses,
    WireProtocol::AnthropicMessages,
    WireProtocol::GeminiGenerateContent,
];

/// The credential header each endpoint documents for its own protocol.
fn auth_for(wire: WireProtocol) -> ChatAuth {
    match wire {
        WireProtocol::ChatCompletions | WireProtocol::Responses => ChatAuth::Bearer,
        WireProtocol::AnthropicMessages => ChatAuth::Header {
            header: "x-api-key".to_string(),
        },
        WireProtocol::GeminiGenerateContent => ChatAuth::Header {
            header: "x-goog-api-key".to_string(),
        },
    }
}

fn wire_provider(base_url: &str, wire: WireProtocol, model: &str) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(
        base_url,
        auth_for(wire),
        Some(TEST_KEY.to_string()),
        model,
        vec![model.to_string()],
    )
    .expect("provider builds")
    .with_wire(wire)
}

/// Concatenated text the model streamed, in arrival order.
fn streamed_text(events: &[StreamEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::TextDelta(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn recorded_body(server: &MockServer, index: usize) -> Value {
    let requests = server.requests();
    let request = requests
        .get(index)
        .unwrap_or_else(|| panic!("request {index} was not recorded"));
    serde_json::from_str(&request.body).expect("a recorded request body is JSON")
}

/// One `data:` frame per event of a small text answer, so every wire's stream is
/// exercised with its own protocol's shape.
fn text_stream_frames(wire: WireProtocol) -> Vec<String> {
    match wire {
        WireProtocol::ChatCompletions => vec![
            sse_frame(json!({ "choices": [{ "index": 0, "delta": { "content": "Hel" } }] })),
            sse_frame(json!({ "choices": [{ "index": 0, "delta": { "content": "lo" } }] })),
            "data: [DONE]\n\n".to_string(),
        ],
        WireProtocol::Responses => vec![
            sse_frame(json!({ "type": "response.output_text.delta", "delta": "Hel" })),
            sse_frame(json!({ "type": "response.output_text.delta", "delta": "lo" })),
            sse_frame(json!({
                "type": "response.completed",
                "response": { "status": "completed", "usage": { "input_tokens": 3, "output_tokens": 2 } }
            })),
        ],
        WireProtocol::AnthropicMessages => vec![
            sse_frame(json!({
                "type": "message_start",
                "message": { "usage": { "input_tokens": 3, "output_tokens": 0 } }
            })),
            sse_frame(json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "text", "text": "" }
            })),
            sse_frame(json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "text_delta", "text": "Hel" }
            })),
            sse_frame(json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "text_delta", "text": "lo" }
            })),
            sse_frame(json!({ "type": "content_block_stop", "index": 0 })),
            sse_frame(json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": { "output_tokens": 2 }
            })),
            sse_frame(json!({ "type": "message_stop" })),
        ],
        WireProtocol::GeminiGenerateContent => vec![
            sse_frame(json!({ "candidates": [{ "content": { "parts": [{ "text": "Hel" }] } }] })),
            sse_frame(json!({
                "candidates": [{
                    "content": { "parts": [{ "text": "lo" }] },
                    "finishReason": "STOP"
                }]
            })),
        ],
    }
}

/// A reply that parses cleanly to end of body for `wire`.
fn text_reply(wire: WireProtocol) -> Reply {
    let frames = text_stream_frames(wire);
    let parts: Vec<&str> = frames.iter().map(String::as_str).collect();
    Reply::sse(&parts)
}

/// A body that closes the stream without content, for tests that only care about
/// the request a wire built.
fn empty_reply(wire: WireProtocol) -> Reply {
    let frame = match wire {
        WireProtocol::ChatCompletions => "data: [DONE]\n\n".to_string(),
        WireProtocol::Responses => sse_frame(json!({ "type": "response.completed", "response": {} })),
        WireProtocol::AnthropicMessages => sse_frame(json!({ "type": "message_stop" })),
        WireProtocol::GeminiGenerateContent => sse_frame(json!({ "candidates": [{ "finishReason": "STOP" }] })),
    };
    Reply::sse(&[&frame])
}

#[tokio::test]
async fn service_tier_is_sent_on_both_openai_wires() -> anyhow::Result<()> {
    tokio::time::timeout(DEADLINE, async {
        for wire in [WireProtocol::ChatCompletions, WireProtocol::Responses] {
            let server = MockServer::start(empty_reply(wire));
            let provider = wire_provider(&server.url, wire, "gpt-6-astra");
            let messages = [Message::user("hello")];
            let mut stream = provider.complete(&messages, &[], "", None).await?;
            collect(&mut stream).await;
            assert!(recorded_body(&server, 0).get("service_tier").is_none());

            provider.set_service_tier("ultrafast")?;
            let mut stream = provider.complete(&messages, &[], "", None).await?;
            collect(&mut stream).await;
            assert_eq!(recorded_body(&server, 1)["service_tier"], "ultrafast");
        }
        Ok(())
    })
    .await?
}

#[test]
fn unsupported_service_tiers_are_rejected() {
    for wire in [WireProtocol::AnthropicMessages, WireProtocol::GeminiGenerateContent] {
        let provider = wire_provider("http://localhost", wire, "test-model");
        let error = provider.set_service_tier("priority").unwrap_err();
        assert!(error.to_string().contains("only supported on OpenAI"));
    }
    let provider = wire_provider("http://localhost", WireProtocol::Responses, "gpt-5");
    for tier in ["auto", "default", "flex", "ultrafast"] {
        let error = provider.set_service_tier(tier).unwrap_err();
        assert!(error.to_string().contains("unsupported service tier"));
    }
}

/// Every object in `value` whose `key` equals `expected`.
fn collect_matching<'a>(value: &'a Value, key: &str, expected: &str, out: &mut Vec<&'a Value>) {
    match value {
        Value::Object(map) => {
            if map.get(key).and_then(Value::as_str) == Some(expected) {
                out.push(value);
            }
            for child in map.values() {
                collect_matching(child, key, expected, out);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_matching(child, key, expected, out);
            }
        }
        _ => {}
    }
}

fn matching<'a>(value: &'a Value, key: &str, expected: &str) -> Vec<&'a Value> {
    let mut out = Vec::new();
    collect_matching(value, key, expected, &mut out);
    out
}

// ---------------------------------------------------------------------------
// (a) Request path, credential header and body shape, per wire

#[tokio::test]
async fn each_wire_posts_to_its_own_path_with_its_own_credential_header() {
    tokio::time::timeout(DEADLINE, async {
        for wire in ALL_WIRES {
            let server = MockServer::start(empty_reply(wire));
            let provider = wire_provider(&format!("{}/v1", server.url), wire, "g-test");
            let mut stream = provider
                .complete(&[Message::user("hi")], &[], "system", None)
                .await
                .unwrap_or_else(|error| panic!("{wire:?} stream opens: {error}"));
            let _ = collect(&mut stream).await;

            let requests = server.requests();
            let request = requests.first().unwrap_or_else(|| panic!("{wire:?} sent no request"));
            let expected_path = match wire {
                WireProtocol::ChatCompletions => "/v1/chat/completions",
                WireProtocol::Responses => "/v1/responses",
                WireProtocol::AnthropicMessages => "/v1/messages",
                WireProtocol::GeminiGenerateContent => "/v1/models/g-test:streamGenerateContent?alt=sse",
            };
            assert_eq!(request.path, expected_path, "{wire:?}");

            match wire {
                WireProtocol::ChatCompletions | WireProtocol::Responses => {
                    assert_eq!(
                        request.header("authorization"),
                        Some(format!("Bearer {TEST_KEY}").as_str()),
                        "{wire:?}"
                    );
                }
                WireProtocol::AnthropicMessages => {
                    assert_eq!(request.header("x-api-key"), Some(TEST_KEY), "{wire:?}");
                    assert!(request.header("authorization").is_none(), "{wire:?}");
                }
                WireProtocol::GeminiGenerateContent => {
                    assert_eq!(request.header("x-goog-api-key"), Some(TEST_KEY), "{wire:?}");
                    assert!(request.header("authorization").is_none(), "{wire:?}");
                }
            }
        }
    })
    .await
    .expect("path and header checks finish within the deadline");
}

#[tokio::test]
async fn each_wire_builds_its_own_body_shape() {
    tokio::time::timeout(DEADLINE, async {
        for wire in ALL_WIRES {
            let server = MockServer::start(empty_reply(wire));
            let provider = wire_provider(&format!("{}/v1", server.url), wire, "g-test");
            let mut stream = provider
                .complete(&[Message::user("hi")], &[], "system prompt", None)
                .await
                .unwrap_or_else(|error| panic!("{wire:?} stream opens: {error}"));
            let _ = collect(&mut stream).await;

            let body = recorded_body(&server, 0);
            match wire {
                WireProtocol::ChatCompletions => {
                    assert!(body["messages"].is_array(), "{body}");
                    assert!(body.get("input").is_none(), "{body}");
                    assert_eq!(body["messages"][0]["content"], json!("system prompt"), "{body}");
                }
                WireProtocol::Responses => {
                    assert!(body["input"].is_array(), "{body}");
                    assert_eq!(body["store"], json!(false), "{body}");
                    assert_eq!(body["instructions"], json!("system prompt"), "{body}");
                }
                WireProtocol::AnthropicMessages => {
                    assert!(body["max_tokens"].as_u64().is_some(), "{body}");
                    assert!(body["messages"].is_array(), "{body}");
                    assert_eq!(body["messages"][0]["role"], json!("user"), "{body}");
                    assert_eq!(body["system"][0]["text"], json!("system prompt"), "{body}");
                }
                WireProtocol::GeminiGenerateContent => {
                    assert!(body["contents"].is_array(), "{body}");
                    assert!(
                        body.get("model").is_none(),
                        "the model lives in the URL, not the body: {body}"
                    );
                    assert_eq!(
                        body["systemInstruction"]["parts"][0]["text"],
                        json!("system prompt"),
                        "{body}"
                    );
                }
            }
        }
    })
    .await
    .expect("body shape checks finish within the deadline");
}

// ---------------------------------------------------------------------------
// (b) Streamed text, in order, through each wire's parser

#[tokio::test]
async fn each_wire_streams_its_text_in_order() {
    tokio::time::timeout(DEADLINE, async {
        for wire in ALL_WIRES {
            let server = MockServer::start(text_reply(wire));
            let provider = wire_provider(&format!("{}/v1", server.url), wire, "g-test");
            let mut stream = provider
                .complete(&[Message::user("hi")], &[], "system", None)
                .await
                .unwrap_or_else(|error| panic!("{wire:?} stream opens: {error}"));
            let events = collect(&mut stream).await;

            let deltas: Vec<&str> = events
                .iter()
                .filter_map(|event| match event {
                    StreamEvent::TextDelta(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(deltas, vec!["Hel", "lo"], "{wire:?}: {events:?}");
            assert_eq!(streamed_text(&events), "Hello", "{wire:?}");
        }
    })
    .await
    .expect("stream checks finish within the deadline");
}

// ---------------------------------------------------------------------------
// (c) Tool call round trip: the second request carries the result in the wire's
//     own shape, addressed the way that protocol addresses results

#[tokio::test]
async fn responses_tool_round_trip_carries_the_output_by_call_id() {
    tokio::time::timeout(DEADLINE, async {
        let first = [
            sse_frame(json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "type": "function_call", "id": "fc_1", "call_id": "call_1",
                    "name": "read", "arguments": ""
                }
            })),
            sse_frame(json!({
                "type": "response.function_call_arguments.delta",
                "output_index": 0, "item_id": "fc_1", "delta": "{\"file\":\"a.txt\"}"
            })),
            sse_frame(json!({
                "type": "response.function_call_arguments.done",
                "output_index": 0, "item_id": "fc_1", "arguments": "{\"file\":\"a.txt\"}"
            })),
            sse_frame(json!({ "type": "response.completed", "response": { "status": "completed" } })),
        ];
        let parts: Vec<&str> = first.iter().map(String::as_str).collect();
        let server = MockServer::start_sequence(vec![Reply::sse(&parts), empty_reply(WireProtocol::Responses)]);
        let provider = wire_provider(&format!("{}/v1", server.url), WireProtocol::Responses, "g-test");

        let mut stream = provider
            .complete(&[Message::user("read the file")], &[], "system", None)
            .await
            .expect("first stream opens");
        let events = collect(&mut stream).await;
        assert!(
            events.iter().any(
                |event| matches!(event, StreamEvent::ToolUseStart { id, name } if id == "call_1" && name == "read")
            ),
            "{events:?}"
        );

        let history = vec![
            Message::user("read the file"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call_1".to_string(),
                    name: "read".to_string(),
                    input: json!({ "file": "a.txt" }),
                    thought_signature: None,
                }],
                timestamp: None,
                tool_duration_ms: None,
            },
            Message::tool_result("call_1", "file body", false),
        ];
        let mut stream = provider
            .complete(&history, &[], "system", None)
            .await
            .expect("second stream opens");
        let _ = collect(&mut stream).await;

        let body = recorded_body(&server, 1);
        let outputs = matching(&body, "type", "function_call_output");
        assert_eq!(outputs.len(), 1, "{body}");
        assert_eq!(outputs[0]["call_id"], json!("call_1"), "{body}");
        assert_eq!(outputs[0]["output"], json!("file body"), "{body}");
    })
    .await
    .expect("round trip finishes within the deadline");
}

#[tokio::test]
async fn anthropic_tool_round_trip_carries_the_result_by_tool_use_id() {
    tokio::time::timeout(DEADLINE, async {
        let first = [
            sse_frame(json!({
                "type": "message_start",
                "message": { "usage": { "input_tokens": 5, "output_tokens": 0 } }
            })),
            sse_frame(json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "tool_use", "id": "toolu_1", "name": "read", "input": {} }
            })),
            sse_frame(json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "input_json_delta", "partial_json": "{\"file\":\"a.txt\"}" }
            })),
            sse_frame(json!({ "type": "content_block_stop", "index": 0 })),
            sse_frame(json!({
                "type": "message_delta",
                "delta": { "stop_reason": "tool_use" },
                "usage": { "output_tokens": 4 }
            })),
            sse_frame(json!({ "type": "message_stop" })),
        ];
        let parts: Vec<&str> = first.iter().map(String::as_str).collect();
        let server = MockServer::start_sequence(vec![Reply::sse(&parts), empty_reply(WireProtocol::AnthropicMessages)]);
        let provider = wire_provider(
            &format!("{}/v1", server.url),
            WireProtocol::AnthropicMessages,
            "claude-opus-4-7",
        );

        let mut stream = provider
            .complete(&[Message::user("read the file")], &[], "system", None)
            .await
            .expect("first stream opens");
        let events = collect(&mut stream).await;
        assert!(
            events.iter().any(
                |event| matches!(event, StreamEvent::ToolUseStart { id, name } if id == "toolu_1" && name == "read")
            ),
            "{events:?}"
        );

        let history = vec![
            Message::user("read the file"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "toolu_1".to_string(),
                    name: "read".to_string(),
                    input: json!({ "file": "a.txt" }),
                    thought_signature: None,
                }],
                timestamp: None,
                tool_duration_ms: None,
            },
            Message::tool_result("toolu_1", "file body", false),
        ];
        let mut stream = provider
            .complete(&history, &[], "system", None)
            .await
            .expect("second stream opens");
        let _ = collect(&mut stream).await;

        let body = recorded_body(&server, 1);
        let results = matching(&body, "type", "tool_result");
        assert_eq!(results.len(), 1, "{body}");
        assert_eq!(results[0]["tool_use_id"], json!("toolu_1"), "{body}");
        assert_eq!(results[0]["content"], json!("file body"), "{body}");
    })
    .await
    .expect("round trip finishes within the deadline");
}

#[tokio::test]
async fn gemini_tool_round_trip_addresses_the_output_by_function_name() {
    tokio::time::timeout(DEADLINE, async {
        let first = [sse_frame(json!({
            "candidates": [{
                "content": { "parts": [{ "functionCall": { "id": "call_9", "name": "read", "args": { "file": "a.txt" } } }] },
                "finishReason": "STOP"
            }]
        }))];
        let parts: Vec<&str> = first.iter().map(String::as_str).collect();
        let server = MockServer::start_sequence(vec![
            Reply::sse(&parts),
            empty_reply(WireProtocol::GeminiGenerateContent),
        ]);
        let provider = wire_provider(
            &format!("{}/v1", server.url),
            WireProtocol::GeminiGenerateContent,
            "gemini-3-pro",
        );

        let mut stream = provider
            .complete(&[Message::user("read the file")], &[], "system", None)
            .await
            .expect("first stream opens");
        let events = collect(&mut stream).await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event, StreamEvent::ToolUseStart { id, name } if id == "call_9" && name == "read")),
            "{events:?}"
        );

        let history = vec![
            Message::user("read the file"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call_9".to_string(),
                    name: "read".to_string(),
                    input: json!({ "file": "a.txt" }),
                    thought_signature: None,
                }],
                timestamp: None,
                tool_duration_ms: None,
            },
            Message::tool_result("call_9", "file body", false),
        ];
        let mut stream = provider
            .complete(&history, &[], "system", None)
            .await
            .expect("second stream opens");
        let _ = collect(&mut stream).await;

        let body = recorded_body(&server, 1);
        let responses: Vec<&Value> = body["contents"]
            .as_array()
            .map(|turns| {
                turns
                    .iter()
                    .filter_map(|turn| turn["parts"].as_array())
                    .flatten()
                    .filter_map(|part| part.get("functionResponse"))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(responses.len(), 1, "{body}");
        assert_eq!(responses[0]["name"], json!("read"), "{body}");
        assert_eq!(responses[0]["response"]["output"], json!("file body"), "{body}");
    })
    .await
    .expect("round trip finishes within the deadline");
}

// ---------------------------------------------------------------------------
// (d) `extra_body` wins over a field the builder generated, on every wire

#[tokio::test]
async fn extra_body_overrides_a_generated_field_on_every_wire() {
    tokio::time::timeout(DEADLINE, async {
        for wire in ALL_WIRES {
            let server = MockServer::start(empty_reply(wire));
            let (key, expected) = match wire {
                WireProtocol::ChatCompletions => ("stream", json!(false)),
                WireProtocol::Responses => ("store", json!(true)),
                WireProtocol::AnthropicMessages => ("max_tokens", json!(7)),
                WireProtocol::GeminiGenerateContent => ("contents", json!([])),
            };
            let provider = wire_provider(&format!("{}/v1", server.url), wire, "g-test")
                .with_extra_body(json!({ key: expected.clone() }));
            let mut stream = provider
                .complete(&[Message::user("hi")], &[], "system", None)
                .await
                .unwrap_or_else(|error| panic!("{wire:?} stream opens: {error}"));
            let _ = collect(&mut stream).await;

            let body = recorded_body(&server, 0);
            assert_eq!(body[key], expected, "{wire:?} kept its own value: {body}");
        }
    })
    .await
    .expect("extra body checks finish within the deadline");
}

// ---------------------------------------------------------------------------
// (e) Metadata the provider reports for the selected wire

#[test]
fn route_metadata_names_the_wire_api_method() {
    for (wire, expected) in [
        (WireProtocol::ChatCompletions, "chat_completions"),
        (WireProtocol::Responses, "responses"),
        (WireProtocol::AnthropicMessages, "messages"),
        (WireProtocol::GeminiGenerateContent, "generate_content"),
    ] {
        let provider = wire_provider("http://127.0.0.1:9/v1", wire, "alpha").with_profile_id("gw");
        let routes = provider.model_routes();
        assert_eq!(routes.len(), 1, "{wire:?}");
        assert_eq!(routes[0].model, "alpha", "{wire:?}");
        assert_eq!(routes[0].detail, "gw", "{wire:?}");
        assert_eq!(routes[0].api_method, expected, "{wire:?}");
    }
}

#[test]
fn usage_is_inclusive_except_on_anthropic() {
    for wire in ALL_WIRES {
        let provider = wire_provider("http://127.0.0.1:9/v1", wire, "alpha");
        let expected = !matches!(wire, WireProtocol::AnthropicMessages);
        assert_eq!(provider.usage_is_inclusive(), Some(expected), "{wire:?}");
    }
}

#[test]
fn available_efforts_follow_the_wire_and_the_model() {
    let chat = wire_provider("http://127.0.0.1:9/v1", WireProtocol::ChatCompletions, "alpha");
    let responses = wire_provider("http://127.0.0.1:9/v1", WireProtocol::Responses, "alpha");
    assert_eq!(
        chat.available_efforts(),
        horon_agent_provider_core::OPENAI_SELECTABLE_EFFORTS.to_vec()
    );
    assert_eq!(chat.available_efforts(), responses.available_efforts());

    // A generation with the full modern ladder offers xhigh and max on top of
    // the shared low..high rungs.
    let opus = wire_provider(
        "http://127.0.0.1:9/v1",
        WireProtocol::AnthropicMessages,
        "claude-opus-4-7",
    );
    assert!(anthropic_reasoning_caps("claude-opus-4-7").max_effort);
    assert_eq!(opus.available_efforts(), vec!["low", "medium", "high", "xhigh", "max"]);

    // A model without reasoning control offers nothing rather than levels its
    // request would refuse.
    let sonnet = wire_provider(
        "http://127.0.0.1:9/v1",
        WireProtocol::AnthropicMessages,
        "claude-sonnet-4-5",
    );
    assert!(
        sonnet.available_efforts().is_empty(),
        "{:?}",
        sonnet.available_efforts()
    );

    let gemini = wire_provider(
        "http://127.0.0.1:9/v1",
        WireProtocol::GeminiGenerateContent,
        "gemini-3-pro",
    );
    assert_eq!(
        gemini.available_efforts(),
        vec!["minimal", "low", "medium", "high", "xhigh"]
    );
}

#[test]
fn a_fork_keeps_the_wire_and_its_endpoint() {
    for wire in ALL_WIRES {
        let parent = wire_provider("http://127.0.0.1:9/v1", wire, "alpha");
        assert_eq!(
            parent.endpoint(),
            wire.endpoint("http://127.0.0.1:9/v1", "alpha"),
            "{wire:?}"
        );
        let fork = parent.fork();
        assert_eq!(fork.model_routes()[0].api_method, wire.api_method(), "{wire:?}");
        assert_eq!(fork.usage_is_inclusive(), Some(wire.usage_is_inclusive()), "{wire:?}");
    }
}

// ---------------------------------------------------------------------------
// (f) A tool schema the wire cannot carry leaves a receipt on the session

#[tokio::test]
async fn anthropic_reports_the_schema_keyword_it_dropped() {
    tokio::time::timeout(DEADLINE, async {
        let server = MockServer::start(empty_reply(WireProtocol::AnthropicMessages));
        let provider = wire_provider(
            &format!("{}/v1", server.url),
            WireProtocol::AnthropicMessages,
            "claude-opus-4-7",
        );
        let session = "wire-anthropic-schema-drop";
        horon_agent_provider_core::degradation::forget_session(session);

        let tool = ToolDefinition::new(
            "search",
            "search tool",
            json!({
                "type": "object",
                "properties": { "q": { "type": "string", "example": "value" } },
            }),
        );
        let context = ProviderRequestContext {
            session_id: Some(session.to_string()),
            ..ProviderRequestContext::default()
        };
        let mut stream = provider
            .complete_with_context(&[Message::user("hi")], &[tool], "system", None, &context)
            .await
            .expect("stream opens");
        let _ = collect(&mut stream).await;

        let receipts = horon_agent_provider_core::degradation::take_pending(session);
        assert_eq!(receipts.len(), 1, "{receipts:?}");
        assert_eq!(receipts[0].kind, DegradationKind::SchemaKeywordDropped);
        assert_eq!(receipts[0].tool, "search");
        assert_eq!(receipts[0].target, "example");

        let body = recorded_body(&server, 0);
        assert_eq!(
            body["tools"][0]["input_schema"]["properties"]["q"],
            json!({ "type": "string" }),
            "the dropped keyword is not sent: {body}"
        );
    })
    .await
    .expect("receipt checks finish within the deadline");
}

#[tokio::test]
async fn gemini_reports_the_schema_keyword_it_dropped() {
    tokio::time::timeout(DEADLINE, async {
        let server = MockServer::start(empty_reply(WireProtocol::GeminiGenerateContent));
        let provider = wire_provider(
            &format!("{}/v1", server.url),
            WireProtocol::GeminiGenerateContent,
            "gemini-3-pro",
        );
        let session = "wire-gemini-schema-drop";
        horon_agent_provider_core::degradation::forget_session(session);

        let tool = ToolDefinition::new(
            "search",
            "search tool",
            json!({
                "type": "object",
                "properties": { "q": { "type": "string" } },
                "$defs": { "Probe": { "type": "string" } },
            }),
        );
        let context = ProviderRequestContext {
            session_id: Some(session.to_string()),
            ..ProviderRequestContext::default()
        };
        let mut stream = provider
            .complete_with_context(&[Message::user("hi")], &[tool], "system", None, &context)
            .await
            .expect("stream opens");
        let _ = collect(&mut stream).await;

        let receipts = horon_agent_provider_core::degradation::take_pending(session);
        assert!(
            receipts
                .iter()
                .any(|receipt| receipt.kind == DegradationKind::SchemaKeywordDropped
                    && receipt.tool == "search"
                    && receipt.target == "$defs"),
            "{receipts:?}"
        );

        let body = recorded_body(&server, 0);
        assert!(
            body["tools"][0]["functionDeclarations"][0]["parameters"]
                .get("$defs")
                .is_none(),
            "the dropped keyword is not sent: {body}"
        );
    })
    .await
    .expect("receipt checks finish within the deadline");
}

// ---------------------------------------------------------------------------
// (g) The strict wires see one merged user turn, not two

#[tokio::test]
async fn anthropic_and_gemini_merge_two_consecutive_user_turns() {
    tokio::time::timeout(DEADLINE, async {
        let history = vec![Message::user("first"), Message::user("second")];

        let server = MockServer::start(empty_reply(WireProtocol::AnthropicMessages));
        let provider = wire_provider(
            &format!("{}/v1", server.url),
            WireProtocol::AnthropicMessages,
            "claude-opus-4-7",
        );
        let mut stream = provider
            .complete(&history, &[], "system", None)
            .await
            .expect("stream opens");
        let _ = collect(&mut stream).await;
        let body = recorded_body(&server, 0);
        let turns = body["messages"].as_array().expect("messages array");
        assert_eq!(turns.len(), 1, "{body}");
        assert_eq!(turns[0]["role"], json!("user"), "{body}");
        assert_eq!(turns[0]["content"][0]["text"], json!("first"), "{body}");
        assert_eq!(turns[0]["content"][1]["text"], json!("second"), "{body}");

        let server = MockServer::start(empty_reply(WireProtocol::GeminiGenerateContent));
        let provider = wire_provider(
            &format!("{}/v1", server.url),
            WireProtocol::GeminiGenerateContent,
            "gemini-3-pro",
        );
        let mut stream = provider
            .complete(&history, &[], "system", None)
            .await
            .expect("stream opens");
        let _ = collect(&mut stream).await;
        let body = recorded_body(&server, 0);
        let contents = body["contents"].as_array().expect("contents array");
        assert_eq!(contents.len(), 1, "{body}");
        assert_eq!(contents[0]["role"], json!("user"), "{body}");
        assert_eq!(contents[0]["parts"][0]["text"], json!("first"), "{body}");
        assert_eq!(contents[0]["parts"][1]["text"], json!("second"), "{body}");
    })
    .await
    .expect("merge checks finish within the deadline");
}

// ---------------------------------------------------------------------------
// (h) The Chat Completions wire is unchanged by the switch

#[tokio::test]
async fn chat_completions_keeps_its_path_body_and_api_method() {
    tokio::time::timeout(DEADLINE, async {
        let server = MockServer::start(text_reply(WireProtocol::ChatCompletions));
        let provider =
            wire_provider(&format!("{}/v1", server.url), WireProtocol::ChatCompletions, "g-test").with_profile_id("gw");

        let mut stream = provider
            .complete(&[Message::user("hi")], &[], "system", None)
            .await
            .expect("stream opens");
        let events = collect(&mut stream).await;
        assert_eq!(streamed_text(&events), "Hello", "{events:?}");

        let body = recorded_body(&server, 0);
        assert!(body["messages"].is_array(), "{body}");
        assert!(body.get("input").is_none(), "{body}");
        assert!(body.get("contents").is_none(), "{body}");
        assert_eq!(provider.model_routes()[0].api_method, "chat_completions");
        assert_eq!(provider.usage_is_inclusive(), Some(true));
    })
    .await
    .expect("chat regression checks finish within the deadline");
}
