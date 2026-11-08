use super::{Tool, ToolContext, ToolOutput};
use crate::bus::{Bus, BusEvent, FileOp, FileTouch};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use similar::{ChangeTag, TextDiff};
use std::borrow::Cow;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

/// A `read` hashline listing starts with a header naming the file and its
/// revision: `[path#rev]`.
static HASHLINE_HEADER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*\[[^#\r\n]+#[^ \t\r\n]*\]\s*$").expect("static hashline header pattern"));

/// One hashline listing line: `    1#abcd:text` (or `----` for a cut line).
static HASHLINE_PREFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*\d+#(?:[0-9a-f]{4}|----):").expect("static hashline prefix pattern"));

const FILE_TOUCH_PREVIEW_MAX_LINES: usize = 6;
const FILE_TOUCH_PREVIEW_MAX_BYTES: usize = 240;

#[path = "write_shebang.rs"]
mod write_shebang;

pub struct WriteTool;

impl WriteTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct WriteInput {
    #[serde(default)]
    intent: Option<String>,
    file_path: String,
    content: String,
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Write a file."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["file_path", "content"],
            "properties": {
                "intent": super::intent_schema_property(),
                "file_path": {
                    "type": "string",
                    "description": "File path."
                },
                "content": {
                    "type": "string",
                    "description": "File content."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        super::path_policy::enforce("write", &input, &ctx)?;
        let params: WriteInput = serde_json::from_value(input)?;

        if crate::agent_result_cache::is_agent_url(&params.file_path) {
            return Err(anyhow::anyhow!(
                "`write` does not accept the agent:// scheme: child results are read-only references, not files. Use the read tool to project a field, and write files to a real path."
            ));
        }

        let path = ctx.resolve_path(Path::new(&params.file_path));
        let (content, stripped_prefix_lines) = strip_hashline_prefixes(&params.content);
        let content = content.as_ref();

        let _lock = super::file_lock::lock(&path).await;
        // The editor's unsaved buffer is the text this write replaces, so the
        // diff and stats describe what the user sees, not a stale disk copy.
        let snapshot = super::client_fs::text_snapshot(&path, &ctx).await;
        let existed = snapshot
            .as_ref()
            .map(|(exists, _)| *exists)
            .unwrap_or_else(|| path.exists());
        let old_content = match snapshot {
            Some((exists, text)) => exists.then_some(text),
            None if path.exists() => tokio::fs::read_to_string(&path).await.ok(),
            None => None,
        };

        if existed && let Some(refusal) = super::generated_guard::refusal(&path, &params.file_path) {
            return Err(anyhow::anyhow!(refusal));
        }

        let wrote_to_client = super::client_fs::write_text(&path, content, &ctx).await?;
        // Only a local create can change a mode, and only its own: an overwrite
        // keeps the mode the file already had.
        let mut made_executable = false;
        if !wrote_to_client {
            // Create parent directories if needed
            if let Some(parent) = path.parent()
                && !parent.exists()
            {
                tokio::fs::create_dir_all(parent).await?;
            }

            let before = super::undo::capture(&path).await;
            tokio::fs::write(&path, content).await?;
            super::undo::record(
                &ctx,
                vec![super::undo::PendingFile {
                    path: path.clone(),
                    before,
                    after: super::undo::bytes(content.as_bytes()),
                }],
            );
            made_executable = !existed && write_shebang::make_executable(&path, content).await?;
        }
        super::edit_stats::record(
            &ctx,
            old_content.as_deref().unwrap_or(""),
            content,
            existed && old_content.is_none(),
        )
        .await;

        let line_count = content.lines().count();
        let diff = if let Some(old) = old_content.as_deref() {
            generate_diff_summary(old, content)
        } else {
            generate_diff_summary("", content)
        };
        let detail = build_file_touch_preview(&diff);

        // Publish file touch event for swarm coordination
        Bus::global().publish(BusEvent::FileTouch(FileTouch {
            session_id: ctx.session_id.clone(),
            path: path.to_path_buf(),
            op: FileOp::Write,
            intent: params.intent.clone().filter(|value| !value.trim().is_empty()),
            summary: Some(if existed {
                format!("overwrote file ({} lines)", line_count)
            } else {
                format!("created new file ({} lines)", line_count)
            }),
            detail,
        }));

        let mut body = if existed {
            format!(
                "Updated {} ({} lines){}\n{}",
                params.file_path,
                line_count,
                if diff.is_empty() { "" } else { ":" },
                diff
            )
        } else {
            // For new files, show all lines as additions
            let diff = generate_diff_summary("", content);
            format!("Created {} ({} lines):\n{}", params.file_path, line_count, diff)
        };
        if stripped_prefix_lines > 0 {
            body.push_str(&format!(
                "\n(Stripped read-output line prefixes from {stripped_prefix_lines} lines before writing.)"
            ));
        }
        if made_executable {
            body.push_str("\n(Made the file executable: it starts with a shebang.)");
        }

        // A write that lands on the active config.toml states exactly which
        // settings changed and whether they are live, so neither the agent nor
        // the user has to guess whether the edit took effect. A client-served
        // write changed no file the daemon reads, so it reports nothing.
        if wrote_to_client {
            body.push_str("\n\nWritten through the ACP client's file buffer; the file on disk is unchanged.");
        } else {
            super::config_edit_notice::append_config_edit_notice(
                &mut body,
                &path,
                old_content.as_deref().unwrap_or(""),
                content,
            );
        }
        if let Some(extra) = super::nested_agents::inject_rules_for_path(&path, &ctx) {
            body.push_str(&extra);
        }

        let output = ToolOutput::new(body).with_title(params.file_path.clone());
        // Do not claim an authoritative diff when the old file was unreadable.
        Ok(if !existed || old_content.is_some() {
            let unified = super::file_diff::unified(
                if existed { &params.file_path } else { "/dev/null" },
                &params.file_path,
                old_content.as_deref().unwrap_or(""),
                content,
            );
            super::file_diff::attach_file_diffs(
                super::file_diff::attach(output, unified),
                vec![(path.clone(), old_content.clone(), content.to_string())],
            )
        } else {
            output
        })
    }
}

/// Removes the line-number prefixes of a `read` hashline listing that was
/// pasted back as file content.
///
/// A model that copies a `read` result into `write` would otherwise write the
/// anchors into the file. Only the fully anchored form is stripped: every
/// non-empty line must carry `N#tag:`, and an optional `[path#rev]` header is
/// dropped first. The plain `N<TAB>` read form is deliberately left alone,
/// because it is indistinguishable from tab-separated data.
///
/// Returns the content to write and how many lines were stripped.
pub(crate) fn strip_hashline_prefixes(content: &str) -> (Cow<'_, str>, usize) {
    let lines: Vec<&str> = content.split('\n').collect();
    let header = lines
        .iter()
        .position(|line| !line.trim().is_empty())
        .filter(|&index| HASHLINE_HEADER_RE.is_match(lines[index]));

    let body: Vec<usize> = (0..lines.len())
        .filter(|&index| Some(index) != header && !lines[index].trim().is_empty())
        .collect();
    if body.is_empty() || body.iter().any(|&index| !HASHLINE_PREFIX_RE.is_match(lines[index])) {
        return (Cow::Borrowed(content), 0);
    }

    let mut out = String::with_capacity(content.len());
    let mut stripped = 0usize;
    let mut first = true;
    for (index, line) in lines.iter().enumerate() {
        if Some(index) == header {
            stripped += 1;
            continue;
        }
        if !first {
            out.push('\n');
        }
        first = false;
        match HASHLINE_PREFIX_RE.find(line) {
            Some(prefix) => {
                out.push_str(&line[prefix.end()..]);
                stripped += 1;
            }
            None => out.push_str(line),
        }
    }
    (Cow::Owned(out), stripped)
}

/// Generate a compact diff: "42- old" / "42+ new" (max 20 lines)
fn generate_diff_summary(old: &str, new: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut output = String::new();
    let mut lines_shown = 0;
    const MAX_LINES: usize = 20;

    let mut old_line = 1usize;
    let mut new_line = 1usize;

    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                old_line += 1;
                new_line += 1;
                continue;
            }
            ChangeTag::Delete => {
                let content = change.value().trim();
                old_line += 1;
                if content.is_empty() {
                    continue;
                }
                if lines_shown >= MAX_LINES {
                    output.push_str("...\n");
                    break;
                }
                output.push_str(&format!("{}- {}\n", old_line - 1, content));
                lines_shown += 1;
            }
            ChangeTag::Insert => {
                let content = change.value().trim();
                new_line += 1;
                if content.is_empty() {
                    continue;
                }
                if lines_shown >= MAX_LINES {
                    output.push_str("...\n");
                    break;
                }
                output.push_str(&format!("{}+ {}\n", new_line - 1, content));
                lines_shown += 1;
            }
        }
    }

    output.trim_end().to_string()
}

