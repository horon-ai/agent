use super::*;

fn cargo_build_fixture() -> String {
    let mut out = String::new();
    for unit in 0..40 {
        out.push_str(&format!("   Compiling unit-{unit} v0.1.0 (/tmp/crates/unit-{unit})\n"));
    }
    out.push_str("    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.23s\n");
    out.push_str("warning: unused variable: `x`\n");
    out.push_str("  --> src/lib.rs:3:9\n");
    out
}

#[test]
fn cargo_build_drops_progress_lines() {
    let output = cargo_build_fixture();
    assert!(output.len() >= MIN_OUTPUT_BYTES);
    let minimized = minimize("cargo build --release", &output, Some(0)).expect("minimized");
    assert!(!minimized.contains("Compiling"), "{minimized}");
    assert!(minimized.contains("Finished"), "{minimized}");
    assert!(minimized.contains("warning: unused variable"), "{minimized}");
}

#[test]
fn cargo_test_success_keeps_only_verdict_lines() {
    let mut out = String::new();
    for unit in 0..40 {
        out.push_str(&format!("   Compiling unit-{unit} v0.1.0\n"));
    }
    out.push_str("running 2 tests\n");
    out.push_str("test first ... ok\n");
    out.push_str("test second ... ok\n");
    out.push_str("\ntest result: ok. 2 passed; 0 failed; 0 ignored\n");
    let minimized = minimize("cargo test", &out, Some(0)).expect("minimized");
    assert!(!minimized.contains("Compiling"));
    assert!(!minimized.contains("test first"));
    assert!(minimized.contains("test result: ok"));
    assert!(minimized.contains("running 2 tests"));
}

#[test]
fn collapses_identical_consecutive_lines() {
    let mut out = String::new();
    for _ in 0..300 {
        out.push_str("same line\n");
    }
    let minimized = minimize("printf dup", &out, Some(0)).expect("minimized");
    assert_eq!(minimized.trim(), "same line (×300)");
}

#[test]
fn piped_commands_pass_through() {
    let output = cargo_build_fixture();
    assert_eq!(minimize("cargo build | tee log", &output, Some(0)), None);
}

#[test]
fn multi_segment_commands_pass_through() {
    let output = cargo_build_fixture();
    assert_eq!(minimize("cargo build && cargo test", &output, Some(0)), None);
}

#[test]
fn substrings_and_heredocs_pass_through() {
    let output = cargo_build_fixture();
    assert_eq!(minimize("echo $(cargo build)", &output, Some(0)), None);
    assert_eq!(minimize("cargo build <<EOF", &output, Some(0)), None);
}

#[test]
fn short_output_passes_through() {
    assert_eq!(minimize("cargo build", "short", Some(0)), None);
}

#[test]
fn unchanged_output_passes_through() {
    let output = "a\nb\nc\n".repeat(100);
    // Only the generic step applies to an unknown program; there is nothing to
    // collapse and no ANSI code to strip.
    assert_eq!(minimize("my-tool", &output, Some(0)), None);
}

#[test]
fn strips_ansi_escapes_before_filtering() {
    let output = format!("{}\u{1b}[31mred line\u{1b}[0m\n", "x\n".repeat(600));
    let minimized = minimize("my-tool", &output, Some(0)).expect("minimized");
    assert!(minimized.contains("red line"));
    assert!(!minimized.contains('\u{1b}'));
}

#[test]
fn pytest_success_keeps_last_non_empty_line() {
    let mut out = String::new();
    for index in 0..60 {
        out.push_str(&format!("test_module::case_{index} PASSED\n"));
    }
    out.push_str("= 60 passed in 1.23s =\n");
    let minimized = minimize("pytest -q", &out, Some(0)).expect("minimized");
    assert_eq!(minimized.trim(), "= 60 passed in 1.23s =");
}

#[test]
fn pytest_failure_keeps_the_failure_section() {
    let mut out = String::new();
    for index in 0..60 {
        out.push_str(&format!("test_module::case_{index} PASSED\n"));
    }
    out.push_str("=================================== FAILURES ===================================\n");
    out.push_str("def test_case_value():\nE   assert 1 == 2\n");
    let minimized = minimize("pytest -q", &out, Some(1)).expect("minimized");
    assert!(minimized.starts_with("===="));
    assert!(minimized.contains("assert 1 == 2"));
}

#[test]
fn git_status_drops_hint_lines() {
    let mut out = String::new();
    out.push_str("On branch main\n");
    for index in 0..80 {
        out.push_str(&format!("  modified:   file-{index}.rs\n"));
    }
    out.push_str("  (use \"git add <file>...\" to update what will be committed)\n");
    let minimized = minimize("git status", &out, Some(0)).expect("minimized");
    assert!(!minimized.contains("(use \"git"));
    assert!(minimized.contains("modified:   file-79.rs"));
}

#[test]
fn env_assignments_and_wrappers_are_stripped() {
    let output = cargo_build_fixture();
    let minimized = minimize("CI=1 cargo build", &output, Some(0)).expect("minimized");
    assert!(!minimized.contains("Compiling"));
}
