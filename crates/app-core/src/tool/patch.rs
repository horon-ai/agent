use super::{Tool, ToolContext, ToolOutput, ToolVerdict};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use similar::{ChangeTag, TextDiff};
use std::path::Path;
use tokio::io::AsyncWriteExt;

pub struct PatchTool;

impl PatchTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct PatchInput {
    patch_text: String,
}

#[derive(Debug)]
struct FilePatch<'a> {
    path: &'a str,
    hunks: Vec<Hunk<'a>>,
    is_new: bool,
    is_delete: bool,
}

/// Body lines stay borrowed from the patch text: the applier only compares and
/// copies them, so one allocation per parsed line would buy nothing.
#[derive(Debug)]
struct Hunk<'a> {
    old_start: usize,
    old_count: usize,
    new_start: usize,
    old_lines: Vec<&'a str>,
    new_lines: Vec<&'a str>,
    old_no_newline: bool,
    new_no_newline: bool,
}

/// One file's operation after preflight, with the text its write must use.
#[derive(Debug)]
enum PreparedAction {
    Create(String),
    Modify {
        old: String,
        new: String,
        first_line: usize,
        hunks: usize,
    },
    Delete {
        old: Option<String>,
    },
}

#[derive(Debug)]
struct PreparedFile<'a> {
    path: &'a str,
    resolved: std::path::PathBuf,
    action: PreparedAction,
}

#[async_trait]
impl Tool for PatchTool {
    fn name(&self) -> &str {
        "patch"
    }

    fn description(&self) -> &str {
        "Apply a unified diff (---/+++ headers). Prefer apply_patch for Codex patches."
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
        super::path_policy::enforce("patch", &input, &ctx)?;
        let params: PatchInput = serde_json::from_value(input)?;

        let patches = parse_patch(&params.patch_text)?;

        if patches.is_empty() {
            return Err(anyhow::anyhow!("No valid patches found in input"));
        }

        let resolved: Vec<(&FilePatch<'_>, std::path::PathBuf)> = patches
            .iter()
            .map(|patch| (patch, ctx.resolve_path(Path::new(patch.path))))
            .collect();

        // One path may appear under several spellings; writing either spelling
        // would make the later operation overwrite what the earlier one just
        // produced. Refuse before any read so the whole request is predictable.
        // A single-file patch cannot repeat a path, so it skips the canonicalize
        // that each identity costs.
        if resolved.len() > 1 {
            let mut seen = std::collections::BTreeMap::new();
            let mut duplicates = Vec::new();
            for (patch, path) in &resolved {
                let identity = identity_path(path);
                if let Some(first) = seen.insert(identity, patch.path) {
                    duplicates.push(if first == patch.path {
                        format!("✗ {}: the patch names the same file more than once", patch.path)
                    } else {
                        format!(
                            "✗ {}: resolves to the same file as {} ({})",
                            patch.path,
                            first,
                            path.display()
                        )
                    });
                }
            }
            if !duplicates.is_empty() {
                let mut body = duplicates.join("\n");
                body.push_str("\n\nNo files changed: the patch failed preflight.");
                return Ok(ToolOutput::new(body).with_verdict(ToolVerdict::Failed));
            }
        }

        // Hold every path for the whole request so preflight and the writes it
        // judges cannot be interleaved by another tool in this process.
        let _locks = super::file_lock::lock_all(resolved.iter().map(|(_, path)| path.clone())).await;
        // Watch config.toml across the whole invocation so an edit that lands
        // on it is reported regardless of which patch produced it.
        let config_watch = super::config_edit_notice::ConfigEditWatch::begin();

        // Preflight every file before the first write. A missing context in the
        // last file must not leave the earlier creations, modifications or
        // deletions already on disk.
        let mut prepared: Vec<PreparedFile<'_>> = Vec::with_capacity(resolved.len());
        let mut failures = Vec::new();
        for (patch, resolved_path) in &resolved {
            match preflight_patch(patch, resolved_path, &ctx).await {
                Ok(action) => prepared.push(PreparedFile {
                    path: patch.path,
                    resolved: resolved_path.clone(),
                    action,
                }),
                Err(error) => failures.push(format!("✗ {}: {error}", patch.path)),
            }
        }
        if !failures.is_empty() {
            let mut body = failures.join("\n");
            body.push_str("\n\nNo files changed: the patch failed preflight.");
            config_watch.finish(&mut body);
            return Ok(ToolOutput::new(body).with_verdict(ToolVerdict::Failed));
        }

        // Below this point a failure can only be external I/O, and files written
        // before it stay written. The journal and the message describe exactly
        // that, rather than pretending the request was atomic.
        let mut journal = super::undo::Journal::begin(&ctx);
        let mut results = Vec::with_capacity(prepared.len());
        let mut written = 0usize;
        let mut failed = false;

        for file in prepared {
            let display_path = file.path;
            match apply_prepared(file, &ctx, &mut journal).await {
                Ok((msg, diff)) => {
                    written += 1;
                    if diff.is_empty() {
                        results.push(format!("✓ {}: {}", display_path, msg));
                    } else {
                        results.push(format!("✓ {}: {}\n{}", display_path, msg, diff));
                    }
                }
                Err(error) => {
                    failed = true;
                    results.push(format!("✗ {}: {}", display_path, error));
                    break;
                }
            }
        }

        drop(journal);
        let mut body = results.join("\n\n");
        if failed {
            if written > 0 {
                body.push_str(&format!(
                    "\n\nPartial patch: {written} file(s) changed before this failure."
                ));
            } else {
                body.push_str("\n\nStopped at the first write failure; later files were not attempted.");
            }
        }
        config_watch.finish(&mut body);
        let verdict = match (failed, written) {
            (false, _) => ToolVerdict::Ok,
            (true, 0) => ToolVerdict::Failed,
            (true, _) => ToolVerdict::Partial,
        };
        Ok(ToolOutput::new(body).with_verdict(verdict))
    }
}

/// A path identity that also works for a file that does not exist yet.
fn identity_path(path: &Path) -> std::path::PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => match parent.canonicalize() {
            Ok(canonical) => canonical.join(name),
            Err(_) => path.to_path_buf(),
        },
        _ => path.to_path_buf(),
    }
}

