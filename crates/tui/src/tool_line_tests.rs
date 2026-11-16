//! Tests for the tool line, kept out of `tool_line.rs` so production code
//! stays inside its size budget.
//!
//! These cover the parts that can be wrong without a terminal: the summary read
//! out of JSON that is still arriving, the preview cut, width-aware truncation
//! and the elapsed wording.

use super::*;
use crate::tool_line::{
    DEFAULT_OUTPUT_LINES, MAX_INPUT_BYTES, MAX_OUTPUT_LINES, MAX_PREVIEW_CELLS, OutputHead, append_input, cut_output,
    format_elapsed, lines, summarize, truncate_cells,
};
use crate::transcript::{Item, ToolCall, ToolState, Transcript};

fn call(input: &str) -> ToolCall {
    ToolCall {
        call_id: "c1".to_string(),
        name: "bash".to_string(),
        state: ToolState::Running,
        input: input.to_string(),
        duration_ms: None,
        output: None,
    }
}

#[test]
fn summary_reads_a_command_from_complete_json() {
    let input = r#"{"command":"ls -la","description":"list files"}"#;
    assert_eq!(summarize(input), Some("ls -la".to_string()));
}

#[test]
fn summary_reads_a_command_from_halffinished_json() {
    // What a streaming call looks like halfway through: no closing brace, no
    // closing quote. A parse would fail, so the value is scanned instead.
    let input = r#"{"command":"cargo test -p horon-tu"#;
    assert_eq!(summarize(input), Some("cargo test -p horon-tu".to_string()));
}

#[test]
fn summary_prefers_the_most_specific_primary_key() {
    let input = r#"{"path":"/tmp/a","command":"rm -rf /tmp/a"}"#;
    assert_eq!(
        summarize(input),
        Some("rm -rf /tmp/a".to_string()),
        "a command says more than the path it touches"
    );
}

#[test]
fn summary_collapses_newlines_onto_one_line() {
    let input = r#"{"command":"echo a\necho b"}"#;
    let summary = summarize(input).expect("a command");
    assert!(!summary.contains('\n'), "{summary:?}");
    assert_eq!(summary, "echo a echo b");
}

