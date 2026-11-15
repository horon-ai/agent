//! The Messages request body and the block-indexed stream it answers with.

use super::{AnthropicParser, build_messages_request};
use crate::chat::transport::REDACTED;
use crate::test_support::{DEADLINE, TEST_KEY};
use crate::wire::sse::{WireParser, WireSseStream};
use bytes::Bytes;
use futures::StreamExt;
use horon_agent_message_types::degradation::{DegradationKind, DegradationReceipt};
use horon_agent_message_types::{CacheControl, ContentBlock, Message, Role, StreamEvent, ToolDefinition};
use horon_agent_provider_core::anthropic::anthropic_max_output_tokens;
use horon_agent_provider_core::anthropic_reasoning_caps;
use serde_json::{Value, json};
use std::collections::VecDeque;

/// The endpoint identity a parser stamps on native blocks and a builder replays.
const TAG: &str = "anthropic-compatible:test/model";

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

fn tool(name: &str, schema: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: format!("{name} tool"),
        input_schema: schema,
        defer_loading: false,
    }
}

fn body(messages: &[Message], tools: &[ToolDefinition], model: &str) -> Value {
    build_messages_request(model, messages, tools, "system prompt", None, TAG, None)
}

fn body_with_effort(model: &str, effort: Option<&str>) -> Value {
    build_messages_request(model, &[user(vec![text("hi")])], &[], "sys", effort, TAG, None)
}

/// The compact shape of one event, with `ThinkingDone`'s measured duration
/// folded away because it is timing, not content.
fn label(event: &StreamEvent) -> String {
    match event {
        StreamEvent::TextDelta(text) => format!("text:{text}"),
        StreamEvent::TextDone => "text_done".to_string(),
        StreamEvent::ThinkingStart => "thinking_start".to_string(),
        StreamEvent::ThinkingDelta(text) => format!("thinking:{text}"),
        StreamEvent::ThinkingEnd => "thinking_end".to_string(),
        StreamEvent::ThinkingDone { .. } => "thinking_done".to_string(),
        StreamEvent::ToolUseStart { id, name } => format!("tool_start:{id}:{name}"),
        StreamEvent::ToolInputDeltaFor { id, delta } => format!("tool_delta:{id}:{delta}"),
        StreamEvent::ToolUseEndFor { id } => format!("tool_end:{id}"),
        StreamEvent::TokenUsage {
            input_tokens,
            output_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
        } => format!(
            "usage:{input_tokens:?}/{output_tokens:?}/{cache_read_input_tokens:?}/{cache_creation_input_tokens:?}"
        ),
        StreamEvent::MessageEnd { stop_reason } => format!("end:{stop_reason:?}"),
        StreamEvent::Error {
            message,
            retry_after_secs,
        } => format!("error:{retry_after_secs:?}:{message}"),
        StreamEvent::ProviderNative { provider, item } => format!("native:{provider}:{item}"),
        other => format!("other:{other:?}"),
    }
}

fn labels(events: &[StreamEvent]) -> Vec<String> {
    events.iter().map(label).collect()
}

/// Feed `payloads` to a fresh parser and collect what it emitted.
fn drive_payloads(payloads: &[Value]) -> Vec<StreamEvent> {
    let mut parser = AnthropicParser::new(TAG);
    let mut out = VecDeque::new();
    for payload in payloads {
        parser.parse(&payload.to_string(), &mut out);
    }
    out.into_iter().collect()
}

/// [`drive_payloads`], then the end of the body.
fn drive_and_finish(payloads: &[Value]) -> Vec<StreamEvent> {
    let mut parser = AnthropicParser::new(TAG);
    let mut out = VecDeque::new();
    for payload in payloads {
        parser.parse(&payload.to_string(), &mut out);
    }
    parser.finish(&mut out);
    out.into_iter().collect()
}

/// Feed raw payload text, malformed frames included.
fn drive_raw(payloads: &[String]) -> Vec<StreamEvent> {
    let mut parser = AnthropicParser::new(TAG);
    let mut out = VecDeque::new();
    for payload in payloads {
        parser.parse(payload.as_str(), &mut out);
    }
    out.into_iter().collect()
}

