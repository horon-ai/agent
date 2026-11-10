#![cfg_attr(test, allow(clippy::items_after_test_module))]
// Env-var tests serialize on a std mutex that must outlive the awaited tool call,
// which is safe under the single-threaded test runtime and is the same allowance
// `tool/goal.rs` takes for the identical pattern.
#![cfg_attr(test, allow(clippy::await_holding_lock))]

use super::read_selector::{
    DEFAULT_LIMIT, Rendered, Selector, push_numbered_line, render_range, render_with_selector, split_path_and_selector,
};
#[cfg(test)]
use super::read_selector::{MAX_LINE_LEN, TRUNCATED_TAG};
use super::read_state::{self, READ_SNAPSHOT_MAX_BYTES, READ_SNAPSHOT_TOOL, ReadRecord};
use super::{Tool, ToolContext, ToolEffects, ToolOutput, artifacts, nested_agents};
use crate::bus::{Bus, BusEvent, FileOp, FileTouch};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub struct ReadTool;

impl ReadTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct ReadInput {
    file_path: String,
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    hashline: bool,
    /// Print the requested lines even when an earlier read already returned
    /// them unchanged.
    #[serde(default)]
    full: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadRangeStyle {
    OffsetLimit,
    StartEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NormalizedReadRange {
    offset: usize,
    limit: usize,
    style: ReadRangeStyle,
}

fn normalize_read_range(params: &ReadInput) -> Result<NormalizedReadRange> {
    let has_start_end = params.start_line.is_some() || params.end_line.is_some();
    let has_mixed_offset = match (params.start_line, params.end_line, params.offset) {
        (Some(start_line), _, Some(offset)) => {
            if start_line == 0 {
                true
            } else {
                offset.checked_add(1) != Some(start_line)
            }
        }
        (None, Some(_), Some(offset)) => offset != 0,
        _ => params.offset.is_some(),
    };

    if has_start_end && has_mixed_offset {
        return Err(anyhow::anyhow!(
            "Use either start_line/end_line (1-based) or offset (0-based), not both. `limit` may be used with either style."
        ));
    }

    if has_start_end {
        let start_line = params.start_line.unwrap_or(1);
        if start_line == 0 {
            return Err(anyhow::anyhow!("start_line must be 1 or greater (it is 1-based)."));
        }

        let limit = if let Some(end_line) = params.end_line {
            if end_line == 0 {
                return Err(anyhow::anyhow!("end_line must be 1 or greater (it is 1-based)."));
            }
            if end_line < start_line {
                return Err(anyhow::anyhow!(
                    "end_line ({}) must be greater than or equal to start_line ({}).",
                    end_line,
                    start_line
                ));
            }
            end_line - start_line + 1
        } else {
            params.limit.unwrap_or(DEFAULT_LIMIT)
        };

        return Ok(NormalizedReadRange {
            offset: start_line - 1,
            limit,
            style: ReadRangeStyle::StartEnd,
        });
    }

    Ok(NormalizedReadRange {
        offset: params.offset.unwrap_or(0),
        limit: params.limit.unwrap_or(DEFAULT_LIMIT),
        style: ReadRangeStyle::OffsetLimit,
    })
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        // Always-on prompt cost: the selector syntax and the `agent://` shapes
        // live in the `file_path` parameter description and the tool's docs.
        "Read a file (text, image, PDF), an archive member, a SQLite table, or `agent://` child result."
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::read()
    }

    fn effects_for_input(&self, input: &Value) -> ToolEffects {
        let remote = input
            .get("file_path")
            .and_then(Value::as_str)
            .is_some_and(is_remote_path);
        if remote {
            ToolEffects::read().union(ToolEffects::network())
        } else {
            ToolEffects::read()
        }
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["file_path"],
            "properties": {
                "intent": super::intent_schema_property(),
                "file_path": {
                    "type": "string",
                    "description": "Path to a file; a line selector like `path:10-40` reads part. An archive lists members, and `bundle.zip:dir/a.txt` reads one; a `.db` lists tables, and `app.db:users?limit=20&offset=40` reads rows. Also artifact:// ids and memory:// references."
                },
                "start_line": {
                    "type": "integer",
                    "description": "1-based start line for text files."
                },
                "limit": {
                    "type": "integer",
                    "description": "Max text lines to read. Default 5000."
                },
                "hashline": {
                    "type": "boolean",
                    "description": "Prefix lines with an edit anchor: `{line:>5}#{tag}:{text}`. Default false."
                },
                "full": {
                    "type": "boolean",
                    "description": "Print the requested lines even when they are unchanged since an earlier read in this session. Default false."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        super::path_policy::enforce("read", &input, &ctx)?;
        let params: ReadInput = serde_json::from_value(input)?;

        if crate::agent_result_cache::is_agent_url(&params.file_path) {
            return read_agent_url(&params, &ctx);
        }
        if starts_with_ignore_ascii_case(&params.file_path, "artifact://") {
            return read_artifact_url(&params, &ctx).await;
        }
        if starts_with_ignore_ascii_case(&params.file_path, read_skill::SCHEME) {
            return read_skill::read_skill_url(&params, &ctx).await;
        }

        if is_history_url(&params.file_path) {
            return read_history_url(&params, &ctx);
        }
        if read_memory_url::is_memory_url(&params.file_path) {
            return read_memory_url::read_memory_url(&params, &ctx);
        }

        let cwd = ctx
            .working_dir
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        // An archive or a SQLite database is a container, not text: it is listed
        // and then read through a member or table selector, so it is dispatched
        // before the plain-file path would report it as binary.
        if read_archive::resolve(&params.file_path, &cwd)?.is_some() {
            return read_archive::read_archive(&params, &ctx, &cwd);
        }
        if read_sqlite::resolve(&params.file_path, &cwd).is_some() {
            return read_sqlite::read_sqlite(&params, &ctx, &cwd);
        }
        let (authored_path, selector) = split_path_and_selector(&params.file_path, &cwd)?;

        let explicit_range = params.start_line.is_some()
            || params.end_line.is_some()
            || params.offset.is_some()
            || params.limit.is_some();
        if !matches!(selector, Selector::None) && explicit_range {
            return Err(anyhow::anyhow!(
                "Use either a path selector (path:N-M) or start_line/end_line/offset/limit, not both."
            ));
        }
        let range = if matches!(selector, Selector::None) {
            Some(normalize_read_range(&params)?)
        } else {
            None
        };

        let mut path = ctx.resolve_path(Path::new(&authored_path));
        let mut suffix_note: Option<String> = None;
        if !path.exists()
            && !authored_path.is_empty()
            && !Path::new(&authored_path).is_absolute()
            && !authored_path.contains("://")
        {
            let lookup_cwd = cwd.clone();
            let needle = authored_path.clone();
            let found = tokio::task::spawn_blocking(move || {
                super::path_suffix::unique_suffix_match(&lookup_cwd, &needle, std::time::Duration::from_secs(5))
            })
            .await
            .unwrap_or(None);
            if let Some(found) = found {
                let relative = super::path_suffix::relative_display(&cwd, &found);
                suffix_note = Some(format!(
                    "[Path '{authored_path}' not found; resolved to '{relative}' via suffix match]\n"
                ));
                path = found;
            }
        }

        // The deny policy saw the raw argument, which may still carry a selector
        // (`secrets.env:1-5`) or a suffix that resolves elsewhere. Check the file
        // that will actually be opened so neither form slips past a deny rule.
        if path != ctx.resolve_path(Path::new(&params.file_path)) {
            super::path_policy::enforce("read", &json!({ "file_path": path.to_string_lossy() }), &ctx)?;
        }

        // An image, PDF or binary path needs bytes, vision encoding or PDF
        // extraction, so it stays on local disk even when the client serves text.
        let local_only = is_image_file(&path) || is_pdf_file(&path) || has_binary_extension(&path);
        let served = if local_only {
            super::client_fs::TextRead::Local
        } else {
            super::client_fs::read_text(&path, &ctx).await?
        };

        if matches!(served, super::client_fs::TextRead::NotFound) {
            return Err(file_not_found(&params.file_path, &path));
        }

        let content = match served {
            super::client_fs::TextRead::Client(text) => text,
            super::client_fs::TextRead::Local | super::client_fs::TextRead::NotFound => {
                // Check if file exists
                if !path.exists() {
                    return Err(file_not_found(&params.file_path, &path));
                }

                // Check for image files and display in terminal if supported
                if is_image_file(&path) {
                    return handle_image_file(&path, &params.file_path);
                }

                // Check for PDF files and extract text
                if is_pdf_file(&path) {
                    return handle_pdf_file(&path, &params.file_path, &ctx.session_id);
                }

                // Check for binary files
                if is_binary_file(&path) {
                    return Ok(ToolOutput::new(format!(
                        "Binary file detected: {}\nUse appropriate tools to handle binary files.",
                        params.file_path
                    )));
                }

                tokio::fs::read_to_string(&path).await?
            }
        };

        // Earlier output of this file still in context: skip unchanged lines,
        // send a diff for a changed whole file. Recorded even under `full`,
        // since that output is in context too. Only the start_line/offset form
        // is tracked; a selector read is rendered as asked.
        let dedup = crate::config::config().tools.read_dedup && range.is_some();
        let read_key = dedup.then(|| read_state::key_for(&path));
        let content_hash: Option<[u8; 32]> = dedup.then(|| Sha256::digest(content.as_bytes()).into());
        let previous = read_key
            .as_ref()
            .and_then(|key| read_state::lookup(&ctx.session_id, key));
        if !params.full
            && let (Some(range), Some(key), Some(hash), Some(prev)) =
                (range, read_key.as_ref(), content_hash, previous.as_ref())
            && let Some(repeated) = repeated_read_response(&params, range, &content, hash, prev, key, &ctx)
        {
            publish_read_touch(&ctx, &path, repeated.summary);
            return Ok(ToolOutput::new(repeated.output));
        }

        let summary = if matches!(selector, Selector::None) && !explicit_range && !params.hashline {
            structural_summary(&content, &authored_path, &path)
        } else {
            None
        };
        // A structural outline elides bodies, so those lines never reached the
        // model and must not count as shown.
        let summarized = summary.is_some();
        let rendered = match summary {
            Some(rendered) => rendered,
            None => match range {
                Some(range) => render_range(&content, range.offset, range.limit, params.hashline),
                None => render_with_selector(&content, &selector, params.hashline)?,
            },
        };
        let mut output = rendered.body;

        if !summarized && let (Some(range), Some(key), Some(hash)) = (range, read_key, content_hash) {
            remember_read(
                &ctx,
                key,
                hash,
                &content,
                (range.offset, rendered.last_line),
                rendered.total_lines,
                params.hashline,
                previous,
            );
        }

        // Publish file touch event for swarm coordination
        publish_read_touch(
            &ctx,
            &path,
            format!(
                "read lines {}-{} of {}",
                range.map_or_else(|| 1, |range| range.offset + 1),
                rendered.last_line,
                rendered.total_lines
            ),
        );

        if rendered.truncated_lines > 0 || rendered.last_line < rendered.total_lines {
            crate::logging::warn(&format!(
                "[tool:read] returned truncated output for {} in session {} (tool_call={} range={}..{} total_lines={} truncated_lines={})",
                authored_path,
                ctx.session_id,
                ctx.tool_call_id,
                range.map_or_else(|| 1, |range| range.offset + 1),
                rendered.last_line,
                rendered.total_lines,
                rendered.truncated_lines
            ));
        }

        // Add metadata
        if rendered.more {
            match range {
                Some(range) => {
                    let continuation_hint = match range.style {
                        ReadRangeStyle::OffsetLimit => format!("offset={}", rendered.last_line),
                        ReadRangeStyle::StartEnd => format!("start_line={}", rendered.last_line + 1),
                    };
                    output.push_str(&format!(
                        "\n... {} more lines (use {} to continue)\n",
                        rendered.total_lines - rendered.last_line,
                        continuation_hint
                    ));
                }
                None => {
                    output.push_str(&format!(
                        "\n\n[{} more lines in file. Use :{} to continue]",
                        rendered.total_lines - rendered.last_line,
                        rendered.last_line + 1
                    ));
                }
            }
        }

        // Emptiness is read off the listing alone: an empty file still gets the
        // contract, and appending it first would otherwise erase the marker that
        // tells the model the file is empty. Oversized listings skip the
        // injection inside `inject_for_read`.
        let base_is_empty = output.is_empty();
        let injected = nested_agents::inject_for_read(&path, output.len(), &ctx);
        // Rulebook-mode rules whose globs cover this file ride along with the
        // read that first touches it, so they enter the session once and only
        // when they are relevant.
        let rule_context = nested_agents::inject_rules_for_path(&path, &ctx);

        let mut final_output = suffix_note.unwrap_or_default();
        if base_is_empty {
            final_output.push_str("(empty file)");
        } else {
            final_output.push_str(&output);
        }
        if let Some(extra) = injected {
            final_output.push_str(&extra);
        }
        if let Some(extra) = rule_context {
            final_output.push_str(&extra);
        }
        Ok(ToolOutput::new(final_output))
    }
}

/// Lines of a body that are kept before the structural summary takes over.
const SUMMARY_MIN_LINES: usize = 400;
/// Largest file the structural summary attempts to parse.
const SUMMARY_MAX_BYTES: usize = 2 * 1024 * 1024;
/// Body length below which a top-level item is kept whole.
const SUMMARY_MIN_BODY_LINES: usize = 4;

/// Renders a whole-file read as a structural outline when the feature is on.
///
/// The read still lists real line numbers, so a follow-up selector can name a
/// span from this output directly; only long bodies become one elided marker.
fn structural_summary(content: &str, display: &str, path: &Path) -> Option<Rendered> {
    if !crate::config::config().tools.read_summary || content.len() > SUMMARY_MAX_BYTES {
        return None;
    }
    let total_lines = content.lines().count();
    if total_lines <= SUMMARY_MIN_LINES {
        return None;
    }
    let lang = horon_agent_ast::Lang::from_path(path)?;
    let segments = horon_agent_ast::summarize(content, lang, SUMMARY_MIN_BODY_LINES)?;
    let lines: Vec<&str> = content.lines().collect();
    let mut body = String::new();
    let mut elided: Vec<(usize, usize)> = Vec::new();
    for segment in &segments {
        if segment.kept {
            for line_no in segment.start_line..=segment.end_line.min(total_lines) {
                if let Some(line) = lines.get(line_no - 1) {
                    push_numbered_line(&mut body, line_no, line, false);
                }
            }
        } else {
            body.push_str(&format!(
                "     … lines {}-{} elided\n",
                segment.start_line, segment.end_line
            ));
            elided.push((segment.start_line, segment.end_line));
        }
    }
    let elided_lines: usize = elided.iter().map(|(start, end)| end - start + 1).sum();
    let example = elided
        .iter()
        .take(2)
        .map(|(start, end)| format!("{display}:{start}-{end}"))
        .collect::<Vec<_>>()
        .join(",");
    body.push_str(&format!(
        "\n\n[{elided_lines} lines elided. Read ranges with path:A-B, e.g. {example}]"
    ));
    Some(Rendered {
        body,
        total_lines,
        last_line: total_lines,
        truncated_lines: 0,
        more: false,
    })
}

fn publish_read_touch(ctx: &ToolContext, path: &Path, summary: String) {
    Bus::global().publish(BusEvent::FileTouch(FileTouch {
        session_id: ctx.session_id.clone(),
        path: path.to_path_buf(),
        op: FileOp::Read,
        intent: None,
        summary: Some(summary),
        detail: None,
    }));
}

/// A repeated read answered without re-sending the file.
struct RepeatedRead {
    output: String,
    summary: String,
}

/// The short answer to a read whose content the model already has: a notice
/// when every requested line is unchanged and was shown before, or a unified
/// diff when the whole file changed since a whole-file read. `None` means
/// render normally.
fn repeated_read_response(
    params: &ReadInput,
    range: NormalizedReadRange,
    content: &str,
    hash: [u8; 32],
    prev: &ReadRecord,
    key: &Path,
    ctx: &ToolContext,
) -> Option<RepeatedRead> {
    let total_lines = content.lines().count();
    if prev.hash == hash {
        let end = range.offset.saturating_add(range.limit).min(total_lines);
        if prev.hashline != params.hashline || !prev.covers(range.offset, end) {
            return None;
        }
        let avoided: usize = content
            .lines()
            .skip(range.offset)
            .take(end - range.offset)
            .map(|line| line.len() + 1)
            .sum();
        horon_agent_perf_counters::record_read_unchanged(avoided as u64);
        return Some(RepeatedRead {
            output: format!(
                "[read: {} lines {}-{} are unchanged since your earlier read (tool call {}); that output is still in your context. Pass full=true to print them again.]",
                params.file_path,
                range.offset + 1,
                end,
                prev.tool_call_id
            ),
            summary: format!("read lines {}-{} of {} (unchanged)", range.offset + 1, end, total_lines),
        });
    }

    let snapshot_id = prev.snapshot_id?;
    let whole_file = range.offset == 0 && range.limit >= total_lines;
    if !whole_file || params.hashline || content.len() > READ_SNAPSHOT_MAX_BYTES {
        return None;
    }
    let old = artifacts::load(&ctx.session_id, snapshot_id).ok()?;
    let diff = similar::TextDiff::from_lines(old.as_str(), content)
        .unified_diff()
        .context_radius(3)
        .header("previous read", "current")
        .to_string();
    if diff.len() > content.len() * 2 / 5 {
        return None;
    }
    read_state::record(
        &ctx.session_id,
        key.to_path_buf(),
        ReadRecord {
            hash,
            ranges: vec![(0, total_lines)],
            hashline: false,
            total_lines,
            tool_call_id: ctx.tool_call_id.clone(),
            snapshot_id: artifacts::store(&ctx.session_id, READ_SNAPSHOT_TOOL, content).ok(),
        },
    );
    horon_agent_perf_counters::record_read_delta(content.len().saturating_sub(diff.len()) as u64);
    Some(RepeatedRead {
        output: format!(
            "[read: {} changed since your earlier read (tool call {}); unified diff against that read, line numbers refer to the current file. Pass full=true for the whole file.]\n{}",
            params.file_path, prev.tool_call_id, diff
        ),
        summary: format!("read diff of {} lines", total_lines),
    })
}

/// Record what this read emitted. Same content and format extend the shown
/// ranges; anything else starts over from this read.
#[expect(
    clippy::too_many_arguments,
    reason = "one call site; the read's facts are passed as-is"
)]
fn remember_read(
    ctx: &ToolContext,
    key: std::path::PathBuf,
    hash: [u8; 32],
    content: &str,
    (start, end): (usize, usize),
    total_lines: usize,
    hashline: bool,
    previous: Option<ReadRecord>,
) {
    if start >= end {
        return;
    }
    let mut record = match previous {
        Some(prev) if prev.hash == hash && prev.hashline == hashline => prev,
        Some(prev) if prev.hash == hash => ReadRecord {
            ranges: Vec::new(),
            ..prev
        },
        _ => ReadRecord {
            hash,
            ranges: Vec::new(),
            hashline,
            total_lines,
            tool_call_id: String::new(),
            snapshot_id: None,
        },
    };
    record.hashline = hashline;
    record.total_lines = total_lines;
    record.tool_call_id = ctx.tool_call_id.clone();
    record.merge_range(start, end);
    if record.snapshot_id.is_none() && start == 0 && end >= total_lines && content.len() <= READ_SNAPSHOT_MAX_BYTES {
        record.snapshot_id = artifacts::store(&ctx.session_id, READ_SNAPSHOT_TOOL, content).ok();
    }
    read_state::record(&ctx.session_id, key, record);
}

#[cfg(test)]
mod agent_url_tests;
#[path = "read_archive.rs"]
mod read_archive;
#[cfg(test)]
mod history_tests;
#[path = "read_memory_url.rs"]
mod read_memory_url;
#[path = "read_skill.rs"]
mod read_skill;
#[path = "read_sqlite.rs"]
mod read_sqlite;
#[cfg(test)]
mod tests;

/// A path that leaves the filesystem: an `agent://` reference or any URL scheme.
///
/// `artifact://`, `history://` and `skill://` name local data, so they are not
/// network effects even though they look like URLs.
fn is_remote_path(path: &str) -> bool {
    if starts_with_ignore_ascii_case(path, "artifact://")
        || starts_with_ignore_ascii_case(path, "history://")
        || starts_with_ignore_ascii_case(path, read_skill::SCHEME)
        || read_memory_url::is_memory_url(path)
    {
        return false;
    }
    crate::agent_result_cache::is_agent_url(path) || path.contains("://")
}

fn starts_with_ignore_ascii_case(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

/// Read a session artifact through the same selector grammar as a file read.
async fn read_artifact_url(params: &ReadInput, ctx: &ToolContext) -> Result<ToolOutput> {
    let (uri, selector) = super::read_selector::split_uri_and_selector(&params.file_path)?;
    let rest = uri
        .find("://")
        .map(|index| &uri[index + 3..])
        .unwrap_or_default()
        .trim();
    if rest.is_empty() || !rest.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(anyhow::anyhow!("artifact:// ID must be numeric, got: {rest}"));
    }
    let id: u64 = rest
        .parse()
        .map_err(|_| anyhow::anyhow!("artifact:// ID must be numeric, got: {rest}"))?;
    let path = crate::artifacts::path(&ctx.session_id, id)?;
    let content = tokio::fs::read_to_string(&path).await?;
    let output = page_text(params, &selector, &content)?;
    Ok(ToolOutput::new(output).with_title(params.file_path.clone()))
}

/// Page `content` with the path selector or the offset/limit of `params`,
/// exactly as a file read pages it, continuation hint included.
fn page_text(params: &ReadInput, selector: &Selector, content: &str) -> Result<String> {
    let explicit_range =
        params.start_line.is_some() || params.end_line.is_some() || params.offset.is_some() || params.limit.is_some();
    if !matches!(selector, Selector::None) && explicit_range {
        return Err(anyhow::anyhow!(
            "Use either a path selector (path:N-M) or start_line/end_line/offset/limit, not both."
        ));
    }

    let (rendered, range) = if matches!(selector, Selector::None) {
        let range = normalize_read_range(params)?;
        (render_range(content, range.offset, range.limit, false), Some(range))
    } else {
        (render_with_selector(content, selector, false)?, None)
    };

    let mut output = rendered.body;
    if rendered.more {
        match range {
            Some(range) => {
                let continuation_hint = match range.style {
                    ReadRangeStyle::OffsetLimit => format!("offset={}", rendered.last_line),
                    ReadRangeStyle::StartEnd => format!("start_line={}", rendered.last_line + 1),
                };
                output.push_str(&format!(
                    "\n... {} more lines (use {} to continue)\n",
                    rendered.total_lines - rendered.last_line,
                    continuation_hint
                ));
            }
            None => {
                output.push_str(&format!(
                    "\n\n[{} more lines in file. Use :{} to continue]",
                    rendered.total_lines - rendered.last_line,
                    rendered.last_line + 1
                ));
            }
        }
    }
    Ok(output)
}

/// Whether a path names the stored-transcript reader (`history://...`).
fn is_history_url(path: &str) -> bool {
    path.get(.."history://".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("history://"))
}

/// Renders this session's stored transcript, then pages it like a file.
///
/// The transcript is the durable history, not the provider-bound view: a
/// compacted-away message is still here. Selectors and offset/limit apply to
/// the rendered text exactly as they do to a file read, so `history://current:1-5`
/// is a bounded peek rather than a whole-transcript dump.
///
/// `history://<session-id>` reads a session in this session's spawn tree: the
/// same boundary a consult fork uses, so a coordinator can re-read a child's
/// transcript after it finished without being able to browse anyone else's.
fn read_history_url(params: &ReadInput, ctx: &ToolContext) -> Result<ToolOutput> {
    if !crate::config::config().tools.context_notes {
        return Err(anyhow::anyhow!(
            "history:// is available when [tools] context_notes is on."
        ));
    }
    let (target, selector) = super::read_selector::split_uri_and_selector(&params.file_path)?;
    // The scheme is matched case-insensitively, exactly as the dispatch that
    // routed here did.
    let scheme = "history://";
    let requested = (target.len() >= scheme.len() && target.is_char_boundary(scheme.len()))
        .then(|| target.get(..scheme.len()))
        .flatten()
        .filter(|prefix| prefix.eq_ignore_ascii_case(scheme))
        .map(|_| &target[scheme.len()..])
        .ok_or_else(|| {
            anyhow::anyhow!("Only history://current and history://<session-id> are supported, got: {target}")
        })?;
    let session_id = if requested.eq_ignore_ascii_case("current") {
        ctx.session_id.clone()
    } else {
        requested.to_string()
    };

    let explicit_range =
        params.start_line.is_some() || params.end_line.is_some() || params.offset.is_some() || params.limit.is_some();
    if !matches!(selector, Selector::None) && explicit_range {
        return Err(anyhow::anyhow!(
            "Use either a path selector (path:N-M) or start_line/end_line/offset/limit, not both."
        ));
    }

    let unavailable = || anyhow::anyhow!("No stored history for this session yet.");
    let session = crate::session::Session::load(&session_id).map_err(|_| unavailable())?;
    // One rule decides who may read another session's records: the same spawn
    // tree boundary a consult fork uses, applied here to stored transcripts.
    if session_id != ctx.session_id {
        crate::session_scope::ensure_visible(&ctx.session_id, &session)?;
    }
    let stored = session.history_messages().map_err(|_| unavailable())?;
    if stored.is_empty() {
        return Err(unavailable());
    }

    let text = render_stored_history(&stored);
    let (mut output, more, last_line) = if matches!(selector, Selector::None) {
        let range = normalize_read_range(params)?;
        let rendered = render_range(&text, range.offset, range.limit, false);
        let more = rendered.more.then(|| {
            let hint = match range.style {
                ReadRangeStyle::OffsetLimit => format!("offset={}", rendered.last_line),
                ReadRangeStyle::StartEnd => format!("start_line={}", rendered.last_line + 1),
            };
            format!(
                "\n... {} more lines (use {} to continue)\n",
                rendered.total_lines - rendered.last_line,
                hint
            )
        });
        (rendered.body, more, rendered.last_line)
    } else {
        let rendered = render_with_selector(&text, &selector, false)?;
        let more = rendered.more.then(|| {
            format!(
                "\n\n[{} more lines in file. Use :{} to continue]",
                rendered.total_lines - rendered.last_line,
                rendered.last_line + 1
            )
        });
        (rendered.body, more, rendered.last_line)
    };
    if let Some(more) = more {
        output.push_str(&more);
    }
    if output.is_empty() {
        output.push_str(&format!("(no messages at or after line {last_line})"));
    }

    Ok(ToolOutput::new(output).with_title(if session_id == ctx.session_id {
        "history://current".to_string()
    } else {
        format!("history://{session_id}")
    }))
}

/// Plain-text rendering of the stored transcript.
///
/// Each message becomes `## {role} #{index}`, its text blocks verbatim, tool
/// calls as `→ {name} {compact json input}`, and tool results as
/// `← {first 2000 chars}`. Reasoning and provider-native blocks are dropped:
/// they are not something a model can act on when re-reading its own history.
fn render_stored_history(messages: &[crate::session::StoredMessage]) -> String {
    let mut out = String::new();
    for (index, message) in messages.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let role = match message.role {
            crate::message::Role::User => "user",
            crate::message::Role::Assistant => "assistant",
        };
        out.push_str(&format!("## {role} #{}\n", index + 1));
        for block in &message.content {
            match block {
                crate::message::ContentBlock::Text { text, .. } => {
                    out.push_str(text);
                    out.push('\n');
                }
                crate::message::ContentBlock::ToolUse { name, input, .. } => {
                    let input = serde_json::to_string(input).unwrap_or_default();
                    out.push_str(&format!("→ {name} {input}\n"));
                }
                crate::message::ContentBlock::ToolResult { content, .. } => {
                    let preview: String = content.chars().take(2000).collect();
                    out.push_str(&format!("← {preview}\n"));
                }
                _ => {}
            }
        }
    }
    out
}

