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

pub struct EditTool;

impl EditTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct EditInput {
    #[serde(default)]
    intent: Option<String>,
    file_path: String,
    #[serde(default)]
    edits: Option<Vec<EditOperation>>,
    #[serde(default)]
    old_string: Option<String>,
    #[serde(default)]
    new_string: Option<String>,
    #[serde(default)]
    replace_all: bool,
}

#[derive(Deserialize, Clone)]
struct EditOperation {
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

impl EditInput {
    /// Accept either the `edits` array or the single-edit shorthand, never both.
    fn operations(&self) -> Result<Vec<EditOperation>> {
        let single = self.old_string.is_some() || self.new_string.is_some();
        match (&self.edits, single) {
            (Some(_), true) => Err(anyhow::anyhow!(
                "Use either `edits` or `old_string`/`new_string`, not both."
            )),
            (Some(edits), false) if edits.is_empty() => Err(anyhow::anyhow!("`edits` must contain at least one edit.")),
            (Some(edits), false) => Ok(edits.clone()),
            (None, true) => Ok(vec![EditOperation {
                old_string: self
                    .old_string
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("`old_string` is required with `new_string`."))?,
                new_string: self
                    .new_string
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("`new_string` is required with `old_string`."))?,
                replace_all: self.replace_all,
            }]),
            (None, false) => Err(anyhow::anyhow!(
                "Provide `edits` (array of old_string/new_string) or `old_string` and `new_string`."
            )),
        }
    }
}

#[derive(Debug)]
struct AppliedEdit {
    occurrences: usize,
    start_line: usize,
    /// Set when the edit was placed by the fuzzy ladder rather than an exact
    /// match, so the result says so instead of looking like an exact hit.
    note: Option<String>,
}

/// Apply every edit in order to an in-memory copy. Any failure aborts the whole
/// call so the file is never left half-edited.
fn apply_edits(original: &str, edits: &[EditOperation], file_path: &str) -> Result<(String, Vec<AppliedEdit>)> {
    apply_edits_with(original, edits, file_path, crate::config::config().tools.edit_fuzzy)
}