fn text_start(index: u64) -> Value {
    json!({
        "type": "content_block_start",
        "index": index,
        "content_block": { "type": "text", "text": "" },
    })
}

fn text_delta(index: u64, text: &str) -> Value {
    json!({
        "type": "content_block_delta",
        "index": index,
        "delta": { "type": "text_delta", "text": text },
    })
}

fn block_stop(index: u64) -> Value {
    json!({ "type": "content_block_stop", "index": index })
}

// ---------------------------------------------------------------------------
// Request body

#[test]
fn a_basic_request_opens_one_user_turn_and_no_reasoning_fields() {
    let request = body(&[user(vec![text("hello")])], &[], "claude-opus-4-7");
    assert_eq!(request["model"], json!("claude-opus-4-7"));
    assert_eq!(request["max_tokens"], json!(128_000));
    assert_eq!(request["stream"], json!(true));
    assert_eq!(
        request["system"],
        json!([{ "type": "text", "text": "system prompt", "cache_control": { "type": "ephemeral" } }])
    );
    assert_eq!(
        request["messages"],
        json!([{ "role": "user", "content": [{ "type": "text", "text": "hello" }] }])
    );
    for absent in ["tools", "thinking", "output_config", "tool_choice"] {
        assert!(request.get(absent).is_none(), "{absent} must be absent: {request}");
    }
}

#[test]
fn an_empty_system_prompt_is_omitted_and_an_unknown_model_keeps_the_small_cap() {
    let request = build_messages_request("gpt-x", &[user(vec![text("hi")])], &[], "", None, TAG, None);
    assert!(request.get("system").is_none());
    assert_eq!(request["max_tokens"], json!(32_768));
    assert_eq!(anthropic_max_output_tokens("gpt-x"), 32_768);
}

#[test]
fn content_blocks_become_their_wire_shapes() {
    let request = body(
        &[
            user(vec![ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "QUJD".to_string(),
            }]),
            assistant(vec![ContentBlock::ToolUse {
                id: "call.1".to_string(),
                name: "read".to_string(),
                input: json!({ "file_path": "a.txt" }),
                thought_signature: None,
            }]),
            user(vec![
                ContentBlock::ToolResult {
                    tool_use_id: "call.1".to_string(),
                    content: "done".to_string(),
                    is_error: Some(true),
                    verdict: None,
                },
                ContentBlock::ToolResult {
                    tool_use_id: "call.1".to_string(),
                    content: "fine".to_string(),
                    is_error: Some(false),
                    verdict: None,
                },
                ContentBlock::ToolResult {
                    tool_use_id: "call.1".to_string(),
                    content: "older".to_string(),
                    is_error: None,
                    verdict: None,
                },
            ]),
        ],
        &[],
        "claude-opus-4-7",
    );
    let turns = request["messages"].as_array().expect("messages is an array");
    assert_eq!(turns.len(), 3, "{turns:?}");
    assert_eq!(
        turns[0]["content"],
        json!([{
            "type": "image",
            "source": { "type": "base64", "media_type": "image/png", "data": "QUJD" },
        }])
    );
    assert_eq!(
        turns[1]["content"],
        json!([{
            "type": "tool_use",
            "id": "call_1",
            "name": "read",
            "input": { "file_path": "a.txt" },
        }])
    );
    assert_eq!(
        turns[2]["content"],
        json!([
            { "type": "tool_result", "tool_use_id": "call_1", "content": "done", "is_error": true },
            { "type": "tool_result", "tool_use_id": "call_1", "content": "fine" },
            { "type": "tool_result", "tool_use_id": "call_1", "content": "older" },
        ])
    );
}

#[test]
fn a_non_object_tool_input_becomes_an_empty_object() {
    let request = body(
        &[assistant(vec![ContentBlock::ToolUse {
            id: "call_1".to_string(),
            name: "read".to_string(),
            input: json!("not an object"),
            thought_signature: None,
        }])],
        &[],
        "claude-opus-4-7",
    );
    assert_eq!(
        request["messages"],
        json!([{
            "role": "assistant",
            "content": [{ "type": "tool_use", "id": "call_1", "name": "read", "input": {} }],
        }])
    );
}

