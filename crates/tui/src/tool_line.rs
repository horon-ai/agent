//! What a tool line says, and how it is drawn.
//!
//! The parts that can be wrong without a terminal live here as plain functions:
//! the argument summary taken from streamed JSON (which is incomplete by
//! construction while the call is still arriving), the cut that keeps a
//! finished call's output small, the elapsed-time wording, and truncation that
//! respects cell width instead of byte or character count. The transcript tests
//! pin these directly, and the renderer below is the only place a tool line's
//! shape is decided.

use std::time::Instant;

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::{Symbols, Theme};
use crate::transcript::{ToolCall, ToolState};

/// Preview rows kept by default, as `transcript.tool_output_lines` resolves.
pub const DEFAULT_OUTPUT_LINES: usize = 3;
/// Ceiling on kept rows: the setting is a preference, not a memory budget.
pub const MAX_OUTPUT_LINES: usize = 40;
/// Cells kept from one preview row; a single-line output can be megabytes.
pub const MAX_PREVIEW_CELLS: usize = 160;
/// Argument bytes kept while a call streams; the summary needs the first field.
pub const MAX_INPUT_BYTES: usize = 4096;
/// Cells a summary needs before it is worth drawing at all.
const MIN_SUMMARY_CELLS: usize = 12;

/// Keys whose value is the interesting argument, most specific first.
const PRIMARY_KEYS: &[&str] = &[
    "command",
    "file_path",
    "path",
    "pattern",
    "query",
    "url",
    "name",
    "prompt",
];

/// The head of a tool's output, kept so a finished line can preview it.
///
/// Only this much of the output is ever held: a tool may return megabytes, and
/// the transcript outlives the call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputHead {
    /// The first rows, each already cut to [`MAX_PREVIEW_CELLS`].
    pub rows: Vec<String>,
    /// How many rows the whole output had.
    pub total_rows: usize,
}

impl OutputHead {
    /// Rows the preview does not show.
    pub fn hidden_rows(&self) -> usize {
        self.total_rows.saturating_sub(self.rows.len())
    }
}

/// Append streamed argument bytes, stopping at [`MAX_INPUT_BYTES`].
///
/// The cap is on bytes because that is what the buffer costs, and it is applied
/// at a character boundary so a truncated buffer stays valid UTF-8.
pub fn append_input(input: &mut String, delta: &str) {
    let room = MAX_INPUT_BYTES.saturating_sub(input.len());
    if room == 0 {
        return;
    }
    if delta.len() <= room {
        input.push_str(delta);
        return;
    }
    let cut = floor_char_boundary(delta, room);
    input.push_str(&delta[..cut]);
}

fn floor_char_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    let mut cut = index;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

/// The one argument worth showing after the tool name.
///
/// While a call is streaming its input is incomplete JSON, so a failed parse is
/// the normal case rather than an error: the first string field is read out of
/// the bytes seen so far.
pub fn summarize(input: &str) -> Option<String> {
    if input.trim().is_empty() {
        return None;
    }
    if let Ok(serde_json::Value::Object(fields)) = serde_json::from_str::<serde_json::Value>(input) {
        let found = PRIMARY_KEYS.iter().find_map(|key| match fields.get(*key) {
            Some(serde_json::Value::String(value)) => Some(value.as_str()),
            Some(serde_json::Value::Number(_) | serde_json::Value::Bool(_) | serde_json::Value::Null)
            | Some(serde_json::Value::Array(_) | serde_json::Value::Object(_))
            | None => None,
        });
        return found.map(one_line).filter(|summary| !summary.is_empty());
    }
    PRIMARY_KEYS
        .iter()
        .find_map(|key| scan_string(input, key))
        .map(|value| one_line(&value))
        .filter(|summary| !summary.is_empty())
}

/// Read `"key": "value"` out of possibly unfinished JSON.
fn scan_string(input: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{key}\"");
    let mut from = 0;
    while let Some(found) = input[from..].find(&pattern) {
        let after = from + found + pattern.len();
        from = after;
        let rest = input[after..].trim_start();
        let Some(rest) = rest.strip_prefix(':') else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('"') else {
            continue;
        };
        return Some(unescape_partial(rest));
    }
    None
}

/// Unescape a value that may end mid-escape or mid-string.
fn unescape_partial(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(other) => out.push(other),
                None => break,
            },
            other => out.push(other),
        }
    }
    out
}

