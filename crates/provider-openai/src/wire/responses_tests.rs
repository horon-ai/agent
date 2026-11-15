//! The OpenAI Responses request body and the payloads a `responses` stream can
//! carry.
//!
//! Every stream test asserts the full ordered event list, because the order is
//! what the turn loop relies on: thinking closes before text, text closes before
//! a call, usage and the end come last.

use super::{ResponsesParser, build_responses_request};
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

fn result(id: &str, content: &str) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: id.to_string(),
        content: content.to_string(),
        is_error: None,
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
    build_responses_request(model, messages, tools, system, effort, None)
}

// ---------------------------------------------------------------------------
// Event labels

/// One compact label per event, so a test can state the whole expected sequence.
fn labels(events: &[StreamEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| match event {
            StreamEvent::TextDelta(text) => format!("text:{text}"),
            StreamEvent::TextDone => "text-done".to_string(),
            StreamEvent::ToolUseStart { id, name } => format!("tool:{id}:{name}"),
            StreamEvent::ToolInputDeltaFor { id, delta } => format!("arg:{id}:{delta}"),
            StreamEvent::ToolUseEndFor { id } => format!("tool-end:{id}"),
            StreamEvent::ThinkingStart => "think".to_string(),
            StreamEvent::ThinkingDelta(delta) => format!("think:{delta}"),
            StreamEvent::ThinkingEnd => "think-end".to_string(),
            // The duration is wall-clock and is not part of the contract.
            StreamEvent::ThinkingDone { .. } => "think-done".to_string(),
            StreamEvent::TokenUsage { .. } => "usage".to_string(),
            StreamEvent::MessageEnd { stop_reason } => format!("end:{stop_reason:?}"),
            StreamEvent::Error { message, .. } => format!("error:{message}"),
            _ => "other".to_string(),
        })
        .collect()
}

/// Drive the parser directly with JSON payloads, one per call.
fn parse_all(payloads: &[Value]) -> Vec<StreamEvent> {
    let mut parser = ResponsesParser::new();
    let mut out = VecDeque::new();
    for payload in payloads {
        parser.parse(&payload.to_string(), &mut out);
    }
    out.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Request body

#[test]
fn a_plain_question_is_a_streamed_non_stored_responses_request() {
    let request = body("gpt-5", &[user(vec![text("hello")])], &[], "be brief", None);

    assert_eq!(request["model"], json!("gpt-5"));
    assert_eq!(request["stream"], json!(true));
    assert_eq!(request["store"], json!(false));
    assert_eq!(request["instructions"], json!("be brief"));
    assert_eq!(
        request["input"],
        json!([{ "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hello" }] }])
    );
    // Each of these would make the request depend on account state or on a
    // transport feature this wire does not use.
    for absent in ["include", "stream_options", "previous_response_id", "prompt_cache_key"] {
        assert!(request.get(absent).is_none(), "unexpected {absent}: {request}");
    }
}

#[test]
fn an_empty_system_prompt_adds_no_instructions() {
    let messages = [user(vec![text("hello")])];

    let empty = body("gpt-5", &messages, &[], "", None);
    assert!(empty.get("instructions").is_none(), "{empty}");

    let blank = body("gpt-5", &messages, &[], "   ", None);
    assert!(blank.get("instructions").is_none(), "{blank}");

    let present = body("gpt-5", &messages, &[], "be brief", None);
    assert_eq!(present["instructions"], json!("be brief"));
}

#[test]
fn one_tool_is_declared_with_automatic_choice() {
    let tools = [tool("read", object_schema(json!({ "path": { "type": "string" } })))];

    let request = body("gpt-5", &[user(vec![text("read it")])], &tools, "", None);

    let declared = request["tools"].as_array().expect("a tools array");
    assert_eq!(declared.len(), 1, "{request}");
    assert_eq!(declared[0]["name"], json!("read"));
    assert_eq!(request["tool_choice"], json!("auto"));
}

