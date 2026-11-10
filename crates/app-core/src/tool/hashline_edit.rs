//! `hashline_edit`: replace line spans named by the content anchors `read`
//! emits in hashline mode.
//!
//! Every anchor is checked against the file before anything is written, so one
//! stale anchor refuses the whole call and leaves the file untouched. The write
//! path is `edit`'s: per-path lock, one undo-journal mutation unit, and the same
//! file-touch bus event.
//!
//! A call may also name a file-level effect, `move_to` or `delete`, which is
//! applied after its line ops. Both run the path policy and the generated-file
//! guard on every path they touch, a move refuses an existing destination rather
//! than overwriting it, and the line ops plus the file-level effect share one
//! undo unit, so one `undo` restores the file and its old path.

use super::{ConcurrencyClass, Tool, ToolContext, ToolOutput};
use crate::bus::{Bus, BusEvent, FileOp, FileTouch};
use anyhow::Result;
use async_trait::async_trait;
use horon_agent_edit_safety::tag_for;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{LazyLock, Mutex};

/// Anchor rendered by `read` for a line cut at its length limit. It names no
/// content, so there is nothing to validate it against.
const TRUNCATED_TAG: &str = "----";

const TAG_LEN: usize = 4;

const FILE_TOUCH_PREVIEW_MAX_LINES: usize = 6;
const FILE_TOUCH_PREVIEW_MAX_BYTES: usize = 240;

pub struct HashlineEditTool;

impl HashlineEditTool {
    pub fn new() -> Self {
        Self
    }
}

/// `[tools] hashline_edit`, off by default.
pub(crate) fn enabled() -> bool {
    crate::config::config().tools.hashline_edit
}

#[derive(Deserialize)]
struct HashlineEditInput {
    #[serde(default)]
    intent: Option<String>,
    file_path: String,
    #[serde(default)]
    ops: Vec<HashlineOp>,
    /// Move the file (after any line ops) to this path, then remove the source.
    #[serde(default)]
    move_to: Option<String>,
    /// Delete the whole file after any line ops.
    #[serde(default)]
    delete: bool,
}

/// The file-level effect of one call, applied after its line ops.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FileLevelOp {
    None,
    Move(String),
    Delete,
}

impl HashlineEditInput {
    /// The file-level effect, or the reason the call names an impossible pair.
    fn file_level_op(&self) -> Result<FileLevelOp> {
        match (&self.move_to, self.delete) {
            (Some(_), true) => anyhow::bail!(
                "`move_to` and `delete` cannot be combined: a file is either moved to a new path or removed."
            ),
            (Some(destination), false) if destination.trim().is_empty() => {
                anyhow::bail!("`move_to` is empty; name the destination path.")
            }
            (Some(destination), false) => Ok(FileLevelOp::Move(destination.clone())),
            (None, true) => Ok(FileLevelOp::Delete),
            (None, false) => Ok(FileLevelOp::None),
        }
    }
}

#[derive(Deserialize)]
struct HashlineOp {
    start_anchor: String,
    #[serde(default)]
    end_anchor: Option<String>,
    #[serde(default)]
    new_text: String,
    #[serde(default)]
    kind: HashlineOpKind,
    #[serde(default)]
    block: bool,
}

/// What an anchored op does to its span.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum HashlineOpKind {
    /// Replace the span with `new_text`; an empty string deletes it.
    #[default]
    Replace,
    /// Insert `new_text` before the line named by `start_anchor`.
    InsertBefore,
    /// Insert `new_text` after the line named by `start_anchor`.
    InsertAfter,
    /// Delete the span and ignore `new_text`.
    Delete,
}

struct Anchor {
    raw: String,
    line: usize,
    tag: String,
}

struct ResolvedOp {
    start: usize,
    end: usize,
    start_anchor: String,
    end_anchor: String,
    parts: Vec<String>,
    kind: HashlineOpKind,
    /// Anchor rebases and other notices for this op, in the order they happened.
    notes: Vec<String>,
}

impl ResolvedOp {
    fn describe(&self) -> String {
        if self.start == self.end {
            format!("line {}", self.start)
        } else {
            format!("lines {}-{}", self.start, self.end)
        }
    }
}

