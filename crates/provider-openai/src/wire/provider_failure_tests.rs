//! Failure paths of the three wires beside Chat Completions.
//!
//! Each test drives the real provider against the loopback fake, so the label a
//! failure carries, the credential redaction, the server hint it reports and the
//! retry the schema-recovery layer performs are all observed on the wire rather
//! than read back from a builder call.

use super::WireProtocol;
use crate::chat::OpenAiCompatibleProvider;
use crate::chat::transport::ChatAuth;
use crate::test_support::{DEADLINE, MockServer, Reply, TEST_KEY, collect, sse_frame};
use horon_agent_message_types::{Message, StreamEvent, ToolDefinition};
use horon_agent_provider_core::Provider;
use horon_agent_schema_dialect::quirks;
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Helpers

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

/// The model name each wire's fixtures below use.
fn model_for(wire: WireProtocol) -> &'static str {
    match wire {
        WireProtocol::ChatCompletions => "gpt-5-mini",
        WireProtocol::Responses => "gpt-5",
        WireProtocol::AnthropicMessages => "claude-opus-4-7",
        WireProtocol::GeminiGenerateContent => "gemini-3-pro",
    }
}

fn recorded_body(server: &MockServer, index: usize) -> Value {
    let requests = server.requests();
    let request = requests
        .get(index)
        .unwrap_or_else(|| panic!("request {index} was not recorded"));
    serde_json::from_str(&request.body).expect("a recorded request body is JSON")
}

/// A body that ends the stream cleanly with no content.
fn closing_reply(wire: WireProtocol) -> Reply {
    let frame = match wire {
        WireProtocol::ChatCompletions => "data: [DONE]\n\n".to_string(),
        WireProtocol::Responses => sse_frame(json!({ "type": "response.completed", "response": {} })),
        WireProtocol::AnthropicMessages => sse_frame(json!({ "type": "message_stop" })),
        WireProtocol::GeminiGenerateContent => sse_frame(json!({ "candidates": [{ "finishReason": "STOP" }] })),
    };
    Reply::sse(&[&frame])
}

/// Frames that carry a text delta and then stop, so the parser reaches its
/// end-of-body path without a terminal event.
fn truncated_frames(wire: WireProtocol) -> Vec<String> {
    match wire {
        WireProtocol::ChatCompletions => vec![sse_frame(json!({ "choices": [{ "delta": { "content": "Hel" } }] }))],
        WireProtocol::Responses => vec![sse_frame(
            json!({ "type": "response.output_text.delta", "delta": "Hel" }),
        )],
        WireProtocol::AnthropicMessages => vec![
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
        ],
        WireProtocol::GeminiGenerateContent => vec![sse_frame(
            json!({ "candidates": [{ "content": { "parts": [{ "text": "Hel" }] } }] }),
        )],
    }
}

/// One in-stream error frame that asks for a seven second wait.
fn rate_limit_frame(wire: WireProtocol) -> String {
    match wire {
        WireProtocol::ChatCompletions => sse_frame(json!({
            "error": { "message": "slow down" },
            "retry_after_seconds": 7
        })),
        WireProtocol::Responses => sse_frame(json!({
            "type": "response.failed",
            "response": { "error": { "message": "slow down" } },
            "retry_after_seconds": 7
        })),
        WireProtocol::AnthropicMessages => sse_frame(json!({
            "type": "error",
            "error": { "type": "overloaded_error", "message": "slow down" },
            "retry_after_seconds": 7
        })),
        WireProtocol::GeminiGenerateContent => sse_frame(json!({
            "error": { "message": "slow down" },
            "retry_after_seconds": 7
        })),
    }
}

/// The status message each wire's label produces for `status`.
fn status_marker(wire: WireProtocol, status: u16) -> String {
    let kind = match status {
        401 => "authentication failed",
        429 => "rate limit exceeded",
        500 => "server error",
        _ => "request failed",
    };
    format!("{} provider {kind} (HTTP {status})", wire.label())
}

