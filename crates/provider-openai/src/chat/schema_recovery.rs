//! Recovery from a provider that rejects a construct in the tool schemas.
//!
//! Prevention keeps an unknown keyword off the wire: the tool catalog is
//! normalized against the OpenAI dialect's allow-list before it is sent. That
//! allow-list cannot be complete, though, and a single rejected keyword fails
//! the whole request rather than the one tool carrying it. The recovery layer
//! closes that gap: it reads the rejection, records the construct it names, and
//! lets the same turn send one request without it.

use crate::chat::transport::{HttpErrorKind, classify_http_status};
use horon_agent_schema_dialect::{RecoveryAction, recover_from_error, registry};
use serde_json::Value;

/// How many rejected requests one turn may spend recovering.
///
/// Learning already bounds the loop: a construct is retried once, and a second
/// rejection naming it is reported instead. This cap is a backstop for a
/// response that keeps naming constructs the dialect has not seen.
pub const MAX_SCHEMA_RECOVERY_ATTEMPTS: u8 = 3;

/// What a failed response means for the caller's retry loop.
#[derive(Debug, PartialEq, Eq)]
pub enum SchemaRecovery {
    /// A new construct was learned and stripped; the request may be rebuilt and
    /// sent again.
    Retry { description: String },
    /// Report the failure instead of retrying, with a hint when the rejection
    /// says something a user can act on.
    Stop { hint: Option<String> },
}

/// Classify a failed status and body, learning any schema construct it names.
///
/// `sent_body` is the request that failed. A rejection naming a construct that
/// request did not carry cannot be fixed by sending it again, so it is reported
/// instead of retried: without this check a keyword prevention had already
/// stripped would still cost one wasted round trip before the repeat rejection
/// said so.
///
/// Only a client error can be a schema rejection. A 429 or a 5xx is a transient
/// provider condition and a 401/403 is a credential problem, so none of them may
/// teach the dialect anything: learning from one would strip that construct from
/// every later request in the process, for a failure that had nothing to do with
/// the schema.
pub fn classify(status: u16, error_body: &str, sent_body: &[u8]) -> SchemaRecovery {
    if classify_http_status(status) != HttpErrorKind::Client {
        return SchemaRecovery::Stop { hint: None };
    }
    // The OpenAI-compatible provider normalizes with this same dialect, so a
    // construct learned here is one its rebuilt request drops.
    match recover_from_error(error_body, &registry::OPENAI) {
        RecoveryAction::NotSchemaRelated => SchemaRecovery::Stop { hint: None },
        RecoveryAction::Unrecoverable { hint } => SchemaRecovery::Stop { hint: Some(hint) },
        RecoveryAction::RetryWithoutConstruct { description } => {
            if carried_by(sent_body, error_body) {
                SchemaRecovery::Retry { description }
            } else {
                SchemaRecovery::Stop {
                    hint: Some(format!(
                        "the rejection names a construct the request horon-agent sent did not \
                         carry, so resending those bytes cannot change the answer; it is now \
                         recorded and stripped from later requests ({description})"
                    )),
                }
            }
        }
    }
}

/// Whether the request that failed carried the construct the rejection names.
///
/// Parsed rather than scanned as text so a conversation that merely mentions the
/// word cannot make a rejection look actionable. This runs only after a request
/// failed, and against a body already sent over the network.
fn carried_by(sent_body: &[u8], error_body: &str) -> bool {
    let Some(rejection) = horon_agent_schema_dialect::rejection::classify(error_body) else {
        // An unrecognized rejection has no named construct to look for.
        return true;
    };
    let Ok(sent) = serde_json::from_slice::<Value>(sent_body) else {
        // An unparsable body says nothing; the caller's attempt cap keeps a
        // retry bounded.
        return true;
    };
    let tools = sent.get("tools").unwrap_or(&Value::Null);
    rejection
        .keywords
        .iter()
        .any(|keyword| horon_agent_schema_dialect::rejection::schema_contains_keyword(tools, keyword))
        || rejection
            .format
            .as_deref()
            .is_some_and(|format| contains_format(tools, format))
}