/// A git-style file header, not a body line whose content begins with `--`.
fn is_old_file_header(line: &str) -> bool {
    line.starts_with("--- ") || line.starts_with("---\t")
}

fn is_new_file_header(line: &str) -> bool {
    line == "+++" || line.starts_with("+++ ") || line.starts_with("+++\t")
}

fn parse_patch(text: &str) -> Result<Vec<FilePatch<'_>>> {
    let mut patches = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        if !is_old_file_header(line) {
            i += 1;
            continue;
        }

        let old_file = line
            .strip_prefix("---")
            .unwrap_or(line)
            .trim()
            .split('\t')
            .next()
            .unwrap_or("");

        i += 1;
        let Some(new_line) = lines.get(i).copied() else {
            anyhow::bail!("file header for '{old_file}' is missing its +++ line");
        };
        if !is_new_file_header(new_line) {
            anyhow::bail!("file header for '{old_file}' is not followed by a +++ line");
        }
        let new_file = new_line
            .strip_prefix("+++")
            .unwrap_or(new_line)
            .trim()
            .split('\t')
            .next()
            .unwrap_or("");
        if old_file.is_empty() || new_file.is_empty() {
            anyhow::bail!("a file header names no path");
        }

        let is_new = old_file == "/dev/null";
        let is_delete = new_file == "/dev/null";
        if is_new && is_delete {
            anyhow::bail!("a file cannot be both created and deleted");
        }
        let path = if is_delete {
            old_file.strip_prefix("a/").unwrap_or(old_file)
        } else {
            new_file.strip_prefix("b/").unwrap_or(new_file)
        };
        if path.is_empty() {
            anyhow::bail!("a file header names no path");
        }

        i += 1;
        let mut hunks = Vec::new();
        while i < lines.len() {
            let current = lines[i];
            if is_old_file_header(current) {
                break;
            }
            if current.starts_with("@@") {
                hunks.push(parse_hunk(&lines, &mut i, path)?);
            } else {
                i += 1;
            }
        }

        if !hunks.is_empty() || is_new || is_delete {
            patches.push(FilePatch {
                path,
                hunks,
                is_new,
                is_delete,
            });
        }
    }

    Ok(patches)
}

