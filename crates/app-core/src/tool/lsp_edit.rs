//! Apply a server-computed `WorkspaceEdit` through the file tools' write path.
//!
//! A language server answers in 0-based positions counted in UTF-16 code units,
//! while a file here is a UTF-8 string, so the first job is one conversion.
//! After that the write path is `edit`'s: path policy and the generated-file
//! guard on every file, per-path locks, a byte check against the content the
//! plan was computed from, and one undo unit for the whole call.

use crate::bus::{Bus, BusEvent, FileOp, FileTouch};
use anyhow::{Result, anyhow, bail};
use horon_agent_lsp::{FileEdit, TextEdit, WorkspaceEdit};
use std::path::{Path, PathBuf};

/// Most files one server edit may touch.
const MAX_FILES: usize = 200;
/// Most text edits one call may apply, across all files.
const MAX_EDITS: usize = 10_000;

/// One file's content before and after the server's edits.
pub(super) struct PlannedFile {
    pub path: PathBuf,
    /// The path as the caller should read it.
    pub display: String,
    pub edits: usize,
    pub before: String,
    pub after: String,
}

impl PlannedFile {
    fn changed(&self) -> bool {
        self.before != self.after
    }
}

/// Read and transform every file the edit names, without writing anything.
pub(super) async fn plan(edit: &WorkspaceEdit, base: &Path) -> Result<Vec<PlannedFile>> {
    if edit.files.len() > MAX_FILES {
        bail!(
            "the language server's edit touches {} files, more than the {MAX_FILES} this tool applies in one call",
            edit.files.len()
        );
    }
    if edit.edit_count() > MAX_EDITS {
        bail!(
            "the language server's edit has {} text edits, more than the {MAX_EDITS} this tool applies in one call",
            edit.edit_count()
        );
    }
    let mut plans = Vec::with_capacity(edit.files.len());
    for FileEdit { path, edits } in &edit.files {
        let resolved = if path.is_absolute() {
            path.clone()
        } else {
            base.join(path)
        };
        let display = resolved.display().to_string();
        let before = match tokio::fs::read(&resolved).await {
            Ok(bytes) => String::from_utf8(bytes)
                .map_err(|_| anyhow!("{display} is not UTF-8 text, so the server's edit cannot be applied"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                bail!("{display} does not exist; the language server's edit cannot be applied")
            }
            Err(error) => return Err(anyhow!("cannot read {display}: {error}")),
        };
        let after = apply_edits(&before, edits, &display)?;
        plans.push(PlannedFile {
            path: resolved,
            display,
            edits: edits.len(),
            before,
            after,
        });
    }
    Ok(plans)
}

/// The content `text` has after `edits`.
///
/// Every range is resolved on the original text and the edits are applied from
/// the end backwards, so an earlier edit cannot shift a later one.
pub(super) fn apply_edits(text: &str, edits: &[TextEdit], display: &str) -> Result<String> {
    let mut ranges = Vec::with_capacity(edits.len());
    for edit in edits {
        let start = char_offset(text, edit.start_line, edit.start_character)
            .ok_or_else(|| stale_range(display, edit.start_line, edit.start_character))?;
        let end = char_offset(text, edit.end_line, edit.end_character)
            .ok_or_else(|| stale_range(display, edit.end_line, edit.end_character))?;
        if start > end {
            bail!(
                "{display}: the server's edit ends before it starts (line {}, character {} to line {}, character {})",
                edit.start_line + 1,
                edit.start_character + 1,
                edit.end_line + 1,
                edit.end_character + 1
            );
        }
        ranges.push((start, end, edit.new_text.as_str()));
    }
    ranges.sort_by_key(|(start, end, _)| (*start, *end));
    for pair in ranges.windows(2) {
        if pair[0].1 > pair[1].0 {
            bail!("{display}: the server's edits overlap, so they cannot be applied");
        }
    }
    let mut out = text.to_string();
    for (start, end, new_text) in ranges.iter().rev() {
        out.replace_range(*start..*end, new_text);
    }
    Ok(out)
}

/// Character offset of a 0-based LSP position, or `None` when it is past the
/// end of the text.
fn char_offset(text: &str, line: u32, character: u32) -> Option<usize> {
    let mut current_line = 0u32;
    let mut units = 0u32;
    for (offset, ch) in text.char_indices() {
        if current_line == line && units == character {
            return Some(offset);
        }
        if ch == '\n' {
            if current_line == line {
                return None;
            }
            current_line += 1;
            units = 0;
        } else {
            units += u32::from(ch.len_utf16() as u16);
        }
    }
    (current_line == line && units == character).then_some(text.len())
}

fn stale_range(display: &str, line: u32, character: u32) -> anyhow::Error {
    anyhow!(
        "{display}: the server's edit points at line {}, character {}, which is past the end of the file; the file changed since the edit was computed",
        line + 1,
        character + 1
    )
}

/// Write every planned file as one mutation unit.
///
/// Nothing is written unless every file still holds the content the plan was
/// computed from.
pub(super) async fn commit(ctx: &crate::tool::ToolContext, plans: &[PlannedFile]) -> Result<String> {
    let mut body = String::new();
    for plan in plans {
        crate::tool::path_policy::enforce("lsp", &serde_json::json!({ "file": plan.display }), ctx)?;
        if let Some(refusal) = crate::tool::generated_guard::refusal(&plan.path, &plan.display) {
            bail!(refusal);
        }
    }
    let _locks = crate::tool::file_lock::lock_all(plans.iter().map(|plan| plan.path.clone())).await;
    for plan in plans {
        let current = tokio::fs::read(&plan.path).await.map_err(|error| {
            anyhow!(
                "{} changed since the language server computed the edit: {error}",
                plan.display
            )
        })?;
        if current != plan.before.as_bytes() {
            bail!(
                "{} changed since the language server computed the edit; nothing was written",
                plan.display
            );
        }
    }

    let mut pending = Vec::with_capacity(plans.len());
    let mut diffs = Vec::with_capacity(plans.len());
    for plan in plans {
        let before = crate::tool::undo::capture(&plan.path).await;
        tokio::fs::write(&plan.path, &plan.after).await?;
        pending.push(crate::tool::undo::PendingFile {
            path: plan.path.clone(),
            before,
            after: crate::tool::undo::bytes(plan.after.as_bytes()),
        });
        crate::tool::edit_stats::record(ctx, &plan.before, &plan.after, false).await;
        let diff = crate::tool::file_diff::unified(&plan.display, &plan.display, &plan.before, &plan.after);
        let summary = format!(
            "applied {} server edit{}",
            plan.edits,
            if plan.edits == 1 { "" } else { "s" }
        );
        Bus::global().publish(BusEvent::FileTouch(FileTouch {
            session_id: ctx.session_id.clone(),
            path: plan.path.clone(),
            op: FileOp::Edit,
            intent: None,
            summary: Some(summary),
            detail: build_preview(&diff),
        }));
        diffs.push((plan.path.clone(), Some(plan.before.clone()), plan.after.clone()));
    }
    crate::tool::undo::record(ctx, pending);

    for plan in plans {
        body.push_str(&format!(
            "{}: {} edit{}\n",
            plan.display,
            plan.edits,
            if plan.edits == 1 { "" } else { "s" }
        ));
    }
    Ok(body)
}

/// Render the plan of a dry run, one line per file.
pub(super) fn render_plan(plans: &[PlannedFile], intro: &str) -> String {
    if plans.iter().all(|plan| !plan.changed()) {
        return format!("{intro}\nNo changes: the server's edit reproduces the current content.");
    }
    let mut body = format!("{intro}\n");
    for plan in plans {
        body.push_str(&format!(
            "  {}: {} edit{}\n",
            plan.display,
            plan.edits,
            if plan.edits == 1 { "" } else { "s" }
        ));
    }
    body
}

fn build_preview(diff: &str) -> Option<String> {
    if diff.trim().is_empty() {
        return None;
    }
    let preview: Vec<&str> = diff.lines().take(6).collect();
    Some(preview.join("\n"))
}

/// Attach the per-file diff summary to the tool output.
pub(super) fn attach(output: crate::tool::ToolOutput, plans: &[PlannedFile]) -> crate::tool::ToolOutput {
    let changes = plans
        .iter()
        .map(|plan| crate::tool::file_diff::FileChange {
            path: plan.path.clone(),
            old_path: None,
            old_text: Some(plan.before.clone()),
            new_text: plan.after.clone(),
            deleted: false,
            created: false,
        })
        .collect();
    crate::tool::file_diff::attach_file_changes(output, changes)
}

/// True when the write path would refuse nothing about these paths.
pub(super) fn any_change(plans: &[PlannedFile]) -> bool {
    plans.iter().any(PlannedFile::changed)
}

#[cfg(test)]
#[path = "lsp_edit_tests.rs"]
mod tests;
