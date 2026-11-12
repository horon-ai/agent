//! Tests for the shared Markdown transcript renderer.
//!
//! These live in their own file because `agent/messages.rs` is close to the
//! per-file size budget, and the renderer is a leaf: it turns stored messages
//! into text and touches no agent state.

use crate::message::{ContentBlock, Role};
use crate::session::StoredMessage;
use crate::session_export::{MAX_TOOL_RESULT_BYTES, render_markdown};

fn message(role: Role, content: Vec<ContentBlock>) -> StoredMessage {
    StoredMessage {
        original_input: None,
        arg_repairs: None,
        degradations: None,
        id: "m1".to_string(),
        role,
        content,
        display_role: None,
        turn_start: false,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
    }
}

fn text(value: &str) -> ContentBlock {
    ContentBlock::Text {
        text: value.to_string(),
        cache_control: None,
    }
}

/// Every user and assistant turn gets its own heading.
#[test]
fn export_markdown_renders_one_heading_per_turn() {
    let messages = vec![
        message(Role::User, vec![text("what does the daemon do?")]),
        message(Role::Assistant, vec![text("it runs turns headlessly.")]),
        message(Role::User, vec![text("and the bridge?")]),
    ];

    let markdown = render_markdown(&messages);

    assert_eq!(markdown.matches("### User").count(), 1 + 1);
    assert_eq!(markdown.matches("### Assistant").count(), 1);
    assert!(markdown.contains("what does the daemon do?"));
    assert!(markdown.contains("it runs turns headlessly."));
    // Headings and bodies stay in transcript order.
    let user_first = markdown.find("### User").expect("user heading");
    let assistant = markdown.find("### Assistant").expect("assistant heading");
    assert!(user_first < assistant, "the assistant reply must follow the user turn");
}

/// A secret inside a tool result never reaches the transcript.
#[test]
fn export_markdown_redacts_secret_in_tool_result() {
    // Split so the repository secret scanner does not flag the fixture.
    let token = concat!("gh", "p_0123456789abcdefghijklmnopqrstuvwxyz");
    let messages = vec![message(
        Role::Assistant,
        vec![ContentBlock::ToolResult {
            tool_use_id: "call_1".to_string(),
            content: format!("GITHUB_TOKEN={token}"),
            is_error: None,
            verdict: None,
        }],
    )];

    let markdown = render_markdown(&messages);

    assert!(!markdown.contains(token), "the token must be redacted");
    assert!(
        markdown.contains("[REDACTED_SECRET]"),
        "a visible marker replaces the secret"
    );
    assert!(markdown.contains("**Result:**"));
}

/// A secret inside a tool call's arguments is redacted too.
#[test]
fn export_markdown_redacts_secret_in_tool_input() {
    let key = concat!("sk-", "ant-abcdefghijklmnopqrstuvwxyz");
    let messages = vec![message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "call_1".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({ "command": format!("export API_KEY={key}") }),
            thought_signature: None,
        }],
    )];

    let markdown = render_markdown(&messages);

    assert!(!markdown.contains(key));
    assert!(markdown.contains("[REDACTED_SECRET]"));
    assert!(markdown.contains("**Tool: `bash`**"));
}

/// A runaway tool result is cut at the budget with a marker naming the size.
#[test]
fn export_markdown_truncates_long_tool_result() {
    let long = "x".repeat(MAX_TOOL_RESULT_BYTES * 2);
    let messages = vec![message(
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: "call_1".to_string(),
            content: long.clone(),
            is_error: Some(true),
            verdict: None,
        }],
    )];

    let markdown = render_markdown(&messages);

    assert!(markdown.contains("**Error:**"));
    assert!(markdown.contains("truncated"), "the marker must be visible");
    assert!(markdown.contains(&format!("{} bytes total", MAX_TOOL_RESULT_BYTES * 2)));
    assert!(
        markdown.len() < long.len(),
        "the export must be smaller than the raw result"
    );
}

/// A short tool result is kept verbatim, so an operator can quote it.
#[test]
fn export_markdown_keeps_short_tool_result_verbatim() {
    let messages = vec![message(
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: "call_1".to_string(),
            content: "exit code 0".to_string(),
            is_error: None,
            verdict: None,
        }],
    )];

    let markdown = render_markdown(&messages);

    assert!(markdown.contains("exit code 0"));
    assert!(!markdown.contains("truncated"));
}

/// An empty transcript is empty text, not a placeholder heading.
#[test]
fn export_markdown_of_empty_history_is_empty() {
    assert_eq!(render_markdown(&[]), "");
}

/// Every role label is one of the two the transcript documents.
#[test]
fn export_markdown_labels_only_user_and_assistant() {
    let messages = vec![
        message(Role::User, vec![text("a")]),
        message(Role::Assistant, vec![text("b")]),
    ];

    let markdown = render_markdown(&messages);
    let headings: Vec<&str> = markdown.lines().filter(|line| line.starts_with("### ")).collect();

    assert_eq!(headings, ["### User", "### Assistant"]);
}
