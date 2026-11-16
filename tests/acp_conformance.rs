#![cfg(unix)]
//! ACP conformance tests against the pinned schemas and, in later sections,
//! the real binary.
//!
//! What is here now is the frame-level `id` rule the v2 schema states: an
//! `Error` response may carry `null` (the request's id could not be read),
//! while a `Result` response and a request may not. The binary-driven
//! scenarios that follow use the same support modules, included with
//! `#[path]` as each one is needed (`acp_client`, `lifecycle`).
#![allow(dead_code)]

#[path = "support/acp_schema.rs"]
mod acp_schema;

use acp_schema::Direction;
use serde_json::{Value, json};

/// The violations one agent-authored v2 frame draws, printed so `--nocapture`
/// output names the frame and its count.
fn v2_violations(frame: &Value) -> Vec<acp_schema::Violation> {
    let violations = acp_schema::validate_outgoing_version(2, frame, Direction::AgentToClient);
    println!(
        "v2 agent frame {frame} -> {} violation(s): {violations:?}",
        violations.len()
    );
    violations
}

#[test]
fn validator_accepts_null_id_on_v2_error_response() {
    let frame = json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": {"code": -32600, "message": "Invalid request"},
    });
    let violations = v2_violations(&frame);
    assert!(violations.is_empty(), "{frame}: {violations:?}");
}

#[test]
fn validator_rejects_null_id_on_v2_result_response() {
    let frame = json!({"jsonrpc": "2.0", "id": null, "result": {}});
    let violations = v2_violations(&frame);
    assert_eq!(violations.len(), 1, "{frame}: {violations:?}");
    assert_eq!(violations[0].path, "/id", "{frame}: {violations:?}");
}

#[test]
fn validator_rejects_null_id_on_v2_request() {
    let frame = json!({"jsonrpc": "2.0", "id": null, "method": "session/list", "params": {}});
    let violations = acp_schema::validate_outgoing_version(2, &frame, Direction::ClientToAgent);
    println!(
        "v2 client frame {frame} -> {} violation(s): {violations:?}",
        violations.len()
    );
    assert!(
        violations.iter().any(|violation| violation.path == "/id"),
        "{frame}: {violations:?}"
    );
}
