//! `yield`: the structured result channel for a spawned child agent.
//!
//! A spawn that names an `output_schema` installs this tool on the child's
//! isolated registry and registers a slot keyed by the child session id. The
//! child calls the tool when its work is done; the tool validates what it
//! submits against that schema and parks the result in the slot. The swarm
//! completion path drains the slot and publishes the structured details on
//! `agent://`, so the coordinator can read `agent://<child>/<field>` instead of
//! re-parsing prose.
//!
//! Nothing here is persisted: a slot lives exactly as long as the child's first
//! completion. The tool is registered on the child's own registry, which the
//! spawn path keeps private to that session; a caller that only holds a shared
//! registry takes [`crate::tool::Registry::with_isolated_tools`] first so a
//! child-only tool cannot leak into the parent and every sibling.

use super::{Tool, ToolContext, ToolEffects, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};

/// Registry name of the tool.
pub(crate) const YIELD_TOOL: &str = "yield";

/// Validation failures a child may collect before its result is accepted
/// unstructured. The first rejection still leaves three retries.
const MAX_VALIDATION_FAILURES: usize = 3;

/// Appended to a structured child's system prompt.
pub(crate) const YIELD_INSTRUCTION: &str = "When your work is done, call the yield tool once with {\"data\": …} matching the output schema, or {\"error\": \"…\"} if you cannot finish. Use \"section\" to submit large results in parts.";

/// Appended to the published text when the child never called the tool.
const MISSING_YIELD_WARNING: &str = "[warning: child finished without calling yield; result is unstructured]";

static SLOTS: LazyLock<Mutex<HashMap<String, Slot>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn slots() -> MutexGuard<'static, HashMap<String, Slot>> {
    SLOTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct Slot {
    schema: Value,
    /// Values submitted with `section`, assembled into the result object.
    sections: Map<String, Value>,
    /// The terminal value, once the child submitted one.
    result: Option<StoredResult>,
    validation_failures: usize,
    schema_overridden: bool,
}

enum StoredResult {
    Success(Value),
    Failure(String),
}

/// What the completion path finds in a child's slot.
pub(crate) enum Completion {
    /// The child submitted a result. Carries the `agent://` details object.
    Yielded { details: Value },
    /// The spawn asked for a schema but the child never submitted a result.
    Missing,
}

/// Reject a schema no validator can use, with the message a spawn reports.
pub(crate) fn validate_schema(schema: &Value) -> Result<()> {
    if !schema.is_object() {
        return Err(anyhow::anyhow!(
            "output_schema must be a JSON Schema object: expected a JSON object, got {}",
            schema_type_name(schema)
        ));
    }
    jsonschema::validator_for(schema)
        .map_err(|error| anyhow::anyhow!("output_schema must be a JSON Schema object: {error}"))?;
    Ok(())
}

/// Register the slot a child's `yield` calls write into.
pub(crate) fn prepare(session_id: &str, schema: Value) {
    slots().insert(
        session_id.to_string(),
        Slot {
            schema,
            sections: Map::new(),
            result: None,
            validation_failures: 0,
            schema_overridden: false,
        },
    );
}

/// The schema this session's child must satisfy, if it has one.
pub(crate) fn schema_for(session_id: &str) -> Option<Value> {
    slots().get(session_id).map(|slot| slot.schema.clone())
}

/// Forget a child's slot without publishing anything (failed/stopped children).
pub(crate) fn discard(session_id: &str) {
    slots().remove(session_id);
}

