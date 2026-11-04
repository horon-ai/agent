use super::{Tool, ToolContext, ToolOutput};
use crate::bus::{Bus, BusEvent, FileOp, FileTouch};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use similar::{ChangeTag, TextDiff};
use std::path::Path;

const FILE_TOUCH_PREVIEW_MAX_LINES: usize = 6;
const FILE_TOUCH_PREVIEW_MAX_BYTES: usize = 240;

pub struct ApplyPatchTool;

impl ApplyPatchTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct ApplyPatchInput {
    #[serde(default)]
    intent: Option<String>,
    patch_text: String,
}

#[derive(Debug, Clone)]
struct UpdateFileChunk {
    change_context: Option<String>,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    is_end_of_file: bool,
}

#[derive(Debug)]
#[expect(
    clippy::enum_variant_names,
    reason = "patch variants intentionally mirror unified diff file-level operations for readability"
)]
enum PatchHunk {
    AddFile {
        path: String,
        contents: String,
    },
    DeleteFile {
        path: String,
    },
    UpdateFile {
        path: String,
        move_to: Option<String>,
        chunks: Vec<UpdateFileChunk>,
    },
}

/// A unified diff has `---`/`+++` file headers and no Codex patch envelope.
fn is_unified_diff(text: &str) -> bool {
    if text.lines().any(|line| line.trim() == "*** Begin Patch") {
        return false;
    }
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.starts_with("--- ") {
            return lines.next().is_some_and(|next| next.starts_with("+++ "));
        }
    }
    false
}

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        "Apply a multi-file patch (Codex *** Begin Patch or unified diff)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["patch_text"],
            "properties": {
                "intent": super::intent_schema_property(),
                "patch_text": {
                    "type": "string",
                    "description": "Patch text."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        super::path_policy::enforce("apply_patch", &input, &ctx)?;
        let params: ApplyPatchInput = serde_json::from_value(input)?;
        // `patch` merged into this tool. Plain unified diffs (---/+++) are
        // routed to the unified-diff applier so either format works here.
        if is_unified_diff(&params.patch_text) {
            return super::patch::PatchTool::new()
                .execute(json!({ "patch_text": params.patch_text }), ctx)
                .await;
        }
        let hunks = parse_apply_patch(&params.patch_text)?;

        // A generated file is regenerated from its source, so editing or
        // deleting one loses the change. Refuse the whole patch before anything
        // is written; `apply_patch` is atomic anyway.
        for hunk in &hunks {
            let path = match hunk {
                PatchHunk::UpdateFile { path, .. } | PatchHunk::DeleteFile { path } => path,
                PatchHunk::AddFile { .. } => continue,
            };
            let resolved = ctx.resolve_path(Path::new(path));
            if let Some(refusal) = super::generated_guard::refusal(&resolved, path) {
                anyhow::bail!(refusal);
            }
        }

        // A patch can reach config.toml through any hunk kind (add, update,
        // move), so watch the file across the whole invocation rather than
        // threading before/after content through each branch.
        let config_watch = super::config_edit_notice::ConfigEditWatch::begin();

        // Capture whole-file states, including move destinations and AddFile
        // overwrites. Diff the final state so repeated hunks share one coordinate
        // system and failed operations cannot produce a speculative preview.
        let _locks = super::file_lock::lock_all(hunks.iter().flat_map(|hunk| {
            let (path, destination) = match hunk {
                PatchHunk::AddFile { path, .. } | PatchHunk::DeleteFile { path } => (path, None),
                PatchHunk::UpdateFile { path, move_to, .. } => (path, move_to.as_ref()),
            };
            std::iter::once(path)
                .chain(destination)
                .map(|path| ctx.resolve_path(Path::new(path)))
                .collect::<Vec<_>>()
        }))
        .await;
        let mut before = std::collections::BTreeMap::new();
        for hunk in &hunks {
            let (path, destination) = match hunk {
                PatchHunk::AddFile { path, .. } | PatchHunk::DeleteFile { path } => (path, None),
                PatchHunk::UpdateFile { path, move_to, .. } => (path, move_to.as_ref()),
            };
            for path in std::iter::once(path).chain(destination) {
                if !before.contains_key(path) {
                    let resolved = ctx.resolve_path(Path::new(path));
                    before.insert(path.clone(), super::client_fs::text_snapshot(&resolved, &ctx).await);
                }
            }
        }

        let mut results = Vec::new();
        let mut touched_paths = Vec::new();
        let mut journal = super::undo::Journal::begin(&ctx);

        for hunk in &hunks {
            match hunk {
                PatchHunk::AddFile { path, contents } => {
                    let resolved = ctx.resolve_path(Path::new(path));
                    let previous = super::client_fs::text_snapshot(&resolved, &ctx).await;
                    // A snapshot is `None` for a file that cannot be read as text
                    // (non-UTF-8), so fall back to the filesystem: overwriting such
                    // a file is an approximate count, not a brand-new file.
                    let existed = previous
                        .as_ref()
                        .map(|(exists, _)| *exists)
                        .unwrap_or_else(|| resolved.exists());
                    let old = previous.and_then(|(exists, text)| exists.then_some(text));
                    let wrote_to_client = super::client_fs::write_text(&resolved, contents, &ctx).await?;
                    if !wrote_to_client {
                        if let Some(parent) = resolved.parent() {
                            tokio::fs::create_dir_all(parent).await?;
                        }
                        let before = super::undo::capture(&resolved).await;
                        tokio::fs::write(&resolved, contents).await?;
                        journal.wrote(super::undo::PendingFile {
                            path: resolved.clone(),
                            before,
                            after: super::undo::bytes(contents.as_bytes()),
                        });
                    }
                    super::edit_stats::record(&ctx, old.as_deref().unwrap_or(""), contents, existed && old.is_none())
                        .await;
                    let diff = generate_diff_summary("", contents);
                    publish_file_touch(&ctx, &resolved, path, "created", &diff, params.intent.as_deref());
                    touched_paths.push(path.clone());
                    if diff.is_empty() {
                        results.push(format!("✓ {}: created", path));
                    } else {
                        results.push(format!("✓ {}: created\n{}", path, diff));
                    }
                }
                PatchHunk::DeleteFile { path } => {
                    let resolved = ctx.resolve_path(Path::new(path));
                    // `resolve_path` passes absolute paths through unchanged, so
                    // a patch can name any file on disk. The bash gate does not
                    // cover this path, so apply the same absolute deny here
                    // (#604). Only the catastrophic tier: ordinary file deletes
                    // are this tool's normal job.
                    if let Some(refusal) = super::path_policy::catastrophic_delete_refusal(&resolved, path, &ctx) {
                        results.push(refusal);
                        continue;
                    }
                    let old = tokio::fs::read_to_string(&resolved).await.ok();
                    let old_contents = old.as_deref().unwrap_or("");
                    let before = super::undo::capture(&resolved).await;
                    if tokio::fs::remove_file(&resolved).await.is_ok() {
                        journal.wrote(super::undo::PendingFile {
                            path: resolved.clone(),
                            before,
                            after: super::undo::ABSENT,
                        });
                        super::edit_stats::record(&ctx, old_contents, "", old.is_none()).await;
                        let diff = generate_diff_summary(old_contents, "");
                        publish_file_touch(&ctx, &resolved, path, "deleted", &diff, params.intent.as_deref());
                        touched_paths.push(path.clone());
                        if diff.is_empty() {
                            results.push(format!("✓ {}: deleted", path));
                        } else {
                            results.push(format!("✓ {}: deleted\n{}", path, diff));
                        }
                    } else {
                        results.push(format!("✗ {}: failed to delete", path));
                    }
                }
                PatchHunk::UpdateFile { path, move_to, chunks } => {
                    let resolved = ctx.resolve_path(Path::new(path));
                    match apply_update_chunks(&resolved, chunks, &ctx).await {
                        Ok((old_contents, new_contents)) => {
                            let diff = generate_diff_summary(&old_contents, &new_contents);
                            if let Some(dest) = move_to {
                                let dest_resolved = ctx.resolve_path(Path::new(dest));
                                if let Some(parent) = dest_resolved.parent() {
                                    tokio::fs::create_dir_all(parent).await?;
                                }
                                let dest_existed = dest_resolved.exists();
                                let dest_old = tokio::fs::read_to_string(&dest_resolved).await.ok();
                                let dest_before = super::undo::capture(&dest_resolved).await;
                                let source_before = super::undo::capture(&resolved).await;
                                tokio::fs::write(&dest_resolved, &new_contents).await?;
                                let source_removed = tokio::fs::remove_file(&resolved).await.is_ok();
                                journal.wrote(super::undo::PendingFile {
                                    path: dest_resolved.clone(),
                                    before: dest_before,
                                    after: super::undo::bytes(new_contents.as_bytes()),
                                });
                                if source_removed {
                                    journal.wrote(super::undo::PendingFile {
                                        path: resolved.clone(),
                                        before: source_before,
                                        after: super::undo::ABSENT,
                                    });
                                }
                                if source_removed {
                                    super::edit_stats::record(&ctx, &old_contents, &new_contents, false).await;
                                    if dest_existed {
                                        super::edit_stats::record(
                                            &ctx,
                                            dest_old.as_deref().unwrap_or(""),
                                            "",
                                            dest_old.is_none(),
                                        )
                                        .await;
                                    }
                                } else {
                                    super::edit_stats::record(
                                        &ctx,
                                        dest_old.as_deref().unwrap_or(""),
                                        &new_contents,
                                        dest_existed && dest_old.is_none(),
                                    )
                                    .await;
                                }
                                publish_file_touch(&ctx, &resolved, path, "modified", &diff, params.intent.as_deref());
                                publish_file_touch(
                                    &ctx,
                                    &dest_resolved,
                                    dest,
                                    "modified",
                                    &diff,
                                    params.intent.as_deref(),
                                );
                                touched_paths.push(path.clone());
                                touched_paths.push(dest.clone());
                                if diff.is_empty() {
                                    results.push(format!(
                                        "✓ {}: modified ({} hunks), moved to {}",
                                        path,
                                        chunks.len(),
                                        dest
                                    ));
                                } else {
                                    results.push(format!(
                                        "✓ {}: modified ({} hunks), moved to {}\n{}",
                                        path,
                                        chunks.len(),
                                        dest,
                                        diff
                                    ));
                                }
                            } else {
                                let wrote_to_client =
                                    super::client_fs::write_text(&resolved, &new_contents, &ctx).await?;
                                if !wrote_to_client {
                                    let before = super::undo::capture(&resolved).await;
                                    tokio::fs::write(&resolved, &new_contents).await?;
                                    journal.wrote(super::undo::PendingFile {
                                        path: resolved.clone(),
                                        before,
                                        after: super::undo::bytes(new_contents.as_bytes()),
                                    });
                                }
                                super::edit_stats::record(&ctx, &old_contents, &new_contents, false).await;
                                publish_file_touch(&ctx, &resolved, path, "modified", &diff, params.intent.as_deref());
                                touched_paths.push(path.clone());
                                if diff.is_empty() {
                                    results.push(format!("✓ {}: modified ({} hunks)", path, chunks.len()));
                                } else {
                                    results.push(format!("✓ {}: modified ({} hunks)\n{}", path, chunks.len(), diff));
                                }
                            }
                        }
                        Err(e) => {
                            results.push(format!("✗ {}: {}", path, e));
                        }
                    }
                }
            }
        }

        if results.is_empty() {
            Ok(ToolOutput::new("No changes applied"))
        } else {
            let mut body = results.join("\n");
            config_watch.finish(&mut body);
            // A patch touches several files; each still-existing one whose
            // globs match a rulebook rule carries that rule once.
            let mut rule_checked = std::collections::BTreeSet::new();
            for path in &touched_paths {
                if !rule_checked.insert(path.as_str()) {
                    continue;
                }
                let resolved = ctx.resolve_path(Path::new(path));
                if resolved.is_file()
                    && let Some(extra) = super::nested_agents::inject_rules_for_path(&resolved, &ctx)
                {
                    body.push_str(&extra);
                }
            }
            let mut unified = String::new();
            let mut structured: Vec<super::file_diff::FileChange> = Vec::new();
            let mut after = std::collections::BTreeMap::new();
            for path in before.keys() {
                after.insert(
                    path.clone(),
                    super::client_fs::text_snapshot(&ctx.resolve_path(Path::new(path)), &ctx).await,
                );
            }
            let mut combined = std::collections::BTreeSet::new();
            // A simple successful move to a new path can retain the source's
            // coordinates. For overwrites or move chains, keep net per-path
            // diffs instead of hiding destination text that was overwritten.
            for hunk in &hunks {
                if let PatchHunk::UpdateFile {
                    path,
                    move_to: Some(dest),
                    ..
                } = hunk
                    && let (
                        Some(Some((true, old))),
                        Some(Some((false, _))),
                        Some(Some((false, _))),
                        Some(Some((true, new))),
                    ) = (before.get(path), before.get(dest), after.get(path), after.get(dest))
                    && !combined.contains(path)
                    && !combined.contains(dest)
                {
                    unified.push_str(&super::file_diff::unified(path, dest, old, new));
                    combined.insert(path.clone());
                    combined.insert(dest.clone());
                    structured.push(super::file_diff::FileChange {
                        path: ctx.resolve_path(Path::new(dest)),
                        old_path: Some(ctx.resolve_path(Path::new(path))),
                        old_text: Some(old.clone()),
                        new_text: new.clone(),
                        deleted: false,
                        created: false,
                    });
                }
            }
            for (path, old) in before {
                if combined.contains(&path) {
                    continue;
                }
                if let (Some((old_exists, old)), Some(Some((new_exists, new)))) = (old, after.remove(&path)) {
                    unified.push_str(&super::file_diff::unified(
                        if old_exists || !new_exists { &path } else { "/dev/null" },
                        if new_exists || !old_exists { &path } else { "/dev/null" },
                        &old,
                        &new,
                    ));
                    structured.push(super::file_diff::FileChange {
                        path: ctx.resolve_path(Path::new(&path)),
                        old_path: None,
                        old_text: Some(old),
                        new_text: new,
                        deleted: old_exists && !new_exists,
                        created: !old_exists && new_exists,
                    });
                }
            }
            let output = super::file_diff::attach_file_changes(
                super::file_diff::attach(ToolOutput::new(body), unified),
                structured,
            );
            if touched_paths.len() == 1 {
                Ok(output.with_title(touched_paths[0].clone()))
            } else {
                Ok(output.with_title(format!("{} files", touched_paths.len())))
            }
        }
    }
}

