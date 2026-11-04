//! Behaviour of the `yield` tool and the slot the completion path drains.

use super::{Completion, YieldTool, publish_completion, take_completion, validate_schema};
use crate::tool::{Registry, Tool, ToolContext, ToolExecutionMode};
use serde_json::{Value, json};
use std::sync::Arc;

const FILES_SCHEMA: &str = r#"{
    "type": "object",
    "properties": { "files": { "type": "array", "items": { "type": "string" } } },
    "required": ["files"]
}"#;

fn schema() -> Value {
    serde_json::from_str(FILES_SCHEMA).expect("the fixture schema parses")
}

fn ctx(session_id: &str) -> ToolContext {
    ToolContext {
        session_id: session_id.to_string(),
        message_id: "test-message".to_string(),
        tool_call_id: "test-call".to_string(),
        working_dir: Some(std::env::temp_dir()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

fn completion_details(session_id: &str) -> Option<Value> {
    match take_completion(session_id) {
        Some(Completion::Yielded { details }) => Some(details),
        _ => None,
    }
}

#[tokio::test]
async fn valid_data_is_recorded_with_success_details() {
    let session = "yield-valid";
    super::prepare(session, schema());
    let tool = YieldTool::new(&schema());

    let output = tool
        .execute(json!({"data": {"files": ["a"]}}), ctx(session))
        .await
        .expect("a schema-valid result is accepted");
    assert_eq!(output.output, "Result recorded.");

    let details = completion_details(session).expect("the result is parked in the slot");
    assert_eq!(
        details,
        json!({"files": ["a"], "status": "success", "schema_overridden": false})
    );
}

#[tokio::test]
async fn error_submission_is_recorded_without_schema_validation() {
    let session = "yield-error";
    super::prepare(session, schema());
    let tool = YieldTool::new(&schema());

    let output = tool
        .execute(json!({"error": "could not finish"}), ctx(session))
        .await
        .expect("an error result is accepted");
    assert_eq!(output.output, "Recorded failure result.");

    let details = completion_details(session).expect("the failure is parked in the slot");
    assert_eq!(
        details,
        json!({"status": "error", "error": "could not finish", "schema_overridden": false})
    );
}

#[tokio::test]
async fn exactly_one_of_data_and_error_is_required() {
    let session = "yield-both";
    super::prepare(session, schema());
    let tool = YieldTool::new(&schema());

    for input in [json!({}), json!({"data": {"files": []}, "error": "no"})] {
        let error = tool
            .execute(input, ctx(session))
            .await
            .expect_err("neither or both of data/error is refused");
        assert_eq!(
            error.to_string(),
            "Submit success as {\"data\": <your output>} or failure as {\"error\": \"message\"}."
        );
    }
}

#[tokio::test]
async fn sections_accumulate_array_parts_into_the_result() {
    let session = "yield-sections";
    super::prepare(session, schema());
    let tool = YieldTool::new(&schema());

    let first = tool
        .execute(json!({"section": "files", "data": ["a"]}), ctx(session))
        .await
        .expect("the first part is recorded");
    assert_eq!(
        first.output,
        "Recorded section 'files'. The assembled result matches the output schema and has been submitted."
    );

    // The slot already holds a valid result; a second part replaces the
    // terminal value with the assembled array.
    let second = tool
        .execute(json!({"section": "files", "data": ["b", "c"]}), ctx(session))
        .await
        .expect("the second part is recorded");
    assert!(second.output.starts_with("Recorded section 'files'."));

    let details = completion_details(session).expect("the assembled result is parked");
    assert_eq!(details["files"], json!(["a", "b", "c"]));
}

#[tokio::test]
async fn a_section_with_missing_required_fields_reports_them() {
    let session = "yield-section-missing";
    let schema: Value = json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "files": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["summary", "files"]
    });
    super::prepare(session, schema.clone());
    let tool = YieldTool::new(&schema);

    let output = tool
        .execute(json!({"section": "files", "data": ["a"]}), ctx(session))
        .await
        .expect("a partial part is recorded, not rejected");
    assert_eq!(
        output.output,
        "Recorded section 'files'. Required fields still missing: summary."
    );

    tool.execute(json!({"section": "summary", "data": "done"}), ctx(session))
        .await
        .expect("the final part completes the object");
    let details = completion_details(session).expect("the assembled result is parked");
    assert_eq!(details["summary"], json!("done"));
    assert_eq!(details["files"], json!(["a"]));
}

#[tokio::test]
async fn unknown_section_lists_the_valid_sections() {
    let session = "yield-unknown-section";
    super::prepare(session, schema());
    let tool = YieldTool::new(&schema());

    let error = tool
        .execute(json!({"section": "paths", "data": []}), ctx(session))
        .await
        .expect_err("an unknown section is refused");
    assert_eq!(error.to_string(), "Unknown section 'paths'; valid sections: files");
    super::discard(session);
}

#[tokio::test]
async fn validation_failures_retry_three_times_then_accept_with_override() {
    let session = "yield-retries";
    super::prepare(session, schema());
    let tool = YieldTool::new(&schema());

    for remaining in [3, 2, 1] {
        let error = tool
            .execute(json!({"data": {"files": "not-an-array"}}), ctx(session))
            .await
            .expect_err("an invalid result is rejected while retries remain");
        let text = error.to_string();
        assert!(text.starts_with("Result does not match the output schema: "), "{text}");
        assert!(
            text.ends_with(&format!(
                "Call yield again with the corrected shape — {remaining} retry attempt(s) remain before the schema is dropped."
            )),
            "{text}"
        );
    }

    tool.execute(json!({"data": {"files": "not-an-array"}}), ctx(session))
        .await
        .expect("the fourth invalid submission is accepted");
    let details = completion_details(session).expect("the overridden result is parked");
    assert_eq!(details["schema_overridden"], json!(true));
    assert_eq!(details["status"], json!("success"));
}

#[tokio::test]
async fn yield_without_a_slot_is_refused() {
    let tool = YieldTool::new(&schema());
    let error = tool
        .execute(json!({"data": {"files": []}}), ctx("yield-no-slot"))
        .await
        .expect_err("a session with no spawn schema cannot yield");
    assert!(
        error
            .to_string()
            .contains("only available to a child spawned with an output schema"),
        "{error}"
    );
}

#[test]
fn non_object_schemas_are_rejected_with_the_spawn_message() {
    let error = validate_schema(&json!(["not", "an", "object"])).expect_err("an array schema is refused");
    assert!(
        error
            .to_string()
            .starts_with("output_schema must be a JSON Schema object: "),
        "{error}"
    );
    validate_schema(&json!({"type": "object"})).expect("an object schema compiles");
}

#[tokio::test]
async fn isolated_registry_keeps_yield_off_the_parent_and_publishes_fields() {
    let parent_session = "yield-parent";
    let child_session = "yield-child";

    let parent = Registry::empty();
    parent
        .register(
            "read".to_string(),
            Arc::new(crate::tool::read::ReadTool::new()) as Arc<dyn Tool>,
        )
        .await;
    let child_registry = parent.with_isolated_tools().await;
    assert!(
        !parent.tool_names().await.iter().any(|name| name == super::YIELD_TOOL),
        "the parent must not see a child-only tool"
    );

    super::prepare(child_session, schema());
    child_registry
        .register(
            super::YIELD_TOOL.to_string(),
            Arc::new(YieldTool::new(&schema())) as Arc<dyn Tool>,
        )
        .await;

    // Dispatch through the registry, the path a model's tool call takes.
    child_registry
        .execute(
            super::YIELD_TOOL,
            json!({"intent": "submitting the result", "data": {"files": ["a"]}}),
            ctx(child_session),
        )
        .await
        .expect("the isolated registry executes yield");

    publish_completion(child_session, parent_session, "the child's final words");

    let projected = parent
        .execute(
            "read",
            json!({"intent": "qa", "file_path": format!("agent://{child_session}/files/0")}),
            ctx(parent_session),
        )
        .await
        .expect("the parent reads the structured field");
    assert_eq!(projected.output, "a");

    let structured = parent
        .execute(
            "read",
            json!({"intent": "qa", "file_path": format!("agent://{child_session}/status")}),
            ctx(parent_session),
        )
        .await
        .expect("the parent reads the structured status");
    assert_eq!(structured.output, "success");

    let text = parent
        .execute(
            "read",
            json!({"intent": "qa", "file_path": format!("agent://{child_session}")}),
            ctx(parent_session),
        )
        .await
        .expect("the parent reads the published text");
    assert_eq!(text.output, "the child's final words");
    assert!(!text.output.contains("warning: child finished without calling yield"));
}

#[tokio::test]
async fn missing_yield_appends_the_unstructured_warning() {
    let parent_session = "yield-warning-parent";
    let child_session = "yield-warning-child";

    super::prepare(child_session, schema());
    publish_completion(child_session, parent_session, "plain prose");

    let registry = Registry::empty();
    registry
        .register(
            "read".to_string(),
            Arc::new(crate::tool::read::ReadTool::new()) as Arc<dyn Tool>,
        )
        .await;
    let text = registry
        .execute(
            "read",
            json!({"intent": "qa", "file_path": format!("agent://{child_session}")}),
            ctx(parent_session),
        )
        .await
        .expect("the warning is readable");
    assert_eq!(
        text.output,
        "plain prose\n\n[warning: child finished without calling yield; result is unstructured]"
    );
}