#[test]
fn a_blank_or_absent_effort_adds_no_reasoning_block() {
    let messages = [user(vec![text("hello")])];

    let high = body("gpt-5", &messages, &[], "", Some("high"));
    assert_eq!(high["reasoning"], json!({ "effort": "high" }));

    let blank = body("gpt-5", &messages, &[], "", Some("   "));
    assert!(blank.get("reasoning").is_none(), "{blank}");

    let absent = body("gpt-5", &messages, &[], "", None);
    assert!(absent.get("reasoning").is_none(), "{absent}");
}

#[test]
fn a_second_tool_with_the_same_name_is_dropped_and_reported() {
    let tools = [
        tool("read", object_schema(json!({ "path": { "type": "string" } }))),
        tool("read", object_schema(json!({ "target": { "type": "string" } }))),
    ];
    let messages = [user(vec![text("read it")])];
    let mut receipts: Vec<DegradationReceipt> = Vec::new();

    let reported = build_responses_request("gpt-5", &messages, &tools, "", None, Some(&mut receipts));

    assert_eq!(
        reported["tools"].as_array().expect("a tools array").len(),
        1,
        "{reported}"
    );
    let duplicate = receipts
        .iter()
        .find(|receipt| receipt.kind == DegradationKind::DuplicateToolDropped)
        .expect("the duplicate is reported");
    assert_eq!(duplicate.tool, "read");
    assert_eq!(duplicate.target, "read");

    // Without a sink the same catalog is still deduplicated and nothing panics.
    let silent = body("gpt-5", &messages, &tools, "", None);
    assert_eq!(silent["tools"], reported["tools"]);
}

#[test]
fn a_call_and_its_output_are_sent_as_items_with_the_same_call_id() {
    let messages = [
        user(vec![text("read it")]),
        assistant(vec![call("call_1", "read", json!({ "path": "a.txt" }))]),
        user(vec![result("call_1", "contents")]),
    ];

    let request = body("gpt-5", &messages, &[], "", None);

    let input = request["input"].as_array().expect("an input array");
    let sent_call = input
        .iter()
        .find(|item| item["type"] == json!("function_call"))
        .expect("the call is replayed");
    assert_eq!(sent_call["call_id"], json!("call_1"));
    assert_eq!(sent_call["name"], json!("read"));
    let sent_output = input
        .iter()
        .find(|item| item["type"] == json!("function_call_output"))
        .expect("the output is replayed");
    assert_eq!(sent_output["call_id"], sent_call["call_id"]);
}

// ---------------------------------------------------------------------------
// Stream events

#[test]
fn two_text_deltas_close_into_one_text_block() {
    let events = parse_all(&[
        json!({ "type": "response.created", "response": { "id": "resp_1" } }),
        json!({ "type": "response.output_text.delta", "delta": "The cause is " }),
        json!({ "type": "response.output_text.delta", "delta": "a missing file." }),
        json!({
            "type": "response.completed",
            "response": { "status": "completed", "usage": { "input_tokens": 10, "output_tokens": 5 } }
        }),
    ]);

    assert_eq!(
        labels(&events),
        vec![
            "text:The cause is ",
            "text:a missing file.",
            "text-done",
            "usage",
            "end:None",
        ]
    );
}

#[test]
fn a_streamed_call_opens_once_and_closes_with_its_arguments() {
    let events = parse_all(&[
        json!({
            "type": "response.output_item.added",
            "output_index": 1,
            "item": {
                "type": "function_call",
                "id": "fc_1",
                "call_id": "call_1",
                "name": "bash",
                "arguments": ""
            }
        }),
        json!({
            "type": "response.function_call_arguments.delta",
            "output_index": 1,
            "item_id": "fc_1",
            "delta": "{\"command\":"
        }),
        json!({
            "type": "response.function_call_arguments.done",
            "output_index": 1,
            "item_id": "fc_1",
            "arguments": "{\"command\":\"pwd\"}"
        }),
        json!({ "type": "response.completed", "response": { "status": "completed" } }),
    ]);

    assert_eq!(
        labels(&events),
        vec![
            "tool:call_1:bash",
            "arg:call_1:{\"command\":",
            // Only the part the done snapshot added is streamed again.
            "arg:call_1:\"pwd\"}",
            "tool-end:call_1",
            "end:None",
        ]
    );
}