fn publish_file_touch(
    ctx: &ToolContext,
    resolved: &Path,
    display_path: &str,
    verb: &str,
    diff: &str,
    intent: Option<&str>,
) {
    let detail = build_file_touch_preview(diff);
    Bus::global().publish(BusEvent::FileTouch(FileTouch {
        session_id: ctx.session_id.clone(),
        path: resolved.to_path_buf(),
        op: FileOp::Edit,
        intent: intent
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        summary: Some(format!("{} via apply_patch", verb)),
        detail,
    }));
    let _ = display_path;
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

async fn apply_update_chunks(path: &Path, chunks: &[UpdateFileChunk], ctx: &ToolContext) -> Result<(String, String)> {
    let original_contents = match super::client_fs::read_text(path, ctx).await? {
        super::client_fs::TextRead::Client(text) => text,
        super::client_fs::TextRead::NotFound => {
            // Report a client `-32002` exactly as the local read reports a missing
            // path, so a patch result does not depend on which side served it.
            return Err(super::client_fs::missing_file_error());
        }
        super::client_fs::TextRead::Local => tokio::fs::read_to_string(path).await?,
    };
    let mut original_lines: Vec<String> = original_contents.split('\n').map(String::from).collect();

    if original_lines.last().is_some_and(String::is_empty) {
        original_lines.pop();
    }

    let replacements = compute_replacements(&original_lines, path, chunks)?;
    let mut new_lines = apply_replacements(original_lines, &replacements);

    if !new_lines.last().is_some_and(String::is_empty) {
        new_lines.push(String::new());
    }
    Ok((original_contents, new_lines.join("\n")))
}

/// Generate a compact diff with line numbers (max 30 lines).
fn generate_diff_summary(old: &str, new: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut output = String::new();
    let mut line_count = 0;
    const MAX_LINES: usize = 30;

    let mut old_line = 1usize;
    let mut new_line = 1usize;

    for change in diff.iter_all_changes() {
        if line_count >= MAX_LINES {
            output.push_str("... (diff truncated)\n");
            break;
        }

        let content = change.value().trim_end_matches('\n');
        let (prefix, line_num) = match change.tag() {
            ChangeTag::Delete => {
                let num = old_line;
                old_line += 1;
                if content.trim().is_empty() {
                    continue;
                }
                ("-", num)
            }
            ChangeTag::Insert => {
                let num = new_line;
                new_line += 1;
                if content.trim().is_empty() {
                    continue;
                }
                ("+", num)
            }
            ChangeTag::Equal => {
                old_line += 1;
                new_line += 1;
                continue;
            }
        };

        output.push_str(&format!("{}{} {}\n", line_num, prefix, content));
        line_count += 1;
    }

    output.trim_end().to_string()
}

fn compute_replacements(
    original_lines: &[String],
    path: &Path,
    chunks: &[UpdateFileChunk],
) -> Result<Vec<(usize, usize, Vec<String>)>> {
    let mut replacements: Vec<(usize, usize, Vec<String>)> = Vec::new();
    let mut line_index: usize = 0;

    for chunk in chunks {
        if let Some(ctx_line) = &chunk.change_context {
            if let Some(idx) = seek_sequence(original_lines, std::slice::from_ref(ctx_line), line_index, false) {
                line_index = idx + 1;
            } else {
                anyhow::bail!("Failed to find context '{}' in {}", ctx_line, path.display());
            }
        }

        if chunk.old_lines.is_empty() {
            let insertion_idx = if original_lines.last().is_some_and(String::is_empty) {
                original_lines.len() - 1
            } else {
                original_lines.len()
            };
            replacements.push((insertion_idx, 0, chunk.new_lines.clone()));
            continue;
        }

        let mut pattern: &[String] = &chunk.old_lines;
        let mut found = seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);

        let mut new_slice: &[String] = &chunk.new_lines;

        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            pattern = &pattern[..pattern.len() - 1];
            if new_slice.last().is_some_and(String::is_empty) {
                new_slice = &new_slice[..new_slice.len() - 1];
            }
            found = seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);
        }

        if let Some(start_idx) = found {
            replacements.push((start_idx, pattern.len(), new_slice.to_vec()));
            line_index = start_idx + pattern.len();
        } else {
            anyhow::bail!(
                "Failed to find expected lines in {}:\n{}",
                path.display(),
                chunk.old_lines.join("\n"),
            );
        }
    }

    replacements.sort_by(|(a, _, _), (b, _, _)| a.cmp(b));
    Ok(replacements)
}