#[test]
fn a_stored_cache_control_rides_along_on_its_text_block() {
    let cached = ContentBlock::Text {
        text: "cached prefix".to_string(),
        cache_control: Some(CacheControl::ephemeral(None)),
    };
    let request = body(&[user(vec![cached])], &[], "claude-opus-4-7");
    assert_eq!(
        request["messages"][0]["content"][0],
        json!({ "type": "text", "text": "cached prefix", "cache_control": { "type": "ephemeral" } })
    );
}

#[test]
fn reasoning_blocks_this_wire_cannot_replay_are_dropped_and_the_turns_around_them_merge() {
    let request = body(
        &[
            user(vec![text("first")]),
            assistant(vec![
                ContentBlock::ReasoningTrace {
                    text: "traced".to_string(),
                },
                ContentBlock::AnthropicThinking {
                    thinking: "thought".to_string(),
                    signature: "sig".to_string(),
                },
                ContentBlock::OpenAIReasoning {
                    id: "rs_1".to_string(),
                    summary: vec!["summary".to_string()],
                    encrypted_content: Some("opaque".to_string()),
                    status: None,
                },
            ]),
            user(vec![text("second")]),
        ],
        &[],
        "claude-opus-4-7",
    );
    assert_eq!(
        request["messages"],
        json!([{
            "role": "user",
            "content": [
                { "type": "text", "text": "first" },
                { "type": "text", "text": "second" },
            ],
        }])
    );
}

#[test]
fn a_matching_native_thinking_block_is_replayed_in_place() {
    let thinking = json!({ "type": "thinking", "thinking": "t", "signature": "sig" });
    let redacted = json!({ "type": "redacted_thinking", "data": "cipher" });
    let request = body(
        &[assistant(vec![
            ContentBlock::ProviderNative {
                provider: TAG.to_string(),
                item: thinking.clone(),
            },
            ContentBlock::ProviderNative {
                provider: TAG.to_string(),
                item: redacted.clone(),
            },
            ContentBlock::ToolUse {
                id: "call_1".to_string(),
                name: "read".to_string(),
                input: json!({}),
                thought_signature: None,
            },
        ])],
        &[],
        "claude-opus-4-7",
    );
    assert_eq!(
        request["messages"],
        json!([{
            "role": "assistant",
            "content": [
                thinking,
                redacted,
                { "type": "tool_use", "id": "call_1", "name": "read", "input": {} },
            ],
        }])
    );
}

#[test]
fn a_native_block_from_another_endpoint_or_of_another_kind_is_not_sent_back() {
    let thinking = json!({ "type": "thinking", "thinking": "t", "signature": "sig" });
    let request = body(
        &[
            assistant(vec![
                ContentBlock::ProviderNative {
                    provider: "anthropic".to_string(),
                    item: thinking.clone(),
                },
                ContentBlock::ProviderNative {
                    provider: "anthropic-compatible:other/model".to_string(),
                    item: thinking.clone(),
                },
                ContentBlock::ProviderNative {
                    provider: TAG.to_string(),
                    item: json!({ "type": "server_tool_use", "id": "srv_1", "name": "web_search" }),
                },
            ]),
            // The same signed block outside an assistant turn is not thinking
            // context and must not be replayed either.
            user(vec![ContentBlock::ProviderNative {
                provider: TAG.to_string(),
                item: thinking,
            }]),
        ],
        &[],
        "claude-opus-4-7",
    );
    assert!(request.get("messages").is_some(), "{request}");
    assert_eq!(request["messages"], json!([]), "{request}");
}

#[test]
fn tools_are_declared_once_and_their_schema_losses_are_reported() {
    let tools = vec![
        tool(
            "read",
            json!({ "type": "object", "properties": { "file_path": { "type": "string" } } }),
        ),
        tool("read", json!({ "type": "object", "properties": {} })),
    ];
    let mut receipts: Vec<DegradationReceipt> = Vec::new();
    let request = build_messages_request(
        "claude-opus-4-7",
        &[user(vec![text("hi")])],
        &tools,
        "sys",
        None,
        TAG,
        Some(&mut receipts),
    );
    let declared = request["tools"].as_array().expect("tools is an array");
    assert_eq!(declared.len(), 1, "{declared:?}");
    assert_eq!(
        declared[0],
        json!({
            "name": "read",
            "description": "read tool",
            "input_schema": { "type": "object", "properties": { "file_path": { "type": "string" } } },
        })
    );
    assert_eq!(
        receipts,
        vec![DegradationReceipt::new(
            DegradationKind::DuplicateToolDropped,
            "read",
            "read"
        )]
    );
}