/// A tool carrying `keyword` on one property, plus the rejection text a provider
/// uses when it refuses that construct.
///
/// `free` declares only a description, which keeps the tool out of OpenAI's
/// strict subset: a strict-eligible schema folds an unsupported keyword into the
/// description instead of sending it, and then there would be nothing on the
/// wire for a rejection to name.
fn rejected_tool(keyword: &str, value: Value) -> (ToolDefinition, String) {
    let schema = json!({
        "type": "object",
        "properties": {
            "q": { "type": "string", keyword: value },
            "free": { "description": "anything" },
        },
    });
    let tool = ToolDefinition::new("search", "search tool", schema);
    let rejection =
        format!("Invalid schema for function 'search': In context=('properties', 'q'), '{keyword}' is not permitted.");
    (tool, rejection)
}

/// The tool parameters a wire put on the wire, for the one tool these tests send.
fn sent_parameters(body: &Value, wire: WireProtocol) -> &Value {
    match wire {
        WireProtocol::Responses => &body["tools"][0]["parameters"],
        WireProtocol::AnthropicMessages => &body["tools"][0]["input_schema"],
        WireProtocol::GeminiGenerateContent => &body["tools"][0]["functionDeclarations"][0]["parameters"],
        WireProtocol::ChatCompletions => &body["tools"][0]["function"]["parameters"],
    }
}

/// Point the learned-quirk store at `dir` and forget anything learned before, so
/// one test cannot teach another.
fn isolate(dir: &tempfile::TempDir, dialect: &str) {
    quirks::use_test_path(dir.path().join("schema-quirks.json"));
    quirks::forget(dialect);
}

// ---------------------------------------------------------------------------
// (a) A rejected credential is reported without the credential

async fn assert_401_redacts_the_key(wire: WireProtocol) {
    let server = MockServer::start(Reply::status(
        401,
        &format!("upstream rejected the request with the key {TEST_KEY}"),
    ));
    let provider = wire_provider(&format!("{}/v1", server.url), wire, model_for(wire));
    let error = provider
        .complete(&[Message::user("hi")], &[], "system", None)
        .await
        .err()
        .expect("an HTTP failure is an error");
    let message = error.to_string();

    assert!(message.contains(&status_marker(wire, 401)), "{message}");
    assert!(!message.contains(TEST_KEY), "the credential leaked into {message}");
    assert!(message.contains("[redacted]"), "{message}");
    assert_eq!(server.requests().len(), 1, "a credential failure is not retried");
}

#[tokio::test]
async fn responses_401_redacts_the_key() {
    tokio::time::timeout(DEADLINE, assert_401_redacts_the_key(WireProtocol::Responses))
        .await
        .expect("the 401 check finishes within the deadline");
}

#[tokio::test]
async fn anthropic_401_redacts_the_key() {
    tokio::time::timeout(DEADLINE, assert_401_redacts_the_key(WireProtocol::AnthropicMessages))
        .await
        .expect("the 401 check finishes within the deadline");
}

#[tokio::test]
async fn gemini_401_redacts_the_key() {
    tokio::time::timeout(
        DEADLINE,
        assert_401_redacts_the_key(WireProtocol::GeminiGenerateContent),
    )
    .await
    .expect("the 401 check finishes within the deadline");
}

// ---------------------------------------------------------------------------
// (b) A rate limit reports both the labelled status and the server's hint

