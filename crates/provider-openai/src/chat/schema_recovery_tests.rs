//! End-to-end recovery from a provider that rejects a tool-schema construct.
//!
//! The classifier's unit tests cover what a response means; these tests drive a
//! real loopback endpoint so the retry, the rebuilt body, the degradation
//! receipt and the surfaced error are proven together.

use super::tests::{MockServer, Reply, collect, provider};
use horon_agent_message_types::degradation::DegradationKind;
use horon_agent_message_types::{Message, StreamEvent, ToolDefinition};
use horon_agent_provider_core::{Provider, ProviderRequestContext};
use horon_agent_schema_dialect::quirks;
use serde_json::json;

/// A tool whose schema the OpenAI allow-list accepts but which is not
/// strict-eligible: `free` declares only a description, so strict mode is off
/// and `pattern` stays on the wire where the provider can reject it.
fn search_tool() -> ToolDefinition {
    ToolDefinition {
        name: "search".to_string(),
        description: "Search.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "q": { "type": "string", "pattern": "^a" },
                "free": { "description": "anything" }
            }
        }),
        defer_loading: false,
    }
}

/// Redirect the learned-quirk store to `dir` and forget anything learned before,
/// so one test cannot teach another. The redirect is thread-local, and the test
/// harness gives every test its own thread.
fn isolate(dir: &tempfile::TempDir) {
    quirks::use_test_path(dir.path().join("schema-quirks.json"));
    quirks::forget("openai");
}

fn bad_request(body: &str) -> Reply {
    Reply::status(400, &format!("{{\"error\":{{\"message\":{}}}}}", json!(body)))
}

fn context(session: &str) -> ProviderRequestContext {
    ProviderRequestContext {
        session_id: Some(session.to_string()),
        ..Default::default()
    }
}

/// A rejection naming a construct the dialect has not learned yet is learned,
/// the request is rebuilt without it, and the loss is recorded as a receipt.
#[tokio::test]
async fn a_rejected_keyword_is_learned_and_the_retried_request_omits_it() {
    let dir = tempfile::tempdir().expect("temporary quirk store");
    isolate(&dir);
    let session = "schema-recovery-learn-and-retry";
    let _ = horon_agent_provider_core::degradation::take_pending(session);

    let rejection = "Invalid schema for function 'search': In context=('properties', 'q'), 'pattern' is not permitted.";
    let server = MockServer::start_sequence(vec![bad_request(rejection), Reply::sse(&["data: [DONE]\n\n"])]);
    let provider = provider(&format!("{}/v1", server.url));

    let mut stream = provider
        .complete_with_context(
            &[Message::user("hello")],
            &[search_tool()],
            "system",
            None,
            &context(session),
        )
        .await
        .expect("the retry succeeds");

    let requests = server.requests();
    assert_eq!(requests.len(), 2, "one rejected request and one retry");
    assert!(
        requests[0].body.contains("\"pattern\""),
        "the first request carries the construct: {}",
        requests[0].body
    );
    assert!(
        !requests[1].body.contains("\"pattern\""),
        "the rebuilt request drops what was learned: {}",
        requests[1].body
    );

    let receipts = horon_agent_provider_core::degradation::take_pending(session);
    assert!(
        receipts
            .iter()
            .any(|receipt| receipt.kind == DegradationKind::SchemaKeywordDropped
                && receipt.tool == "search"
                && receipt.target == "pattern"),
        "the retry records what the schema lost: {receipts:?}"
    );

    let events = collect(&mut stream).await;
    assert!(
        matches!(events.last(), Some(StreamEvent::MessageEnd { .. })),
        "the retried stream completes: {events:?}"
    );
}

/// A rejection that names nothing strippable is reported on the first failure,
/// not retried: prevention already declined strict mode for this shape, so a
/// second identical request cannot help.
#[tokio::test]
async fn a_structural_rejection_is_reported_without_a_retry() {
    let dir = tempfile::tempdir().expect("temporary quirk store");
    isolate(&dir);
    let session = "schema-recovery-structural";

    let rejection = "Invalid schema for function 'search': schema must have a 'type' key.";
    let server = MockServer::start_sequence(vec![bad_request(rejection), Reply::sse(&["data: [DONE]\n\n"])]);
    let provider = provider(&format!("{}/v1", server.url));

    let error = provider
        .complete_with_context(
            &[Message::user("hello")],
            &[search_tool()],
            "system",
            None,
            &context(session),
        )
        .await
        .err()
        .expect("a 400 is an error");

    assert_eq!(server.requests().len(), 1, "an unactionable rejection is not retried");
    let message = format!("{error:#}");
    assert!(message.contains("HTTP 400"), "the status marker survives: {message}");
    assert!(
        message.contains("naming no actionable construct"),
        "the hint says why a retry cannot help: {message}"
    );
}

/// A failure that is not about tool schemas must not teach the dialect
/// anything, so a later request is byte-identical to the one before it.
#[tokio::test]
async fn a_non_schema_failure_teaches_nothing() {
    let dir = tempfile::tempdir().expect("temporary quirk store");
    isolate(&dir);
    let session = "schema-recovery-non-schema";

    let server = MockServer::start_sequence(vec![bad_request("the model does not exist")]);
    let provider = provider(&format!("{}/v1", server.url));

    let error = provider
        .complete_with_context(
            &[Message::user("hello")],
            &[search_tool()],
            "system",
            None,
            &context(session),
        )
        .await
        .err()
        .expect("a 400 is an error");

    assert_eq!(server.requests().len(), 1);
    assert!(quirks::learned_for("openai").is_empty(), "nothing was learned");
    assert!(
        !format!("{error:#}").contains("tool schema"),
        "no schema hint is invented"
    );
}

/// A server error whose body happens to name a schema keyword is a transient
/// provider condition, not a schema rejection: learning from it would strip the
/// construct from every later request for the wrong reason.
#[tokio::test]
async fn a_server_error_that_mentions_a_keyword_teaches_nothing() {
    let dir = tempfile::tempdir().expect("temporary quirk store");
    isolate(&dir);
    let session = "schema-recovery-server-error";

    let body = "Invalid schema for function 'search': 'pattern' is not permitted.";
    let server = MockServer::start_sequence(vec![Reply::status(
        500,
        &format!("{{\"error\":{{\"message\":{}}}}}", json!(body)),
    )]);
    let provider = provider(&format!("{}/v1", server.url));

    let error = provider
        .complete_with_context(
            &[Message::user("hello")],
            &[search_tool()],
            "system",
            None,
            &context(session),
        )
        .await
        .err()
        .expect("a 500 is an error");

    assert_eq!(server.requests().len(), 1, "a 5xx is never retried for schema recovery");
    assert!(
        quirks::learned_for("openai").is_empty(),
        "a transient failure teaches nothing"
    );
    assert!(format!("{error:#}").contains("HTTP 500"));
}