fn read_agent_url(params: &ReadInput, ctx: &ToolContext) -> Result<ToolOutput> {
    if params.offset.is_some()
        || params.start_line.is_some()
        || params.end_line.is_some()
        || params.limit.is_some()
        || params.hashline
    {
        return Err(crate::agent_result_cache::AgentUrlError::RangeNotApplicable.into());
    }
    let rest = crate::agent_result_cache::strip_scheme(&params.file_path)
        .ok_or_else(|| anyhow::anyhow!("not an agent:// path: {}", params.file_path))?;
    let text = crate::agent_result_cache::read(rest, &ctx.session_id)?;
    Ok(ToolOutput::new(text).with_title(params.file_path.clone()))
}

fn has_binary_extension(path: &Path) -> bool {
    let Some(ext) = path.extension() else {
        return false;
    };
    let ext = ext.to_string_lossy().to_lowercase();
    let binary_exts = [
        "png", "jpg", "jpeg", "gif", "bmp", "ico", "webp", "zip", "tar", "gz", "bz2", "xz", "7z", "rar", "exe", "dll",
        "so", "dylib", "o", "a", "class", "pyc", "wasm", "mp3", "mp4", "avi", "mov", "mkv", "flac", "ogg", "wav",
    ];
    binary_exts.contains(&ext.as_str())
}

