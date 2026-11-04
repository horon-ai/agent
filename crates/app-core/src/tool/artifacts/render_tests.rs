use super::*;

const BUDGET: usize = 48 * 1024;

/// ~200 KB of numbered lines with one panic line in the middle (line 50,000).
fn log_with_panic() -> String {
    let mut text = String::new();
    for line in 1..=60_000 {
        if line == 50_000 {
            text.push_str("thread panicked at x\n");
        } else {
            text.push_str(&format!("{line}\n"));
        }
    }
    text
}

#[test]
fn render_keeps_head_tail_pointer_and_signal_lines_within_budget() {
    let text = log_with_panic();
    assert!(text.len() > 200_000);
    let out = render_condensed(&text, "bash", BUDGET, Ok("0123456789abcdef"));

    assert!(out.len() <= BUDGET, "{} > {BUDGET}", out.len());
    assert!(out.starts_with("1\n2\n"), "head keeps the first lines");
    assert!(out.ends_with("59999\n60000\n"), "tail keeps the last lines");
    assert!(out.contains("lines omitted from `bash` output. Full output ("));
    assert!(out.contains("read file_path=\"artifact://0123456789abcdef\" with start_line/end_line.]"));
    assert!(out.contains("[matching lines from the omitted part:]\nL50000: thread panicked at x\n"));
}

#[test]
fn render_counts_omitted_bytes_exactly() {
    let text = log_with_panic();
    let out = render_condensed(&text, "bash", BUDGET, Ok("0123456789abcdef"));
    let pointer = out
        .lines()
        .find(|line| line.starts_with("[horon: "))
        .expect("pointer line");
    let omitted: usize = pointer["[horon: ".len()..].split(' ').next().unwrap().parse().unwrap();
    let head_len = out.find("[horon: ").unwrap();
    let last_signal = "L50000: thread panicked at x\n";
    let tail_len = out.len() - (out.find(last_signal).unwrap() + last_signal.len());
    assert_eq!(head_len + omitted + tail_len, text.len());
}

#[test]
fn render_without_signals_omits_the_match_header() {
    let text = "x\n".repeat(100_000);
    let out = render_condensed(&text, "mcp__fs__dump", 4_096, Ok("0123456789abcdef"));
    assert!(!out.contains("[matching lines"));
    assert!(out.len() <= 4_096);
}

#[test]
fn render_respects_char_boundaries_without_newlines() {
    // One long line of 3-byte characters: neither cut can land on a newline.
    let text = "가".repeat(100_000);
    let out = render_condensed(&text, "webfetch", 10_000, Ok("0123456789abcdef"));
    assert!(out.len() <= 10_000);
    let head = out.split('\n').next().unwrap();
    assert!(head.chars().all(|c| c == '가'));
    assert!(out.ends_with('가'));
}

#[test]
fn render_reports_a_store_failure_instead_of_a_pointer() {
    let text = "line\n".repeat(20_000);
    let out = render_condensed(&text, "bash", 4_096, Err("disk full"));
    assert!(out.contains("lines omitted from `bash` output; the full output could not be saved: disk full]"));
    assert!(!out.contains("artifact://"));
}

#[test]
fn render_caps_each_signal_line() {
    let mut text = "ok\n".repeat(30_000);
    text.push_str(&format!("error: {}\n", "z".repeat(5_000)));
    text.push_str(&"ok\n".repeat(30_000));
    let out = render_condensed(&text, "bash", BUDGET, Ok("0123456789abcdef"));
    let signal = out
        .lines()
        .find(|line| line.starts_with("L30001: error: "))
        .expect("signal line");
    assert!(signal.len() <= "L30001: ".len() + SIGNAL_LINE_MAX_BYTES);
}

#[test]
fn condense_returns_small_text_unchanged() {
    let text = "short".to_string();
    let ptr = text.as_ptr();
    let out = condense_for_context("session-a", "bash", text, BUDGET);
    assert_eq!(out.as_ptr(), ptr, "no copy when it fits");
}

#[test]
fn condense_saves_the_full_text_and_points_at_it() {
    let _home = super::tests::IsolatedHome::new();
    let text = log_with_panic();
    let out = condense_for_context("session-a", "bash", text.clone(), BUDGET);
    let start = out.find("artifact://").unwrap() + "artifact://".len();
    let digits: String = out[start..].chars().take_while(char::is_ascii_digit).collect();
    assert_eq!(load("session-a", digits.parse().unwrap()).unwrap(), text);
}
