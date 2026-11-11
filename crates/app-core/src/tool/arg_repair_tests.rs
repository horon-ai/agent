use super::repair_recorded;
use crate::message::{Message, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::tool::{Registry, ToolContext, ToolExecutionMode};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// The value to run and the notes explaining each change, in the shape the
/// checks below read.
fn repair(schema: &Value, input: Value) -> (Value, Vec<String>) {
    let repaired = repair_recorded(schema, input);
    (repaired.input, repaired.changes)
}

/// The repair pass runs on every dispatch, so these are all pure-function
/// checks against the shapes a provider's JSON writer actually produces.
#[test]
fn numeric_strings_become_numbers() {
    let schema = json!({
        "type": "object",
        "properties": { "timeout": { "type": "integer" } },
        "required": ["timeout"]
    });
    let (repaired, changes) = repair(&schema, json!({ "timeout": "5000" }));
    assert_eq!(repaired, json!({ "timeout": 5000 }));
    assert_eq!(changes.len(), 1, "{changes:?}");
}

#[test]
fn integer_targets_reject_fractional_literals() {
    let schema = json!({
        "type": "object",
        "properties": { "count": { "type": "integer" } },
        "required": ["count"]
    });
    let input = json!({ "count": "1.5" });
    let (repaired, changes) = repair(&schema, input.clone());
    assert_eq!(repaired, input, "a fractional value cannot satisfy an integer");
    assert!(changes.is_empty());
}

#[test]
fn boolean_strings_and_numbers_become_booleans() {
    let schema = json!({
        "type": "object",
        "properties": {
            "force": { "type": "boolean" },
            "quiet": { "type": "boolean" }
        },
        "required": ["force", "quiet"]
    });
    let (repaired, _) = repair(&schema, json!({ "force": "yes", "quiet": 0 }));
    assert_eq!(repaired, json!({ "force": true, "quiet": false }));
}

#[test]
fn objects_and_arrays_encoded_as_strings_are_parsed() {
    let schema = json!({
        "type": "object",
        "properties": {
            "config": { "type": "object", "properties": { "a": { "type": "string" } } },
            "tags": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["config", "tags"]
    });
    let (repaired, _) = repair(
        &schema,
        json!({ "config": "{\"a\":\"x\"}", "tags": "[\"one\",\"two\"]" }),
    );
    assert_eq!(repaired["config"], json!({ "a": "x" }));
    assert_eq!(repaired["tags"], json!(["one", "two"]));
}

#[test]
fn double_encoded_containers_are_unwrapped() {
    let schema = json!({
        "type": "object",
        "properties": { "tags": { "type": "array", "items": { "type": "string" } } },
        "required": ["tags"]
    });
    let (repaired, _) = repair(&schema, json!({ "tags": "\"[\\\"a\\\"]\"" }));
    assert_eq!(repaired["tags"], json!(["a"]));
}

#[test]
fn a_scalar_matching_the_item_type_is_wrapped_in_an_array() {
    let schema = json!({
        "type": "object",
        "properties": { "paths": { "type": "array", "items": { "type": "string" } } },
        "required": ["paths"]
    });
    let (repaired, _) = repair(&schema, json!({ "paths": "src/lib.rs" }));
    assert_eq!(repaired, json!({ "paths": ["src/lib.rs"] }));
}

#[test]
fn a_scalar_of_the_wrong_item_type_is_not_wrapped() {
    let schema = json!({
        "type": "object",
        "properties": { "counts": { "type": "array", "items": { "type": "integer" } } },
        "required": ["counts"]
    });
    let input = json!({ "counts": "nope" });
    let (repaired, changes) = repair(&schema, input.clone());
    assert_eq!(repaired, input);
    assert!(changes.is_empty());
}

#[test]
fn null_optional_properties_are_dropped() {
    let schema = json!({
        "type": "object",
        "properties": {
            "command": { "type": "string" },
            "timeout": { "type": "integer" }
        },
        "required": ["command"]
    });
    let (repaired, changes) = repair(&schema, json!({ "command": "ls", "timeout": "null" }));
    assert_eq!(repaired, json!({ "command": "ls" }));
    assert_eq!(changes.len(), 1, "{changes:?}");
}

#[test]
fn null_required_properties_are_left_alone() {
    let schema = json!({
        "type": "object",
        "properties": { "command": { "type": "string" } },
        "required": ["command"]
    });
    let input = json!({ "command": null });
    let (repaired, _) = repair(&schema, input.clone());
    assert_eq!(repaired, input);
}

#[test]
fn quoted_root_keys_are_unquoted() {
    let schema = json!({
        "type": "object",
        "properties": { "op": { "type": "string" } },
        "required": ["op"]
    });
    let (repaired, _) = repair(&schema, json!({ "\"op\"": "read" }));
    assert_eq!(repaired, json!({ "op": "read" }));
}

#[test]
fn flattened_bracket_keys_are_rebuilt() {
    let schema = json!({
        "type": "object",
        "properties": {
            "items": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "count": { "type": "integer" }
                    },
                    "required": ["name"]
                }
            }
        },
        "required": ["items"]
    });
    let (repaired, _) = repair(
        &schema,
        json!({
            "items[0].name": "first",
            "items[1].name": "second",
            "items[1].count": "2"
        }),
    );
    assert_eq!(
        repaired,
        json!({
            "items": [
                { "name": "first" },
                { "name": "second", "count": 2 }
            ]
        })
    );
}

