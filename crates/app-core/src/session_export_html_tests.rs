//! Tests for the standalone HTML report.
//!
//! They live beside the Markdown renderer's tests and check the two promises a
//! saved report makes: the redacted projection is the one Markdown shows, and
//! hostile transcript text stays inert.

use crate::message::{ContentBlock, Role};
use crate::session::StoredMessage;
use crate::session_export::MAX_TOOL_RESULT_BYTES;
use crate::session_export_html::{REPORT_CSP, render_html};

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

/// Hostile transcript markup arrives as characters, never as nodes.
#[test]
fn html_report_escapes_hostile_transcript_markup() {
    let hostile = "<script>alert('canary')</script><img src=\"https://example.invalid/x.png\" onerror=\"alert(1)\">";
    let messages = vec![message(Role::User, vec![text(hostile)])];

    let html = render_html(&messages);

    assert!(
        !html.contains("<script"),
        "a stored script tag must not become a node:\n{html}"
    );
    assert!(
        !html.contains("<img"),
        "a stored image tag must not become a node:\n{html}"
    );
    assert!(html.contains("&lt;script&gt;alert(&#39;canary&#39;)&lt;/script&gt;"));
    assert!(html.contains("&lt;img src=&quot;https://example.invalid/x.png&quot; onerror=&quot;alert(1)&quot;&gt;"));
}

/// The document loads nothing: no script, no remote style, no external asset.
#[test]
fn html_report_loads_nothing_from_the_network() {
    let messages = vec![message(Role::Assistant, vec![text("an answer")])];

    let html = render_html(&messages);

    assert!(html.starts_with("<!DOCTYPE html>"), "a standalone document");
    assert!(html.trim_end().ends_with("</html>"), "a whole document:\n{html}");
    assert!(html.contains(&format!(
        "<meta http-equiv=\"Content-Security-Policy\" content=\"{REPORT_CSP}\">"
    )));
    assert!(html.contains(REPORT_CSP));
    for forbidden in [
        "<script", "<link", "<iframe", "src=", "href=", "@import", "url(", "http://", "https://",
    ] {
        assert!(
            !html.contains(forbidden),
            "the report carries {forbidden:?}, which could load something:\n{html}"
        );
    }
}

/// A secret inside a tool result never reaches the report either.
#[test]
fn html_report_redacts_secret_in_tool_result() {
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

    let html = render_html(&messages);

    assert!(!html.contains(token), "the token must be redacted:\n{html}");
    assert!(html.contains("[REDACTED_SECRET]"));
    assert!(html.contains("<div class=\"label\">Result</div>"));
}

/// Turns keep transcript order and each one is named by its role.
#[test]
fn html_report_names_each_turn_in_order() {
    let messages = vec![
        message(Role::User, vec![text("what does the daemon do?")]),
        message(Role::Assistant, vec![text("it runs turns headlessly.")]),
    ];

    let html = render_html(&messages);

    let user = html.find("<h2>User</h2>").expect("user heading");
    let assistant = html.find("<h2>Assistant</h2>").expect("assistant heading");
    assert!(user < assistant, "the assistant reply must follow the user turn");
    assert!(html.contains("it runs turns headlessly."));
}

/// A runaway tool result is cut with the same marker the Markdown export uses.
#[test]
fn html_report_truncates_long_tool_result() {
    let long = "x".repeat(MAX_TOOL_RESULT_BYTES * 2);
    let messages = vec![message(
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: "call_1".to_string(),
            content: long,
            is_error: Some(true),
            verdict: None,
        }],
    )];

    let html = render_html(&messages);

    assert!(html.contains("<div class=\"label\">Error</div>"));
    assert!(html.contains(&format!("{} bytes total", MAX_TOOL_RESULT_BYTES * 2)));
}

/// An empty transcript is still a complete, offline document.
#[test]
fn html_report_of_empty_history_is_a_whole_document() {
    let html = render_html(&[]);

    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.trim_end().ends_with("</html>"));
    assert!(!html.contains("<section"), "no turn without a message:\n{html}");
}