#[test]
fn reasoning_closes_before_the_answer_starts() {
    let events = parse_all(&[
        json!({ "type": "response.reasoning_summary_text.delta", "delta": "looking" }),
        json!({ "type": "response.reasoning_summary_text.delta", "delta": " closer" }),
        json!({ "type": "response.output_text.delta", "delta": "Found it." }),
        json!({ "type": "response.completed", "response": { "status": "completed" } }),
    ]);

    assert_eq!(
        labels(&events),
        vec![
            "think",
            "think:looking",
            "think: closer",
            "think-end",
            "think-done",
            "text:Found it.",
            "text-done",
            "end:None",
        ]
    );
}

#[test]
fn reasoning_still_open_at_the_end_is_closed_before_usage() {
    let events = parse_all(&[
        json!({ "type": "response.reasoning_summary_text.delta", "delta": "still thinking" }),
        json!({
            "type": "response.completed",
            "response": { "status": "completed", "usage": { "input_tokens": 3, "output_tokens": 2 } }
        }),
    ]);

    assert_eq!(
        labels(&events),
        vec![
            "think",
            "think:still thinking",
            "think-end",
            "think-done",
            "usage",
            "end:None",
        ]
    );
}

#[test]
fn a_replayed_reasoning_item_surfaces_as_thinking_only() {
    let events = parse_all(&[
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "reasoning",
                "id": "rs_1",
                "summary": [{ "type": "summary_text", "text": "already streamed" }]
            }
        }),
        json!({ "type": "response.output_text.delta", "delta": "Answer." }),
        json!({ "type": "response.completed", "response": { "status": "completed" } }),
    ]);

    let labelled = labels(&events);
    assert_eq!(
        labelled,
        vec![
            "think",
            "think:already streamed",
            "think-end",
            "think-done",
            "text:Answer.",
            "text-done",
            "end:None",
        ],
        "a replayed item becomes one thinking block, never a native event"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StreamEvent::OpenAIReasoning { .. })),
        "{labelled:?}"
    );
    assert!(!labelled.contains(&"other".to_string()), "{labelled:?}");
}

#[test]
fn a_reasoning_item_already_streamed_live_adds_no_second_block() {
    let events = parse_all(&[
        json!({ "type": "response.reasoning_summary_text.delta", "delta": "live" }),
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "reasoning",
                "id": "rs_1",
                "summary": [{ "type": "summary_text", "text": "live" }]
            }
        }),
        json!({ "type": "response.completed", "response": { "status": "completed" } }),
    ]);

    assert_eq!(
        labels(&events),
        vec!["think", "think:live", "think-end", "think-done", "end:None",],
        "the summary must not be rendered twice"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StreamEvent::OpenAIReasoning { .. })),
        "{:?}",
        labels(&events)
    );
}

#[test]
fn an_incomplete_response_reports_the_reason() {
    let events = parse_all(&[json!({
        "type": "response.incomplete",
        "response": { "incomplete_details": { "reason": "max_output_tokens" } }
    })]);

    assert_eq!(labels(&events), vec!["end:Some(\"max_output_tokens\")"]);
}

#[test]
fn a_failed_response_is_one_terminal_error() {
    let mut parser = ResponsesParser::new();
    let mut out = VecDeque::new();
    parser.parse(
        &json!({
            "type": "response.failed",
            "response": { "error": { "type": "server_error", "message": "boom" } }
        })
        .to_string(),
        &mut out,
    );

    let after_failure: Vec<StreamEvent> = out.drain(..).collect();
    assert_eq!(
        labels(&after_failure),
        vec!["error:OpenAI Responses stream returned an error: server_error: boom"]
    );

    parser.parse(
        &json!({ "type": "response.output_text.delta", "delta": "late" }).to_string(),
        &mut out,
    );
    parser.finish(&mut out);
    assert!(
        out.is_empty(),
        "a failed response is terminal: {:?}",
        labels(out.make_contiguous())
    );
}