#[test]
fn a_keyword_the_anthropic_dialect_drops_is_reported() {
    // `example` (singular) is outside the Anthropic keyword set, which carries
    // `examples` only.
    let schema = json!({
        "type": "object",
        "properties": { "q": { "type": "string", "example": "value" } },
    });
    let mut receipts: Vec<DegradationReceipt> = Vec::new();
    let request = build_messages_request(
        "claude-opus-4-7",
        &[user(vec![text("hi")])],
        &[tool("search", schema)],
        "sys",
        None,
        TAG,
        Some(&mut receipts),
    );
    assert_eq!(
        request["tools"][0]["input_schema"],
        json!({ "type": "object", "properties": { "q": { "type": "string" } } }),
        "{request}"
    );
    assert_eq!(
        receipts,
        vec![DegradationReceipt::new(
            DegradationKind::SchemaKeywordDropped,
            "search",
            "example"
        )]
    );
}

#[test]
fn a_non_object_schema_becomes_an_empty_object_schema() {
    let request = body(
        &[user(vec![text("hi")])],
        &[tool("odd", json!("not a schema"))],
        "claude-opus-4-7",
    );
    assert_eq!(
        request["tools"][0],
        json!({
            "name": "odd",
            "description": "odd tool",
            "input_schema": { "type": "object", "properties": {} },
        })
    );
}

#[test]
fn the_documented_caps_hold() {
    let opus_47 = anthropic_reasoning_caps("claude-opus-4-7");
    assert!(opus_47.output_effort && opus_47.adaptive_thinking && opus_47.xhigh_effort && opus_47.max_effort);
    assert!(!opus_47.manual_thinking);
    let opus_46 = anthropic_reasoning_caps("claude-opus-4-6");
    assert!(opus_46.output_effort && opus_46.adaptive_thinking && opus_46.max_effort);
    assert!(!opus_46.xhigh_effort && !opus_46.manual_thinking);
    let opus_45 = anthropic_reasoning_caps("claude-opus-4-5");
    assert!(opus_45.output_effort && opus_45.manual_thinking);
    assert!(!opus_45.adaptive_thinking && !opus_45.xhigh_effort && !opus_45.max_effort);
    let sonnet_37 = anthropic_reasoning_caps("claude-3-7-sonnet");
    assert!(sonnet_37.manual_thinking);
    assert!(!sonnet_37.output_effort && !sonnet_37.adaptive_thinking);
    assert!(!anthropic_reasoning_caps("claude-haiku-4-5").supports_reasoning_effort());
    assert!(!anthropic_reasoning_caps("gpt-x").supports_reasoning_effort());
    assert_eq!(anthropic_max_output_tokens("claude-opus-4-7"), 128_000);
    assert_eq!(anthropic_max_output_tokens("claude-opus-4-5"), 32_768);
    assert_eq!(anthropic_max_output_tokens("claude-haiku-4-5"), 64_000);
}