#[test]
fn summary_is_absent_without_a_usable_argument() {
    assert_eq!(summarize(""), None);
    assert_eq!(summarize("   "), None);
    assert_eq!(summarize(r#"{"count":3}"#), None, "a number says nothing");
    assert_eq!(summarize(r#"{"command":""}"#), None);
}

#[test]
fn input_stops_growing_at_the_byte_cap() {
    let mut input = String::new();
    let chunk = "a".repeat(1000);
    for _ in 0..10 {
        append_input(&mut input, &chunk);
    }
    assert_eq!(input.len(), MAX_INPUT_BYTES, "the cap is what is kept");
}

#[test]
fn input_cap_lands_on_a_character_boundary() {
    let mut input = String::new();
    append_input(&mut input, &"가".repeat(MAX_INPUT_BYTES));
    assert!(input.is_char_boundary(input.len()));
    assert!(input.len() <= MAX_INPUT_BYTES);
    assert!(
        !input.is_empty(),
        "a cut that keeps nothing is worse than a cut at the boundary"
    );
}

#[test]
fn output_preview_keeps_the_head_and_counts_the_rest() {
    let output = "one\ntwo\nthree\nfour\nfive";
    let head = cut_output(output, 3).expect("three rows");
    assert_eq!(head.rows, ["one", "two", "three"]);
    assert_eq!(head.total_rows, 5);
    assert_eq!(head.hidden_rows(), 2);
}

#[test]
fn output_preview_hides_everything_when_the_setting_is_zero() {
    assert_eq!(cut_output("one\ntwo", 0), None);
    assert_eq!(cut_output("", 3), None, "an empty output draws no block");
}

#[test]
fn output_preview_is_capped_even_when_the_setting_is_larger() {
    let output = (0..MAX_OUTPUT_LINES + 10)
        .map(|index| index.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let head = cut_output(&output, MAX_OUTPUT_LINES + 100).expect("a preview");
    assert_eq!(head.rows.len(), MAX_OUTPUT_LINES);
    assert_eq!(head.hidden_rows(), 10);
}

#[test]
fn output_preview_cuts_a_very_wide_row() {
    let output = "x".repeat(MAX_PREVIEW_CELLS * 3);
    let head = cut_output(&output, 1).expect("one row");
    let row = &head.rows[0];
    assert!(row.chars().count() <= MAX_PREVIEW_CELLS);
    assert!(row.ends_with('…'), "{row:?}");
}

#[test]
fn truncation_never_splits_a_wide_character() {
    // Three cells of CJK: each character is two cells, and the marker takes one,
    // so two characters fit and the third cannot start.
    assert_eq!(truncate_cells("한국어", 5), "한국…");
    assert_eq!(truncate_cells("한국어", 6), "한국어");
    assert_eq!(truncate_cells("abc", 3), "abc");
    assert_eq!(truncate_cells("abcd", 3), "ab…");
}

#[test]
fn truncation_keeps_an_emoji_whole() {
    let cut = truncate_cells("🙂🙂🙂", 5);
    assert!(cut.ends_with('…'), "{cut:?}");
    assert_eq!(cut, "🙂🙂…");
}

#[test]
fn elapsed_is_shown_in_seconds_under_a_minute() {
    assert_eq!(format_elapsed(0), "0.0s");
    assert_eq!(format_elapsed(420), "0.4s");
    assert_eq!(format_elapsed(59_949), "59.9s");
}

#[test]
fn elapsed_is_shown_in_minutes_at_a_minute_and_over() {
    assert_eq!(format_elapsed(60_000), "1m 00s");
    assert_eq!(format_elapsed(125_000), "2m 05s");
}

#[test]
fn a_running_line_names_the_tool_and_its_argument() {
    let theme = Theme::default();
    let symbols = Symbols::default();
    let rows = lines(&call(r#"{"command":"ls -la"}"#), &theme, symbols, 80);
    assert_eq!(rows.len(), 1, "a running call draws one row");
    let text: String = rows[0].spans.iter().map(|span| span.content.as_ref()).collect();
    assert!(text.contains("bash"), "{text:?}");
    assert!(text.contains("ls -la"), "{text:?}");
    assert!(text.contains("running"), "{text:?}");
    assert!(
        !text.contains('*'),
        "the lead is a glyph now, not an asterisk: {text:?}"
    );
}

#[test]
fn a_finished_line_shows_elapsed_and_the_preview() {
    let theme = Theme::default();
    let symbols = Symbols::default();
    let mut finished = call("");
    finished.state = ToolState::Done;
    finished.duration_ms = Some(1_500);
    finished.output = cut_output("a\nb\nc\nd", 3);
    let rows = lines(&finished, &theme, symbols, 80);
    let first: String = rows[0].spans.iter().map(|span| span.content.as_ref()).collect();
    assert!(first.contains("done"), "{first:?}");
    assert!(first.contains("1.5s"), "{first:?}");
    let rest: Vec<String> = rows[1..]
        .iter()
        .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect())
        .collect();
    assert_eq!(rest, ["  a", "  b", "  c", "  +1 lines"]);
}

#[test]
fn a_failed_line_carries_the_error_text() {
    let theme = Theme::default();
    let symbols = Symbols::default();
    let mut failed = call("");
    failed.state = ToolState::Failed("permission denied".to_string());
    let rows = lines(&failed, &theme, symbols, 80);
    let text: String = rows
        .iter()
        .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("failed"), "{text:?}");
    assert!(text.contains("permission denied"), "{text:?}");
}

#[test]
fn a_failure_that_its_output_repeats_is_drawn_once() {
    let theme = Theme::default();
    let symbols = Symbols::default();
    let mut failed = call("");
    failed.state = ToolState::Failed("Error: read a.txt: no such file".to_string());
    failed.output = cut_output("Error: read a.txt: no such file", 3);
    let rows = lines(&failed, &theme, symbols, 80);
    let text: String = rows
        .iter()
        .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        text.matches("no such file").count(),
        1,
        "the message is said once: {text:?}"
    );
}

#[test]
fn a_narrow_pane_drops_the_summary_rather_than_the_name() {
    let theme = Theme::default();
    let symbols = Symbols::default();
    let rows = lines(&call(r#"{"command":"ls -la"}"#), &theme, symbols, 12);
    let text: String = rows[0].spans.iter().map(|span| span.content.as_ref()).collect();
    assert!(text.contains("bash"), "{text:?}");
    assert!(!text.contains("ls -la"), "{text:?}");
}

#[test]
fn parallel_calls_of_one_tool_finish_by_their_own_id() {
    let mut transcript = Transcript::new();
    transcript.tool_start("a", "bash");
    transcript.tool_start("b", "bash");
    transcript.tool_input("a", r#"{"command":"echo a"}"#);
    transcript.tool_input("b", r#"{"command":"echo b"}"#);
    // The second call finishes first: only the id says which line it belongs to.
    transcript.tool_done("b", "bash", Some("b out"), None);
    transcript.tool_done("a", "bash", Some("a out"), None);

    let calls = tool_calls(&transcript);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0].input, r#"{"command":"echo a"}"#, "input stayed with its call");
    assert_eq!(calls[1].input, r#"{"command":"echo b"}"#);
    assert_eq!(
        calls[0].output.as_ref().map(|head| head.rows.join("")),
        Some("a out".to_string())
    );
    assert_eq!(
        calls[1].output.as_ref().map(|head| head.rows.join("")),
        Some("b out".to_string())
    );
    assert!(calls.iter().all(|call| call.state == ToolState::Done), "{calls:?}");
}

#[test]
fn a_result_without_an_id_lands_on_the_oldest_unfinished_call() {
    let mut transcript = Transcript::new();
    transcript.tool_start("", "bash");
    transcript.tool_start("", "bash");
    transcript.tool_done("", "bash", None, None);
    let calls = tool_calls(&transcript);
    assert_eq!(
        calls[0].state,
        ToolState::Done,
        "the first call finished first: {calls:?}"
    );
    assert_eq!(calls[1].state, ToolState::Running, "{calls:?}");
}

#[test]
fn an_unknown_result_is_appended_rather_than_dropped() {
    let mut transcript = Transcript::new();
    transcript.tool_done("ghost", "bash", None, Some("no such tool"));
    let calls = tool_calls(&transcript);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "bash");
    assert_eq!(calls[0].state, ToolState::Failed("no such tool".to_string()));
}

#[test]
fn a_result_for_a_call_that_never_started_keeps_its_id() {
    let mut transcript = Transcript::new();
    transcript.tool_done("late", "read", Some("body"), None);
    let calls = tool_calls(&transcript);
    assert_eq!(calls[0].call_id, "late");
    assert_eq!(calls[0].output.as_ref().map(|head| head.rows.len()), Some(1));
}

#[test]
fn a_replayed_result_does_not_erase_the_first_one() {
    let mut transcript = Transcript::new();
    transcript.tool_start("c1", "bash");
    transcript.tool_done("c1", "bash", Some("body"), Some("it broke"));
    let first = tool_calls(&transcript)[0].clone();
    assert!(first.duration_ms.is_some(), "the first result timed the call");

    // The same frame arrives again, as a reconnect replays it.
    transcript.tool_done("c1", "bash", Some("body"), Some("it broke"));
    let after = &tool_calls(&transcript)[0];
    assert_eq!(after.duration_ms, first.duration_ms, "the duration survives: {after:?}");
    assert_eq!(
        after.state,
        ToolState::Failed("it broke".to_string()),
        "a replay must not turn a failure into a success: {after:?}"
    );

    // A repeat that claims success does not overwrite the failure either.
    transcript.tool_done("c1", "bash", Some("body"), None);
    assert_eq!(
        tool_calls(&transcript)[0].state,
        ToolState::Failed("it broke".to_string())
    );
}

#[test]
fn a_start_that_arrives_after_its_result_draws_no_second_line() {
    let mut transcript = Transcript::new();
    transcript.tool_done("c1", "bash", Some("body"), None);
    transcript.tool_start("c1", "bash");
    let calls = tool_calls(&transcript);
    assert_eq!(calls.len(), 1, "one call, one line: {calls:?}");
    assert_eq!(calls[0].state, ToolState::Done, "the result still stands");
}

#[test]
fn the_preview_setting_is_read_when_the_call_finishes() {
    let mut transcript = Transcript::new();
    transcript.set_tool_output_lines(0);
    transcript.tool_start("c1", "bash");
    transcript.tool_done("c1", "bash", Some("a\nb"), None);
    assert_eq!(tool_calls(&transcript)[0].output, None, "zero hides the preview");
    assert_eq!(transcript.tool_output_lines(), 0);
}

fn tool_calls(transcript: &Transcript) -> Vec<&ToolCall> {
    transcript
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::Tool(call) => Some(call),
            _ => None,
        })
        .collect()
}

#[test]
fn the_default_preview_setting_is_three_rows() {
    assert_eq!(DEFAULT_OUTPUT_LINES, 3);
    assert_eq!(
        cut_output("1\n2\n3\n4", DEFAULT_OUTPUT_LINES).map(|head| head.rows.len()),
        Some(3)
    );
}

#[test]
fn an_output_head_reports_what_it_hides() {
    let head = OutputHead {
        rows: vec!["a".to_string()],
        total_rows: 4,
    };
    assert_eq!(head.hidden_rows(), 3);
}

#[test]
fn a_call_left_running_ends_neutrally_when_the_turn_ends() {
    let mut transcript = Transcript::new();
    transcript.tool_start("c1", "bash");
    transcript.tool_input("c1", r#"{"command":"sleep 30"}"#);
    transcript.end_running_tools();

    let theme = Theme::default();
    let symbols = Symbols::default();
    let calls = tool_calls(&transcript);
    let text: String = lines(calls[0], &theme, symbols, 80)
        .iter()
        .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!text.contains("running"), "the line stopped running: {text:?}");
    assert!(!text.contains("done"), "an unfinished call is not a success: {text:?}");
    assert!(text.contains(crate::transcript::TURN_ENDED), "{text:?}");
}

/// The sentence the approval gate writes into a refused call, request id and
/// all: the id is how a parked prompt finds its own line.
fn refusal(request: &str) -> String {
    format!(
        "Error: Tool 'write' needs approval before it runs: write mutates a file in write mode. Request {request} \
         is queued; answer it through the approval surface. Nothing was executed."
    )
}

fn drawn(rows: &[ratatui::text::Line<'static>]) -> String {
    rows.iter()
        .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_refused_call_is_drawn_as_one_short_phrase() {
    let theme = Theme::default();
    let symbols = Symbols::default();
    let mut refused = call(r#"{"file_path":"/tmp/x"}"#);
    refused.name = "write".to_string();
    refused.state = ToolState::Failed(refusal("r1"));
    for width in [100, 40] {
        let rows = lines(&refused, &theme, symbols, width);
        assert_eq!(
            rows.len(),
            2,
            "the refusal took {} rows at width {width}: {}",
            rows.len(),
            drawn(&rows)
        );
        let text = drawn(&rows);
        assert!(text.contains("needs approval"), "{text}");
        assert!(
            !text.contains("is queued") && !text.contains("r1"),
            "the gate's sentence is still drawn: {text}"
        );
    }
}

#[test]
fn a_refused_call_still_stores_the_sentence_the_prompt_pairs_with() {
    let theme = Theme::default();
    let symbols = Symbols::default();
    let mut refused = call("");
    refused.state = ToolState::Failed(refusal("req_42"));
    let _ = lines(&refused, &theme, symbols, 80);
    let ToolState::Failed(stored) = &refused.state else {
        panic!("the call stopped being a failure");
    };
    assert!(
        stored.contains("req_42"),
        "the stored refusal lost its request id: {stored}"
    );
}

#[test]
fn a_parked_call_is_drawn_as_waiting_not_as_a_failure() {
    let theme = Theme::default();
    let symbols = Symbols::default();
    let mut parked = call(r#"{"file_path":"/tmp/x"}"#);
    parked.name = "write".to_string();
    // What a parked call looks like: the gate's sentence, no duration, no
    // output, and the request still waiting for a decision.
    parked.state = ToolState::Waiting(refusal("r1"));

    let rows = lines(&parked, &theme, symbols, 100);
    assert_eq!(rows.len(), 1, "a parked call is one row: {}", drawn(&rows));
    let text = drawn(&rows);
    assert!(text.contains("needs approval"), "{text}");
    assert!(!text.contains("failed"), "a waiting call says it failed: {text}");
    assert!(
        text.contains(symbols.tool_running) && !text.contains(symbols.tool_error),
        "a waiting call wears the failure glyph: {text}"
    );
}

#[test]
fn a_real_failure_and_a_denial_are_still_drawn_as_failed() {
    let theme = Theme::default();
    let symbols = Symbols::default();
    let mut failed = call("");
    failed.state = ToolState::Failed("Error: read a.txt: no such file".to_string());
    failed.duration_ms = Some(40);
    let text = drawn(&lines(&failed, &theme, symbols, 100));
    assert!(text.contains("failed"), "{text}");
    assert!(text.contains(symbols.tool_error), "{text}");

    // A denial ends the wait, so the line is the failure the harness reported
    // and the refusal still names its request.
    let mut transcript = Transcript::new();
    transcript.tool_start("c1", "write");
    transcript.tool_input("c1", r#"{"file_path":"/tmp/x"}"#);
    transcript.tool_done("c1", "write", None, Some(&refusal("req_9")));
    transcript.park_request("req_9");
    let waiting = match &transcript.items()[0] {
        Item::Tool(call) => call.state.clone(),
        other => panic!("the call was not recorded: {other:?}"),
    };
    assert!(matches!(waiting, ToolState::Waiting(_)), "{waiting:?}");
    transcript.settle_request("req_9");
    let settled = match &transcript.items()[0] {
        Item::Tool(call) => call.state.clone(),
        other => panic!("the call was not recorded: {other:?}"),
    };
    assert!(matches!(settled, ToolState::Failed(_)), "{settled:?}");
    assert!(
        transcript.argument_summary_for_request("req_9").is_some(),
        "a denied call stopped naming its request, so its prompt can no longer be paired"
    );
    let ToolState::Failed(message) = settled else {
        unreachable!()
    };
    let mut denied = call("");
    denied.state = ToolState::Failed(message);
    let text = drawn(&lines(&denied, &theme, symbols, 100));
    assert!(text.contains("failed"), "a denial is not drawn as failed: {text}");
}
