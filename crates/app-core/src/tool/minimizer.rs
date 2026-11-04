//! Opt-in output minimizer for `bash`.
//!
//! A build or package manager spends most of its output on progress lines that
//! say nothing after the fact. With `[tools] bash_minimizer = true`, a command
//! that is one simple, non-piped segment has its output reduced to the lines
//! that carry a verdict: errors, test results, and warnings. The full text is
//! still saved as an artifact, so nothing is lost.
//!
//! `None` means "leave it alone": the command is not a shape this filter
//! understands, or the filter changed nothing.

use horon_agent_command_risk::{Token, split_segments};

/// Outputs below this are already cheap; minimizing them would only risk
/// hiding a line the model needed.
const MIN_OUTPUT_BYTES: usize = 1000;

/// `(command, output, exit_code)` → a shorter rendering, or `None` to pass the
/// original through.
pub(crate) fn minimize(command: &str, output: &str, exit_code: Option<i32>) -> Option<String> {
    if output.len() < MIN_OUTPUT_BYTES {
        return None;
    }
    if command.contains("$(") || command.contains('`') || command.contains("<<") || command.contains('|') {
        return None;
    }
    let segments = split_segments(command);
    if segments.len() != 1 {
        return None;
    }
    let tokens = &segments[0];
    if tokens.iter().any(|token| token.is_operator) {
        return None;
    }

    let (program_index, program) = detect_program(tokens)?;
    let args: Vec<&str> = tokens[program_index + 1..]
        .iter()
        .map(|token| token.text.as_str())
        .collect();
    let subcommand = args
        .iter()
        .find(|arg| !arg.starts_with('-'))
        .map(|arg| arg.to_ascii_lowercase());

    let minimized = std::panic::catch_unwind(|| {
        let stripped = strip_ansi(output);
        let lines = apply_filters(&program.name, subcommand.as_deref(), &args, &stripped, exit_code);
        collapse_repeats(&lines)
    })
    .ok()?;

    if minimized == output {
        return None;
    }
    if minimized.trim().is_empty() && exit_code.unwrap_or(0) == 0 {
        return Some("OK".to_string());
    }
    Some(minimized)
}

struct Program {
    /// Basename, lowercased.
    name: String,
}

/// Strip wrappers and `NAME=value` assignments to find the real program.
fn detect_program(tokens: &[Token]) -> Option<(usize, Program)> {
    let mut index = 0;
    while index < tokens.len() {
        let text = tokens[index].text.as_str();
        if is_assignment(text) || matches!(text, "env" | "sudo" | "command" | "time") {
            index += 1;
            continue;
        }
        let name = text.rsplit('/').next().unwrap_or(text).to_ascii_lowercase();
        return Some((index, Program { name }));
    }
    None
}

fn is_assignment(text: &str) -> bool {
    let Some((name, _)) = text.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
}

/// Strip ANSI escape sequences (CSI and OSC) so later line matching sees text.
fn strip_ansi(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != 0x1b {
            let Some(ch) = text[index..].chars().next() else {
                break;
            };
            out.push(ch);
            index += ch.len_utf8();
            continue;
        }
        index += 1;
        match bytes.get(index) {
            Some(b'[') => {
                index += 1;
                while index < bytes.len() {
                    let byte = bytes[index];
                    index += 1;
                    if (0x40..=0x7e).contains(&byte) {
                        break;
                    }
                }
            }
            Some(b']') => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == 0x07 {
                        index += 1;
                        break;
                    }
                    if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            _ => match text.get(index..).and_then(|rest| rest.chars().next()) {
                Some(ch) => index += ch.len_utf8(),
                None => index += 1,
            },
        }
    }
    out
}

fn apply_filters(
    program: &str,
    subcommand: Option<&str>,
    args: &[&str],
    output: &str,
    exit_code: Option<i32>,
) -> Vec<String> {
    let lines: Vec<String> = output.lines().map(str::to_string).collect();
    match program {
        "cargo" => match subcommand {
            Some("build" | "check" | "clippy" | "doc" | "run") => lines
                .into_iter()
                .filter(|line| !has_prefix(line, CARGO_PROGRESS_PREFIXES))
                .collect(),
            Some("test" | "nextest") => cargo_test_filter(lines, exit_code),
            _ => lines,
        },
        "npm" | "pnpm" | "yarn" | "pip" | "pip3" | "uv" | "bun" => match subcommand {
            Some("install" | "i" | "add" | "ci" | "update" | "upgrade" | "remove" | "sync") => {
                let filtered: Vec<String> = lines.into_iter().filter(|line| !is_progress_line(line)).collect();
                tail_lines(filtered, if exit_code.unwrap_or(0) == 0 { 40 } else { 160 })
            }
            _ => lines,
        },
        "pytest" => pytest_filter(lines, exit_code),
        "python" | "python3" if args.contains(&"-m") && args.contains(&"pytest") => pytest_filter(lines, exit_code),
        "go" if subcommand == Some("test") => go_test_filter(lines, exit_code),
        "git" if subcommand == Some("status") => {
            let porcelain = args.iter().any(|arg| *arg == "--porcelain" || *arg == "-z");
            if exit_code.unwrap_or(0) == 0 && !porcelain {
                lines.into_iter().filter(|line| !is_git_status_hint(line)).collect()
            } else {
                lines
            }
        }
        "tsc" | "eslint" => lines
            .into_iter()
            .filter(|line| !line.trim().is_empty() && !is_caret_gutter(line))
            .collect(),
        _ => lines,
    }
}