fn apply_edits_with(
    original: &str,
    edits: &[EditOperation],
    file_path: &str,
    fuzzy: bool,
) -> Result<(String, Vec<AppliedEdit>)> {
    let mut content = original.to_string();
    let mut applied = Vec::with_capacity(edits.len());
    let mut failures = Vec::new();
    let label = |index: usize| {
        if edits.len() == 1 {
            String::new()
        } else {
            format!("Edit {}: ", index + 1)
        }
    };

    for (index, edit) in edits.iter().enumerate() {
        if edit.old_string == edit.new_string {
            failures.push(format!("{}old_string and new_string must be different", label(index)));
            continue;
        }
        if edit.old_string.is_empty() {
            failures.push(format!("{}old_string must not be empty", label(index)));
            continue;
        }
        let occurrences = content.matches(&edit.old_string).count();
        if occurrences == 0 {
            match closest_match(&content, &edit.old_string) {
                Some(found) if fuzzy && found.unique => {
                    let matched_text = &content[found.start_byte..found.end_byte];
                    let replacement = reindent(&edit.new_string, &edit.old_string, matched_text);
                    content.replace_range(found.start_byte..found.end_byte, &replacement);
                    applied.push(AppliedEdit {
                        occurrences: 1,
                        start_line: found.line,
                        note: Some(format!(
                            " (applied via fuzzy match, {:.0}% similar, at line {})",
                            found.similarity * 100.0,
                            found.line
                        )),
                    });
                    continue;
                }
                found => {
                    failures.push(format!(
                        "{}{}",
                        label(index),
                        fuzzy_miss_message(&content, file_path, found.as_ref(), fuzzy)
                    ));
                    continue;
                }
            }
        }
        if occurrences > 1 && !edit.replace_all {
            failures.push(format!(
                "{}old_string found {occurrences} times. Either:\n\
                 1. Provide more context to make it unique, or\n\
                 2. Set replace_all: true to replace all occurrences",
                label(index)
            ));
            continue;
        }
        let start_line = find_line_number(&content, &edit.old_string);
        content = if edit.replace_all {
            content.replace(&edit.old_string, &edit.new_string)
        } else {
            content.replacen(&edit.old_string, &edit.new_string, 1)
        };
        applied.push(AppliedEdit {
            occurrences,
            start_line,
            note: None,
        });
    }

    if failures.is_empty() {
        return Ok((content, applied));
    }
    if edits.len() == 1 {
        return Err(anyhow::anyhow!(failures.remove(0)));
    }
    Err(anyhow::anyhow!(
        "No changes written to {file_path}. {} of {} edits failed:\n{}\n\
         Edits apply in order, so later edits see the result of earlier ones.",
        failures.len(),
        edits.len(),
        failures
            .iter()
            .map(|failure| format!("  ✗ {failure}"))
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Edit a file by exact replacement. All edits apply or none do."
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
                "edits": {
                    "type": "array",
                    "description": "Replacements applied in order. Each old_string must match exactly once unless replace_all is set.",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "required": ["old_string", "new_string"],
                        "properties": {
                            "old_string": {"type": "string", "description": "Exact text to replace."},
                            "new_string": {"type": "string", "description": "Replacement text."},
                            "replace_all": {"type": "boolean", "description": "Replace every match."}
                        }
                    }
                },
                "old_string": {
                    "type": "string",
                    "description": "Single-edit shorthand: exact text to replace. Omit when using edits."
                },
                "new_string": {
                    "type": "string",
                    "description": "Single-edit shorthand: replacement text."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Single-edit shorthand: replace every match."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        super::path_policy::enforce("edit", &input, &ctx)?;
        let params: EditInput = serde_json::from_value(input)?;
        let mut edits = params.operations()?;
        let mut stripped_prefix_lines = 0usize;
        for edit in &mut edits {
            let (stripped, count) = super::write::strip_hashline_prefixes(&edit.new_string);
            if count > 0 {
                edit.new_string = stripped.into_owned();
                stripped_prefix_lines += count;
            }
        }
        let path = ctx.resolve_path(Path::new(&params.file_path));

        let _lock = super::file_lock::lock(&path).await;
        let content = match super::client_fs::read_text(&path, &ctx).await? {
            super::client_fs::TextRead::Client(text) => text,
            super::client_fs::TextRead::NotFound => {
                return Err(anyhow::anyhow!("File not found: {}", params.file_path));
            }
            super::client_fs::TextRead::Local => {
                if !path.exists() {
                    return Err(anyhow::anyhow!("File not found: {}", params.file_path));
                }
                tokio::fs::read_to_string(&path).await?
            }
        };
        if let Some(refusal) = super::generated_guard::refusal(&path, &params.file_path) {
            return Err(anyhow::anyhow!(refusal));
        }

        let (new_content, applied) = apply_edits(&content, &edits, &params.file_path)?;

        let wrote_to_client = super::client_fs::write_text(&path, &new_content, &ctx).await?;
        if !wrote_to_client {
            let before = super::undo::capture(&path).await;
            tokio::fs::write(&path, &new_content).await?;
            super::undo::record(
                &ctx,
                vec![super::undo::PendingFile {
                    path: path.clone(),
                    before,
                    after: super::undo::bytes(new_content.as_bytes()),
                }],
            );
        }
        super::edit_stats::record(&ctx, &content, &new_content, false).await;

        let intent = params.intent.clone().filter(|value| !value.trim().is_empty());

        let mut body = if let [edit] = edits.as_slice() {
            let applied = &applied[0];
            let diff = generate_diff(&edit.old_string, &edit.new_string, applied.start_line);
            let end_line = applied.start_line + edit.new_string.lines().count().saturating_sub(1);
            Bus::global().publish(BusEvent::FileTouch(FileTouch {
                session_id: ctx.session_id.clone(),
                path: path.to_path_buf(),
                op: FileOp::Edit,
                intent,
                summary: Some(format!(
                    "edited lines {}-{} ({} occurrence{})",
                    applied.start_line,
                    end_line,
                    applied.occurrences,
                    if applied.occurrences == 1 { "" } else { "s" }
                )),
                detail: build_file_touch_preview(&diff),
            }));
            let context = extract_context(&new_content, applied.start_line, end_line, 3);
            let note = applied.note.clone().unwrap_or_default();
            format!(
                "Edited {}: replaced {} occurrence(s){note}\n{}\n\nContext after edit (lines {}-{}):\n{}",
                params.file_path, applied.occurrences, diff, context.0, context.1, context.2
            )
        } else {
            let diff = generate_diff_summary(&content, &new_content);
            let replaced: usize = applied.iter().map(|edit| edit.occurrences).sum();
            Bus::global().publish(BusEvent::FileTouch(FileTouch {
                session_id: ctx.session_id.clone(),
                path: path.to_path_buf(),
                op: FileOp::Edit,
                intent,
                summary: Some(format!(
                    "applied {} edits ({replaced} replacement{})",
                    edits.len(),
                    if replaced == 1 { "" } else { "s" }
                )),
                detail: build_file_touch_preview(&diff),
            }));
            let mut body = format!("Edited {}: applied {} edits\n", params.file_path, edits.len());
            for (index, edit) in applied.iter().enumerate() {
                body.push_str(&format!(
                    "  ✓ Edit {}: replaced {} occurrence{} at line {}{}\n",
                    index + 1,
                    edit.occurrences,
                    if edit.occurrences == 1 { "" } else { "s" },
                    edit.start_line,
                    edit.note.clone().unwrap_or_default()
                ));
            }
            if !diff.is_empty() {
                body.push_str("\nDiff:\n");
                body.push_str(&diff);
            }
            body
        };
        if stripped_prefix_lines > 0 {
            body.push_str(&format!(
                "\n(Stripped read-output line prefixes from {stripped_prefix_lines} lines before writing.)"
            ));
        }
        if wrote_to_client {
            body.push_str(
                "\n\nThe edit was applied through the ACP client's file buffer; the file on disk is unchanged.",
            );
        } else {
            super::config_edit_notice::append_config_edit_notice(&mut body, &path, &content, &new_content);
            if let Some(extra) = super::nested_agents::inject_rules_for_path(&path, &ctx) {
                body.push_str(&extra);
            }
        }

        Ok(super::file_diff::attach_file_diffs(
            super::file_diff::attach(
                ToolOutput::new(body).with_title(params.file_path.clone()),
                super::file_diff::unified(&params.file_path, &params.file_path, &content, &new_content),
            ),
            vec![(path.clone(), Some(content), new_content)],
        ))
    }
}

/// Find the 1-based line number where a substring starts
fn find_line_number(content: &str, substring: &str) -> usize {
    if let Some(pos) = content.find(substring) {
        content[..pos].bytes().filter(|&byte| byte == b'\n').count() + 1
    } else {
        1
    }
}

/// Generate a compact diff: "42- old" / "42+ new"
fn generate_diff(old: &str, new: &str, start_line: usize) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut output = String::new();

    let mut old_line = start_line;
    let mut new_line = start_line;

    for change in diff.iter_all_changes() {
        let content = change.value().trim();
        let (prefix, line_num) = match change.tag() {
            ChangeTag::Delete => {
                let num = old_line;
                old_line += 1;
                if content.is_empty() {
                    continue;
                }
                ("-", num)
            }
            ChangeTag::Insert => {
                let num = new_line;
                new_line += 1;
                if content.is_empty() {
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

        // Compact format: "42- content" (no spaces)
        output.push_str(&format!("{}{} {}\n", line_num, prefix, content));
    }

    if output.is_empty() {
        String::new()
    } else {
        output.trim_end().to_string()
    }
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

/// Extract lines around the edited region, returns (start_line, end_line, content)
fn extract_context(content: &str, edit_start: usize, edit_end: usize, padding: usize) -> (usize, usize, String) {
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    // Calculate range with padding (1-indexed to 0-indexed)
    let start = edit_start.saturating_sub(padding + 1);
    let end = (edit_end + padding).min(total_lines);

    let context_lines: Vec<String> = lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>4}│ {}", start + i + 1, line))
        .collect();

    (start + 1, end, context_lines.join("\n"))
}

/// One candidate location for an `old_string` that did not match exactly.
struct FuzzyMatch {
    start_byte: usize,
    end_byte: usize,
    line: usize,
    similarity: f64,
    /// Exactly one location is at least as close as this one.
    unique: bool,
}

/// Similarity at or above which the fuzzy ladder accepts a window when
/// `[tools] edit_fuzzy` is on.
const FUZZY_THRESHOLD: f64 = 0.95;
/// Above this, the best window wins even if another window is close.
const FUZZY_DOMINANT: f64 = 0.97;
/// The runner-up must trail by at least this much for a dominant match.
const FUZZY_DOMINANCE_GAP: f64 = 0.08;

fn leading_depth(line: &str) -> usize {
    line.chars().take_while(|c| c.is_whitespace()).count()
}

fn trim_end_ws(line: &str) -> &str {
    line.trim_end()
}

/// Folds the punctuation agents commonly retype differently: curly quotes and
/// typographic dashes, plus runs of spaces or tabs.
fn normalize_typography(line: &str) -> String {
    let mut mapped = String::with_capacity(line.len());
    for character in line.chars() {
        mapped.push(match character {
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' | '\u{ab}' | '\u{bb}' => '"',
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' | '\u{b4}' => '\'',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
            other => other,
        });
    }
    let mut collapsed = String::with_capacity(mapped.len());
    let mut in_space = false;
    for character in mapped.chars() {
        if character == ' ' || character == '\t' {
            if !in_space {
                collapsed.push(' ');
            }
            in_space = true;
        } else {
            collapsed.push(character);
            in_space = false;
        }
    }
    collapsed
}

fn levenshtein(a: &str, b: &str) -> usize {
    if a == b {
        return 0;
    }
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];
    for (i, left) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, right) in b.iter().enumerate() {
            let cost = usize::from(left != right);
            current[j + 1] = (previous[j + 1] + 1).min(current[j] + 1).min(previous[j] + cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

fn line_similarity(left: &str, right: &str) -> f64 {
    let left = normalize_typography(left.trim());
    let right = normalize_typography(right.trim());
    let longest = left.chars().count().max(right.chars().count());
    if longest == 0 {
        return 1.0;
    }
    let distance = levenshtein(&left, &right);
    1.0 - (distance as f64 / longest as f64)
}

fn line_offsets(content: &str) -> Vec<(usize, usize, &str)> {
    let mut lines = Vec::new();
    let mut offset = 0usize;
    for chunk in content.split_inclusive('\n') {
        let text = chunk.strip_suffix('\n').unwrap_or(chunk);
        let text = text.strip_suffix('\r').unwrap_or(text);
        lines.push((offset, offset + text.len(), text));
        offset += chunk.len();
    }
    if lines.is_empty() && !content.is_empty() {
        lines.push((0, content.len(), content));
    }
    lines
}

/// Finds the window in `content` closest to `old_string`.
///
/// The ladder runs from cheapest to most permissive comparison: exact after
/// trailing-whitespace trim, exact after full trim, exact after punctuation
/// normalization, then Levenshtein similarity. The first level with a match
/// decides, so a whitespace-only difference never reports a fuzzy percentage.
type LineCompare = dyn Fn(&str, &str) -> bool;

fn closest_match(content: &str, old_string: &str) -> Option<FuzzyMatch> {
    let old_lines: Vec<&str> = old_string.lines().collect();
    if old_lines.is_empty() {
        return None;
    }
    let lines = line_offsets(content);
    let window = old_lines.len();
    if lines.len() < window {
        return None;
    }

    let equality = |compare: &dyn Fn(&str, &str) -> bool| -> Option<Vec<usize>> {
        let mut hits = Vec::new();
        for index in 0..=lines.len() - window {
            if lines[index..index + window]
                .iter()
                .zip(&old_lines)
                .all(|((_, _, text), old)| compare(text, old))
            {
                hits.push(index);
            }
        }
        (!hits.is_empty()).then_some(hits)
    };

    let levels: [&LineCompare; 3] = [
        &|text, old| trim_end_ws(text) == trim_end_ws(old),
        &|text, old| text.trim() == old.trim(),
        &|text, old| normalize_typography(text.trim()) == normalize_typography(old.trim()),
    ];
    for compare in levels {
        if let Some(hits) = equality(compare) {
            let index = hits[0];
            return Some(FuzzyMatch {
                start_byte: lines[index].0,
                end_byte: lines[index + window - 1].1,
                line: index + 1,
                similarity: 1.0,
                unique: hits.len() == 1,
            });
        }
    }

    let mut scored: Vec<(usize, f64)> = Vec::with_capacity(lines.len());
    for index in 0..=lines.len() - window {
        let base = leading_depth(lines[index].2);
        let mut total = 0.0;
        for (offset, old) in old_lines.iter().enumerate() {
            let text = lines[index + offset].2;
            let relative = leading_depth(text).saturating_sub(base);
            let old_relative = leading_depth(old);
            let left = format!("{relative}:{}", normalize_typography(text.trim()));
            let right = format!("{old_relative}:{}", normalize_typography(old.trim()));
            total += line_similarity(&left, &right);
        }
        scored.push((index, total / window as f64));
    }
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    let (index, best) = *scored.first()?;
    let runner_up = scored.get(1).map(|(_, score)| *score).unwrap_or(0.0);
    let close = scored.iter().filter(|(_, score)| *score >= FUZZY_THRESHOLD).count();
    let unique =
        best >= FUZZY_THRESHOLD && (close == 1 || (best >= FUZZY_DOMINANT && best - runner_up >= FUZZY_DOMINANCE_GAP));
    Some(FuzzyMatch {
        start_byte: lines[index].0,
        end_byte: lines[index + window - 1].1,
        line: index + 1,
        similarity: best.max(0.0),
        unique,
    })
}

/// Shifts `new_string` to the indentation of the text it replaces.
fn reindent(new_string: &str, old_string: &str, matched_text: &str) -> String {
    let first_line = old_string.lines().next().unwrap_or("");
    let from_indent: String = first_line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
    let to_line = matched_text.lines().next().unwrap_or("");
    let to_indent: String = to_line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
    if from_indent == to_indent {
        return new_string.to_string();
    }
    let from_depth = from_indent.chars().count() as i64;
    let to_depth = to_indent.chars().count() as i64;
    let delta = to_depth - from_depth;
    let mut out = String::with_capacity(new_string.len());
    for (index, line) in new_string.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        if line.trim().is_empty() {
            out.push_str(line);
            continue;
        }
        if !from_indent.is_empty() && line.starts_with(&from_indent) {
            out.push_str(&to_indent);
            out.push_str(&line[from_indent.len()..]);
        } else if delta > 0 {
            out.push_str(&to_indent);
            out.push_str(line);
        } else if delta < 0 {
            let strip = (-delta) as usize;
            let removable = line.chars().take_while(|c| *c == ' ' || *c == '\t').count();
            let keep = line
                .char_indices()
                .nth(removable.min(strip))
                .map_or(line.len(), |(idx, _)| idx);
            out.push_str(&line[keep..]);
        } else {
            out.push_str(line);
        }
    }
    out
}

/// The miss message: the closest window, then how to accept it when it is a
/// unique match.
fn fuzzy_miss_message(content: &str, file_path: &str, found: Option<&FuzzyMatch>, fuzzy: bool) -> String {
    let Some(found) = found else {
        return format!("old_string not found in {file_path}. Use the read tool to see the current file contents.");
    };
    let matched = &content[found.start_byte..found.end_byte];
    let mut message = format!(
        "old_string not found in {file_path}.\n\nClosest match ({:.0}% similar) at line {}:\n",
        found.similarity * 100.0,
        found.line
    );
    for line in matched.lines().take(5) {
        message.push_str(&format!("  | {line}\n"));
    }
    if found.unique && !fuzzy {
        message.push_str("Fuzzy matching is off; enable [tools] edit_fuzzy to accept close matches.");
    }
    message
}

/// Generate a compact whole-file diff: "42- old" / "42+ new" (max 30 lines)
fn generate_diff_summary(old: &str, new: &str) -> String {
    const MAX_LINES: usize = 30;
    let diff = TextDiff::from_lines(old, new);
    let mut output = String::new();
    let mut lines_shown = 0;
    let mut old_line = 1usize;
    let mut new_line = 1usize;

    for change in diff.iter_all_changes() {
        let (prefix, number) = match change.tag() {
            ChangeTag::Equal => {
                old_line += 1;
                new_line += 1;
                continue;
            }
            ChangeTag::Delete => {
                old_line += 1;
                ("-", old_line - 1)
            }
            ChangeTag::Insert => {
                new_line += 1;
                ("+", new_line - 1)
            }
        };
        let content = change.value().trim();
        if content.is_empty() {
            continue;
        }
        if lines_shown >= MAX_LINES {
            output.push_str("...\n");
            break;
        }
        output.push_str(&format!("{number}{prefix} {content}\n"));
        lines_shown += 1;
    }

    output.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_diff_single_line_change() {
        let old = "hello world";
        let new = "hello rust";
        let diff = generate_diff(old, new, 10);

        // Compact format: "10- content" / "10+ content"
        assert!(diff.contains("10- hello world"), "Should show deleted line");
        assert!(diff.contains("10+ hello rust"), "Should show added line");
    }

    #[test]
    fn test_generate_diff_multi_line() {
        let old = "line one\nline two\nline three";
        let new = "line one\nmodified two\nline three";
        let diff = generate_diff(old, new, 5);

        // Line 6 should be the changed line (5 + 1 for "line two")
        assert!(diff.contains("6- line two"), "Should show deleted line");
        assert!(diff.contains("6+ modified two"), "Should show added line");
        // Equal lines should not appear
        assert!(!diff.contains("line one"), "Should not show unchanged lines");
        assert!(!diff.contains("line three"), "Should not show unchanged lines");
    }

    #[test]
    fn test_generate_diff_addition_only() {
        let old = "first\nthird";
        let new = "first\nsecond\nthird";
        let diff = generate_diff(old, new, 1);

        assert!(diff.contains("+ second"), "Should show added line");
    }

    #[test]
    fn test_generate_diff_deletion_only() {
        let old = "first\nsecond\nthird";
        let new = "first\nthird";
        let diff = generate_diff(old, new, 1);

        assert!(diff.contains("- second"), "Should show deleted line");
    }

    #[test]
    fn test_generate_diff_no_changes() {
        let old = "same content";
        let new = "same content";
        let diff = generate_diff(old, new, 1);

        assert!(diff.is_empty(), "No changes should produce empty diff");
    }

    #[test]
    fn test_generate_diff_line_number_format() {
        let old = "old";
        let new = "new";
        let diff = generate_diff(old, new, 42);

        // Compact format: no padding
        assert!(
            diff.contains("42- old"),
            "Should have line number directly before minus"
        );
        assert!(diff.contains("42+ new"), "Should have line number directly before plus");
    }

    fn op(old: &str, new: &str) -> EditOperation {
        EditOperation {
            old_string: old.into(),
            new_string: new.into(),
            replace_all: false,
        }
    }

    #[test]
    fn apply_edits_is_all_or_nothing() {
        let error = apply_edits(
            "alpha\nbeta\ngamma\n",
            &[op("alpha", "a"), op("missing", "x"), op("gamma", "g")],
            "f.rs",
        )
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("No changes written to f.rs"), "{error}");
        assert!(error.contains("1 of 3 edits failed"), "{error}");
        assert!(error.contains("Edit 2: old_string not found"), "{error}");
    }

    #[test]
    fn apply_edits_applies_sequentially() {
        let (content, applied) = apply_edits(
            "alpha\nbeta\n",
            &[op("alpha", "temp"), op("temp", "done"), op("beta", "b")],
            "f.rs",
        )
        .unwrap();
        assert_eq!(content, "done\nb\n");
        assert_eq!(applied.len(), 3);
        assert_eq!(applied[2].start_line, 2);
    }

    #[test]
    fn apply_edits_rejects_ambiguous_match_without_replace_all() {
        assert!(apply_edits("x x", &[op("x", "y")], "f").is_err());
        let mut all = op("x", "y");
        all.replace_all = true;
        let (content, applied) = apply_edits("x x", &[all], "f").unwrap();
        assert_eq!(content, "y y");
        assert_eq!(applied[0].occurrences, 2);
    }

    #[test]
    fn input_accepts_single_or_array_but_not_both() {
        let parse = |value: Value| serde_json::from_value::<EditInput>(value).unwrap();
        assert_eq!(
            parse(json!({"file_path":"f","old_string":"a","new_string":"b"}))
                .operations()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            parse(json!({"file_path":"f","edits":[{"old_string":"a","new_string":"b"},{"old_string":"c","new_string":"d"}]}))
                .operations()
                .unwrap()
                .len(),
            2
        );
        assert!(
            parse(json!({"file_path":"f","edits":[],"old_string":"a","new_string":"b"}))
                .operations()
                .is_err()
        );
        assert!(parse(json!({"file_path":"f"})).operations().is_err());
        assert!(parse(json!({"file_path":"f","old_string":"a"})).operations().is_err());
    }

    #[test]
    fn test_find_line_number() {
        let content = "line 1\nline 2\nline 3\nline 4";

        assert_eq!(find_line_number(content, "line 1"), 1);
        assert_eq!(find_line_number(content, "line 2"), 2);
        assert_eq!(find_line_number(content, "ine 2"), 2);
        assert_eq!(find_line_number(content, "line 3"), 3);
        assert_eq!(find_line_number(content, "line 4"), 4);
        assert_eq!(find_line_number(content, "not found"), 1);
    }

    #[test]
    fn test_extract_context() {
        let content = "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9\nline 10";

        // Edit at line 5, with 2 lines padding
        let (start, end, ctx) = extract_context(content, 5, 5, 2);

        assert_eq!(start, 3, "Should start at line 3 (5 - 2)");
        assert_eq!(end, 7, "Should end at line 7 (5 + 2)");
        assert!(ctx.contains("line 3"), "Should include line 3");
        assert!(ctx.contains("line 5"), "Should include edited line 5");
        assert!(ctx.contains("line 7"), "Should include line 7");
        assert!(!ctx.contains("line 2"), "Should not include line 2");
        assert!(!ctx.contains("line 8"), "Should not include line 8");
    }

    #[test]
    fn test_extract_context_at_start() {
        let content = "line 1\nline 2\nline 3\nline 4\nline 5";

        // Edit at line 1, with 2 lines padding - shouldn't go negative
        let (start, _end, ctx) = extract_context(content, 1, 1, 2);

        assert_eq!(start, 1, "Should start at line 1 (can't go before)");
        assert!(ctx.contains("line 1"), "Should include line 1");
        assert!(ctx.contains("line 3"), "Should include line 3");
    }

    #[test]
    fn test_extract_context_at_end() {
        let content = "line 1\nline 2\nline 3\nline 4\nline 5";

        // Edit at line 5, with 2 lines padding - shouldn't go past end
        let (_start, end, ctx) = extract_context(content, 5, 5, 2);

        assert_eq!(end, 5, "Should end at line 5 (can't go past)");
        assert!(ctx.contains("line 5"), "Should include line 5");
        assert!(ctx.contains("line 3"), "Should include line 3");
    }

    #[test]
    fn test_extract_context_range_past_end() {
        let content = "line 1\nline 2\nline 3\nline 4\nline 5";

        // Edit range extends past the end of the file.
        let (start, end, ctx) = extract_context(content, 4, 10, 1);

        assert_eq!(start, 3, "Should start at line 3 (4 - 1)");
        assert_eq!(end, 5, "Should clamp to last line");
        assert!(ctx.contains("line 3"), "Should include line 3");
        assert!(ctx.contains("line 5"), "Should include line 5");
    }

    fn edit_test_ctx(working_dir: std::path::PathBuf) -> crate::tool::ToolContext {
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

    #[tokio::test]
    async fn edit_refuses_a_generated_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("service.go");
        let original = "package service\n\n// Code generated by protoc-gen-go. DO NOT EDIT.\n";
        std::fs::write(&path, original).expect("write file");

        let error = EditTool::new()
            .execute(
                json!({
                    "file_path": "service.go",
                    "old_string": "package service",
                    "new_string": "package svc"
                }),
                edit_test_ctx(temp.path().to_path_buf()),
            )
            .await
            .expect_err("a generated file must not be edited");

        let text = error.to_string();
        assert!(
            text.starts_with("Cannot modify auto-generated file: service.go\n"),
            "{text}"
        );
        assert!(
            text.contains("Set [tools] block_auto_generated = false to allow edits."),
            "{text}"
        );
        assert_eq!(std::fs::read_to_string(&path).expect("read back"), original);
    }

    #[tokio::test]
    async fn edit_strips_read_output_prefixes_from_new_text() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("a.rs");
        std::fs::write(&path, "fn a() {}\n").expect("write file");

        EditTool::new()
            .execute(
                json!({
                    "file_path": "a.rs",
                    "old_string": "fn a() {}",
                    "new_string": "    1#abcd:fn a() {}\n    2#abcd:fn b() {}"
                }),
                edit_test_ctx(temp.path().to_path_buf()),
            )
            .await
            .expect("edit should succeed");

        assert_eq!(
            std::fs::read_to_string(&path).expect("read back"),
            "fn a() {}\nfn b() {}\n"
        );
    }
}

#[cfg(test)]
mod fuzzy_tests {
    use super::*;

    fn op(old: &str, new: &str) -> EditOperation {
        EditOperation {
            old_string: old.into(),
            new_string: new.into(),
            replace_all: false,
        }
    }

    #[test]
    fn typography_only_difference_is_a_unique_full_similarity_match() {
        let content = "let name = “value”;\nlet other = 1;\n";
        let found = closest_match(content, "let name = \"value\";").expect("closest match");
        assert!(found.unique);
        assert_eq!(found.line, 1);
        assert!((found.similarity - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_near_miss_reports_the_closest_window() {
        let content = "alpha\nlet value = compute_total();\ndelta\n";
        let found = closest_match(content, "let value = compute_totai();").expect("closest match");
        assert_eq!(found.line, 2);
        assert!(found.unique, "similarity {}", found.similarity);
        assert!(found.similarity > 0.95, "similarity {}", found.similarity);
    }

    #[test]
    fn fuzzy_off_reports_the_prefix_and_the_opt_in_hint() {
        let content = "let name = “value”;\n";
        let error = apply_edits_with(
            content,
            &[op("let name = \"value\";", "let name = \"x\";")],
            "f.rs",
            false,
        )
        .expect_err("exact match is missing")
        .to_string();
        assert!(error.contains("old_string not found in f.rs."), "{error}");
        assert!(error.contains("Closest match (100% similar) at line 1:"), "{error}");
        assert!(error.contains("  | let name = “value”;"), "{error}");
        assert!(
            error.contains("Fuzzy matching is off; enable [tools] edit_fuzzy to accept close matches."),
            "{error}"
        );
    }

    #[test]
    fn fuzzy_on_applies_and_reindents_the_replacement() {
        let content = "fn a() {\n    let name = “value”;\n}\n";
        let (edited, applied) = apply_edits_with(
            content,
            &[op("let name = \"value\";", "let name = \"x\";")],
            "f.rs",
            true,
        )
        .expect("fuzzy edit");
        assert_eq!(edited, "fn a() {\n    let name = \"x\";\n}\n");
        assert!(applied[0].note.as_deref().unwrap().contains("applied via fuzzy match"));
        assert!(applied[0].note.as_deref().unwrap().contains("100% similar"));
        assert_eq!(applied[0].start_line, 2);
    }

    #[test]
    fn fuzzy_on_keeps_refusing_an_ambiguous_match() {
        let content = "alpha one\nalpha one\n";
        let error = apply_edits_with(content, &[op("alpha ono", "beta")], "f.rs", true)
            .expect_err("two windows are equally close")
            .to_string();
        assert!(error.contains("Closest match"), "{error}");
        assert!(!error.contains("Fuzzy matching is off"), "{error}");
    }
}