fn parse_anchor(raw: &str) -> Result<Anchor> {
    let text = raw.trim();
    let Some((line_text, rest)) = text.split_once('#') else {
        anyhow::bail!("anchor `{raw}` is not `{{line}}#{{tag}}` (for example `12#a1b2`)");
    };
    if line_text.is_empty() || !line_text.chars().all(|c| c.is_ascii_digit()) {
        anyhow::bail!("anchor `{raw}` has no 1-based line number before `#`");
    }
    let line: usize = line_text
        .parse()
        .map_err(|_| anyhow::anyhow!("anchor `{raw}` has an unreadable line number"))?;
    if line == 0 {
        anyhow::bail!("anchor `{raw}` names line 0; line numbers start at 1");
    }
    if rest.starts_with(TRUNCATED_TAG) {
        anyhow::bail!(
            "anchor `{raw}` carries `{TRUNCATED_TAG}`, which marks a line cut by the read length limit and names no content. \
             Re-read the file without the anchor or edit that line another way."
        );
    }
    if rest.len() < TAG_LEN || !rest.is_char_boundary(TAG_LEN) {
        anyhow::bail!("anchor `{raw}` has no four-character tag after `#`");
    }
    let tag = &rest[..TAG_LEN];
    if !tag.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!("anchor `{raw}` has a non-hex tag `{tag}`; a tag is four hexadecimal characters");
    }
    let tail = &rest[TAG_LEN..];
    if !tail.is_empty() && !tail.starts_with(':') {
        anyhow::bail!("anchor `{raw}` has more than four characters after `#`; expected a four-character tag");
    }
    Ok(Anchor {
        raw: raw.trim().to_string(),
        line,
        tag: tag.to_ascii_lowercase(),
    })
}

/// One trailing newline separates lines; it is not content.
fn new_text_parts(new_text: &str) -> Vec<String> {
    let text = new_text.strip_suffix('\n').unwrap_or(new_text);
    if text.is_empty() {
        return Vec::new();
    }
    text.split('\n').map(str::to_string).collect()
}

/// Re-anchors a stale anchor by searching within ±20 lines for its tag.
///
/// A stale anchor is usually a line-number drift from an edit made since the
/// read. Exactly one matching tag in the neighbourhood is decisive; zero or
/// several keep the original refusal, because guessing would edit the wrong
/// line.
fn relocate(anchor: &Anchor, lines: &[&str]) -> Option<usize> {
    let line = anchor.line;
    let low = line.saturating_sub(20).max(1);
    let high = (line + 20).min(lines.len());
    let mut hits = Vec::new();
    for candidate in low..=high {
        if candidate == line {
            continue;
        }
        if let Some(text) = lines.get(candidate - 1)
            && tag_for(text.as_bytes()).to_string() == anchor.tag
        {
            hits.push(candidate);
        }
    }
    if hits.len() == 1 { hits.pop() } else { None }
}

fn resolve_one_anchor(
    anchor: &mut Anchor,
    lines: &[&str],
    file_path: &str,
    notes: &mut Vec<String>,
) -> std::result::Result<(), String> {
    match lines.get(anchor.line - 1) {
        Some(text) if tag_for(text.as_bytes()).to_string() == anchor.tag => Ok(()),
        in_range => {
            let Some(replacement) = relocate(anchor, lines) else {
                return Err(match in_range {
                    Some(text) => format!(
                        "anchor `{}` is stale: expected tag {}, actual tag {} at line {} (`{}`)",
                        anchor.raw,
                        anchor.tag,
                        tag_for(text.as_bytes()),
                        anchor.line,
                        text
                    ),
                    None => format!(
                        "anchor `{}` names line {} but {file_path} has {} line(s)",
                        anchor.raw,
                        anchor.line,
                        lines.len()
                    ),
                });
            };
            notes.push(format!("  ↻ anchor {} rebased to line {}", anchor.raw, replacement));
            anchor.line = replacement;
            Ok(())
        }
    }
}