fn invalid_hunk_range(token: &str, path: &str, header: &str) -> anyhow::Error {
    anyhow::anyhow!("invalid hunk range '{token}' in {path}: {header}")
}

fn parse_hunk_range(token: &str, sign: char, path: &str, header: &str) -> Result<(usize, usize)> {
    let body = token
        .strip_prefix(sign)
        .ok_or_else(|| invalid_hunk_range(token, path, header))?;
    let (start, count) = match body.split_once(',') {
        Some((start, count)) => (
            start,
            count
                .parse::<usize>()
                .map_err(|_| invalid_hunk_range(token, path, header))?,
        ),
        None => (body, 1),
    };
    let start = start
        .parse::<usize>()
        .map_err(|_| invalid_hunk_range(token, path, header))?;
    Ok((start, count))
}

fn parse_hunk<'a>(lines: &[&'a str], i: &mut usize, path: &str) -> Result<Hunk<'a>> {
    // @@ -start[,count] +start[,count] @@ [section heading]
    let header = lines[*i];
    let rest = header
        .strip_prefix("@@")
        .ok_or_else(|| anyhow::anyhow!("invalid hunk header: {header}"))?;
    let (range_text, _section) = rest
        .split_once("@@")
        .ok_or_else(|| anyhow::anyhow!("hunk header is missing the closing '@@': {header}"))?;
    let mut parts = range_text.split_whitespace();
    let old_token = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("hunk header names no old range: {header}"))?;
    let new_token = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("hunk header names no new range: {header}"))?;
    if parts.next().is_some() {
        anyhow::bail!("unexpected text in hunk header: {header}");
    }

    let (old_start, old_count) = parse_hunk_range(old_token, '-', path, header)?;
    let (new_start, new_count) = parse_hunk_range(new_token, '+', path, header)?;
    if old_start == 0 && old_count > 0 {
        anyhow::bail!("hunk range '{old_token}' in {path} starts at line 0 but declares lines");
    }
    if new_start == 0 && new_count > 0 {
        anyhow::bail!("hunk range '{new_token}' in {path} starts at line 0 but declares lines");
    }
    if old_count == 0 && new_count == 0 {
        anyhow::bail!("hunk '{header}' in {path} changes nothing");
    }

    *i += 1;
    let remaining = lines.len().saturating_sub(*i);
    let mut old_lines = Vec::with_capacity(old_count.min(remaining));
    let mut new_lines = Vec::with_capacity(new_count.min(remaining));
    let mut old_remaining = old_count;
    let mut new_remaining = new_count;
    let mut old_no_newline = false;
    let mut new_no_newline = false;
    let mut last_kind: Option<char> = None;

    while *i < lines.len() && (old_remaining > 0 || new_remaining > 0) {
        let line = lines[*i];

        if line == "\\ No newline at end of file" {
            mark_no_newline(
                last_kind,
                old_remaining,
                new_remaining,
                &mut old_no_newline,
                &mut new_no_newline,
                path,
            )?;
            *i += 1;
            continue;
        }

        if let Some(content) = line.strip_prefix(' ') {
            if old_remaining == 0 || new_remaining == 0 {
                anyhow::bail!("hunk '{header}' in {path} has more lines than it declares");
            }
            old_lines.push(content);
            new_lines.push(content);
            old_remaining -= 1;
            new_remaining -= 1;
            last_kind = Some(' ');
        } else if let Some(content) = line.strip_prefix('-') {
            if old_remaining == 0 {
                anyhow::bail!("hunk '{header}' in {path} removes more lines than it declares");
            }
            old_lines.push(content);
            old_remaining -= 1;
            last_kind = Some('-');
        } else if let Some(content) = line.strip_prefix('+') {
            if new_remaining == 0 {
                anyhow::bail!("hunk '{header}' in {path} adds more lines than it declares");
            }
            new_lines.push(content);
            new_remaining -= 1;
            last_kind = Some('+');
        } else if line.is_empty() {
            // Some generators write an empty context line without the leading space.
            if old_remaining == 0 || new_remaining == 0 {
                anyhow::bail!("hunk '{header}' in {path} has more lines than it declares");
            }
            old_lines.push("");
            new_lines.push("");
            old_remaining -= 1;
            new_remaining -= 1;
            last_kind = Some(' ');
        } else {
            anyhow::bail!("hunk '{header}' in {path} ends before its declared lines are present");
        }

        *i += 1;
    }

    if old_remaining > 0 || new_remaining > 0 {
        anyhow::bail!(
            "hunk '{header}' in {path} declares {old_count} old and {new_count} new line(s) but the patch supplies {} and {}",
            old_lines.len(),
            new_lines.len()
        );
    }

    // The marker can follow the last line of the hunk, after both counts are met.
    while *i < lines.len() && lines[*i] == "\\ No newline at end of file" {
        mark_no_newline(
            last_kind,
            old_remaining,
            new_remaining,
            &mut old_no_newline,
            &mut new_no_newline,
            path,
        )?;
        *i += 1;
    }

    if let Some(next) = lines.get(*i)
        && (next.starts_with('+') || next.starts_with(' ') || (next.starts_with('-') && !is_old_file_header(next)))
        && !is_mail_signature(&lines[*i..])
    {
        anyhow::bail!("hunk '{header}' in {path} has more lines than it declares");
    }

    Ok(Hunk {
        old_start,
        old_count,
        new_start,
        old_lines,
        new_lines,
        old_no_newline,
        new_no_newline,
    })
}

