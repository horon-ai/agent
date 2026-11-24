//! What the bridge makes of one background work list: the rows it builds, the
//! output tail it keeps, and the fields it refuses to draw raw.
//!
//! Nothing here talks to a daemon. Each test feeds one item to the function the
//! worker calls, which is the part a list answer goes through.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use horon_agent_sdk::api::BackgroundWorkItem;

use super::BackgroundJobRow;
use super::jobs::rows as build_rows;

fn item(call_id: &str, kind: &str, output: Option<&str>) -> BackgroundWorkItem {
    BackgroundWorkItem {
        call_id: call_id.to_string(),
        task_id: format!("task-{call_id}"),
        kind: kind.to_string(),
        command: Some("cargo test --workspace".to_string()),
        cwd: Some("/work/project".to_string()),
        output: output.map(|text| STANDARD.encode(text)),
    }
}

#[test]
fn a_terminal_row_keeps_the_command_and_the_last_four_output_lines() {
    let listed = build_rows(vec![item("call-1", "terminal", Some("one\ntwo\nthree\nfour\nfive\n"))]);
    assert_eq!(
        listed,
        vec![BackgroundJobRow {
            call_id: "call-1".to_string(),
            task_id: "task-call-1".to_string(),
            kind: "terminal".to_string(),
            command: Some("cargo test --workspace".to_string()),
            cwd: Some("/work/project".to_string()),
            output: vec![
                "two".to_string(),
                "three".to_string(),
                "four".to_string(),
                "five".to_string(),
            ],
        }]
    );
}

#[test]
fn a_tool_call_row_is_listed_and_is_not_cancellable() {
    let listed = build_rows(vec![item("call-2", "tool_call", None)]);
    assert_eq!(listed.len(), 1);
    assert!(!listed[0].cancellable(), "a tool call was offered as a terminal");
    assert!(listed[0].output.is_empty(), "a row with no output invented lines");
    assert!(build_rows(vec![item("call-1", "terminal", None)])[0].cancellable());
}

#[test]
fn an_output_escape_never_reaches_the_row() {
    // A job's output is daemon data, and a terminal sequence in it would act on
    // the screen the row is drawn on rather than be read.
    let row = &build_rows(vec![item("call-3", "terminal", Some("\x1b[31mred\x1b[0m\nplain"))])[0];
    assert_eq!(row.output, vec!["red".to_string(), "plain".to_string()]);
}

#[test]
fn output_that_is_not_base64_is_an_empty_tail() {
    let mut broken = item("call-4", "terminal", None);
    broken.output = Some("not base64!".to_string());
    assert!(build_rows(vec![broken])[0].output.is_empty());
}

#[test]
fn one_long_line_is_cut_to_what_a_preview_keeps() {
    let long = "x".repeat(400);
    let row = &build_rows(vec![item("call-5", "terminal", Some(&long))])[0];
    assert_eq!(row.output[0].chars().count(), 160);
}

#[test]
fn a_terminal_row_skips_blank_tail_lines_while_preserving_order() {
    let listed = build_rows(vec![item(
        "call-6",
        "terminal",
        Some("older\none\n\ntwo\nthree\n   \nfour\n\n"),
    )]);
    assert_eq!(
        listed[0].output,
        vec![
            "one".to_string(),
            "two".to_string(),
            "three".to_string(),
            "four".to_string(),
        ]
    );
}