fn is_binary_file(path: &Path) -> bool {
    // Check by extension first (no I/O needed)
    if has_binary_extension(path) {
        return true;
    }

    // Read only the first 8KB to check for binary content (not the entire file)
    use std::io::Read;
    if let Ok(mut file) = std::fs::File::open(path) {
        let mut buf = [0u8; 8192];
        if let Ok(n) = file.read(&mut buf)
            && n > 0
        {
            let null_count = buf[..n].iter().filter(|&&b| b == 0).count();
            return null_count > n / 10;
        }
    }

    false
}

fn file_not_found(requested: &str, path: &Path) -> anyhow::Error {
    let suggestions = find_similar_files(path);
    if suggestions.is_empty() {
        anyhow::anyhow!("File not found: {requested}")
    } else {
        anyhow::anyhow!("File not found: {requested}\nDid you mean: {}", suggestions.join(", "))
    }
}

fn find_similar_files(path: &Path) -> Vec<String> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let filename = path.file_name().map(|s| s.to_string_lossy().to_lowercase());

    let mut suggestions = Vec::new();

    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if let Some(ref target) = filename {
                // Simple similarity check
                let target_str: &str = target.as_ref();
                if name.contains(target_str) || target_str.contains(&name as &str) {
                    suggestions.push(entry.path().display().to_string());
                    if suggestions.len() >= 3 {
                        break;
                    }
                }
            }
        }
    }

    suggestions
}