fn is_mail_signature(lines: &[&str]) -> bool {
    let ["-- ", version, trailing @ ..] = lines else {
        return false;
    };
    version.as_bytes().first().is_some_and(u8::is_ascii_digit) && trailing.iter().all(|line| line.is_empty())
}

fn mark_no_newline(
    kind: Option<char>,
    old_remaining: usize,
    new_remaining: usize,
    old_no_newline: &mut bool,
    new_no_newline: &mut bool,
    path: &str,
) -> Result<()> {
    match kind {
        Some('-') if old_remaining == 0 => *old_no_newline = true,
        Some('+') if new_remaining == 0 => *new_no_newline = true,
        Some(' ') if old_remaining == 0 && new_remaining == 0 => {
            *old_no_newline = true;
            *new_no_newline = true;
        }
        Some('-') | Some('+') | Some(' ') => {
            anyhow::bail!("no-newline marker in {path} is only valid on the last line of its side");
        }
        _ => anyhow::bail!("no-newline marker in {path} has no line to describe"),
    }
    Ok(())
}

/// Read and validate one file's operation without writing anything.
async fn preflight_patch(patch: &FilePatch<'_>, path: &Path, ctx: &ToolContext) -> Result<PreparedAction> {
    if patch.is_delete {
        // The bash gate cannot see this route, so an absolute protected path
        // would otherwise be deleted here (#604). Refusing during preflight
        // keeps the rest of the request unapplied too.
        if let Some(refusal) = super::path_policy::catastrophic_delete_refusal(path, patch.path, ctx) {
            let detail = refusal.strip_prefix(&format!("✗ {}: ", patch.path)).unwrap_or(&refusal);
            anyhow::bail!("{detail}");
        }
        if !path.exists() {
            anyhow::bail!("file does not exist");
        }
        let old = tokio::fs::read_to_string(path).await.ok();
        if !patch.hunks.is_empty() {
            let Some(text) = old.as_deref() else {
                anyhow::bail!("cannot verify the delete context: the file is not valid UTF-8 text");
            };
            let remaining = apply_hunks(text, &patch.hunks, patch.path)?;
            if !remaining.is_empty() {
                anyhow::bail!("delete hunk does not remove every line; the file no longer matches the patch");
            }
        }
        return Ok(PreparedAction::Delete { old });
    }

    if patch.is_new {
        // The client may report a buffer that the disk does not have, or the
        // reverse. Either one existing means the create would overwrite text.
        let exists = match super::client_fs::text_snapshot(path, ctx).await {
            Some((client_exists, _)) => client_exists || path.exists(),
            None => path.exists(),
        };
        if exists {
            anyhow::bail!("file already exists");
        }
        return Ok(PreparedAction::Create(new_file_content(&patch.hunks, patch.path)?));
    }

    // The editor buffer is the source of truth when a client serves reads; a
    // patch that only matches the stale disk copy is rejected here.
    let old = match super::client_fs::read_text(path, ctx).await? {
        super::client_fs::TextRead::Client(text) => text,
        super::client_fs::TextRead::NotFound => return Err(anyhow::anyhow!("file does not exist")),
        super::client_fs::TextRead::Local => {
            if !path.exists() {
                return Err(anyhow::anyhow!("file does not exist"));
            }
            tokio::fs::read_to_string(path).await?
        }
    };
    let new = apply_hunks(&old, &patch.hunks, patch.path)?;
    let first_line = patch.hunks.iter().map(|hunk| hunk.old_start.max(1)).min().unwrap_or(1);
    Ok(PreparedAction::Modify {
        old,
        new,
        first_line,
        hunks: patch.hunks.len(),
    })
}