#[test]
fn effort_uses_what_the_model_accepts() {
    let full = body_with_effort("claude-opus-4-7", Some("high"));
    assert_eq!(full["output_config"], json!({ "effort": "high" }));
    assert_eq!(full["thinking"], json!({ "type": "adaptive" }));
    assert_eq!(
        body_with_effort("claude-opus-4-7", Some("xhigh"))["output_config"],
        json!({ "effort": "xhigh" })
    );
    assert_eq!(
        body_with_effort("claude-opus-4-7", Some("max"))["output_config"],
        json!({ "effort": "max" })
    );
    assert_eq!(
        body_with_effort("claude-opus-4-7", Some("minimal"))["output_config"],
        json!({ "effort": "low" })
    );

    // No `xhigh` level on this generation: it degrades to `high` instead of
    // sending a word the endpoint rejects.
    let no_xhigh = body_with_effort("claude-opus-4-6", Some("xhigh"));
    assert_eq!(no_xhigh["output_config"], json!({ "effort": "high" }));
    assert_eq!(no_xhigh["thinking"], json!({ "type": "adaptive" }));

    let manual_effort = body_with_effort("claude-opus-4-5", Some("medium"));
    assert_eq!(manual_effort["output_config"], json!({ "effort": "medium" }));
    assert_eq!(
        manual_effort["thinking"],
        json!({ "type": "enabled", "budget_tokens": 10_000 })
    );
    let manual_high = body_with_effort("claude-opus-4-5", Some("high"));
    assert_eq!(manual_high["thinking"]["budget_tokens"], json!(32_000));

    let manual_only = body_with_effort("claude-3-7-sonnet", Some("low"));
    assert!(manual_only.get("output_config").is_none(), "{manual_only}");
    assert_eq!(
        manual_only["thinking"],
        json!({ "type": "enabled", "budget_tokens": 4_096 })
    );

    for model in ["claude-haiku-4-5", "gpt-x"] {
        for effort in ["minimal", "low", "medium", "high", "xhigh", "max"] {
            let request = body_with_effort(model, Some(effort));
            assert!(request.get("thinking").is_none(), "{model} {effort}: {request}");
            assert!(request.get("output_config").is_none(), "{model} {effort}: {request}");
        }
    }
}

#[test]
fn an_absent_empty_or_unknown_effort_adds_nothing() {
    for model in [
        "claude-opus-4-7",
        "claude-opus-4-6",
        "claude-opus-4-5",
        "claude-3-7-sonnet",
        "claude-haiku-4-5",
        "gpt-x",
    ] {
        for effort in [None, Some(""), Some("   "), Some("none"), Some("swarm")] {
            let request = body_with_effort(model, effort);
            assert!(request.get("thinking").is_none(), "{model} {effort:?}: {request}");
            assert!(request.get("output_config").is_none(), "{model} {effort:?}: {request}");
        }
    }
}

// ---------------------------------------------------------------------------
// Stream

#[test]
fn a_text_response_reports_its_usage_on_the_event_that_carries_it() {
    let events = drive_payloads(&[
        json!({
            "type": "message_start",
            "message": {
                "id": "msg_1",
                "usage": { "input_tokens": 10, "cache_read_input_tokens": 3, "cache_creation_input_tokens": 2 },
            },
        }),
        text_start(0),
        text_delta(0, "Hel"),
        text_delta(0, "lo"),
        block_stop(0),
        json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 7 } }),
        json!({ "type": "message_stop" }),
    ]);
    assert_eq!(
        labels(&events),
        vec![
            "usage:Some(10)/None/Some(3)/Some(2)",
            "text:Hel",
            "text:lo",
            "text_done",
            "usage:None/Some(7)/None/None",
            "end:Some(\"end_turn\")",
        ]
    );
}

#[test]
fn a_thinking_block_closes_into_a_replayable_native_item() {
    let events = drive_payloads(&[
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "thinking", "thinking": "" },
        }),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "thinking_delta", "thinking": "a" },
        }),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "thinking_delta", "thinking": "b" },
        }),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "signature_delta", "signature": "sig" },
        }),
        block_stop(0),
        text_start(1),
        text_delta(1, "hi"),
        block_stop(1),
    ]);
    let item = json!({ "type": "thinking", "thinking": "ab", "signature": "sig" });
    assert_eq!(
        labels(&events),
        vec![
            "thinking_start".to_string(),
            "thinking:a".to_string(),
            "thinking:b".to_string(),
            "thinking_end".to_string(),
            "thinking_done".to_string(),
            format!("native:{TAG}:{item}"),
            "text:hi".to_string(),
            "text_done".to_string(),
        ]
    );
}

#[test]
fn an_unsigned_thinking_block_is_not_stored() {
    let events = drive_payloads(&[
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "thinking", "thinking": "" },
        }),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "thinking_delta", "thinking": "a" },
        }),
        block_stop(0),
    ]);
    assert_eq!(
        labels(&events),
        vec!["thinking_start", "thinking:a", "thinking_end", "thinking_done"]
    );
}