/// Drain a completed child's slot.
///
/// A successful result keeps the yielded object's fields at the root of the
/// published details, so the coordinator can read the field it asked for
/// (`agent://<child>/files/0`) instead of unwrapping an envelope. The two
/// envelope keys are added on top. A non-object result has no field to project,
/// so it travels under `data`.
pub(crate) fn take_completion(session_id: &str) -> Option<Completion> {
    let slot = slots().remove(session_id)?;
    Some(match slot.result {
        Some(StoredResult::Success(data)) => {
            let mut details = match data {
                Value::Object(fields) => fields,
                other => {
                    let mut fields = Map::new();
                    fields.insert("data".to_string(), other);
                    fields
                }
            };
            details.insert("status".to_string(), Value::String("success".to_string()));
            details.insert("schema_overridden".to_string(), Value::Bool(slot.schema_overridden));
            Completion::Yielded {
                details: Value::Object(details),
            }
        }
        Some(StoredResult::Failure(message)) => Completion::Yielded {
            details: json!({
                "status": "error",
                "error": message,
                "schema_overridden": slot.schema_overridden,
            }),
        },
        None => Completion::Missing,
    })
}

/// Publish a finished child's result, preferring the structured `yield` result.
///
/// Called from every swarm completion path, so a child without a schema slot
/// keeps the existing behaviour: the report is published with whatever
/// structured details its JSON body happens to carry.
pub(crate) fn publish_completion(child_session_id: &str, parent_session_id: &str, text: &str) {
    let (text, details) = match take_completion(child_session_id) {
        Some(Completion::Yielded { details }) => (text.to_string(), Some(details)),
        Some(Completion::Missing) => {
            let details = crate::agent_result_cache::structured_details_from_text(text);
            let text = if text.trim().is_empty() {
                MISSING_YIELD_WARNING.to_string()
            } else {
                format!("{text}\n\n{MISSING_YIELD_WARNING}")
            };
            (text, details)
        }
        None => (
            text.to_string(),
            crate::agent_result_cache::structured_details_from_text(text),
        ),
    };
    if let Err(error) =
        crate::agent_result_cache::publish_completed(child_session_id, parent_session_id, &text, details)
    {
        crate::logging::warn(&format!("[agent://] {error}"));
    }
}

/// The tool the child calls. Its description carries the schema, so the model
/// sees the required shape in the same request as the tool itself.
pub(crate) struct YieldTool {
    description: String,
}

impl YieldTool {
    pub(crate) fn new(schema: &Value) -> Self {
        let schema = serde_json::to_string(schema).unwrap_or_else(|_| "{}".to_string());
        Self {
            description: format!(
                "Submit your final result. Call once with {{\"data\": …}} matching the output schema, or with {{\"error\": \"…\"}} when you cannot finish. Use \"section\" to submit large results in parts.\n\nOutput schema:\n{schema}"
            ),
        }
    }
}

