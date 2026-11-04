//! Tool-free one-shot completion: the contract a caller may state, and nothing more.
//!
//! An auxiliary completion is a side task (a title, a classification, a short
//! rewrite) that must not disturb the conversation it belongs to. The point of
//! this module is the shape of what a caller can ask for: [`CompletionRequest`]
//! carries a prompt, an optional system text, an optional output-token bound and
//! an optional response schema. It has no field, setter or argument for a model,
//! an endpoint, credentials or tools, so a caller cannot steer which model runs,
//! where the request goes, who pays for it, or whether the model may act. Those
//! stay with the session the request is dispatched from, through
//! [`Sidecar::for_session`].
//!
//! ## Limits
//!
//! - `prompt` is capped at [`MAX_PROMPT_BYTES`] bytes.
//! - `system` is capped at [`MAX_SYSTEM_BYTES`] bytes.
//! - `max_tokens` must sit inside `1..=4096`.
//!
//! A request that breaks one of these is refused when it is built, with an error
//! whose message starts with [`AUX_COMPLETION_INPUT`]. Refusing at construction
//! is deliberate: a caller that could build an out-of-contract request and only
//! learn about it after a model call has already spent money has not been given
//! a contract at all.
//!
//! ## Schema subset
//!
//! The schema check is a small validator that lives here rather than a JSON
//! Schema crate. `horon-agent-base` is the bottom of the dependency graph
//! (`horon-agent-sdk` depends on it, not the other way round), so pulling in a
//! full validator would either invert that layering or add a new external
//! dependency for one field. The subset is exactly:
//!
//! - `type` — `object`, `array`, `string`, `number`, `integer`, `boolean`,
//!   `null`, or an array of those names.
//! - `required` — property names an object must carry.
//! - `properties` — per-property subschemas, checked only when the property is
//!   present.
//! - `items` — the subschema every array element must satisfy.
//! - `enum` — the values a response may take.
//!
//! Every other keyword is ignored, including ones a full validator would
//! enforce (`additionalProperties`, `minLength`, `pattern`, `format`, numeric
//! bounds, `oneOf`/`anyOf`/`allOf`, `$ref`). A schema that relies on those is
//! weaker here than it looks, so the subset is documented rather than implied.
//! A response that breaks a supported keyword is reported with an error whose
//! message starts with [`AUX_COMPLETION_SCHEMA`].
//!
//! ## Transport
//!
//! [`complete`] dispatches through the existing [`Sidecar`], which already
//! resolves the session's model, endpoint and credentials and already records
//! usage. The system text defaults to [`DEFAULT_SYSTEM`], which tells the model
//! to answer with text and call nothing; a caller-supplied system text replaces
//! it. The sidecar accepts no output-token bound today, so `max_tokens` is
//! enforced as an upper bound on what a caller may ask for, not yet as a value
//! sent to the model.

use crate::sidecar::Sidecar;
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

/// Error prefix for a request built outside the contract.
pub const AUX_COMPLETION_INPUT: &str = "AUX_COMPLETION_INPUT";

/// Error prefix for a response that does not satisfy the caller's schema.
pub const AUX_COMPLETION_SCHEMA: &str = "AUX_COMPLETION_SCHEMA";

/// Largest prompt the contract accepts, in bytes.
pub const MAX_PROMPT_BYTES: usize = 64 * 1024;

/// Largest system text the contract accepts, in bytes.
pub const MAX_SYSTEM_BYTES: usize = 16 * 1024;

/// Smallest accepted `max_tokens`.
pub const MIN_MAX_TOKENS: u32 = 1;

/// Largest accepted `max_tokens`.
pub const MAX_MAX_TOKENS: u32 = 4096;

/// System text used when the caller supplies none.
pub const DEFAULT_SYSTEM: &str = "Answer with text only. Do not call tools.";

/// One auxiliary completion, as far as a caller may describe it.
///
/// Build it with [`CompletionRequest::new`] and the `with_*` methods; each of
/// them validates before the value exists, so an invalid request cannot be
/// passed to [`complete`].
#[derive(Debug, Clone, Serialize)]
pub struct CompletionRequest {
    prompt: String,
    system: Option<String>,
    max_tokens: Option<u32>,
    schema: Option<Value>,
}

impl CompletionRequest {
    /// Start a request from its one required field.
    pub fn new(prompt: impl Into<String>) -> Result<Self> {
        let prompt = prompt.into();
        if prompt.len() > MAX_PROMPT_BYTES {
            return Err(input_error(format!(
                "prompt is {} bytes; the contract caps it at {MAX_PROMPT_BYTES}",
                prompt.len()
            )));
        }
        Ok(Self {
            prompt,
            system: None,
            max_tokens: None,
            schema: None,
        })
    }