fn contains_format(value: &Value, format: &str) -> bool {
    match value {
        Value::Object(map) => {
            map.get("format").and_then(Value::as_str) == Some(format)
                || map.values().any(|child| contains_format(child, format))
        }
        Value::Array(items) => items.iter().any(|item| contains_format(item, format)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horon_agent_schema_dialect::quirks;
    use serde_json::json;

    /// A store redirected into `dir` and emptied, so one case cannot teach
    /// another. The redirect is thread-local, and the test harness gives each
    /// test its own thread.
    fn isolated() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temporary quirk store");
        quirks::use_test_path(dir.path().join("schema-quirks.json"));
        quirks::forget("openai");
        dir
    }

    fn body_with(schema: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "model": "m",
            "tools": [{ "type": "function", "function": { "name": "search", "parameters": schema } }]
        }))
        .expect("body is JSON")
    }

    fn error(message: &str) -> String {
        format!("{{\"error\":{{\"message\":{}}}}}", json!(message))
    }

    #[test]
    fn a_transient_failure_teaches_nothing() {
        let _dir = isolated();
        let rejection = error("Invalid schema for function 'search': 'uniqueItems' is not permitted.");
        let sent = body_with(json!({ "type": "object", "uniqueItems": true }));
        for status in [401u16, 403, 429, 500, 503] {
            assert_eq!(
                classify(status, &rejection, &sent),
                SchemaRecovery::Stop { hint: None },
                "{status}"
            );
        }
        assert!(quirks::learned_for("openai").rejected_keywords.is_empty());
    }

    #[test]
    fn an_unrelated_client_error_has_no_hint() {
        let _dir = isolated();
        let sent = body_with(json!({ "type": "object" }));
        assert_eq!(
            classify(400, &error("model not found"), &sent),
            SchemaRecovery::Stop { hint: None }
        );
    }

    #[test]
    fn a_construct_the_request_does_not_carry_is_not_retried() {
        let _dir = isolated();
        let sent = body_with(json!({ "type": "object", "properties": { "q": { "type": "string" } } }));
        let rejection =
            error("Invalid schema for function 'search': In context=('properties', 'q'), 'pattern' is not permitted.");
        let recovery = classify(400, &rejection, &sent);
        let SchemaRecovery::Stop { hint } = recovery else {
            panic!("a construct that was never sent must not be retried: {recovery:?}");
        };
        let hint = hint.expect("the failure explains itself");
        assert!(hint.contains("did not"), "{hint}");
        assert!(hint.contains("recorded and stripped"), "{hint}");
    }

    #[test]
    fn a_construct_the_request_carries_is_learned() {
        let _dir = isolated();
        let sent = body_with(json!({ "type": "object", "properties": { "q": { "type": "string", "pattern": "^a" } } }));
        let rejection =
            error("Invalid schema for function 'search': In context=('properties', 'q'), 'pattern' is not permitted.");
        match classify(400, &rejection, &sent) {
            SchemaRecovery::Retry { description } => assert!(description.contains("pattern"), "{description}"),
            other => panic!("a construct on the wire is learned: {other:?}"),
        }
        assert!(
            quirks::learned_for("openai")
                .rejected_keywords
                .iter()
                .any(|keyword| keyword == "pattern"),
            "the construct is remembered for the rebuilt request"
        );
    }

    #[test]
    fn a_format_the_request_carries_is_learned() {
        let _dir = isolated();
        let sent = body_with(json!({
            "type": "object",
            "properties": { "url": { "type": "string", "format": "uri" } }
        }));
        let rejection = error(
            "Invalid schema for function 'search': In context=('properties', 'url'), 'uri' is not a valid format.",
        );
        match classify(400, &rejection, &sent) {
            SchemaRecovery::Retry { description } => assert!(description.contains("uri"), "{description}"),
            other => panic!("a format on the wire is learned: {other:?}"),
        }
        assert!(
            quirks::learned_for("openai")
                .rejected_formats
                .iter()
                .any(|format| format == "uri")
        );
    }
}