const CARGO_PROGRESS_PREFIXES: &[&str] = &[
    "Compiling ",
    "Checking ",
    "Downloaded ",
    "Downloading ",
    "Fresh ",
    "Blocking waiting for file lock",
];

fn has_prefix(line: &str, prefixes: &[&str]) -> bool {
    let trimmed = line.trim_start();
    prefixes.iter().any(|prefix| trimmed.starts_with(prefix))
}

fn cargo_test_filter(lines: Vec<String>, exit_code: Option<i32>) -> Vec<String> {
    if exit_code.unwrap_or(0) == 0 {
        return lines
            .into_iter()
            .filter(|line| {
                let trimmed = line.trim_start();
                trimmed.contains("test result:") || trimmed.starts_with("running ") || trimmed.starts_with("Finished")
            })
            .collect();
    }
    lines
        .into_iter()
        .filter(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("Compiling ") || trimmed.starts_with("Checking ") {
                return false;
            }
            !is_passing_test_line(trimmed)
        })
        .collect()
}

/// `test foo ... ok` (libtest and nextest both spell it this way).
fn is_passing_test_line(trimmed: &str) -> bool {
    trimmed.starts_with("test ") && trimmed.ends_with(" ok")
}

fn is_progress_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return false;
    }
    let first = trimmed.chars().next().unwrap_or(' ');
    // Braille spinner frames used by npm/yarn/pnpm/uv.
    if ('\u{2801}'..='\u{28ff}').contains(&first) {
        return true;
    }
    if let Some(rest) = trimmed.strip_prefix('[') {
        // Progress bars such as `[####    ] 40%`.
        let bar: String = rest
            .chars()
            .take_while(|ch| matches!(ch, '#' | '=' | '>' | '.' | '-' | ' '))
            .collect();
        if rest.len() > bar.len() && rest[bar.len()..].starts_with(']') && !bar.is_empty() {
            return true;
        }
    }
    trimmed.starts_with("Downloading ")
        || trimmed.starts_with("Resolving ")
        || trimmed.starts_with("Fetching ")
        || trimmed.starts_with("Progress:")
        || trimmed.contains("WARN deprecated")
}

fn pytest_filter(lines: Vec<String>, exit_code: Option<i32>) -> Vec<String> {
    if exit_code.unwrap_or(0) == 0 {
        return lines
            .into_iter()
            .rfind(|line| !line.trim().is_empty())
            .into_iter()
            .collect();
    }
    let start = lines
        .iter()
        .position(|line| line.contains("= FAILURES =") || line.contains("= ERRORS ="));
    match start {
        Some(start) => lines[start..].to_vec(),
        None => lines,
    }
}

fn go_test_filter(lines: Vec<String>, exit_code: Option<i32>) -> Vec<String> {
    if exit_code.unwrap_or(0) == 0 {
        return lines
            .into_iter()
            .filter(|line| {
                let trimmed = line.trim_start();
                trimmed.starts_with("ok") || trimmed.starts_with('?') || trimmed.starts_with("PASS")
            })
            .collect();
    }
    lines
        .into_iter()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with("=== RUN") && !trimmed.starts_with("--- PASS")
        })
        .collect()
}

fn is_git_status_hint(line: &str) -> bool {
    line.trim_start().starts_with("(use \"git")
}

fn is_caret_gutter(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty() && trimmed.chars().all(|ch| ch == '~' || ch == '^')
}

fn tail_lines(lines: Vec<String>, count: usize) -> Vec<String> {
    if lines.len() <= count {
        return lines;
    }
    lines[lines.len() - count..].to_vec()
}

/// Collapse runs of identical consecutive lines into `line (×N)`.
fn collapse_repeats(lines: &[String]) -> String {
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut index = 0;
    while index < lines.len() {
        let line = &lines[index];
        let mut count = 1;
        while index + count < lines.len() && lines[index + count] == *line {
            count += 1;
        }
        if count > 1 {
            out.push(format!("{line} (×{count})"));
        } else {
            out.push(line.clone());
        }
        index += count;
    }
    let mut rendered = out.join("\n");
    rendered.push('\n');
    rendered
}

#[cfg(test)]
#[path = "minimizer_tests.rs"]
mod tests;