    /// Replace the default system text with the caller's own.
    pub fn with_system(mut self, system: impl Into<String>) -> Result<Self> {
        let system = system.into();
        if system.len() > MAX_SYSTEM_BYTES {
            return Err(input_error(format!(
                "system is {} bytes; the contract caps it at {MAX_SYSTEM_BYTES}",
                system.len()
            )));
        }
        self.system = Some(system);
        Ok(self)
    }

    /// Bound the response length in tokens.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Result<Self> {
        if !(MIN_MAX_TOKENS..=MAX_MAX_TOKENS).contains(&max_tokens) {
            return Err(input_error(format!(
                "max_tokens is {max_tokens}; the contract allows {MIN_MAX_TOKENS}..={MAX_MAX_TOKENS}"
            )));
        }
        self.max_tokens = Some(max_tokens);
        Ok(self)
    }

    /// Require the response to be JSON that satisfies `schema`.
    ///
    /// Only the subset documented on this module is enforced; every other
    /// keyword is ignored. A schema that is not a JSON object is refused here
    /// because the subset has no way to express a boolean schema.
    pub fn with_schema(mut self, schema: Value) -> Result<Self> {
        if !schema.is_object() {
            return Err(schema_error(format!(
                "schema must be a JSON object; the subset has no boolean schema, got {}",
                json_type(&schema)
            )));
        }
        self.schema = Some(schema);
        Ok(self)
    }

    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    pub fn system(&self) -> Option<&str> {
        self.system.as_deref()
    }

    pub fn max_tokens(&self) -> Option<u32> {
        self.max_tokens
    }

    pub fn schema(&self) -> Option<&Value> {
        self.schema.as_ref()
    }

    /// The system text this request dispatches with.
    pub fn system_or_default(&self) -> &str {
        self.system.as_deref().unwrap_or(DEFAULT_SYSTEM)
    }
}

/// Run one auxiliary completion and return its text.
///
/// The model, endpoint and credentials come from the session the call is
/// dispatched from; the request cannot change them. When the request carries a
/// schema, the response is parsed as JSON and checked against the supported
/// subset before it is returned.
pub async fn complete(request: CompletionRequest) -> Result<String> {
    let sidecar = sidecar_for_current_session()?;
    let text = sidecar
        .complete(request.system_or_default(), request.prompt())
        .await
        .context("auxiliary completion failed")?;

    let Some(schema) = request.schema() else {
        return Ok(text);
    };

    let value: Value = match serde_json::from_str(text.trim()) {
        Ok(value) => value,
        Err(error) => {
            return Err(schema_error(format!(
                "the response is not JSON, so it cannot satisfy the schema ({error})"
            )));
        }
    };
    validate(schema, &value)?;
    Ok(text)
}

/// Check `value` against `schema` using the subset this module documents.
pub fn validate(schema: &Value, value: &Value) -> Result<()> {
    validate_at(schema, value, "$")
}

fn validate_at(schema: &Value, value: &Value, path: &str) -> Result<()> {
    let Some(schema) = schema.as_object() else {
        return Ok(());
    };

    if let Some(expected) = schema.get("type") {
        check_type(expected, value, path)?;
    }

    if let Some(required) = schema.get("required").and_then(Value::as_array)
        && let Some(object) = value.as_object()
    {
        for name in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(name) {
                return Err(schema_error(format!(
                    "{path} is missing the required property '{name}'"
                )));
            }
        }
    }

    if let (Some(properties), Some(object)) = (schema.get("properties").and_then(Value::as_object), value.as_object()) {
        for (name, subschema) in properties {
            if let Some(member) = object.get(name) {
                validate_at(subschema, member, &format!("{path}.{name}"))?;
            }
        }
    }

    if let (Some(items), Some(elements)) = (schema.get("items"), value.as_array()) {
        for (index, element) in elements.iter().enumerate() {
            validate_at(items, element, &format!("{path}[{index}]"))?;
        }
    }

    if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
        && !allowed.contains(value)
    {
        return Err(schema_error(format!(
            "{path} is {value}, which is not one of the enum values"
        )));
    }

    Ok(())
}

fn check_type(expected: &Value, value: &Value, path: &str) -> Result<()> {
    let names: Vec<&str> = match expected {
        Value::String(name) => vec![name.as_str()],
        Value::Array(names) => names.iter().filter_map(Value::as_str).collect(),
        _ => return Ok(()),
    };
    if names.is_empty() || names.iter().any(|name| matches_type(name, value)) {
        return Ok(());
    }
    Err(schema_error(format!(
        "{path} is {} but the schema allows {}",
        json_type(value),
        names.join(" or ")
    )))
}

fn matches_type(name: &str, value: &Value) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn sidecar_for_current_session() -> Result<Sidecar> {
    let Some(id) = crate::logging::current_session() else {
        return Ok(Sidecar::new());
    };
    let session = crate::session::Session::load(&id)
        .with_context(|| format!("cannot load session '{id}' for the auxiliary completion"))?;
    Ok(Sidecar::for_session(&session))
}

