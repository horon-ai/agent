//! Applying `post_tool_feedback` to the result the model sees.
//!
//! The hook decides text, the error flag and system reminders, but the tool
//! output is the only place the round reads them from: the text is replaced in
//! place, and the two flags ride on the output metadata because the round that
//! builds the history message no longer has the hook's own result. The file
//! state and the undo journal are untouched, exactly as for every other hook.

use crate::hooks::post_tool_feedback::{Feedback, Part};
use serde_json::Value;

/// Metadata key carrying the `is_error` flag a feedback hook asked for.
pub(crate) const ERROR_METADATA_KEY: &str = "post_tool_feedback_is_error";
/// Metadata key carrying the system reminders a feedback hook asked for.
pub(crate) const NOTE_METADATA_KEY: &str = "post_tool_feedback_note";

/// Apply every effect of `feedback` to `output`.
pub(crate) fn apply(output: &mut super::ToolOutput, feedback: &Feedback) {
    if feedback.is_empty() {
        return;
    }
    let mut text = std::mem::take(&mut output.output);
    for part in feedback.parts() {
        match part {
            Part::Replace(replacement) => text = replacement.clone(),
            Part::Text(section) => append_section(&mut text, section),
        }
    }
    output.output = text;
    if feedback.is_error().is_none() && feedback.notes().is_empty() {
        return;
    }
    let metadata = output
        .metadata
        .get_or_insert_with(|| Value::Object(serde_json::Map::new()));
    let Some(fields) = object_fields(metadata) else {
        return;
    };
    if let Some(is_error) = feedback.is_error() {
        fields.insert(ERROR_METADATA_KEY.to_string(), Value::Bool(is_error));
    }
    if !feedback.notes().is_empty() {
        fields.insert(
            NOTE_METADATA_KEY.to_string(),
            Value::String(feedback.notes().join("\n\n")),
        );
    }
}

/// The object inside `value`, replacing a non-object with an empty one so a
/// tool's own metadata stays readable.
fn object_fields(value: &mut Value) -> Option<&mut serde_json::Map<String, Value>> {
    if !value.is_object() {
        *value = Value::Object(serde_json::Map::new());
    }
    value.as_object_mut()
}

/// Append one feedback section under the `[post_tool_feedback]` header.
fn append_section(text: &mut String, section: &str) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("\n[post_tool_feedback]\n");
    text.push_str(section);
}

/// Whether a feedback hook marked this result as an error.
pub(crate) fn is_error(output: &super::ToolOutput) -> bool {
    output
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get(ERROR_METADATA_KEY))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// The system reminder a feedback hook attached to this result.
pub(crate) fn note(output: &super::ToolOutput) -> Option<String> {
    output
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get(NOTE_METADATA_KEY))
        .and_then(Value::as_str)
        .map(str::to_string)
}
