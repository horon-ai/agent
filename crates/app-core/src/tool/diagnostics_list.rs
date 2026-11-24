//! Language diagnostics listed before any edit, and a brief for fixing a chosen subset.
//!
//! Collection asks the built-in language servers exactly as the `lsp` tool does.
//! Grouping, rendering and the handoff brief are pure so a fixed list of
//! diagnostics can be checked without a server.

use horon_agent_lsp::Diagnostic;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    /// 1-based position in the whole listing; `--select` names these.
    pub index: usize,
    pub line: u32,
    pub column: u32,
    pub severity: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileReport {
    pub path: String,
    /// Why no diagnostics could be read for this file, when that is the case.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub diagnostics: Vec<Item>,
}

/// Group diagnostics by file in first-seen order, errors before warnings, then by position.
pub fn group(found: Vec<(String, Vec<Diagnostic>)>) -> Vec<FileReport> {
    let mut next = 1;
    found
        .into_iter()
        .map(|(path, mut diagnostics)| {
            diagnostics.sort_by_key(|d| (d.severity, d.line, d.column));
            let diagnostics = diagnostics
                .into_iter()
                .map(|d| {
                    let item = Item {
                        index: next,
                        line: d.line,
                        column: d.column,
                        severity: d.severity.as_str(),
                        message: one_line(&d.message),
                    };
                    next += 1;
                    item
                })
                .collect();
            FileReport {
                path,
                note: None,
                diagnostics,
            }
        })
        .collect()
}