fn build_file_touch_preview(diff: &str) -> Option<String> {
    let trimmed = diff.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut lines = trimmed.lines();
    let mut preview = lines
        .by_ref()
        .take(FILE_TOUCH_PREVIEW_MAX_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    let mut truncated = lines.next().is_some();

    if preview.len() > FILE_TOUCH_PREVIEW_MAX_BYTES {
        preview = crate::util::truncate_str(&preview, FILE_TOUCH_PREVIEW_MAX_BYTES)
            .trim_end()
            .to_string();
        truncated = true;
    }

    if truncated {
        preview.push_str("\n…");
    }

    Some(preview)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_diff_summary_single_change() {
        let old = "hello world";
        let new = "hello rust";
        let diff = generate_diff_summary(old, new);

        // Compact format: "1- content" / "1+ content"
        assert!(diff.contains("1- hello world"), "Should show deleted line");
        assert!(diff.contains("1+ hello rust"), "Should show added line");
    }

    #[test]
    fn test_generate_diff_summary_multi_line() {
        let old = "line one\nline two\nline three";
        let new = "line one\nchanged two\nline three";
        let diff = generate_diff_summary(old, new);

        assert!(diff.contains("2- line two"), "Should show deleted line");
        assert!(diff.contains("2+ changed two"), "Should show added line");
        // Equal lines should not appear
        assert!(!diff.contains("line one"), "Should not show unchanged lines");
    }

    #[test]
    fn test_generate_diff_summary_new_file() {
        let old = "";
        let new = "line one\nline two\nline three";
        let diff = generate_diff_summary(old, new);

        assert!(diff.contains("1+ line one"), "Should show line 1 added");
        assert!(diff.contains("2+ line two"), "Should show line 2 added");
        assert!(diff.contains("3+ line three"), "Should show line 3 added");
    }

    #[test]
    fn test_generate_diff_summary_truncation() {
        // Create old and new with more than 20 changed lines
        let old = (1..=25)
            .map(|i| format!("old line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let new = (1..=25)
            .map(|i| format!("new line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let diff = generate_diff_summary(&old, &new);

        assert!(diff.contains("..."), "Should truncate after 20 lines");
    }

    #[test]
    fn test_generate_diff_summary_line_number_format() {
        let old = "old";
        let new = "new";
        let diff = generate_diff_summary(old, new);

        // Compact format: no padding
        assert!(diff.contains("1- old"), "Should have line number directly before minus");
        assert!(diff.contains("1+ new"), "Should have line number directly before plus");
    }

    #[test]
    fn test_generate_diff_summary_empty_result() {
        let old = "same content";
        let new = "same content";
        let diff = generate_diff_summary(old, new);

        assert!(diff.is_empty(), "No changes should produce empty diff");
    }

    fn write_test_ctx(working_dir: std::path::PathBuf) -> crate::tool::ToolContext {
        crate::tool::ToolContext {
            session_id: "test-session".to_string(),
            message_id: "test-message".to_string(),
            tool_call_id: "test-call".to_string(),
            working_dir: Some(working_dir),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: crate::tool::ToolExecutionMode::Direct,
        }
    }

    #[test]
    fn strip_hashline_prefixes_removes_the_header_and_every_prefix() {
        let (stripped, count) = strip_hashline_prefixes("[src/a.rs#abcd]\n    1#abcd:fn a() {}\n    2#----:b\n");
        assert_eq!(stripped, "fn a() {}\nb\n");
        assert_eq!(count, 3);
    }

    #[test]
    fn strip_hashline_prefixes_keeps_blank_lines() {
        let (stripped, count) = strip_hashline_prefixes("    1#abcd:a\n\n    3#abcd:b\n");
        assert_eq!(stripped, "a\n\nb\n");
        assert_eq!(count, 2);
    }

    #[test]
    fn strip_hashline_prefixes_leaves_plain_and_tabular_text_alone() {
        let (stripped, count) = strip_hashline_prefixes("    1\tfn a() {}\n    2\tfn b() {}\n");
        assert_eq!(stripped, "    1\tfn a() {}\n    2\tfn b() {}\n");
        assert_eq!(count, 0);

        // A mixed listing is not a listing: one plain line keeps everything.
        let (stripped, count) = strip_hashline_prefixes("    1#abcd:a\nplain\n");
        assert_eq!(stripped, "    1#abcd:a\nplain\n");
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn write_strips_read_output_prefixes_before_writing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let output = WriteTool::new()
            .execute(
                json!({"file_path": "a.rs", "content": "    1#abcd:fn a() {}\n"}),
                write_test_ctx(temp.path().to_path_buf()),
            )
            .await
            .expect("write should succeed");

        assert_eq!(
            std::fs::read_to_string(temp.path().join("a.rs")).expect("read back"),
            "fn a() {}\n"
        );
        assert!(
            output
                .output
                .contains("(Stripped read-output line prefixes from 1 lines before writing.)"),
            "output={:?}",
            output.output
        );
    }

    #[tokio::test]
    async fn write_refuses_to_overwrite_a_generated_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("schema.pb.go");
        std::fs::write(&path, "package schema\n").expect("write file");

        let error = WriteTool::new()
            .execute(
                json!({"file_path": "schema.pb.go", "content": "changed\n"}),
                write_test_ctx(temp.path().to_path_buf()),
            )
            .await
            .expect_err("a generated file must not be overwritten");

        assert!(
            error
                .to_string()
                .starts_with("Cannot modify auto-generated file: schema.pb.go\n"),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&path).expect("read back"), "package schema\n");
    }
}
