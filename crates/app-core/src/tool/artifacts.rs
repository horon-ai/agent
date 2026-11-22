//! Condensing tool output for the model's context.
//!
//! A tool output too large for the model's context is cut to its head, the
//! error/warning lines of the omitted middle, and its tail. The full
//! (history-masked) text is saved in the session artifact store
//! ([`crate::artifacts`]), so the model can read any slice of it back through
//! `read file_path="artifact://<id>"`. Nothing is lost to truncation.

use anyhow::Result;
use std::sync::LazyLock;

pub(crate) const ARTIFACT_SCHEME: &str = "artifact://";
/// Context budget for a tool output that has no tool-specific budget.
pub(crate) const GENERIC_CONDENSE_BUDGET_BYTES: usize = 48 * 1024;
/// Longest single matching line copied from the omitted middle.
const SIGNAL_LINE_MAX_BYTES: usize = 300;
/// Bytes reserved for the pointer notice between head and tail.
const POINTER_RESERVE_BYTES: usize = 512;

const SIGNAL_HEADER: &str = "[matching lines from the omitted part:]\n";

static SIGNAL_LINE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)\b(error|warning|failed|failure|panic(ked)?|exception|traceback|fatal|assert(ion)?)\b")
        .expect("signal regex compiles")
});

/// Save `content` as the next artifact of `session_id`, named after `tool`.
pub(crate) fn store(session_id: &str, tool: &str, content: &str) -> Result<u64> {
    crate::artifacts::save(session_id, tool, content)
}

/// Artifact `id` of `session_id`. Never looks at another session's artifacts.
pub(crate) fn load(session_id: &str, id: u64) -> Result<String> {
    let path = crate::artifacts::path(session_id, id)?;
    Ok(std::fs::read_to_string(path)?)
}

/// Fit `text` into `budget_bytes` for the model's context, saving the full
/// (history-masked) text as a session artifact the condensed form points to.
/// Returns `text` unchanged (no copy) when it already fits.
pub(crate) fn condense_for_context(session_id: &str, tool_name: &str, text: String, budget_bytes: usize) -> String {
    condense_preview(session_id, tool_name, &text, budget_bytes).unwrap_or(text)
}

pub(super) fn condense_preview(session_id: &str, tool_name: &str, text: &str, budget_bytes: usize) -> Option<String> {
    let config = &crate::config::config().tools;
    preview(
        session_id,
        tool_name,
        text,
        budget_bytes,
        config.output_column_cap,
        config.secrets_mode,
        &config.artifact_output,
    )
}

fn preview(
    session_id: &str,
    tool_name: &str,
    text: &str,
    budget_bytes: usize,
    mut columns: usize,
    secrets_mode: crate::config::SecretsMode,
    settings: &crate::config::ArtifactOutputConfig,
) -> Option<String> {
    let threshold = settings.spill_threshold_kib.map_or(budget_bytes, |kib| {
        kib.clamp(1, 64).saturating_mul(1024).min(budget_bytes)
    });
    let mut over_budget = text.len() > threshold;
    if (columns != 0 || (over_budget && text.len() <= budget_bytes))
        && serde_json::from_str::<serde::de::IgnoredAny>(text).is_ok()
    {
        columns = 0;
        over_budget = text.len() > budget_bytes;
    }
    if columns != 0
        && (text.contains(ARTIFACT_SCHEME) || !text.lines().any(|line| line.char_indices().nth(columns).is_some()))
    {
        columns = 0;
    }
    if !over_budget && columns == 0 {
        return None;
    }

    // `Off` means there are no vault placeholders to write into the text, so
    // the store reads it in place instead of receiving a full copy.
    let masked = match secrets_mode {
        crate::config::SecretsMode::Off => std::borrow::Cow::Borrowed(text),
        mode => std::borrow::Cow::Owned(crate::secrets::mask_for_history(text, session_id, mode)),
    };
    let stored = crate::artifacts::save_with_file_cap(session_id, tool_name, masked.as_ref(), settings.file_cap_mib);
    let condensed = match stored {
        Ok(id) if !over_budget => {
            let pointer =
                format!("\n[horon: output lines shortened. Full output: read file_path=\"{ARTIFACT_SCHEME}{id}\".]");
            let mut clipped = String::new();
            push_preview_lines(&mut clipped, text, columns);
            if clipped.len() + pointer.len() > budget_bytes {
                clipped.truncate(head_end(&clipped, budget_bytes.saturating_sub(pointer.len())));
            }
            clipped.push_str(&pointer);
            clipped
        }
        Ok(id) => render_condensed(text, tool_name, threshold, Ok(&id.to_string()), columns, settings),
        Err(error) => {
            crate::logging::warn(&format!(
                "[tool:{tool_name}] could not save full output as an artifact for session {session_id}: {error}"
            ));
            if !over_budget {
                return None;
            }
            let detail = format!("{error}");
            render_condensed(
                text,
                tool_name,
                threshold,
                Err(crate::util::truncate_str(&detail, 200)),
                columns,
                settings,
            )
        }
    };
    horon_agent_perf_counters::record_tool_output_spill(text.len() as u64, condensed.len() as u64);
    Some(condensed)
}