/// Check if a file is an image based on extension
fn is_image_file(path: &Path) -> bool {
    if let Some(ext) = path.extension() {
        let ext = ext.to_string_lossy().to_lowercase();
        matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff"
        )
    } else {
        false
    }
}

/// Handle reading an image file: return it base64-encoded for model vision.
/// Clients render the attached image themselves. The server never writes
/// terminal escape sequences, since it may have no terminal (daemon, remote).
fn handle_image_file(path: &Path, file_path: &str) -> Result<ToolOutput> {
    let data = std::fs::read(path)?;
    let file_size = data.len() as u64;

    let dimensions = get_image_dimensions_from_data(&data);

    let dim_str = dimensions
        .map(|(w, h)| format!("{}x{}", w, h))
        .unwrap_or_else(|| "unknown".to_string());

    let size_str = if file_size < 1024 {
        format!("{} bytes", file_size)
    } else if file_size < 1024 * 1024 {
        format!("{:.1} KB", file_size as f64 / 1024.0)
    } else {
        format!("{:.1} MB", file_size as f64 / 1024.0 / 1024.0)
    };

    const MAX_IMAGE_SIZE: u64 = 20 * 1024 * 1024;
    let mut output = if file_size <= MAX_IMAGE_SIZE {
        // Detect the format from the bytes and only ever attach a format every
        // provider accepts. A stored BMP/ICO block used to 400 every later
        // request in the session (#1712).
        match crate::image_normalize::normalize_image_bytes(data) {
            Ok(image) => {
                let converted_note = image
                    .converted_from
                    .map(|format| format!("Converted from {format} to PNG. "))
                    .unwrap_or_default();
                let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &image.data);
                ToolOutput::new(format!(
                    "Image: {} ({})\nDimensions: {}\n{}Image sent to model for vision analysis.",
                    file_path, size_str, dim_str, converted_note
                ))
                .with_labeled_image(image.media_type, b64, file_path.to_string())
            }
            Err(reason) => ToolOutput::new(format!(
                "Image: {} ({})\nDimensions: {}\nNot sent to the model: {}.",
                file_path, size_str, dim_str, reason
            )),
        }
    } else {
        ToolOutput::new(format!(
            "Image: {} ({})\nDimensions: {}\nImage too large for vision (max 20MB).",
            file_path, size_str, dim_str
        ))
    };

    output = output.with_title(format!("📷 {}", file_path));
    Ok(output)
}