#[async_trait]
impl Tool for YieldTool {
    fn name(&self) -> &str {
        YIELD_TOOL
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "data": {
                    "description": "The complete result, matching the output schema. Omit when reporting an error. Combined with 'section', one part of that top-level property."
                },
                "error": {
                    "type": "string",
                    "description": "Why you could not finish. Omit when submitting data."
                },
                "section": {
                    "type": "string",
                    "description": "Name of a top-level property of the output schema to submit in parts. Array-typed properties accumulate across calls; other properties are replaced."
                }
            }
        })
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::read()
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let mut slots = slots();
        let Some(slot) = slots.get_mut(&ctx.session_id) else {
            return Err(anyhow::anyhow!(
                "yield is only available to a child spawned with an output schema"
            ));
        };

        let has_data = input.get("data").is_some();
        let has_error = input.get("error").is_some_and(|error| !error.is_null());
        if has_data == has_error {
            return Err(anyhow::anyhow!(
                "Submit success as {{\"data\": <your output>}} or failure as {{\"error\": \"message\"}}."
            ));
        }

        if has_error {
            let Some(message) = input.get("error").and_then(Value::as_str) else {
                return Err(anyhow::anyhow!("The yield \"error\" must be a string message."));
            };
            slot.result = Some(StoredResult::Failure(message.to_string()));
            return Ok(ToolOutput::new("Recorded failure result."));
        }

        let data = input.get("data").cloned().unwrap_or(Value::Null);
        let section = input
            .get("section")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|section| !section.is_empty());

        let Some(section) = section else {
            return match validate(&slot.schema, &data) {
                Ok(()) => {
                    slot.result = Some(StoredResult::Success(data));
                    slot.validation_failures = 0;
                    Ok(ToolOutput::new("Result recorded."))
                }
                Err(issues) => reject_or_accept(slot, data, issues),
            };
        };

        let Some(properties) = slot.schema.get("properties").and_then(Value::as_object) else {
            return Err(anyhow::anyhow!("Unknown section '{section}'; valid sections: none"));
        };
        let Some(property) = properties.get(section) else {
            let mut names: Vec<&str> = properties.keys().map(String::as_str).collect();
            names.sort_unstable();
            let list = if names.is_empty() {
                "none".to_string()
            } else {
                names.join(", ")
            };
            return Err(anyhow::anyhow!("Unknown section '{section}'; valid sections: {list}"));
        };

        let array_typed = property.get("type").is_some_and(|kind| match kind {
            Value::String(kind) => kind == "array",
            Value::Array(kinds) => kinds.iter().any(|kind| kind.as_str() == Some("array")),
            _ => false,
        });
        let existing = slot.sections.remove(section);
        let merged = if array_typed {
            let mut items = match existing {
                Some(Value::Array(items)) => items,
                // Only arrays are stored for an array-typed section, so any
                // other stored shape can only come from a schema that changed
                // under the child; start over rather than corrupt it.
                _ => Vec::new(),
            };
            match data {
                Value::Array(more) => items.extend(more),
                item => items.push(item),
            }
            Value::Array(items)
        } else {
            data
        };
        slot.sections.insert(section.to_string(), merged);

        let missing: Vec<&str> = required_names(&slot.schema)
            .into_iter()
            .filter(|name| !slot.sections.contains_key(*name))
            .collect();
        if !missing.is_empty() {
            return Ok(ToolOutput::new(format!(
                "Recorded section '{section}'. Required fields still missing: {}.",
                missing.join(", ")
            )));
        }

        let assembled = Value::Object(slot.sections.clone());
        match validate(&slot.schema, &assembled) {
            Ok(()) => {
                slot.result = Some(StoredResult::Success(assembled));
                slot.validation_failures = 0;
                Ok(ToolOutput::new(format!(
                    "Recorded section '{section}'. The assembled result matches the output schema and has been submitted."
                )))
            }
            Err(issues) => reject_or_accept(slot, assembled, issues),
        }
    }
}

/// Report a validation failure, or accept the value once the retry budget is
/// spent. Accepting marks `schema_overridden` on the published details so the
/// coordinator knows the shape was not enforced.
fn reject_or_accept(slot: &mut Slot, value: Value, issues: String) -> Result<ToolOutput> {
    slot.validation_failures += 1;
    if slot.validation_failures > MAX_VALIDATION_FAILURES {
        slot.schema_overridden = true;
        slot.result = Some(StoredResult::Success(value));
        return Ok(ToolOutput::new(format!(
            "Result recorded despite not matching the output schema; the schema was dropped after {MAX_VALIDATION_FAILURES} retries."
        )));
    }
    let remaining = MAX_VALIDATION_FAILURES + 1 - slot.validation_failures;
    Err(anyhow::anyhow!(
        "Result does not match the output schema: {issues}. Call yield again with the corrected shape — {remaining} retry attempt(s) remain before the schema is dropped."
    ))
}

fn required_names(schema: &Value) -> Vec<&str> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

fn validate(schema: &Value, value: &Value) -> Result<(), String> {
    let validator = jsonschema::validator_for(schema).map_err(|error| error.to_string())?;
    let mut issues = Vec::new();
    for error in validator.iter_errors(value) {
        let path = error.instance_path().to_string();
        if path.is_empty() {
            issues.push(error.to_string());
        } else {
            issues.push(format!("{path}: {error}"));
        }
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues.join("; "))
    }
}

fn schema_type_name(schema: &Value) -> &'static str {
    match schema {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
#[path = "yield_result_tests.rs"]
mod tests;