#[test]
fn a_body_that_ends_mid_answer_reports_truncation() {
    let mut parser = ResponsesParser::new();
    let mut out = VecDeque::new();
    parser.parse(
        &json!({ "type": "response.output_text.delta", "delta": "partial" }).to_string(),
        &mut out,
    );
    parser.finish(&mut out);

    let events: Vec<StreamEvent> = out.into_iter().collect();
    assert_eq!(
        labels(&events),
        vec![
            "text:partial",
            "text-done",
            "error:OpenAI Responses stream ended before the response finished (no response.completed event)",
        ]
    );
}

#[test]
fn a_malformed_chunk_is_reported_once_and_the_stream_keeps_going() {
    let mut parser = ResponsesParser::new();
    let mut out = VecDeque::new();
    parser.parse("  {not json  ", &mut out);

    let first: Vec<StreamEvent> = out.drain(..).collect();
    assert_eq!(first.len(), 1, "{:?}", labels(&first));
    match first.first() {
        Some(StreamEvent::Error {
            message,
            retry_after_secs,
        }) => {
            let expected_prefix = "OpenAI Responses stream sent a malformed JSON chunk: ";
            assert!(message.starts_with(expected_prefix), "{message}");
            // The diagnostic keeps the payload as it was reported, trimmed.
            let payload_part = format!("; payload={}", "{not json");
            assert!(message.ends_with(&payload_part), "{message}");
            assert!(
                message.len() > expected_prefix.len() + payload_part.len(),
                "the parse error is quoted: {message}"
            );
            assert!(retry_after_secs.is_none(), "{retry_after_secs:?}");
        }
        other => panic!("expected one parse error, got {other:?}"),
    }

    parser.parse(
        &json!({ "type": "response.output_text.delta", "delta": "recovered" }).to_string(),
        &mut out,
    );
    parser.parse(
        &json!({ "type": "response.completed", "response": { "status": "completed" } }).to_string(),
        &mut out,
    );

    let rest: Vec<StreamEvent> = out.into_iter().collect();
    assert_eq!(labels(&rest), vec!["text:recovered", "text-done", "end:None"]);
}

#[test]
fn a_transport_disconnect_frame_ends_the_stream() {
    let mut parser = ResponsesParser::new();
    let mut out = VecDeque::new();
    parser.parse("stream disconnected before completion", &mut out);

    // The consumer acts on the first `Error`, so a second one only repeats it.
    parser.parse(
        &json!({ "type": "response.output_text.delta", "delta": "after" }).to_string(),
        &mut out,
    );
    parser.finish(&mut out);

    assert_eq!(
        labels(out.make_contiguous()),
        vec!["error:stream disconnected before completion"]
    );
}

#[test]
fn an_inconsistent_argument_snapshot_ends_the_stream() {
    let mut parser = ResponsesParser::new();
    let mut out = VecDeque::new();
    parser.parse(
        &json!({
            "type": "response.function_call_arguments.delta",
            "output_index": 1,
            "item_id": "fc_1",
            "call_id": "call_1",
            "name": "bash",
            "delta": "{\"command\":\"ls\"}"
        })
        .to_string(),
        &mut out,
    );
    // A done snapshot that disagrees with what was already streamed.
    parser.parse(
        &json!({
            "type": "response.function_call_arguments.done",
            "output_index": 1,
            "item_id": "fc_1",
            "arguments": "{\"command\":\"rm\"}"
        })
        .to_string(),
        &mut out,
    );
    parser.parse(
        &json!({ "type": "response.output_text.delta", "delta": "after" }).to_string(),
        &mut out,
    );
    parser.finish(&mut out);

    // The partial call is left open in the list only because the error ended the
    // read; nothing after it may arrive.
    assert_eq!(
        labels(out.make_contiguous()),
        vec![
            "tool:call_1:bash",
            "arg:call_1:{\"command\":\"ls\"}",
            "error:OpenAI tool argument snapshot differs from streamed arguments",
        ]
    );
}