fn push_preview_lines(out: &mut String, text: &str, columns: usize) {
    if columns == 0 {
        out.push_str(text);
        return;
    }
    for line in text.split_inclusive('\n') {
        let body = line
            .strip_suffix("\r\n")
            .or_else(|| line.strip_suffix('\n'))
            .unwrap_or(line);
        let mut chars = body.char_indices();
        if let Some((end, _)) = chars.nth(columns - 1)
            && chars.next().is_some()
        {
            let marker = if body.len() - end >= '…'.len_utf8() {
                '…'
            } else {
                '>'
            };
            out.push_str(&body[..end]);
            out.push(marker);
            out.push_str(&line[body.len()..]);
        } else {
            out.push_str(line);
        }
    }
}

/// End of a head slice of at most `budget` bytes, preferring a line boundary
/// when one falls in the second half of the window.
pub(crate) fn head_end(text: &str, budget: usize) -> usize {
    let window = crate::util::truncate_str(text, budget);
    match window.rfind('\n') {
        Some(pos) if pos + 1 >= window.len() / 2 => pos + 1,
        _ => window.len(),
    }
}

/// Start of a tail slice of at most `budget` bytes, preferring a line start
/// when one falls in the first half of the window. Never before `floor`.
pub(crate) fn tail_start(text: &str, budget: usize, floor: usize) -> usize {
    let mut start = text.len().saturating_sub(budget).max(floor);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    let window = &text[start..];
    match window.find('\n') {
        Some(pos) if pos < window.len() / 2 => start + pos + 1,
        _ => start,
    }
}

/// Pure layout; `stored` is the artifact id or the store error message.
fn render_condensed(
    text: &str,
    tool_name: &str,
    budget_bytes: usize,
    stored: Result<&str, &str>,
    columns: usize,
    settings: &crate::config::ArtifactOutputConfig,
) -> String {
    use std::fmt::Write as _;

    let segment_limit = budget_bytes * 2 / 5;
    let head_budget = settings
        .head_kib
        .map_or(segment_limit, |kib| kib.saturating_mul(1024).min(segment_limit));
    let tail_budget = settings
        .tail_kib
        .map_or(segment_limit, |kib| kib.saturating_mul(1024).min(segment_limit));
    let signal_budget = budget_bytes.saturating_sub(head_budget + tail_budget + POINTER_RESERVE_BYTES);

    let head_end = head_end(text, head_budget);
    let tail_start = tail_start(text, tail_budget, head_end);
    let tail_start = settings.tail_lines.map_or(tail_start, |lines| {
        if lines == 0 {
            return text.len();
        }
        let suffix = &text[tail_start..];
        suffix
            .strip_suffix('\n')
            .unwrap_or(suffix)
            .rmatch_indices('\n')
            .nth(lines - 1)
            .map_or(tail_start, |(index, _)| tail_start + index + 1)
    });
    let head = &text[..head_end];
    let omitted = &text[head_end..tail_start];
    let tail = &text[tail_start..];

    let total_lines = text.lines().count();
    let omitted_lines = omitted.lines().count();

    let mut signals = String::new();
    if signal_budget > SIGNAL_HEADER.len() {
        // Counted here so a call that has no room for signals, or none to show,
        // does not walk the head at all.
        let first_omitted_line = head.bytes().filter(|&b| b == b'\n').count() + 1;
        let room = signal_budget - SIGNAL_HEADER.len();
        for (index, line) in omitted.lines().enumerate() {
            if !SIGNAL_LINE.is_match(line) {
                continue;
            }
            let entry = format!(
                "L{}: {}\n",
                first_omitted_line + index,
                crate::util::truncate_str(line, SIGNAL_LINE_MAX_BYTES)
            );
            if signals.len() + entry.len() > room {
                break;
            }
            signals.push_str(&entry);
        }
    }

    let mut out = String::with_capacity(head.len() + tail.len() + signals.len() + POINTER_RESERVE_BYTES);
    push_preview_lines(&mut out, head, columns);
    if !head.is_empty() && !head.ends_with('\n') {
        out.push('\n');
    }
    match stored {
        Ok(id) => {
            let _ = writeln!(
                out,
                "[horon: {} bytes / {} lines omitted from `{}` output. Full output ({} bytes, {} lines): read file_path=\"{}{}\" with start_line/end_line.]",
                omitted.len(),
                omitted_lines,
                tool_name,
                text.len(),
                total_lines,
                ARTIFACT_SCHEME,
                id
            );
        }
        Err(error) => {
            let _ = writeln!(
                out,
                "[horon: {} bytes / {} lines omitted from `{}` output; the full output could not be saved: {}]",
                omitted.len(),
                omitted_lines,
                tool_name,
                error
            );
        }
    }
    if !signals.is_empty() {
        out.push_str(SIGNAL_HEADER);
        push_preview_lines(&mut out, &signals, columns);
    }
    push_preview_lines(&mut out, tail, columns);
    out
}

#[cfg(test)]
mod render_tests;
#[cfg(test)]
mod tests;
