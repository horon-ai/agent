use super::*;

const BUDGET: usize = 48 * 1024;
const DEFAULT_SETTINGS: crate::config::ArtifactOutputConfig = crate::config::ArtifactOutputConfig {
    spill_threshold_kib: None,
    head_kib: None,
    tail_kib: None,
    tail_lines: None,
    file_cap_mib: None,
};

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
    let out = render_condensed(&text, "bash", BUDGET, Ok("0123456789abcdef"), 0, &DEFAULT_SETTINGS);

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
    let out = render_condensed(&text, "bash", BUDGET, Ok("0123456789abcdef"), 0, &DEFAULT_SETTINGS);
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
    let out = render_condensed(
        &text,
        "mcp__fs__dump",
        4_096,
        Ok("0123456789abcdef"),
        0,
        &DEFAULT_SETTINGS,
    );
    assert!(!out.contains("[matching lines"));
    assert!(out.len() <= 4_096);
}

#[test]
fn render_respects_char_boundaries_without_newlines() {
    // One long line of 3-byte characters: neither cut can land on a newline.
    let text = "가".repeat(100_000);
    let out = render_condensed(&text, "webfetch", 10_000, Ok("0123456789abcdef"), 0, &DEFAULT_SETTINGS);
    assert!(out.len() <= 10_000);
    let head = out.split('\n').next().unwrap();
    assert!(head.chars().all(|c| c == '가'));
    assert!(out.ends_with('가'));
}

#[test]
fn render_reports_a_store_failure_instead_of_a_pointer() {
    let text = "line\n".repeat(20_000);
    let out = render_condensed(&text, "bash", 4_096, Err("disk full"), 0, &DEFAULT_SETTINGS);
    assert!(out.contains("lines omitted from `bash` output; the full output could not be saved: disk full]"));
    assert!(!out.contains("artifact://"));
}

#[test]
fn render_caps_each_signal_line() {
    let mut text = "ok\n".repeat(30_000);
    text.push_str(&format!("error: {}\n", "z".repeat(5_000)));
    text.push_str(&"ok\n".repeat(30_000));
    let out = render_condensed(&text, "bash", BUDGET, Ok("0123456789abcdef"), 0, &DEFAULT_SETTINGS);
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
    assert_eq!(out, "short", "output that fits the budget is returned as it is");
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

#[test]
fn preview_column_cap_keeps_utf8_and_retains_original() {
    let _home = super::tests::IsolatedHome::new();
    let text = "가나다라마\r\nok\r\n";
    let out = preview(
        "width",
        "bash",
        text,
        BUDGET,
        4,
        crate::config::SecretsMode::Off,
        &DEFAULT_SETTINGS,
    )
    .unwrap();
    assert!(out.starts_with("가나다…\r\nok\r\n"));
    let (_, address) = out.split_once("artifact://").unwrap();
    let (id, _) = address.split_once('"').unwrap();
    assert_eq!(load("width", id.parse().unwrap()).unwrap(), text);
}

#[test]
fn preview_column_cap_leaves_inline_json_parseable() {
    let text = r#"{"message":"가나다라마"}"#;
    assert!(
        preview(
            "width-json",
            "bash",
            text,
            BUDGET,
            4,
            crate::config::SecretsMode::Off,
            &DEFAULT_SETTINGS
        )
        .is_none()
    );
}

#[test]
fn shared_output_budget_limits_preview_and_retains_original() {
    let _home = super::tests::IsolatedHome::new();
    let text = format!("first\n{}penultimate\nlast\n", "body\n".repeat(400));
    let settings = crate::config::ArtifactOutputConfig {
        spill_threshold_kib: Some(1),
        head_kib: Some(0),
        tail_kib: Some(usize::MAX),
        tail_lines: Some(1),
        file_cap_mib: Some(1),
    };
    let out = preview(
        "budget",
        "bash",
        &text,
        BUDGET,
        0,
        crate::config::SecretsMode::Off,
        &settings,
    )
    .unwrap();
    assert!(
        out.starts_with("[horon:") && out.ends_with("\nlast\n") && out.len() <= 1024,
        "{out}"
    );
    assert!(!out.contains("penultimate"));
    let (_, address) = out.split_once("artifact://").unwrap();
    let (id, _) = address.split_once('"').unwrap();
    assert_eq!(load("budget", id.parse().unwrap()).unwrap(), text);

    let json = format!("{{\"text\":\"{}\"}}", "x".repeat(1500));
    assert!(
        preview(
            "budget",
            "mcp",
            &json,
            BUDGET,
            0,
            crate::config::SecretsMode::Off,
            &settings
        )
        .is_none()
    );
}

#[test]
fn shared_output_budget_caps_the_saved_file_and_keeps_both_ends() {
    let _home = super::tests::IsolatedHome::new();
    let text = format!("first\n{}last\n", "body\n".repeat(250_000));
    let settings = crate::config::ArtifactOutputConfig {
        file_cap_mib: Some(1),
        ..DEFAULT_SETTINGS
    };
    let out = preview(
        "file-cap",
        "bash",
        &text,
        BUDGET,
        0,
        crate::config::SecretsMode::Off,
        &settings,
    )
    .unwrap();
    let (_, address) = out.split_once("artifact://").unwrap();
    let (id, _) = address.split_once('"').unwrap();
    let saved = load("file-cap", id.parse().unwrap()).unwrap();
    assert!(saved.len() <= 1024 * 1024);
    assert!(saved.starts_with("first\n") && saved.ends_with("last\n"));
    assert!(saved.contains("[ARTIFACT TRUNCATED:"));
}

#[test]
fn column_cap_handles_exact_multibyte_boundaries_and_non_json_text() {
    let _home = super::tests::IsolatedHome::new();
    let exact_chars = "가나다라12345678\n123456789012";
    assert!(
        preview(
            "cols",
            "bash",
            exact_chars,
            BUDGET,
            12,
            crate::config::SecretsMode::Off,
            &DEFAULT_SETTINGS,
        )
        .is_none()
    );

    let curly_non_json = "let x = { very_long_non_json_expression };";
    let wrapped = preview(
        "cols",
        "bash",
        curly_non_json,
        BUDGET,
        12,
        crate::config::SecretsMode::Off,
        &DEFAULT_SETTINGS,
    )
    .expect("long non-json text exceeding columns triggers preview");
    assert!(wrapped.contains("artifact://"), "{wrapped}");
}