#[test]
fn a_websocket_fallback_notice_is_not_a_parse_error() {
    let mut parser = ResponsesParser::new();
    let mut out = VecDeque::new();
    parser.parse("falling back from websockets to https transport", &mut out);
    assert!(out.is_empty(), "{:?}", labels(out.make_contiguous()));

    // The stream itself is still alive after the notice.
    parser.parse(
        &json!({ "type": "response.output_text.delta", "delta": "still here" }).to_string(),
        &mut out,
    );
    parser.parse(
        &json!({ "type": "response.completed", "response": { "status": "completed" } }).to_string(),
        &mut out,
    );
    assert_eq!(
        labels(out.make_contiguous()),
        vec!["text:still here", "text-done", "end:None"]
    );
}

#[test]
fn a_lifecycle_marker_alone_produces_nothing() {
    let events = parse_all(&[json!({ "type": "response.created", "response": {} })]);

    assert!(events.is_empty(), "{:?}", labels(&events));
}

#[test]
fn nothing_follows_the_end_of_the_message() {
    let mut parser = ResponsesParser::new();
    let mut out = VecDeque::new();
    parser.parse(
        &json!({ "type": "response.completed", "response": { "status": "completed" } }).to_string(),
        &mut out,
    );
    let ended: Vec<StreamEvent> = out.drain(..).collect();
    assert_eq!(labels(&ended), vec!["end:None"]);

    parser.parse(
        &json!({ "type": "response.output_text.delta", "delta": "late" }).to_string(),
        &mut out,
    );
    parser.finish(&mut out);
    assert!(out.is_empty(), "{:?}", labels(out.make_contiguous()));
}

// ---------------------------------------------------------------------------
// Through the shared SSE stream

type Chunks = Vec<Result<bytes::Bytes, reqwest::Error>>;

fn ok(text: &str) -> Chunks {
    vec![Ok(bytes::Bytes::from(text.to_string()))]
}

#[tokio::test]
async fn a_key_quoted_in_a_server_error_is_redacted() {
    let failure = json!({
        "type": "response.failed",
        "response": { "error": { "type": "invalid_request_error", "message": format!("bad key {TEST_KEY}") } }
    });
    let stream = WireSseStream::new(
        futures::stream::iter(ok(&sse_frame(failure))),
        ResponsesParser::new(),
        "OpenAI Responses",
        Some(Arc::from(TEST_KEY)),
    );
    let mut stream: horon_agent_provider_core::EventStream = Box::pin(stream);
    let events = crate::test_support::collect(&mut stream).await;

    assert_eq!(
        labels(&events),
        vec![format!(
            "error:OpenAI Responses stream returned an error: invalid_request_error: bad key {REDACTED}"
        )],
        "one terminal error, with the key replaced"
    );
}

#[tokio::test]
async fn a_frame_split_across_chunks_is_one_payload() {
    let frame = sse_frame(json!({ "type": "response.output_text.delta", "delta": "split" }));
    let (head, tail) = frame.split_at(frame.len() / 2);
    let chunks: Chunks = vec![
        Ok(bytes::Bytes::from(head.to_string())),
        Ok(bytes::Bytes::from(tail.to_string())),
        Ok(bytes::Bytes::from(sse_frame(json!({
            "type": "response.completed",
            "response": { "status": "completed" }
        })))),
    ];
    let stream = WireSseStream::new(
        futures::stream::iter(chunks),
        ResponsesParser::new(),
        "OpenAI Responses",
        None,
    );
    let mut stream: horon_agent_provider_core::EventStream = Box::pin(stream);
    let events = crate::test_support::collect(&mut stream).await;

    assert!(!events.is_empty(), "the split frame must parse");
    assert_eq!(labels(&events), vec!["text:split", "text-done", "end:None"]);
}