fn resolve_ops(ops: &[HashlineOp], content: &str, lines: &[&str], file_path: &str) -> Result<Vec<ResolvedOp>> {
    let mut resolved = Vec::with_capacity(ops.len());
    let mut failures = Vec::new();
    let label = |index: usize| {
        if ops.len() == 1 {
            String::new()
        } else {
            format!("op {}: ", index + 1)
        }
    };

    for (index, op) in ops.iter().enumerate() {
        let mut start = match parse_anchor(&op.start_anchor) {
            Ok(anchor) => anchor,
            Err(error) => {
                failures.push(format!("{}{error}", label(index)));
                continue;
            }
        };
        let single_line = matches!(op.kind, HashlineOpKind::InsertBefore | HashlineOpKind::InsertAfter);
        let mut end = if single_line {
            Anchor {
                raw: start.raw.clone(),
                line: start.line,
                tag: start.tag.clone(),
            }
        } else {
            match op.end_anchor.as_deref() {
                Some(raw) => match parse_anchor(raw) {
                    Ok(anchor) => anchor,
                    Err(error) => {
                        failures.push(format!("{}{error}", label(index)));
                        continue;
                    }
                },
                None => Anchor {
                    raw: start.raw.clone(),
                    line: start.line,
                    tag: start.tag.clone(),
                },
            }
        };

        let mut notes = Vec::new();
        if let Err(error) = resolve_one_anchor(&mut start, lines, file_path, &mut notes) {
            failures.push(format!("{}{error}", label(index)));
            continue;
        }
        if end.line != start.line
            && let Err(error) = resolve_one_anchor(&mut end, lines, file_path, &mut notes)
        {
            failures.push(format!("{}{error}", label(index)));
            continue;
        }
        if single_line {
            end.line = start.line;
        }

        if start.line > end.line {
            failures.push(format!(
                "{}start_anchor `{}` names line {} but end_anchor `{}` names line {}; the span must run downward",
                label(index),
                start.raw,
                start.line,
                end.raw,
                end.line
            ));
            continue;
        }

        if op.block {
            let expanded = horon_agent_ast::Lang::from_path(Path::new(file_path))
                .and_then(|lang| horon_agent_ast::enclosing_block(content, lang, start.line));
            match expanded {
                Some((block_start, block_end)) => {
                    start.line = block_start;
                    end.line = block_end;
                }
                None => {
                    failures.push(format!(
                        "{}block=true needs a supported language and a block at line {}",
                        label(index),
                        start.line
                    ));
                    continue;
                }
            }
        }

        let parts = match op.kind {
            HashlineOpKind::Delete => Vec::new(),
            _ => new_text_parts(&op.new_text),
        };
        resolved.push(ResolvedOp {
            start: start.line,
            end: end.line,
            start_anchor: start.raw,
            end_anchor: end.raw,
            parts,
            kind: op.kind,
            notes,
        });
    }

    if failures.is_empty() {
        let mut ordered: Vec<&ResolvedOp> = resolved.iter().collect();
        ordered.sort_by_key(|op| op.start);
        for pair in ordered.windows(2) {
            if pair[0].end >= pair[1].start {
                failures.push(format!(
                    "spans overlap: {} (from `{}`) and {} (from `{}`)",
                    pair[0].describe(),
                    pair[0].start_anchor,
                    pair[1].describe(),
                    pair[1].start_anchor
                ));
            }
        }
    }

    if failures.is_empty() {
        return Ok(resolved);
    }

    if ops.len() == 1 {
        return Err(anyhow::anyhow!(
            "No changes written to {file_path}. {}",
            failures.remove(0)
        ));
    }
    Err(anyhow::anyhow!(
        "No changes written to {file_path}. {} of {} anchored edits failed:\n{}\n\
         Every anchor is checked against the file as it is on disk before anything is written.",
        failures.len(),
        ops.len(),
        failures
            .iter()
            .map(|failure| format!("  ✗ {failure}"))
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

fn render(parts: &[String], ops: &[ResolvedOp]) -> String {
    let mut parts: Vec<String> = parts.to_vec();
    let mut ordered: Vec<&ResolvedOp> = ops.iter().collect();
    // Later spans first, so earlier line numbers stay valid.
    ordered.sort_by_key(|op| std::cmp::Reverse((op.start, op.end)));
    for op in ordered {
        let start = op.start - 1;
        let end = op.end - 1;
        match op.kind {
            HashlineOpKind::Replace | HashlineOpKind::Delete => {
                let mut replacement = op.parts.clone();
                if parts.get(end).is_some_and(|line| line.ends_with('\r')) {
                    for line in &mut replacement {
                        if !line.ends_with('\r') {
                            line.push('\r');
                        }
                    }
                }
                parts.splice(start..=end, replacement);
            }
            HashlineOpKind::InsertBefore => {
                parts.splice(start..start, op.parts.clone());
            }
            HashlineOpKind::InsertAfter => {
                parts.splice(end + 1..end + 1, op.parts.clone());
            }
        }
    }
    parts.join("\n")
}

pub(super) fn build_file_touch_preview(diff: &str) -> Option<String> {
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

type NoopKey = (String, String);
type NoopEntry = (u64, u32);

/// `(session, path)` -> `(content hash, consecutive identical no-ops)`.
static NOOP_TRACKER: LazyLock<Mutex<HashMap<NoopKey, NoopEntry>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn content_hash(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Counts an op set that leaves the file byte-identical, keyed by the current
/// content so a real change in between restarts the streak.
fn record_noop(session_id: &str, path: &Path, content: &str) -> u32 {
    let key = (session_id.to_string(), path.to_string_lossy().to_string());
    let hash = content_hash(content);
    let mut tracker = NOOP_TRACKER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = tracker.entry(key).or_insert((hash, 0));
    if entry.0 == hash {
        entry.1 += 1;
    } else {
        *entry = (hash, 1);
    }
    entry.1
}

fn clear_noop(session_id: &str, path: &Path) {
    let key = (session_id.to_string(), path.to_string_lossy().to_string());
    NOOP_TRACKER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&key);
}

#[async_trait]
impl Tool for HashlineEditTool {
    fn name(&self) -> &str {
        "hashline_edit"
    }

    fn description(&self) -> &str {
        "Replace line spans named by hashline anchors, and optionally move or delete the whole file. Every anchor is validated before anything is written."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["file_path"],
            "properties": {
                "intent": super::intent_schema_property(),
                "file_path": {
                    "type": "string",
                    "description": "File path."
                },
                "move_to": {
                    "type": "string",
                    "description": "After the line ops, move the file to this path and remove the source. The destination must not exist. Cannot be combined with `delete`."
                },
                "delete": {
                    "type": "boolean",
                    "description": "After the line ops, delete the whole file. Cannot be combined with `move_to`."
                },
                "ops": {
                    "type": "array",
                    "description": "Anchored replacements, applied as one all-or-nothing call. Spans may not overlap. Omit only when `move_to` or `delete` names the call's whole effect.",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "required": ["start_anchor"],
                        "properties": {
                            "start_anchor": {
                                "type": "string",
                                "description": "Anchor of the first line of the span, as `{line}#{tag}` copied from a `read` with hashline: true. Inserts use only this anchor."
                            },
                            "end_anchor": {
                                "type": "string",
                                "description": "Anchor of the last line of the span, inclusive. Omit to use the single line named by start_anchor."
                            },
                            "new_text": {
                                "type": "string",
                                "description": "Replacement or inserted text. An empty string deletes the span for kind=replace, and is ignored for kind=delete."
                            },
                            "kind": {
                                "type": "string",
                                "enum": ["replace", "insert_before", "insert_after", "delete"],
                                "description": "What to do with the span. Default replace. Inserts use only start_anchor; delete ignores new_text."
                            },
                            "block": {
                                "type": "boolean",
                                "description": "Expand the span to the enclosing function, impl, class, or block of the start line. Requires a supported language (rust, typescript, tsx, javascript, python, go)."
                            }
                        }
                    }
                }
            }
        })
    }

    fn concurrency_class(&self) -> ConcurrencyClass {
        ConcurrencyClass::Ordered
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        super::path_policy::enforce("hashline_edit", &input, &ctx)?;
        let mut params: HashlineEditInput = serde_json::from_value(input)?;
        let file_op = params.file_level_op()?;
        if params.ops.is_empty() && file_op == FileLevelOp::None {
            anyhow::bail!("`ops` must contain at least one anchored edit, or name `move_to` or `delete`.");
        }
        let path = ctx.resolve_path(Path::new(&params.file_path));
        if !path.exists() {
            anyhow::bail!("File not found: {}", params.file_path);
        }
        if let Some(refusal) = super::generated_guard::refusal(&path, &params.file_path) {
            anyhow::bail!(refusal);
        }
        if params.delete
            && let Some(refusal) = super::path_policy::catastrophic_delete_refusal(&path, &params.file_path, &ctx)
        {
            anyhow::bail!(refusal);
        }
        // A move is decided before any write: an existing destination is
        // refused rather than overwritten, because moving onto a file the model
        // has not read would lose it, and `apply_patch` remains the way to
        // replace a destination's contents deliberately.
        let move_destination = match &file_op {
            FileLevelOp::Move(destination) => {
                let resolved = ctx.resolve_path(Path::new(destination));
                if resolved == path {
                    anyhow::bail!("`move_to` names the file itself: {destination}");
                }
                if let Some(refusal) = super::generated_guard::refusal(&resolved, destination) {
                    anyhow::bail!(refusal);
                }
                if resolved.exists() {
                    anyhow::bail!(
                        "`move_to` destination already exists: {destination}. Choose a new path; `hashline_edit` never overwrites a destination."
                    );
                }
                Some(resolved)
            }
            FileLevelOp::Delete | FileLevelOp::None => None,
        };

        let mut stripped_prefix_lines = 0usize;
        for op in &mut params.ops {
            let (stripped, count) = super::write::strip_hashline_prefixes(&op.new_text);
            if count > 0 {
                op.new_text = stripped.into_owned();
                stripped_prefix_lines += count;
            }
        }

        let _locks =
            super::file_lock::lock_all(std::iter::once(path.clone()).chain(move_destination.iter().cloned())).await;
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|error| anyhow::anyhow!("cannot read {} as UTF-8 text: {error}", params.file_path))?;

        let lines: Vec<&str> = content.lines().collect();
        let parts: Vec<String> = content.split('\n').map(str::to_string).collect();
        let ops = if params.ops.is_empty() {
            Vec::new()
        } else {
            resolve_ops(&params.ops, &content, &lines, &params.file_path)?
        };
        let new_content = if ops.is_empty() {
            content.clone()
        } else {
            render(&parts, &ops)
        };
        let content_changed = new_content != content;

        if !content_changed && file_op == FileLevelOp::None {
            let streak = record_noop(&ctx.session_id, &path, &content);
            if streak >= 3 {
                return Ok(ToolOutput::new(format!(
                    "STOP. Edits to {} have been a byte-identical no-op 3 times in a row. Re-read the file and change your approach.",
                    params.file_path
                )));
            }
            return Ok(ToolOutput::new(format!(
                "No changes: the edits reproduce the current content of {}.",
                params.file_path
            )));
        }
        clear_noop(&ctx.session_id, &path);

        let before = super::undo::capture(&path).await;
        let mut pending: Vec<super::undo::PendingFile> = Vec::new();
        match &file_op {
            FileLevelOp::None => {
                tokio::fs::write(&path, &new_content).await?;
                pending.push(super::undo::PendingFile {
                    path: path.clone(),
                    before,
                    after: super::undo::bytes(new_content.as_bytes()),
                });
            }
            FileLevelOp::Delete => {
                tokio::fs::remove_file(&path)
                    .await
                    .map_err(|error| anyhow::anyhow!("cannot delete {}: {error}", params.file_path))?;
                pending.push(super::undo::PendingFile {
                    path: path.clone(),
                    before,
                    after: super::undo::ABSENT,
                });
            }
            FileLevelOp::Move(destination) => {
                let Some(dest) = &move_destination else {
                    anyhow::bail!("`move_to` is missing its destination path.");
                };
                if let Some(parent) = dest.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                let dest_before = super::undo::capture(dest).await;
                tokio::fs::write(dest, &new_content).await?;
                if let Err(error) = tokio::fs::remove_file(&path).await {
                    // Leave one file, not two: drop the copy we just wrote.
                    if let Err(rollback) = tokio::fs::remove_file(dest).await {
                        crate::logging::warn(&format!(
                            "hashline_edit: cannot roll back {} after a failed move: {rollback}",
                            dest.display()
                        ));
                    }
                    anyhow::bail!(
                        "cannot remove {} after writing {destination}: {error}",
                        params.file_path
                    );
                }
                pending.push(super::undo::PendingFile {
                    path: dest.clone(),
                    before: dest_before,
                    after: super::undo::bytes(new_content.as_bytes()),
                });
                pending.push(super::undo::PendingFile {
                    path: path.clone(),
                    before,
                    after: super::undo::ABSENT,
                });
            }
        }
        // One mutation unit for the whole call, so `undo` reverses the edits and
        // the file-level effect together.
        super::undo::record(&ctx, pending);
        super::edit_stats::record(&ctx, &content, &new_content, false).await;

        let intent = params.intent.clone().filter(|value| !value.trim().is_empty());
        let diff = match &file_op {
            FileLevelOp::None => {
                super::file_diff::unified(&params.file_path, &params.file_path, &content, &new_content)
            }
            FileLevelOp::Delete => super::file_diff::unified(&params.file_path, "/dev/null", &content, ""),
            FileLevelOp::Move(destination) => {
                super::file_diff::unified(&params.file_path, destination, &content, &new_content)
            }
        };
        let summary = match &file_op {
            FileLevelOp::None => format!(
                "applied {} anchored edit{} ({})",
                ops.len(),
                if ops.len() == 1 { "" } else { "s" },
                ops.iter().map(ResolvedOp::describe).collect::<Vec<_>>().join(", ")
            ),
            FileLevelOp::Delete => "deleted the file".to_string(),
            FileLevelOp::Move(destination) => format!("moved the file to {destination}"),
        };
        Bus::global().publish(BusEvent::FileTouch(FileTouch {
            session_id: ctx.session_id.clone(),
            path: path.to_path_buf(),
            op: FileOp::Edit,
            intent,
            summary: Some(summary),
            detail: build_file_touch_preview(&diff),
        }));
        if let FileLevelOp::Move(destination) = &file_op {
            Bus::global().publish(BusEvent::FileTouch(FileTouch {
                session_id: ctx.session_id.clone(),
                path: move_destination
                    .clone()
                    .unwrap_or_else(|| Path::new(destination).to_path_buf()),
                op: FileOp::Write,
                intent: None,
                summary: Some(format!("moved here from {}", params.file_path)),
                detail: None,
            }));
        }

        let mut body = match &file_op {
            FileLevelOp::None => format!(
                "Edited {}: applied {} anchored edit{}\n",
                params.file_path,
                ops.len(),
                if ops.len() == 1 { "" } else { "s" }
            ),
            FileLevelOp::Delete => format!("Deleted {}.\n", params.file_path),
            FileLevelOp::Move(destination) => format!("Moved {} to {destination}.\n", params.file_path),
        };
        for op in &ops {
            let from = if op.start_anchor == op.end_anchor {
                format!("`{}`", op.start_anchor)
            } else {
                format!("`{}`..`{}`", op.start_anchor, op.end_anchor)
            };
            match op.kind {
                HashlineOpKind::InsertBefore => body.push_str(&format!(
                    "  ✓ inserted {} line{} before line {}\n",
                    op.parts.len(),
                    if op.parts.len() == 1 { "" } else { "s" },
                    op.start
                )),
                HashlineOpKind::InsertAfter => body.push_str(&format!(
                    "  ✓ inserted {} line{} after line {}\n",
                    op.parts.len(),
                    if op.parts.len() == 1 { "" } else { "s" },
                    op.end
                )),
                HashlineOpKind::Delete => body.push_str(&format!("  ✓ deleted {}\n", op.describe())),
                HashlineOpKind::Replace => body.push_str(&format!(
                    "  ✓ {} -> {} replacement line{}\n",
                    op.describe(),
                    op.parts.len(),
                    if op.parts.len() == 1 { "" } else { "s" }
                )),
            }
            body.push_str(&format!("    anchors {from}\n"));
            for note in &op.notes {
                body.push_str(note);
                body.push('\n');
            }
        }
        if !ops.is_empty() && file_op == FileLevelOp::Delete {
            body.push_str("  (the file is gone, so these line edits were not kept)\n");
        }
        if !diff.trim().is_empty() && diff.trim_start().starts_with("---") {
            body.push_str("\nDiff:\n");
            body.push_str(diff.trim_end());
        }
        if stripped_prefix_lines > 0 {
            body.push_str(&format!(
                "\n(Stripped read-output line prefixes from {stripped_prefix_lines} lines before writing.)"
            ));
        }
        if file_op == FileLevelOp::None {
            super::config_edit_notice::append_config_edit_notice(&mut body, &path, &content, &new_content);
            if let Some(extra) = super::nested_agents::inject_rules_for_path(&path, &ctx) {
                body.push_str(&extra);
            }
        }

        Ok(super::file_diff::attach(
            ToolOutput::new(body).with_title(params.file_path.clone()),
            diff,
        ))
    }
}

#[cfg(test)]
#[path = "hashline_edit_tests.rs"]
mod hashline_edit_tests;