fn apply_replacements(mut lines: Vec<String>, replacements: &[(usize, usize, Vec<String>)]) -> Vec<String> {
    for (start_idx, old_len, new_segment) in replacements.iter().rev() {
        let start_idx = *start_idx;
        let old_len = *old_len;

        for _ in 0..old_len {
            if start_idx < lines.len() {
                lines.remove(start_idx);
            }
        }

        for (offset, new_line) in new_segment.iter().enumerate() {
            lines.insert(start_idx + offset, new_line.clone());
        }
    }

    lines
}

fn seek_sequence(lines: &[String], pattern: &[String], start: usize, eof: bool) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start);
    }

    if pattern.len() > lines.len() {
        return None;
    }

    let search_start = if eof && lines.len() >= pattern.len() {
        lines.len() - pattern.len()
    } else {
        start
    };

    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        if lines[i..i + pattern.len()] == *pattern {
            return Some(i);
        }
    }

    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        let mut ok = true;
        for (p_idx, pat) in pattern.iter().enumerate() {
            if lines[i + p_idx].trim_end() != pat.trim_end() {
                ok = false;
                break;
            }
        }
        if ok {
            return Some(i);
        }
    }

    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        let mut ok = true;
        for (p_idx, pat) in pattern.iter().enumerate() {
            if lines[i + p_idx].trim() != pat.trim() {
                ok = false;
                break;
            }
        }
        if ok {
            return Some(i);
        }
    }

    None
}