fn push_usize(out: &mut String, mut value: usize) {
    let mut digits = [0u8; 20];
    let mut pos = digits.len();
    loop {
        pos -= 1;
        digits[pos] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    if let Ok(text) = std::str::from_utf8(&digits[pos..]) {
        out.push_str(text);
    }
}

fn one_line(text: &str) -> String {
    let mut words = text.split_whitespace();
    let Some(first) = words.next() else {
        return String::new();
    };
    let Some(second) = words.next() else {
        return first.to_owned();
    };
    let mut out = String::with_capacity(text.len());
    out.push_str(first);
    out.push(' ');
    out.push_str(second);
    for word in words {
        out.push(' ');
        out.push_str(word);
    }
    out
}

pub fn render(reports: &[FileReport]) -> String {
    use std::fmt::Write as _;

    let total_bytes: usize = reports
        .iter()
        .map(|report| {
            let header = report.path.len() + report.note.as_ref().map_or(8, |note| note.len() + 4);
            let items: usize = report.diagnostics.iter().map(|item| item.message.len() + 28).sum();
            header + items
        })
        .sum();
    let mut out = String::with_capacity(total_bytes);
    for report in reports {
        out.push_str(&report.path);
        match &report.note {
            Some(note) => {
                out.push_str(": ");
                out.push_str(note);
                out.push('\n');
            }
            None => {
                out.push_str(" (");
                push_usize(&mut out, report.diagnostics.len());
                out.push_str(")\n");
            }
        }
        for item in &report.diagnostics {
            out.push_str("  [");
            push_usize(&mut out, item.index);
            out.push_str("] ");
            if writeln!(out, "{} {}:{} {}", item.severity, item.line, item.column, item.message).is_err() {
                return out;
            }
        }
    }
    if reports.iter().all(|r| r.diagnostics.is_empty() && r.note.is_none()) {
        out.push_str("No diagnostics.\n");
    }
    out
}

/// The files and the task text for the chosen entries. Errors name a number that is not in the listing.
pub fn handoff(reports: &[FileReport], selected: &[usize]) -> anyhow::Result<(Vec<String>, String)> {
    anyhow::ensure!(!selected.is_empty(), "select at least one diagnostic");
    let mut paths: Vec<String> = Vec::new();
    let mut brief = String::from("Fix these language-server diagnostics. Change only what each one needs.\n");
    for &wanted in selected {
        let (report, item) = reports
            .iter()
            .flat_map(|r| r.diagnostics.iter().map(move |i| (r, i)))
            .find(|(_, i)| i.index == wanted)
            .ok_or_else(|| anyhow::anyhow!("no diagnostic [{wanted}] in the listing"))?;
        if !paths.contains(&report.path) {
            paths.push(report.path.clone());
        }
        if std::fmt::Write::write_fmt(
            &mut brief,
            format_args!(
                "- {}:{}:{} {}: {}\n",
                report.path, item.line, item.column, item.severity, item.message
            ),
        )
        .is_err()
        {
            return Ok((paths, brief));
        }
    }
    Ok((paths, brief))
}

/// Ask each file's language server for its diagnostics. A file that cannot be
/// checked stays in the listing with the reason.
pub async fn collect(files: &[String], working_dir: &Path) -> Vec<FileReport> {
    let mut reports = Vec::with_capacity(files.len());
    for file in files {
        let path = working_dir.join(file);
        let display = display(&path, working_dir);
        let (found, note) = match collect_one(&path, working_dir).await {
            Ok(found) => (found, None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
        reports.extend(group(vec![(display, found)]).into_iter().map(|mut report| {
            report.note = note.clone();
            report
        }));
    }
    // Numbering runs across files, so it is assigned once the whole list exists.
    let items = reports.iter_mut().flat_map(|r| r.diagnostics.iter_mut());
    for (index, item) in (1..).zip(items) {
        item.index = index;
    }
    reports
}

async fn collect_one(path: &Path, working_dir: &Path) -> anyhow::Result<Vec<Diagnostic>> {
    if !path.exists() {
        anyhow::bail!("file not found");
    }
    let path: PathBuf = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let spec = horon_agent_lsp::server_for_path(&path)
        .ok_or_else(|| anyhow::anyhow!("no language server for this file type"))?;
    if !horon_agent_lsp::is_installed(spec) {
        anyhow::bail!("{} is not installed or not on PATH", spec.program);
    }
    let root = horon_agent_lsp::workspace_root(&path, spec, working_dir);
    let client = horon_agent_lsp::Client::get(spec, root)
        .await
        .map_err(|error| anyhow::anyhow!("language server failed to start: {error}"))?;
    client
        .diagnostics(&path, TIMEOUT)
        .await
        .map_err(|error| anyhow::anyhow!("diagnostics unavailable: {error}"))
}

fn display(path: &Path, base: &Path) -> String {
    path.strip_prefix(base).unwrap_or(path).display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use horon_agent_lsp::DiagnosticSeverity as S;

    fn diag(path: &str, line: u32, severity: S, message: &str) -> Diagnostic {
        Diagnostic {
            path: PathBuf::from(path),
            line,
            column: 5,
            severity,
            message: message.into(),
        }
    }

    fn fixture() -> Vec<FileReport> {
        group(vec![
            (
                "src/a.rs".into(),
                vec![
                    diag("src/a.rs", 9, S::Warning, "unused\nvariable"),
                    diag("src/a.rs", 30, S::Error, "mismatched types"),
                ],
            ),
            (
                "src/b.rs".into(),
                vec![diag("src/b.rs", 2, S::Error, "cannot find value")],
            ),
        ])
    }

    #[test]
    fn listing_groups_by_file_with_errors_first_and_numbers_across_files() {
        let text = render(&fixture());
        assert_eq!(
            text,
            "src/a.rs (2)\n  [1] error 30:5 mismatched types\n  [2] warning 9:5 unused variable\n\
src/b.rs (1)\n  [3] error 2:5 cannot find value\n"
        );
    }

    #[test]
    fn a_file_that_cannot_be_checked_stays_listed_with_its_reason() {
        let mut reports = fixture();
        reports.push(FileReport {
            path: "notes.txt".into(),
            note: Some("no language server for this file type".into()),
            diagnostics: vec![],
        });
        assert!(render(&reports).ends_with("notes.txt: no language server for this file type\n"));
    }

    #[test]
    fn handoff_names_each_selected_path_once_and_only_the_chosen_entries() {
        let (paths, brief) = handoff(&fixture(), &[3, 1, 2]).unwrap();
        assert_eq!(paths, ["src/b.rs", "src/a.rs"]);
        assert!(brief.contains("- src/b.rs:2:5 error: cannot find value"));
        assert!(brief.contains("- src/a.rs:9:5 warning: unused variable"));
        let (paths, brief) = handoff(&fixture(), &[1]).unwrap();
        assert_eq!(paths, ["src/a.rs"]);
        assert!(!brief.contains("cannot find value"));
    }

    #[test]
    fn handoff_rejects_an_empty_or_unknown_selection() {
        assert!(handoff(&fixture(), &[]).is_err());
        assert!(handoff(&fixture(), &[4]).unwrap_err().to_string().contains("[4]"));
    }
}