/// Read the current text through the same client/disk route preflight used.
async fn current_text(path: &Path, ctx: &ToolContext) -> Result<Option<(String, bool)>> {
    match super::client_fs::read_text(path, ctx).await? {
        super::client_fs::TextRead::Client(text) => Ok(Some((text, false))),
        super::client_fs::TextRead::NotFound => Ok(None),
        super::client_fs::TextRead::Local => match tokio::fs::read_to_string(path).await {
            Ok(text) => Ok(Some((text, true))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        },
    }
}

#[derive(Clone, Copy, Debug)]
enum LineEnding {
    Lf,
    CrLf,
}

impl LineEnding {
    fn separator(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
        }
    }
}

/// Lines stay borrowed from `content`: the applier only compares and copies
/// them, so owning one `String` per line would buy nothing.
fn split_lines(content: &str) -> (Vec<&str>, bool, LineEnding) {
    if content.is_empty() {
        return (Vec::new(), false, LineEnding::Lf);
    }
    let ends_with_newline = content.ends_with('\n');
    let bytes = content.as_bytes();
    let mut newline_count = 0usize;
    let mut crlf_count = 0usize;
    for (index, &byte) in bytes.iter().enumerate() {
        if byte == b'\n' {
            newline_count += 1;
            if index > 0 && bytes[index - 1] == b'\r' {
                crlf_count += 1;
            }
        }
    }
    // A uniformly CRLF file round-trips through the patch, which stores its
    // body lines without the carriage return. Mixed endings are left as-is and
    // will fail the exact context check rather than being silently converted.
    let crlf = newline_count > 0 && crlf_count == newline_count;
    let ending = if crlf { LineEnding::CrLf } else { LineEnding::Lf };
    let mut lines: Vec<&str> = Vec::with_capacity(newline_count + 1);
    if crlf {
        lines.extend(content.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line)));
    } else {
        lines.extend(content.split('\n'));
    }
    if ends_with_newline {
        lines.pop();
    }
    (lines, ends_with_newline, ending)
}

/// Append `lines` to `text`, separating them from `written` earlier lines.
fn push_lines(text: &mut String, lines: &[&str], separator: &str, mut written: usize) -> usize {
    for line in lines {
        if written > 0 {
            text.push_str(separator);
        }
        text.push_str(line);
        written += 1;
    }
    written
}

