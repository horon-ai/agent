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
    if text.len() <= budget_bytes {
        return text;
    }
    let masked = crate::secrets::mask_for_history(&text, session_id, crate::config::config().tools.secrets_mode);
    let condensed = match store(session_id, tool_name, &masked) {
        Ok(id) => render_condensed(&text, tool_name, budget_bytes, Ok(&id.to_string())),
        Err(error) => {
            crate::logging::warn(&format!(
                "[tool:{tool_name}] could not save full output as an artifact for session {session_id}: {error}"
            ));
            let detail = format!("{error}");
            render_condensed(
                &text,
                tool_name,
                budget_bytes,
                Err(crate::util::truncate_str(&detail, 200)),
            )
        }
    };
    horon_agent_perf_counters::record_tool_output_spill(text.len() as u64, condensed.len() as u64);
    condensed
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
fn render_condensed(text: &str, tool_name: &str, budget_bytes: usize, stored: Result<&str, &str>) -> String {
    use std::fmt::Write as _;

    let head_budget = budget_bytes * 2 / 5;
    let tail_budget = budget_bytes * 2 / 5;
    let signal_budget = budget_bytes.saturating_sub(head_budget + tail_budget + POINTER_RESERVE_BYTES);

    let head_end = head_end(text, head_budget);
    let tail_start = tail_start(text, tail_budget, head_end);
    let head = &text[..head_end];
    let omitted = &text[head_end..tail_start];
    let tail = &text[tail_start..];

    let total_lines = text.lines().count();
    let omitted_lines = omitted.lines().count();
    let first_omitted_line = head.bytes().filter(|&b| b == b'\n').count() + 1;

    let mut signals = String::new();
    if signal_budget > SIGNAL_HEADER.len() {
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
    out.push_str(head);
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
        out.push_str(&signals);
    }
    out.push_str(tail);
    out
}

#[cfg(test)]
mod render_tests;
#[cfg(test)]
mod tests;