async fn assert_429_reports_retry_after(wire: WireProtocol) {
    let limited = MockServer::start(Reply::status(
        429,
        &json!({ "error": { "message": "slow down" }, "retry_after_seconds": 7 }).to_string(),
    ));
    let provider = wire_provider(&format!("{}/v1", limited.url), wire, model_for(wire));
    let error = provider
        .complete(&[Message::user("hi")], &[], "system", None)
        .await
        .err()
        .expect("a 429 is an error");
    let message = error.to_string();
    assert!(message.contains(&status_marker(wire, 429)), "{message}");
    assert!(
        message.contains("retry_after_seconds"),
        "the server hint survives: {message}"
    );

    // The delay itself is reported on the stream event, which is the only path
    // the fake endpoint can exercise: its replies carry no custom headers.
    let hinted = MockServer::start(Reply::sse(&[&rate_limit_frame(wire)]));
    let provider = wire_provider(&format!("{}/v1", hinted.url), wire, model_for(wire));
    let mut stream = provider
        .complete(&[Message::user("hi")], &[], "system", None)
        .await
        .expect("the stream opens");
    let events = collect(&mut stream).await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            StreamEvent::Error {
                retry_after_secs: Some(7),
                ..
            }
        )),
        "the wire reports the delay it was given: {events:?}"
    );
}

#[tokio::test]
async fn responses_429_reports_retry_after() {
    tokio::time::timeout(DEADLINE, assert_429_reports_retry_after(WireProtocol::Responses))
        .await
        .expect("the 429 check finishes within the deadline");
}

#[tokio::test]
async fn anthropic_429_reports_retry_after() {
    tokio::time::timeout(
        DEADLINE,
        assert_429_reports_retry_after(WireProtocol::AnthropicMessages),
    )
    .await
    .expect("the 429 check finishes within the deadline");
}

#[tokio::test]
async fn gemini_429_reports_retry_after() {
    tokio::time::timeout(
        DEADLINE,
        assert_429_reports_retry_after(WireProtocol::GeminiGenerateContent),
    )
    .await
    .expect("the 429 check finishes within the deadline");
}

// ---------------------------------------------------------------------------
// (c) A 5xx names the wire and the endpoint it reached

async fn assert_500_reports_server_error(wire: WireProtocol) {
    let server = MockServer::start(Reply::status(500, "{\"error\":{\"message\":\"boom\"}}"));
    let base = format!("{}/v1", server.url);
    let provider = wire_provider(&base, wire, model_for(wire));
    let error = provider
        .complete(&[Message::user("hi")], &[], "system", None)
        .await
        .err()
        .expect("a 500 is an error");
    let message = error.to_string();

    assert!(message.contains(&status_marker(wire, 500)), "{message}");
    assert!(
        message.contains(&wire.endpoint(&base, model_for(wire))),
        "the endpoint is named: {message}"
    );
    assert_eq!(server.requests().len(), 1, "a server error is not retried here");
}

#[tokio::test]
async fn responses_500_reports_server_error() {
    tokio::time::timeout(DEADLINE, assert_500_reports_server_error(WireProtocol::Responses))
        .await
        .expect("the 500 check finishes within the deadline");
}

#[tokio::test]
async fn anthropic_500_reports_server_error() {
    tokio::time::timeout(
        DEADLINE,
        assert_500_reports_server_error(WireProtocol::AnthropicMessages),
    )
    .await
    .expect("the 500 check finishes within the deadline");
}

#[tokio::test]
async fn gemini_500_reports_server_error() {
    tokio::time::timeout(
        DEADLINE,
        assert_500_reports_server_error(WireProtocol::GeminiGenerateContent),
    )
    .await
    .expect("the 500 check finishes within the deadline");
}

// ---------------------------------------------------------------------------
// (d) A body that ends before the terminal event is truncation, not completion

async fn assert_eof_reports_truncation(wire: WireProtocol) {
    let frames = truncated_frames(wire);
    let parts: Vec<&str> = frames.iter().map(String::as_str).collect();
    let server = MockServer::start(Reply::sse(&parts));
    let provider = wire_provider(&format!("{}/v1", server.url), wire, model_for(wire));
    let mut stream = provider
        .complete(&[Message::user("hi")], &[], "system", None)
        .await
        .expect("the stream opens");
    let events = collect(&mut stream).await;

    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta(text) if text == "Hel")),
        "the frames before the cut still arrive: {events:?}"
    );
    assert!(
        matches!(events.last(), Some(StreamEvent::Error { message, .. })
            if message.contains(&format!("{} stream ended before the response finished", wire.label()))),
        "the cut is reported as truncation: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StreamEvent::MessageEnd { .. })),
        "a truncated stream must not look complete: {events:?}"
    );
}