fn parse_apply_patch(input: &str) -> Result<Vec<PatchHunk>> {
    let lines: Vec<&str> = input.lines().collect();

    let start = lines
        .iter()
        .position(|l| l.trim() == "*** Begin Patch")
        .ok_or_else(|| anyhow::anyhow!("Patch must contain *** Begin Patch"))?;

    let mut hunks = Vec::new();
    let mut i = start + 1;

    while i < lines.len() {
        let line = lines[i].trim_end();
        if line.trim() == "*** End Patch" {
            break;
        }

        if let Some(path) = line.strip_prefix("*** Add File: ") {
            let path = path.trim().to_string();
            i += 1;
            let mut contents = String::new();
            while i < lines.len() {
                let current = lines[i];
                if current.starts_with("*** ") {
                    break;
                }
                if let Some(added) = current.strip_prefix('+') {
                    contents.push_str(added);
                    contents.push('\n');
                }
                i += 1;
            }
            hunks.push(PatchHunk::AddFile { path, contents });
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            hunks.push(PatchHunk::DeleteFile {
                path: path.trim().to_string(),
            });
            i += 1;
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Update File: ") {
            let path = path.trim().to_string();
            i += 1;

            let mut move_to = None;
            if i < lines.len()
                && let Some(target) = lines[i].trim_end().strip_prefix("*** Move to: ")
            {
                move_to = Some(target.trim().to_string());
                i += 1;
            }

            let mut chunks = Vec::new();
            let mut is_first_chunk = true;

            while i < lines.len() {
                let current = lines[i].trim_end();

                if current.starts_with("*** ") && current != "*** End of File" {
                    break;
                }

                if current.trim().is_empty()
                    && !current.starts_with(' ')
                    && !current.starts_with('+')
                    && !current.starts_with('-')
                {
                    i += 1;
                    continue;
                }

                let change_context;
                if current == "@@" {
                    change_context = None;
                    i += 1;
                } else if let Some(ctx) = current.strip_prefix("@@ ") {
                    change_context = Some(ctx.to_string());
                    i += 1;
                } else if is_first_chunk {
                    change_context = None;
                } else {
                    break;
                }

                let mut old_lines = Vec::new();
                let mut new_lines = Vec::new();
                let mut is_end_of_file = false;
                let mut had_diff_lines = false;

                while i < lines.len() {
                    let cl = lines[i];

                    if cl == "*** End of File" {
                        is_end_of_file = true;
                        i += 1;
                        break;
                    }

                    if cl.starts_with("*** ") || cl.starts_with("@@") {
                        break;
                    }

                    if let Some(content) = cl.strip_prefix(' ') {
                        old_lines.push(content.to_string());
                        new_lines.push(content.to_string());
                        had_diff_lines = true;
                    } else if let Some(content) = cl.strip_prefix('+') {
                        new_lines.push(content.to_string());
                        had_diff_lines = true;
                    } else if let Some(content) = cl.strip_prefix('-') {
                        old_lines.push(content.to_string());
                        had_diff_lines = true;
                    } else if cl.is_empty() {
                        old_lines.push(String::new());
                        new_lines.push(String::new());
                        had_diff_lines = true;
                    } else {
                        if had_diff_lines {
                            break;
                        }
                        i += 1;
                        continue;
                    }

                    i += 1;
                }

                if had_diff_lines || change_context.is_some() {
                    chunks.push(UpdateFileChunk {
                        change_context,
                        old_lines,
                        new_lines,
                        is_end_of_file,
                    });
                }

                is_first_chunk = false;
            }

            if chunks.is_empty() {
                anyhow::bail!("Update file hunk for '{}' has no changes", path);
            }

            hunks.push(PatchHunk::UpdateFile { path, move_to, chunks });
            continue;
        }

        i += 1;
    }

    if hunks.is_empty() {
        anyhow::bail!("No valid patch directives found");
    }

    Ok(hunks)
}

#[cfg(test)]
#[path = "apply_patch_tests.rs"]
mod apply_patch_tests;