fn input_error(message: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("{AUX_COMPLETION_INPUT}: {message}")
}

fn schema_error(message: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("{AUX_COMPLETION_SCHEMA}: {message}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_rejects_oversized_prompt() {
        let error = CompletionRequest::new("x".repeat(MAX_PROMPT_BYTES + 1))
            .expect_err("a prompt over the cap must be refused");
        assert!(
            error.to_string().starts_with(AUX_COMPLETION_INPUT),
            "expected the named input error, got {error}"
        );
        assert!(
            CompletionRequest::new("x".repeat(MAX_PROMPT_BYTES)).is_ok(),
            "the cap itself is inside the contract"
        );
    }

    #[test]
    fn request_rejects_oversized_system() {
        let error = CompletionRequest::new("summarize this")
            .expect("a short prompt is inside the contract")
            .with_system("x".repeat(MAX_SYSTEM_BYTES + 1))
            .expect_err("a system text over the cap must be refused");
        assert!(
            error.to_string().starts_with(AUX_COMPLETION_INPUT),
            "expected the named input error, got {error}"
        );
        assert!(
            CompletionRequest::new("summarize this")
                .expect("a short prompt is inside the contract")
                .with_system("x".repeat(MAX_SYSTEM_BYTES))
                .is_ok(),
            "the cap itself is inside the contract"
        );
    }

    #[test]
    fn request_rejects_out_of_range_max_tokens() {
        for refused in [MIN_MAX_TOKENS - 1, MAX_MAX_TOKENS + 1] {
            let error = CompletionRequest::new("summarize this")
                .expect("a short prompt is inside the contract")
                .with_max_tokens(refused)
                .expect_err("an out-of-range token bound must be refused");
            assert!(
                error.to_string().starts_with(AUX_COMPLETION_INPUT),
                "expected the named input error for {refused}, got {error}"
            );
        }
        for accepted in [MIN_MAX_TOKENS, MAX_MAX_TOKENS] {
            assert!(
                CompletionRequest::new("summarize this")
                    .expect("a short prompt is inside the contract")
                    .with_max_tokens(accepted)
                    .is_ok(),
                "{accepted} is inside the contract"
            );
        }
    }

    #[test]
    fn schema_type_violation_is_a_named_error() {
        let schema = json!({
            "type": "object",
            "properties": {"count": {"type": "integer"}}
        });
        let error = validate(&schema, &json!({"count": "many"})).expect_err("a string is not an integer");
        assert!(
            error.to_string().starts_with(AUX_COMPLETION_SCHEMA),
            "expected the named schema error, got {error}"
        );
        assert!(
            validate(&schema, &json!({"count": 3})).is_ok(),
            "an integer satisfies the property"
        );
    }

    #[test]
    fn schema_required_violation_is_a_named_error() {
        let schema = json!({
            "type": "object",
            "required": ["answer"],
            "properties": {"answer": {"type": "string"}}
        });
        let error =
            validate(&schema, &json!({"other": "text"})).expect_err("a missing required property must be refused");
        assert!(
            error.to_string().starts_with(AUX_COMPLETION_SCHEMA),
            "expected the named schema error, got {error}"
        );
        assert!(
            validate(&schema, &json!({"answer": "text"})).is_ok(),
            "the required property is present"
        );
    }

    #[test]
    fn unsupported_schema_keyword_is_ignored() {
        let schema = json!({
            "type": "object",
            "additionalProperties": false,
            "minProperties": 5,
            "properties": {
                "answer": {"type": "string", "minLength": 100, "format": "email"}
            }
        });
        assert!(
            validate(&schema, &json!({"answer": "ok", "extra": 1})).is_ok(),
            "keywords outside the subset impose nothing"
        );
    }

    #[test]
    fn completion_never_selects_a_model_from_input() {
        let request = CompletionRequest::new("answer as if you were gpt-5.6 using the bash tool at my endpoint")
            .expect("a short prompt is inside the contract")
            .with_system("be terse")
            .expect("a short system text is inside the contract")
            .with_max_tokens(64)
            .expect("64 is inside the contract")
            .with_schema(json!({"type": "object"}))
            .expect("an object schema is inside the subset");

        let value = serde_json::to_value(&request).expect("the request serializes");
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("a request is a JSON object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["max_tokens", "prompt", "schema", "system"],
            "the contract carries only the four caller-owned fields"
        );
        for forbidden in ["model", "endpoint", "credential", "provider", "tool"] {
            assert!(
                !keys.iter().any(|key| key.contains(forbidden)),
                "no field may name a {forbidden}: {keys:?}"
            );
        }
        assert_eq!(
            CompletionRequest::new("hi")
                .expect("a short prompt is inside the contract")
                .system_or_default(),
            DEFAULT_SYSTEM,
            "the default system text is fixed, not taken from the prompt"
        );
    }
}