#[test]
fn enum_values_are_trimmed() {
    let schema = json!({
        "type": "object",
        "properties": { "mode": { "type": "string", "enum": ["fast", "slow"] } },
        "required": ["mode"]
    });
    let (repaired, _) = repair(&schema, json!({ "mode": " fast " }));
    assert_eq!(repaired, json!({ "mode": "fast" }));
}

#[test]
fn unrepaired_input_is_returned_untouched() {
    let schema = json!({
        "type": "object",
        "properties": { "command": { "type": "string" } },
        "required": ["command"]
    });
    let input = json!({ "command": 7 });
    let (repaired, changes) = repair(&schema, input.clone());
    assert_eq!(repaired, input, "a repair the schema cannot prove is not applied");
    assert!(changes.is_empty());
}

#[test]
fn valid_input_is_not_rewritten() {
    let schema = json!({
        "type": "object",
        "properties": { "timeout": { "type": "integer" } },
        "required": ["timeout"]
    });
    let input = json!({ "timeout": 5000 });
    let (repaired, changes) = repair(&schema, input.clone());
    assert_eq!(repaired, input);
    assert!(changes.is_empty());
}

#[test]
fn non_object_schemas_are_skipped() {
    let input = json!({ "anything": "goes" });
    let (repaired, changes) = repair(&json!({ "type": "string" }), input.clone());
    assert_eq!(repaired, input);
    assert!(changes.is_empty());
}

struct StubProvider;

#[async_trait]
impl Provider for StubProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        Err(anyhow::anyhow!("the stub provider never completes a request"))
    }

    fn name(&self) -> &str {
        "stub"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(StubProvider)
    }
}

fn context(working_dir: &std::path::Path) -> ToolContext {
    ToolContext {
        session_id: "arg-repair-dispatch".to_string(),
        message_id: "arg-repair-dispatch-message".to_string(),
        tool_call_id: "arg-repair-dispatch-call".to_string(),
        working_dir: Some(working_dir.to_path_buf()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

/// The repair has to happen on the real dispatch path, not just in the helper:
/// a quoted timeout used to fail bash's deserialization.
#[tokio::test]
async fn dispatch_repairs_a_quoted_bash_timeout() {
    let directory = tempfile::tempdir().expect("a temp working directory");
    let registry = Registry::new(Arc::new(StubProvider)).await;
    let output = registry
        .execute(
            "bash",
            json!({ "command": "echo hi", "timeout": "5000" }),
            context(directory.path()),
        )
        .await
        .expect("a quoted timeout must be repaired, not rejected");
    assert!(output.output.contains("hi"), "{}", output.output);
}