/// Rebuild a file from its exact old text and the hunks that describe it.
fn apply_hunks(old: &str, hunks: &[Hunk<'_>], path: &str) -> Result<String> {
    let (lines, old_ends_with_newline, ending) = split_lines(old);
    let separator = ending.separator();
    let mut text = String::with_capacity(old.len());
    let mut produced = 0usize;
    let mut cursor = 0usize;

    for (index, hunk) in hunks.iter().enumerate() {
        // For a range with no old lines, `old_start` is the line the insertion
        // follows, so the zero-based insertion index is `old_start` itself.
        let position = if hunk.old_count == 0 {
            hunk.old_start
        } else {
            hunk.old_start.saturating_sub(1)
        };
        if position < cursor {
            anyhow::bail!(
                "hunks are out of order or overlap in {path}: a hunk at line {} starts before the earlier hunk ends",
                hunk.old_start
            );
        }
        let end = position
            .checked_add(hunk.old_count)
            .ok_or_else(|| anyhow::anyhow!("hunk at line {} in {path} overflows the file position", hunk.old_start))?;
        if end > lines.len() {
            anyhow::bail!(
                "hunk at line {} in {path} reaches past the end of the file ({} lines)",
                hunk.old_start,
                lines.len()
            );
        }
        if (hunk.old_no_newline || hunk.new_no_newline) && index + 1 != hunks.len() {
            anyhow::bail!("no-newline marker in {path} is only valid on the last hunk");
        }
        if hunk.old_no_newline {
            if end != lines.len() {
                anyhow::bail!("no-newline marker in {path} names an old line that is not at the end of the file");
            }
            if old_ends_with_newline {
                anyhow::bail!(
                    "no-newline marker in {path} claims the original ends without a newline, but it ends with one"
                );
            }
        }
        if hunk.new_no_newline && end != lines.len() {
            anyhow::bail!("no-newline marker in {path} would change the ending of lines after the hunk");
        }
        if lines.get(position..end) != Some(hunk.old_lines.as_slice()) {
            anyhow::bail!(
                "hunk at line {} in {path}: the context does not match the current file",
                hunk.old_start
            );
        }
        let produced_before = produced + (position - cursor);
        if hunk.new_lines.is_empty() {
            if hunk.new_start != produced_before {
                anyhow::bail!(
                    "hunk at line {} in {path}: the declared new range does not match the patch position",
                    hunk.old_start
                );
            }
        } else if hunk.new_start != produced_before + 1 {
            anyhow::bail!(
                "hunk at line {} in {path}: the declared new range does not match the patch position",
                hunk.old_start
            );
        }
        produced = push_lines(&mut text, &lines[cursor..position], separator, produced);
        produced = push_lines(&mut text, &hunk.new_lines, separator, produced);
        cursor = end;
    }
    produced = push_lines(&mut text, &lines[cursor..], separator, produced);

    let trailing_newline = match hunks.last() {
        Some(last) if last.new_no_newline => false,
        Some(last) if last.old_no_newline => true,
        _ => old_ends_with_newline || old.is_empty(),
    };
    if trailing_newline && produced > 0 {
        text.push_str(separator);
    }
    Ok(text)
}

fn new_file_content(hunks: &[Hunk<'_>], path: &str) -> Result<String> {
    let mut text = String::new();
    let mut produced = 0usize;
    for (index, hunk) in hunks.iter().enumerate() {
        if hunk.old_count != 0 || !hunk.old_lines.is_empty() {
            anyhow::bail!("new-file hunk in {path} must not remove lines");
        }
        if hunk.old_start != 0 {
            anyhow::bail!("new-file hunk in {path} must start at line 0");
        }
        if hunk.new_no_newline && index + 1 != hunks.len() {
            anyhow::bail!("no-newline marker in {path} is only valid on the last hunk");
        }
        if hunk.new_start != produced + 1 {
            anyhow::bail!("new-file hunk in {path} declares a new range that does not match its position");
        }
        produced = push_lines(&mut text, &hunk.new_lines, "\n", produced);
    }
    if produced > 0 && !hunks.last().is_some_and(|hunk| hunk.new_no_newline) {
        text.push('\n');
    }
    Ok(text)
}

/// Write one preflighted operation and return (status_message, diff_output).
async fn apply_prepared(
    file: PreparedFile<'_>,
    ctx: &ToolContext,
    journal: &mut super::undo::Journal<'_>,
) -> Result<(String, String)> {
    match file.action {
        PreparedAction::Create(content) => {
            let exists = match super::client_fs::text_snapshot(&file.resolved, ctx).await {
                Some((client_exists, _)) => client_exists || file.resolved.exists(),
                None => file.resolved.exists(),
            };
            if exists {
                anyhow::bail!("file appeared after preflight; refusing to overwrite it");
            }
            let wrote_to_client = super::client_fs::write_text(&file.resolved, &content, ctx).await?;
            if !wrote_to_client {
                if let Some(parent) = file.resolved.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                // `create_new` closes the gap between the check above and the
                // write: a file created by another process in between is an
                // error instead of an overwrite.
                let write_result = {
                    let mut handle = match tokio::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&file.resolved)
                        .await
                    {
                        Ok(handle) => handle,
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                            anyhow::bail!("file appeared after preflight; refusing to overwrite it");
                        }
                        Err(error) => return Err(error.into()),
                    };
                    match handle.write_all(content.as_bytes()).await {
                        Ok(()) => handle.flush().await,
                        Err(error) => Err(error),
                    }
                };
                if let Err(error) = write_result {
                    if let Err(cleanup) = tokio::fs::remove_file(&file.resolved).await {
                        crate::logging::warn(&format!(
                            "apply_patch: cannot remove a partially created {}: {cleanup}",
                            file.resolved.display()
                        ));
                    }
                    return Err(error.into());
                }
            }
            super::edit_stats::record(ctx, "", &content, false).await;
            let diff = generate_diff("", &content, 1);
            if !wrote_to_client {
                journal.wrote(super::undo::PendingFile {
                    path: file.resolved,
                    before: super::undo::ABSENT,
                    after: horon_agent_edit_safety::FileContent::Bytes(content.into_bytes()),
                });
            }
            Ok(("created".to_string(), diff))
        }
        PreparedAction::Delete { old } => {
            let before = if let Some(expected) = &old {
                // A delete acts on the disk file, so the freshness check reads
                // the same target rather than the editor buffer.
                let current = tokio::fs::read_to_string(&file.resolved).await.ok();
                if current.as_deref() != Some(expected.as_str()) {
                    anyhow::bail!("file changed after preflight; refusing to delete it");
                }
                match current {
                    Some(text) => horon_agent_edit_safety::FileContent::Bytes(text.into_bytes()),
                    None => super::undo::capture(&file.resolved).await,
                }
            } else {
                super::undo::capture(&file.resolved).await
            };
            let old_content = old.as_deref().unwrap_or("");
            tokio::fs::remove_file(&file.resolved).await?;
            journal.wrote(super::undo::PendingFile {
                path: file.resolved,
                before,
                after: super::undo::ABSENT,
            });
            super::edit_stats::record(ctx, old_content, "", old.is_none()).await;
            Ok(("deleted".to_string(), generate_diff(old_content, "", 1)))
        }
        PreparedAction::Modify {
            old,
            new,
            first_line,
            hunks,
        } => {
            let current = current_text(&file.resolved, ctx).await?;
            let Some((current_content, from_local_disk)) = current else {
                anyhow::bail!("file changed after preflight; refusing to overwrite it");
            };
            if current_content != old {
                anyhow::bail!("file changed after preflight; refusing to overwrite it");
            }
            let wrote_to_client = super::client_fs::write_text(&file.resolved, &new, ctx).await?;
            let before = if !wrote_to_client {
                let before = if from_local_disk {
                    horon_agent_edit_safety::FileContent::Bytes(current_content.into_bytes())
                } else {
                    super::undo::capture(&file.resolved).await
                };
                tokio::fs::write(&file.resolved, &new).await?;
                Some(before)
            } else {
                None
            };
            super::edit_stats::record(ctx, &old, &new, false).await;
            let diff = generate_diff(&old, &new, first_line);
            if let Some(before) = before {
                journal.wrote(super::undo::PendingFile {
                    path: file.resolved,
                    before,
                    after: horon_agent_edit_safety::FileContent::Bytes(new.into_bytes()),
                });
            }
            Ok((format!("modified ({hunks} hunks)"), diff))
        }
    }
}

/// Generate a compact diff with line numbers (max 30 lines)
fn generate_diff(old: &str, new: &str, start_line: usize) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut output = String::new();
    let mut line_count = 0;
    const MAX_LINES: usize = 30;

    let mut old_line = start_line;
    let mut new_line = start_line;

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