/// Collapse whitespace so a value with newlines stays on one line.
fn one_line(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for word in value.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Keep at most `lines` rows of `output` and count the rows it had.
///
/// `None` when nothing will be drawn: the setting is off, or the tool produced
/// no output at all.
pub fn cut_output(output: &str, lines: usize) -> Option<OutputHead> {
    let keep = lines.min(MAX_OUTPUT_LINES);
    if keep == 0 {
        return None;
    }
    let total_rows = output.lines().count();
    if total_rows == 0 {
        return None;
    }
    let rows = output
        .lines()
        .take(keep)
        .map(|row| truncate_cells(row, MAX_PREVIEW_CELLS))
        .collect();
    Some(OutputHead { rows, total_rows })
}

/// `text` cut to at most `width` cells, with a trailing `…` when it was cut.
///
/// A wide character that would overflow starts nothing: the cut lands before it
/// rather than splitting it, so the result is always valid text of a known
/// display width.
pub fn truncate_cells(text: &str, width: usize) -> String {
    let width = width.max(1);
    if text.width() <= width {
        return text.to_string();
    }
    let budget = width - 1;
    let mut used = 0;
    let mut out = String::new();
    for ch in text.chars() {
        let cells = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cells > budget {
            break;
        }
        used += cells;
        out.push(ch);
    }
    out.push('…');
    out
}

/// The wording the approval gate refuses a call with.
///
/// The request id in it is how a parked prompt pairs itself with this line, so
/// the sentence is stored whole and cannot be shortened at the source.
const GATE_REFUSAL: &str = "needs approval before it runs";
/// What that refusal is drawn as: one phrase, because the prompt beside it
/// already asks the question and three rows of the same words push the rest of
/// the conversation off the pane.
const GATE_REFUSAL_DISPLAY: &str = "needs approval";

/// What a failure says on screen, which is not always what it stores.
fn display_error(message: &str) -> &str {
    if message.contains(GATE_REFUSAL) {
        GATE_REFUSAL_DISPLAY
    } else {
        message
    }
}

/// How long a call took, in the shortest form that stays readable.
pub fn format_elapsed(millis: u64) -> String {
    if millis < 60_000 {
        return format!("{:.1}s", millis as f64 / 1000.0);
    }
    let minutes = millis / 60_000;
    let seconds = (millis % 60_000) / 1000;
    format!("{minutes}m {seconds:02}s")
}

/// The rows one tool call occupies.
///
/// The head row carries the name, the argument and the outcome. A failure puts
/// its message on the rows below rather than in the head: the message is
/// usually longer than the pane, and splicing it into the head would push the
/// duration and the argument off the right edge where neither can be read.
pub fn lines(call: &ToolCall, theme: &Theme, symbols: Symbols, width: u16) -> Vec<Line<'static>> {
    let (glyph, color) = match &call.state {
        ToolState::Running => (symbols.tool_running, theme.transcript.tool_running),
        ToolState::Done => (symbols.tool_done, theme.transcript.tool_done),
        ToolState::Failed(_) => (symbols.tool_error, theme.transcript.tool_error),
    };
    let mut tail = format!(" {glyph} {}", call.state.label());
    if let Some(millis) = call.duration_ms {
        tail.push(' ');
        tail.push_str(&format_elapsed(millis));
    }

    let lead = format!("{} ", symbols.tool_lead);
    let fixed = lead.width() + call.name.width() + tail.width();
    let mut head = vec![
        Span::styled(lead, Style::default().fg(color)),
        Span::styled(call.name.clone(), Style::default().fg(theme.transcript.tool_title)),
    ];
    if let Some(summary) = summarize(&call.input) {
        let budget = usize::from(width).max(1).saturating_sub(fixed + 1);
        if budget >= MIN_SUMMARY_CELLS {
            head.push(Span::styled(
                format!(" {}", truncate_cells(&summary, budget)),
                Style::default().fg(theme.core.muted),
            ));
        }
    }
    head.push(Span::styled(tail, Style::default().fg(color)));

    let mut rows = vec![Line::from(head)];
    let mut error_text: Option<&str> = None;
    if let ToolState::Failed(message) = &call.state {
        error_text = Some(message.as_str());
        rows.push(Line::from(Span::styled(
            format!("  {}", truncate_cells(display_error(message), MAX_PREVIEW_CELLS)),
            Style::default().fg(color),
        )));
    }
    if let Some(output) = &call.output {
        // A failed call's stored output is the failure text itself, so drawing
        // it again would say the same thing twice.
        // The head row is cut with a marker, so the marker comes off before the
        // comparison: what is left must be a prefix of the message itself.
        let repeats_error = output.rows.first().is_some_and(|first| {
            let shown = first.trim_end_matches('…').trim();
            !shown.is_empty() && error_text.is_some_and(|message| message.contains(shown))
        });
        if !repeats_error {
            for row in &output.rows {
                rows.push(Line::from(Span::styled(
                    format!("  {row}"),
                    Style::default().fg(theme.transcript.notice),
                )));
            }
            let hidden = output.hidden_rows();
            if hidden > 0 {
                rows.push(Line::from(Span::styled(
                    format!("  +{hidden} lines"),
                    Style::default().fg(theme.transcript.notice),
                )));
            }
        }
    }
    rows
}

/// When a call started, so a finished line can say how long it took.
///
/// Kept beside the pure functions because the duration wording lives here; the
/// reading itself belongs to the call that recorded it.
pub(crate) fn elapsed_since(started: Option<Instant>) -> Option<u64> {
    started.map(|start| u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX))
}