/// Get image dimensions from raw data (duplicated from tui::image for convenience)
fn get_image_dimensions_from_data(data: &[u8]) -> Option<(u32, u32)> {
    // PNG: check signature and parse IHDR chunk
    if data.len() > 24 && &data[0..8] == b"\x89PNG\r\n\x1a\n" {
        let width = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
        let height = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
        return Some((width, height));
    }

    // JPEG: look for SOF0/SOF2 markers
    if data.len() > 2 && data[0] == 0xFF && data[1] == 0xD8 {
        let mut i = 2;
        while i + 9 < data.len() {
            if data[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = data[i + 1];
            // SOF0 (baseline) or SOF2 (progressive)
            if marker == 0xC0 || marker == 0xC2 {
                let height = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
                let width = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
                return Some((width, height));
            }
            // Skip to next marker
            if i + 3 < data.len() {
                let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
                i += 2 + len;
            } else {
                break;
            }
        }
    }

    // GIF: parse header
    if data.len() > 10 && (&data[0..6] == b"GIF87a" || &data[0..6] == b"GIF89a") {
        let width = u16::from_le_bytes([data[6], data[7]]) as u32;
        let height = u16::from_le_bytes([data[8], data[9]]) as u32;
        return Some((width, height));
    }

    None
}

/// Check if a file is a PDF based on extension
fn is_pdf_file(path: &Path) -> bool {
    if let Some(ext) = path.extension() {
        ext.to_string_lossy().to_lowercase() == "pdf"
    } else {
        false
    }
}

/// Context budget for extracted PDF text, which is not on disk to re-read.
#[cfg(feature = "pdf")]
const PDF_OUTPUT_BUDGET_BYTES: usize = 64 * 1024;

/// Handle reading a PDF file - extract text content
#[cfg(feature = "pdf")]
fn handle_pdf_file(path: &Path, file_path: &str, session_id: &str) -> Result<ToolOutput> {
    // Get file metadata
    let metadata = std::fs::metadata(path)?;
    let file_size = metadata.len();

    let size_str = if file_size < 1024 {
        format!("{} bytes", file_size)
    } else if file_size < 1024 * 1024 {
        format!("{:.1} KB", file_size as f64 / 1024.0)
    } else {
        format!("{:.1} MB", file_size as f64 / 1024.0 / 1024.0)
    };

    // Extract text from PDF
    match horon_agent_pdf::extract_text(path) {
        Ok(text) => {
            let mut output = String::new();
            output.push_str(&format!("PDF: {} ({})\n", file_path, size_str));
            output.push_str(&format!("{}\n", "=".repeat(60)));

            // Split into pages (pdf_extract uses form feed \x0c as page separator)
            let pages: Vec<&str> = text.split('\x0c').collect();
            let page_count = pages.len();

            output.push_str(&format!("Pages: {}\n\n", page_count));

            for (i, page) in pages.iter().enumerate() {
                let page_text = page.trim();
                if !page_text.is_empty() {
                    output.push_str(&format!("--- Page {} ---\n", i + 1));
                    output.push_str(page_text);
                    output.push_str("\n\n");
                }
            }

            // Extracted text is not on disk; a long PDF is condensed with the
            // full text kept as an artifact rather than cut per page.
            Ok(ToolOutput::new(artifacts::condense_for_context(
                session_id,
                "read",
                output,
                PDF_OUTPUT_BUDGET_BYTES,
            )))
        }
        Err(e) => {
            // Fall back to metadata only if text extraction fails
            Ok(ToolOutput::new(format!(
                "PDF: {} ({})\nCould not extract text: {}\nThis may be a scanned/image-based PDF.",
                file_path, size_str, e
            )))
        }
    }
}

/// Handle reading a PDF file when PDF support is not compiled in.
#[cfg(not(feature = "pdf"))]
fn handle_pdf_file(path: &Path, file_path: &str, _session_id: &str) -> Result<ToolOutput> {
    let metadata = std::fs::metadata(path)?;
    let file_size = metadata.len();

    let size_str = if file_size < 1024 {
        format!("{} bytes", file_size)
    } else if file_size < 1024 * 1024 {
        format!("{:.1} KB", file_size as f64 / 1024.0)
    } else {
        format!("{:.1} MB", file_size as f64 / 1024.0 / 1024.0)
    };

    Ok(ToolOutput::new(format!(
        "PDF: {} ({})\nPDF text extraction is not available in this build. Rebuild with the `pdf` feature enabled to extract text.",
        file_path, size_str
    )))
}