#[test]
fn a_redacted_thinking_block_is_forwarded_verbatim() {
    let redacted = json!({ "type": "redacted_thinking", "data": "cipher" });
    let events = drive_payloads(&[
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": redacted.clone(),
        }),
        block_stop(0),
    ]);
    assert_eq!(labels(&events), vec![format!("native:{TAG}:{redacted}")]);
}

#[test]
fn a_tool_call_streams_as_json_fragments() {
    let events = drive_payloads(&[
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "tool_use", "id": "toolu_1", "name": "read", "input": {} },
        }),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "input_json_delta", "partial_json": "{\"file_path\":" },
        }),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "input_json_delta", "partial_json": "\"a.txt\"}" },
        }),
        block_stop(0),
        json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" }, "usage": {} }),
        json!({ "type": "message_stop" }),
    ]);
    assert_eq!(
        labels(&events),
        vec![
            "tool_start:toolu_1:read",
            "tool_delta:toolu_1:{\"file_path\":",
            "tool_delta:toolu_1:\"a.txt\"}",
            "tool_end:toolu_1",
            "end:Some(\"tool_use\")",
        ]
    );
    let joined: String = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ToolInputDeltaFor { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        serde_json::from_str::<Value>(&joined).expect("the deltas form one JSON document"),
        json!({ "file_path": "a.txt" })
    );
}

#[test]
fn an_input_sent_in_full_on_the_start_event_is_emitted_once() {
    let full = drive_payloads(&[
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "tool_use", "id": "toolu_1", "name": "read", "input": { "a": 1 } },
        }),
        block_stop(0),
    ]);
    assert_eq!(
        labels(&full),
        vec![
            "tool_start:toolu_1:read".to_string(),
            format!("tool_delta:toolu_1:{}", json!({ "a": 1 })),
            "tool_end:toolu_1".to_string(),
        ]
    );

    let streamed = drive_payloads(&[
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "tool_use", "id": "toolu_1", "name": "read", "input": { "a": 1 } },
        }),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "input_json_delta", "partial_json": "{\"a\":1}" },
        }),
        block_stop(0),
    ]);
    assert_eq!(
        labels(&streamed),
        vec![
            "tool_start:toolu_1:read",
            "tool_delta:toolu_1:{\"a\":1}",
            "tool_end:toolu_1",
        ]
    );
}

#[test]
fn a_native_thinking_event_round_trips_into_the_next_request() {
    let events = drive_payloads(&[
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "thinking", "thinking": "" },
        }),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "thinking_delta", "thinking": "ab" },
        }),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "signature_delta", "signature": "sig" },
        }),
        block_stop(0),
    ]);
    let native = events
        .iter()
        .find_map(|event| match event {
            StreamEvent::ProviderNative { provider, item } => Some((provider.clone(), item.clone())),
            _ => None,
        })
        .expect("the signed block is emitted as a native item");
    let request = body(
        &[assistant(vec![
            ContentBlock::ProviderNative {
                provider: native.0.clone(),
                item: native.1.clone(),
            },
            text("answer"),
        ])],
        &[],
        "claude-opus-4-7",
    );
    assert_eq!(native.0, TAG);
    assert_eq!(
        request["messages"][0]["content"][0],
        json!({ "type": "thinking", "thinking": "ab", "signature": "sig" })
    );
    assert_eq!(
        request["messages"][0]["content"][1],
        json!({ "type": "text", "text": "answer" })
    );
}

#[test]
fn keepalives_and_unhandled_events_produce_nothing() {
    let events = drive_payloads(&[
        json!({ "type": "ping" }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "citations_delta", "citation": {} } }),
        json!({ "type": "some_future_event", "value": 1 }),
    ]);
    assert_eq!(labels(&events), Vec::<String>::new());
}

#[test]
fn a_server_error_event_ends_the_stream_with_the_server_wording() {
    let mut parser = AnthropicParser::new(TAG);
    let mut out = VecDeque::new();
    parser.parse(
        &json!({
            "type": "error",
            "error": { "type": "overloaded_error", "message": "Overloaded" },
        })
        .to_string(),
        &mut out,
    );
    parser.parse(&text_delta(0, "late").to_string(), &mut out);
    parser.finish(&mut out);
    let events: Vec<StreamEvent> = out.into_iter().collect();
    assert_eq!(
        labels(&events),
        vec!["error:None:Anthropic-compatible stream returned an error: overloaded_error: Overloaded"]
    );
}

