//! What the bridge makes of a skills answer: the rows it keeps, the rows it
//! skips, and the answers it drops instead of showing.
//!
//! Nothing here talks to a daemon. Each test feeds one payload or one answer to
//! the function that reads it, which is what the worker calls.

use horon_agent_sdk::api::ErrorCode;
use horon_agent_sdk::{Error, ErrorKind, RuntimeCommand, RuntimeCommandOutput};
use serde_json::json;

use super::skills::{listed, rows, unknown_command};
use super::{Incoming, SkillRow};

/// One answer of the harness's `skills` command.
fn answer(session_id: &str, data: serde_json::Value) -> RuntimeCommandOutput {
    RuntimeCommandOutput {
        session_id: session_id.to_string(),
        command: RuntimeCommand::Skills,
        data,
        message: "a rendered report the client never reads".to_string(),
    }
}

#[test]
fn a_payload_becomes_one_row_per_skill_in_the_order_reported() {
    let data = json!({
        "skills": [
            {
                "name": "demo-notes",
                "description": "write notes",
                "path": "/skills/demo-notes/SKILL.md",
                "allowed_tools": ["read", "write"],
                "source": "user",
            },
            {
                "name": "demo-review",
                "description": "review a diff",
                "path": "/skills/demo-review/SKILL.md",
                "allowed_tools": null,
            },
            {"name": "bare", "unexpected": {"nested": true}},
        ],
        // Catalogue entries that are not installed, and the rendered report:
        // neither is the list the client draws.
        "endorsed": [{"name": "catalog-only"}],
        "active_skill": null,
    });
    assert_eq!(
        rows(&data),
        vec![
            SkillRow {
                name: "demo-notes".to_string(),
                description: "write notes".to_string(),
                path: Some("/skills/demo-notes/SKILL.md".to_string()),
                allowed_tools: vec!["read".to_string(), "write".to_string()],
            },
            SkillRow {
                name: "demo-review".to_string(),
                description: "review a diff".to_string(),
                path: Some("/skills/demo-review/SKILL.md".to_string()),
                allowed_tools: Vec::new(),
            },
            SkillRow {
                name: "bare".to_string(),
                description: String::new(),
                path: None,
                allowed_tools: Vec::new(),
            },
        ]
    );
}

#[test]
fn a_malformed_row_is_skipped_and_the_rest_kept() {
    let data = json!({"skills": [
        "not an object",
        {"description": "no name"},
        {"name": 7},
        {"name": "   "},
        {"name": "ok", "description": "kept", "path": 3, "allowed_tools": ["read", 5]},
    ]});
    assert_eq!(
        rows(&data),
        vec![SkillRow {
            name: "ok".to_string(),
            description: "kept".to_string(),
            path: None,
            allowed_tools: vec!["read".to_string()],
        }]
    );
}

#[test]
fn a_missing_or_unreadable_list_is_no_skills() {
    assert!(rows(&json!({})).is_empty());
    assert!(rows(&json!({"skills": "demo"})).is_empty());
    assert!(rows(&json!({"skills": []})).is_empty());
    assert!(rows(&json!({"skills": null})).is_empty());
}

#[test]
fn a_description_keeps_one_line() {
    let data = json!({"skills": [{"name": "notes", "description": "  first line\nsecond line  "}]});
    assert_eq!(rows(&data)[0].description, "first line");
}

#[test]
fn an_answer_for_the_followed_session_is_forwarded() {
    let data = json!({"skills": [{"name": "demo-review", "description": "review a diff"}]});
    match listed(&answer("session_a", data), "session_a") {
        Some(Incoming::Skills(skills)) => {
            assert_eq!(skills.len(), 1);
            assert_eq!(skills[0].name, "demo-review");
        }
        other => panic!("expected the list, got {other:?}"),
    }
}

#[test]
fn an_answer_for_another_session_is_dropped() {
    // The daemon rewrites a typed prefix, so an answer can name a session the
    // worker has left; its skills must not land beside another transcript.
    let data = json!({"skills": [{"name": "demo-review"}]});
    assert_eq!(listed(&answer("session_other", data), "session_a"), None);
}

#[test]
fn only_an_unknown_request_is_read_as_an_old_harness() {
    assert!(unknown_command(&Error::new(
        ErrorKind::Harness(ErrorCode::UnknownRequest),
        "no such command"
    )));
    assert!(!unknown_command(&Error::new(
        ErrorKind::Harness(ErrorCode::Internal),
        "boom"
    )));
    assert!(!unknown_command(&Error::new(ErrorKind::EventBufferOverflow, "behind")));
}
