//! Recovery band and emergency-truncation signal tests.
//!
//! Kept in their own file because `crates/compaction-core/src/lib.rs` is
//! already over the code-size budget and the ratchet forbids growing it.

use super::emergency_truncate::line_has_signal_word;
use super::*;

const TRUNCATION_BUDGET: usize = EMERGENCY_TOOL_RESULT_MAX_CHARS;

fn middle_with_error_line(pad: usize) -> String {
    format!(
        "{}\nreading src/lib.rs\n{}\nerror: the build failed\n{}\ntrailing output\n{}",
        "head line\n".repeat(pad),
        "noise\n".repeat(pad),
        "more noise\n".repeat(pad),
        "tail line\n".repeat(pad),
    )
}

#[test]
fn recovery_band_clamps_the_24k_tail() {
    let budget = compute_context_budget(24_000, DEFAULT_RESERVE_TOKENS, DEFAULT_RECENT_TOKENS, None)
        .expect("24k window fits a budget");
    let summary_allowance = (budget.threshold_tokens / 4).min(SUMMARY_TOKEN_ALLOWANCE_CAP);
    println!(
        "24k budget: threshold={} recent={} summary_allowance={}",
        budget.threshold_tokens, budget.recent_tokens, summary_allowance
    );
    assert_eq!(budget.threshold_tokens, 6_592);
    assert_eq!(budget.recent_tokens, 3_625);
    assert!(
        budget.recent_tokens + summary_allowance <= ((budget.threshold_tokens as f64) * 0.8) as usize,
        "recent tail plus summary must fit the recovery band"
    );
}

#[test]
fn recovery_band_leaves_the_200k_budget_unchanged() {
    let budget = compute_context_budget(200_000, DEFAULT_RESERVE_TOKENS, DEFAULT_RECENT_TOKENS, None)
        .expect("200k window fits a budget");
    println!(
        "200k budget: threshold={} recent={}",
        budget.threshold_tokens, budget.recent_tokens
    );
    assert_eq!(budget.threshold_tokens, 140_000);
    assert_eq!(budget.recent_tokens, 20_000);
}

#[test]
fn recovery_band_never_starves_the_tail_floor() {
    // A window whose band has no room still keeps the 1,024-token floor.
    let budget = compute_context_budget(18_000, DEFAULT_RESERVE_TOKENS, DEFAULT_RECENT_TOKENS, None)
        .expect("18k window fits a budget");
    assert!(budget.recent_tokens >= MIN_RECENT_TOKENS);
}

#[test]
fn truncation_without_signals_stays_in_the_byte_budget() {
    let content = "a".repeat(1024 * 1024);
    let truncated = emergency_truncated_tool_result(&content, TRUNCATION_BUDGET, false);
    println!(
        "1 MiB no-signal truncation: bytes={} budget={} error_prefix={} signal_header={}",
        truncated.len(),
        TRUNCATION_BUDGET,
        truncated.starts_with("[tool error] "),
        truncated.contains("error/warning lines from the omitted part")
    );
    assert!(truncated.len() <= TRUNCATION_BUDGET, "got {} bytes", truncated.len());
    assert!(truncated.contains("chars truncated for context recovery"));
    assert!(!truncated.starts_with("[tool error] "));
    assert!(!truncated.contains("error/warning lines from the omitted part"));
}

#[test]
fn truncation_keeps_middle_error_lines_and_the_error_marker() {
    let content = middle_with_error_line(400);
    let truncated = emergency_truncated_tool_result(&content, TRUNCATION_BUDGET, true);
    println!(
        "error truncation: prefix={} middle_error={} bytes={}",
        truncated.starts_with("[tool error] "),
        truncated.contains("error: the build failed"),
        truncated.len()
    );
    assert!(truncated.starts_with("[tool error] "));
    assert!(truncated.contains("chars truncated for context recovery"));
    assert!(
        truncated.contains("error: the build failed"),
        "the omitted middle's error line must survive"
    );
    assert!(truncated.len() <= TRUNCATION_BUDGET, "got {} bytes", truncated.len());
}

#[test]
fn truncation_bounds_the_signal_block() {
    let mut content = String::new();
    for index in 0..40 {
        content.push_str(&"filler\n".repeat(200));
        content.push_str(&format!("warning: retry {index} failed\n"));
    }
    let truncated = emergency_truncated_tool_result(&content, TRUNCATION_BUDGET, false);
    let header = "[error/warning lines from the omitted part:]\n";
    let start = truncated.find(header).expect("signal header");
    let rest = &truncated[start + header.len()..];
    let block_end = rest.find("\n\n").expect("signal block ends with a blank line");
    let block = &rest[..block_end];
    let lines: Vec<&str> = block.lines().collect();
    assert!(!lines.is_empty());
    assert!(lines.len() <= EMERGENCY_SIGNAL_MAX_LINES, "got {} lines", lines.len());
    assert!(
        block.len() <= EMERGENCY_SIGNAL_TOTAL_MAX_BYTES,
        "got {} bytes",
        block.len()
    );
    assert!(lines.iter().all(|line| line.len() <= EMERGENCY_SIGNAL_LINE_MAX_BYTES));
    assert!(truncated.len() <= TRUNCATION_BUDGET, "got {} bytes", truncated.len());
}

#[test]
fn truncation_matches_only_whole_signal_words() {
    assert!(line_has_signal_word("error: boom"));
    assert!(line_has_signal_word("Traceback (most recent call last):"));
    assert!(line_has_signal_word("PANIC at the core"));
    assert!(!line_has_signal_word("errors_handled = 3"));
    assert!(!line_has_signal_word("no problem here"));
}

#[test]
fn truncation_keeps_hangul_char_boundaries() {
    let content = format!("{}오류: 실패한 도구{}", "가".repeat(2_000), "나".repeat(2_000));
    let truncated = emergency_truncated_tool_result(&content, TRUNCATION_BUDGET, true);
    assert!(truncated.is_char_boundary(truncated.len()));
    assert!(truncated.len() <= TRUNCATION_BUDGET, "got {} bytes", truncated.len());
    assert!(truncated.contains("chars truncated for context recovery"));
}

#[test]
fn truncation_is_deterministic_across_repeated_calls() {
    let content = middle_with_error_line(300);
    let first = emergency_truncated_tool_result(&content, TRUNCATION_BUDGET, true);
    let second = emergency_truncated_tool_result(&content, TRUNCATION_BUDGET, true);
    assert_eq!(first, second);
}