#[tokio::test]
async fn responses_eof_reports_truncation() {
    tokio::time::timeout(DEADLINE, assert_eof_reports_truncation(WireProtocol::Responses))
        .await
        .expect("the truncation check finishes within the deadline");
}

#[tokio::test]
async fn anthropic_eof_reports_truncation() {
    tokio::time::timeout(DEADLINE, assert_eof_reports_truncation(WireProtocol::AnthropicMessages))
        .await
        .expect("the truncation check finishes within the deadline");
}

#[tokio::test]
async fn gemini_eof_reports_truncation() {
    tokio::time::timeout(
        DEADLINE,
        assert_eof_reports_truncation(WireProtocol::GeminiGenerateContent),
    )
    .await
    .expect("the truncation check finishes within the deadline");
}

// ---------------------------------------------------------------------------
// (e) A rejected tool-schema construct costs one rebuilt request, not the turn

async fn assert_schema_rejection_retries_once(wire: WireProtocol) {
    let dir = tempfile::tempdir().expect("temporary quirk store");
    isolate(&dir, wire.dialect().id);

    // `uniqueItems` and `minItems` survive each dialect's prevention pass, so
    // the rejection reaches the recovery layer with the construct still on the
    // wire - which is the only shape a retry can change.
    let (keyword, value) = match wire {
        WireProtocol::AnthropicMessages => ("uniqueItems", json!(true)),
        _ => ("minItems", json!(1)),
    };
    let (tool, rejection) = rejected_tool(keyword, value);
    let bad_request = Reply::status(400, &json!({ "error": { "message": rejection } }).to_string());
    let server = MockServer::start_sequence(vec![bad_request, closing_reply(wire)]);
    let provider = wire_provider(&format!("{}/v1", server.url), wire, model_for(wire));

    let mut stream = provider
        .complete(&[Message::user("hi")], &[tool], "system", None)
        .await
        .expect("the retry succeeds");
    let events = collect(&mut stream).await;

    let requests = server.requests();
    assert_eq!(requests.len(), 2, "one rejected request and one retry: {requests:?}");
    assert!(
        recorded_body(&server, 0)["tools"].to_string().contains(keyword),
        "the first request carries the construct: {}",
        requests[0].body
    );
    let retried = recorded_body(&server, 1);
    assert!(
        !sent_parameters(&retried, wire).to_string().contains(keyword),
        "the rebuilt request drops what the rejection named: {retried}"
    );
    assert!(
        matches!(events.last(), Some(StreamEvent::MessageEnd { .. })),
        "the retried stream completes: {events:?}"
    );
}

#[tokio::test]
async fn responses_schema_rejection_retries_once() {
    tokio::time::timeout(DEADLINE, assert_schema_rejection_retries_once(WireProtocol::Responses))
        .await
        .expect("the schema recovery check finishes within the deadline");
}

#[tokio::test]
async fn anthropic_schema_rejection_retries_once() {
    tokio::time::timeout(
        DEADLINE,
        assert_schema_rejection_retries_once(WireProtocol::AnthropicMessages),
    )
    .await
    .expect("the schema recovery check finishes within the deadline");
}

#[tokio::test]
async fn gemini_schema_rejection_retries_once() {
    tokio::time::timeout(
        DEADLINE,
        assert_schema_rejection_retries_once(WireProtocol::GeminiGenerateContent),
    )
    .await
    .expect("the schema recovery check finishes within the deadline");
}