#[test]
fn a_body_that_ends_mid_response_is_reported_as_truncated() {
    let events = drive_and_finish(&[text_start(0), text_delta(0, "partial")]);
    assert_eq!(
        labels(&events),
        vec![
            "text:partial",
            "text_done",
            "error:None:Anthropic-compatible stream ended before the response finished \
             (no stop_reason or message_stop)",
        ]
    );
}

#[test]
fn a_body_that_ends_after_its_stop_reason_completes_normally() {
    let events = drive_and_finish(&[
        text_start(0),
        text_delta(0, "done"),
        block_stop(0),
        json!({ "type": "message_delta", "delta": { "stop_reason": "refusal" }, "usage": {} }),
    ]);
    assert_eq!(labels(&events), vec!["text:done", "text_done", "end:Some(\"refusal\")"]);
}

#[test]
fn one_malformed_frame_is_reported_and_the_rest_still_arrives() {
    let frames = [
        "{not json".to_string(),
        text_start(0).to_string(),
        text_delta(0, "kept").to_string(),
        block_stop(0).to_string(),
        json!({ "type": "message_stop" }).to_string(),
    ];
    let events = drive_raw(&frames);
    assert_eq!(events.len(), 4, "{:?}", labels(&events));
    let StreamEvent::Error { message, .. } = &events[0] else {
        panic!("expected one malformed-frame error, got {:?}", labels(&events));
    };
    assert!(
        message.starts_with("Anthropic-compatible stream sent a malformed JSON chunk:"),
        "{message}"
    );
    assert_eq!(labels(&events[1..]), vec!["text:kept", "text_done", "end:None"]);
}

#[test]
fn a_block_still_open_at_the_end_marker_is_closed_before_it() {
    let text_events = drive_payloads(&[text_start(0), text_delta(0, "tail"), json!({ "type": "message_stop" })]);
    assert_eq!(labels(&text_events), vec!["text:tail", "text_done", "end:None"]);

    let tool_events = drive_payloads(&[
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "tool_use", "id": "toolu_1", "name": "read", "input": {} },
        }),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "input_json_delta", "partial_json": "{}" },
        }),
        json!({ "type": "message_stop" }),
    ]);
    assert_eq!(
        labels(&tool_events),
        vec![
            "tool_start:toolu_1:read",
            "tool_delta:toolu_1:{}",
            "tool_end:toolu_1",
            "end:None",
        ]
    );
}

// ---------------------------------------------------------------------------
// Through the shared SSE stream

type Chunks = Vec<Result<Bytes, reqwest::Error>>;

fn ok(text: &str) -> Chunks {
    vec![Ok(Bytes::from(text.to_string()))]
}

async fn drain(stream: &mut WireSseStream<AnthropicParser>) -> Vec<anyhow::Result<StreamEvent>> {
    let mut events = Vec::new();
    loop {
        let next = tokio::time::timeout(DEADLINE, stream.next())
            .await
            .expect("an event within the deadline");
        match next {
            Some(event) => events.push(event),
            None => return events,
        }
    }
}

#[tokio::test]
async fn an_error_naming_the_credential_leaves_it_redacted() {
    let body = crate::test_support::sse_frame(json!({
        "type": "error",
        "error": { "type": "authentication_error", "message": format!("bad key {TEST_KEY}") },
    }));
    let mut stream = WireSseStream::new(
        futures::stream::iter(ok(&body)),
        AnthropicParser::new(TAG),
        "Anthropic-compatible",
        Some(std::sync::Arc::from(TEST_KEY)),
    );
    let events = drain(&mut stream).await;
    let Ok(StreamEvent::Error { message, .. }) = &events[0] else {
        panic!("expected an error event, got {events:?}");
    };
    assert!(message.contains(REDACTED), "{message}");
    assert!(!message.contains(TEST_KEY), "{message}");
    assert_eq!(
        message.as_str(),
        format!("Anthropic-compatible stream returned an error: authentication_error: bad key {REDACTED}")
    );
}
