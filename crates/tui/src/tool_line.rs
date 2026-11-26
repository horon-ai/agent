//! What a tool call's card says, and how it is drawn.
//!
//! The parts that can be wrong without a terminal live here as plain functions:
//! the argument summary taken from streamed JSON (which is incomplete by
//! construction while the call is still arriving), the cut that keeps a
//! finished call's output small, the elapsed-time wording, and truncation that
//! respects cell width instead of byte or character count. The transcript tests
//! pin these directly, and the renderer below is the only place a card's shape
//! is decided.

use std::time::Instant;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::keys::Key;
use crate::theme::changes::{self, RowStyle};
use crate::theme::symbols::Border;
use crate::theme::{Symbols, Theme};
use crate::transcript::{Decision, ToolCall, ToolState};
use crate::wrap::{self, Indented, Sides};

/// Preview rows kept by default, as `transcript.tool_output_lines` resolves.
pub const DEFAULT_OUTPUT_LINES: usize = 3;
/// Ceiling on kept rows: the setting is a preference, not a memory budget.
pub const MAX_OUTPUT_LINES: usize = 40;
/// Cells kept from one preview row; a single-line output can be megabytes.
pub const MAX_PREVIEW_CELLS: usize = 160;
/// Argument bytes kept while a call streams; the summary needs the first field.
pub const MAX_INPUT_BYTES: usize = 4096;
/// Lines of an argument the card shows under its title when the title has no
/// room for it; the rest are counted.
const MAX_ARGUMENT_LINES: usize = 6;

/// Columns a border row spends around its label: the corner and the line
/// before it, a space on each side of the label, and the closing corner.
const BORDER_COLUMNS: usize = 5;

/// The key whose value is drawn as a shell command, under a `$ ` prompt.
const COMMAND_KEY: &str = "command";

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

/// What the client kept of a call's output beyond the preview.
///
/// Gathered here rather than on the call so a line's shape stays the same in
/// every build: `key` is the key that expands the call, as the bindings in force
/// name it, and `text` is the whole output while the call is drawn expanded.
/// `kept` says the client still holds the text, which the marker needs even when
/// the call no longer names the key: a line that hid output keeps its count row
/// after the label moves to another call, so the two states take the same rows.
/// `selected` marks the call the expand key acts on, which is the one the view
/// shows.
#[derive(Debug, Clone, Copy, Default)]
pub struct Retained<'a> {
    /// The key that shows the rest of this call's output.
    pub key: Option<Key>,
    /// The whole output, when the call is drawn expanded.
    pub text: Option<&'a str>,
    /// Whether the client still holds this call's text.
    pub kept: bool,
    /// Whether this is the call the expand key acts on.
    pub selected: bool,
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
    primary(input).map(|(_, value)| one_line(&value))
}

/// The interesting argument as the call wrote it, and the key it came from.
fn primary(input: &str) -> Option<(&'static str, String)> {
    if input.trim().is_empty() {
        return None;
    }
    let found = if let Ok(serde_json::Value::Object(fields)) = serde_json::from_str::<serde_json::Value>(input) {
        PRIMARY_KEYS.iter().find_map(|key| match fields.get(*key) {
            Some(serde_json::Value::String(value)) => Some((*key, value.clone())),
            Some(serde_json::Value::Number(_) | serde_json::Value::Bool(_) | serde_json::Value::Null)
            | Some(serde_json::Value::Array(_) | serde_json::Value::Object(_))
            | None => None,
        })
    } else {
        PRIMARY_KEYS
            .iter()
            .find_map(|key| scan_string(input, key).map(|value| (*key, value)))
    };
    found.filter(|(_, value)| value.split_whitespace().next().is_some())
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
    let mut rows = Vec::new();
    let mut total_rows = 0usize;
    for row in output.lines() {
        total_rows += 1;
        if rows.len() < keep {
            if rows.is_empty() {
                rows.reserve(keep);
            }
            rows.push(truncate_cells(row, MAX_PREVIEW_CELLS));
        }
    }
    if total_rows == 0 {
        return None;
    }
    Some(OutputHead { rows, total_rows })
}

/// `text` cut to at most `width` cells, with a trailing `…` when it was cut.
///
/// A wide character that would overflow starts nothing: the cut lands before it
/// rather than splitting it, so the result is always valid text of a known
/// display width.
pub fn truncate_cells(text: &str, width: usize) -> String {
    let width = width.max(1);
    if text.bytes().all(|byte| (0x20..=0x7e).contains(&byte)) {
        if text.len() <= width {
            return text.to_string();
        }
        let budget = width - 1;
        let mut out = String::with_capacity(budget + '…'.len_utf8());
        out.push_str(&text[..budget]);
        out.push('…');
        return out;
    }
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
/// the sentence is stored whole and cannot be shortened at the source. A
/// recording reads this too: a call the gate held is an approval matter, and
/// the default recording leaves those out.
pub(crate) const GATE_REFUSAL: &str = "needs approval before it runs";
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
    let mut out = String::with_capacity(12);
    push_elapsed(&mut out, millis);
    out
}

/// Append `value` in base 10 directly into `out`.
pub(crate) fn push_u64(out: &mut String, mut value: u64) {
    let mut digits = [0u8; 20];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    for &byte in &digits[start..] {
        out.push(byte as char);
    }
}

/// Append [`format_elapsed`]'s representation directly into `out`.
pub(crate) fn push_elapsed(out: &mut String, millis: u64) {
    if millis < 60_000 {
        use std::fmt::Write as _;
        if write!(out, "{:.1}s", millis as f64 / 1000.0).is_err() {
            // `fmt::Write` for `String` is infallible.
        }
        return;
    }
    let minutes = millis / 60_000;
    let seconds = (millis % 60_000) / 1000;
    push_u64(out, minutes);
    out.push_str("m ");
    out.push((b'0' + (seconds / 10) as u8) as char);
    out.push((b'0' + (seconds % 10) as u8) as char);
    out.push('s');
}

/// The rows one tool call occupies on a pane `width` columns wide.
///
/// The rows are the ones the pane draws, already broken at the width.
pub fn lines(call: &ToolCall, theme: &Theme, symbols: Symbols, width: u16) -> Vec<Line<'static>> {
    wrap::Source::built(lines_with(call, theme, symbols, width, Retained::default()))
        .rows_from_end(width, wrap::FromEnd::new(0, usize::MAX))
}

/// The lines of one tool call's card, with the output the client kept for it.
///
/// An expanded call is drawn as its whole output rather than as the preview, and
/// its rows are left as the tool wrote them: the card wraps them, so a column
/// that lines up in the tool's own output still lines up on screen.
pub fn lines_with(
    call: &ToolCall,
    theme: &Theme,
    symbols: Symbols,
    width: u16,
    retained: Retained<'_>,
) -> Vec<Indented> {
    match rows_with(call, theme, symbols, width, retained) {
        CallRows::Built(rows) => rows,
        CallRows::Expanded {
            mut head,
            text,
            style,
            sides,
            foot,
        } => {
            // The whole output, one row at a time: a caller that draws it builds
            // every row, which is what the pane avoids by keeping the text.
            for row in text.lines() {
                head.push(Indented::boxed(
                    Line::from(Span::styled(row.to_string(), style.of(row))),
                    0,
                    sides,
                ));
            }
            head.push(foot);
            head
        }
    }
}

/// The lines one tool call occupies, before the whole output is expanded into
/// them.
///
/// A cut output drawn in full is the one case where the lines are not all
/// built: it is one line per row of the output, and the pane shows only the
/// rows near its end, so [`CallRows::Expanded`] hands over the text instead and
/// the pane makes the rows it draws.
pub(crate) enum CallRows<'a> {
    /// Every line is built: the title, the argument, a failure, the preview and
    /// its marker, and the closing row.
    Built(Vec<Indented>),
    /// The lines above the output, the output itself, and the closing row.
    Expanded {
        /// The lines above the output: the title, the argument, and a
        /// failure's message.
        head: Vec<Indented>,
        /// The whole kept output, one row per line.
        text: &'a str,
        /// The style every output row is drawn in, or the styles that tell an
        /// added line and a removed one apart.
        style: RowStyle,
        /// The card's sides, which every output row is drawn between.
        sides: Sides,
        /// The card's closing row.
        foot: Indented,
    },
}

/// The box one card is drawn in.
struct Card {
    /// The glyphs of the box.
    border: Border,
    /// The style of every part of the box.
    edge: Style,
    /// The pane's width, which every border row fills exactly.
    width: usize,
}

impl Card {
    /// The sides every row between the top and the bottom is drawn between.
    fn sides(&self) -> Sides {
        Sides {
            edge: self.border.vertical,
            style: self.edge,
        }
    }

    /// `line` inside the card, its continuation rows keeping `indent` columns.
    fn boxed(&self, line: Line<'static>, indent: usize) -> Indented {
        Indented::boxed(line, indent, self.sides())
    }

    /// Columns a border row's label may take.
    fn label_budget(&self) -> usize {
        self.width.saturating_sub(BORDER_COLUMNS + 1)
    }

    /// Columns the text of a row inside the card gets.
    fn inner(&self) -> usize {
        self.width.saturating_sub(2 * (self.border.vertical.width() + 1))
    }

    /// A border row: `open`, the line with `label` near its start, and `close`.
    ///
    /// The row is exactly the pane's width whenever the label fits its budget,
    /// so the closing corner lines up with the sides of the rows between.
    fn rule(&self, open: &'static str, close: &'static str, label: Vec<Span<'static>>) -> Indented {
        let line = self.border.horizontal;
        let mut used = open.width() + line.width() + close.width();
        let mut spans = Vec::with_capacity(label.len() + 4);
        spans.push(Span::styled(format!("{open}{line}"), self.edge));
        if !label.is_empty() {
            used += 2 + label.iter().map(|span| span.content.width()).sum::<usize>();
            spans.push(Span::raw(" "));
            spans.extend(label);
            spans.push(Span::raw(" "));
        }
        let fill = self.width.saturating_sub(used) / line.width().max(1);
        spans.push(Span::styled(format!("{}{close}", line.repeat(fill)), self.edge));
        Indented::plain(Line::from(spans))
    }
}

/// The lines of one tool call's card, keeping a cut output as the text it came
/// from.
///
/// The title row carries the outcome's glyph, the name and, when it fits, the
/// argument. An argument that does not fit, or that runs over several lines,
/// is drawn under the title instead, and an `Output` divider separates it from
/// what the call returned. A failure puts its message on the rows below the
/// title: the message is usually longer than the pane. The closing row says
/// what became of the call and how long it took.
pub(crate) fn rows_with<'a>(
    call: &ToolCall,
    theme: &Theme,
    symbols: Symbols,
    width: u16,
    retained: Retained<'a>,
) -> CallRows<'a> {
    // A call the approval gate refused never ran, so it is never drawn as a
    // failure: it waits under the in-progress glyph, and once the request is
    // decided it says what the answer was. The dot is the quietest marker the
    // client has and it is not the tick of a call that ran.
    let (glyph, color) = match &call.state {
        ToolState::Running | ToolState::Waiting(_) => (symbols.tool_running, theme.transcript.tool_running),
        ToolState::Done => (symbols.tool_done, theme.transcript.tool_done),
        ToolState::Failed(_) => (symbols.tool_error, theme.transcript.tool_error),
        ToolState::Settled(Decision::Allowed | Decision::AllowedAlways, _) => (symbols.bullet, theme.transcript.notice),
        ToolState::Settled(Decision::Denied | Decision::Unanswered, _) => {
            (symbols.tool_error, theme.transcript.tool_error)
        }
    };
    // A card that did not get what it asked for is boxed in the error colour,
    // so a failure is found by scanning the edges, not by reading every title.
    let failed = matches!(
        call.state,
        ToolState::Failed(_) | ToolState::Settled(Decision::Denied | Decision::Unanswered, _)
    );
    let card = Card {
        border: symbols.border,
        edge: Style::default().fg(if failed { color } else { theme.core.border }),
        width: usize::from(width),
    };
    let muted = Style::default().fg(theme.core.muted);
    // Neither a parked call nor a decided one ran, so neither has a duration,
    // and neither previews the refusal the harness echoed back as output.
    let settled = matches!(call.state, ToolState::Waiting(_) | ToolState::Settled(_, _));

    let mut name = Style::default()
        .fg(theme.transcript.tool_title)
        .add_modifier(Modifier::BOLD);
    if retained.selected {
        // The view decides which call the key acts on, so the card has to show
        // it even where the marker's key label does not fit on one row.
        name = name.add_modifier(Modifier::UNDERLINED);
    }
    let budget = card.label_budget();
    let lead = glyph.width() + 1;
    let shown_name = truncate_cells(&call.name, budget.saturating_sub(lead));
    let mut title = vec![
        Span::styled(glyph, Style::default().fg(color)),
        Span::raw(" "),
        Span::styled(shown_name, name),
    ];
    // The argument stays in the title only when all of it fits there: a cut
    // command reads as a different command, so one that does not fit is
    // drawn whole under the title instead.
    let room = budget.saturating_sub(lead + call.name.width() + 1);
    let mut below = None;
    if let Some((key, value)) = primary(&call.input) {
        let summary = one_line(&value);
        if value.contains('\n') || summary.width() > room {
            below = Some((key, value));
        } else {
            title.push(Span::raw(" "));
            title.push(Span::styled(summary, muted));
        }
    }
    let mut rows = vec![card.rule(card.border.top_left, card.border.top_right, title)];

    if let Some((key, value)) = &below {
        argument_rows(&mut rows, &card, key, value, theme, muted);
    }
    let divider = |rows: &mut Vec<Indented>| {
        if below.is_some() {
            rows.push(card.rule(
                card.border.tee_left,
                card.border.tee_right,
                vec![Span::styled("Output", muted)],
            ));
        }
    };

    let mut error_text: Option<&str> = None;
    if let ToolState::Failed(message) = &call.state {
        error_text = Some(message.as_str());
        divider(&mut rows);
        rows.push(card.boxed(
            Line::from(Span::styled(
                truncate_cells(display_error(message), MAX_PREVIEW_CELLS),
                Style::default().fg(color),
            )),
            0,
        ));
    }
    let failure_divided = error_text.is_some();

    let mut status = call.state.label().to_string();
    if !settled && let Some(millis) = call.duration_ms {
        status.push(' ');
        push_elapsed(&mut status, millis);
    }
    let status = truncate_cells(&status, budget);
    let foot = card.rule(
        card.border.bottom_left,
        card.border.bottom_right,
        vec![Span::styled(status, Style::default().fg(color))],
    );

    // The change palette colours an added or removed line and leaves every
    // other row as the output's own quiet style. The row keeps its own sign
    // either way, so the colour is never the only thing that marks a change,
    // and a block without a diff keeps one style: a leading sign means a change
    // only inside a diff.
    let plain = Style::default().fg(theme.transcript.notice);
    if let Some(text) = retained.text.filter(|_| !settled) {
        // An expanded call draws every row it kept. That is true even where the
        // preview would have repeated the failure text above it: the press was a
        // request for the output itself, not for the summary the card already
        // carries. The output stays a borrowed str: a row is made only where the
        // pane draws one.
        if !failure_divided {
            divider(&mut rows);
        }
        return CallRows::Expanded {
            head: rows,
            text,
            style: RowStyle::for_theme(theme, plain, || changes::looks_like_diff_text(text)),
            sides: card.sides(),
            foot,
        };
    }
    if let Some(output) = call.output.as_ref().filter(|_| !settled) {
        // A failed call's stored output is the failure text itself, so drawing
        // it again would say the same thing twice.
        // The failure row is cut with a marker, so the marker comes off before
        // the comparison: what is left must be a prefix of the message itself.
        let repeats_error = output.rows.first().is_some_and(|first| {
            let shown = first.trim_end_matches('…').trim();
            !shown.is_empty() && error_text.is_some_and(|message| message.contains(shown))
        });
        if !repeats_error {
            if !failure_divided {
                divider(&mut rows);
            }
            let style = RowStyle::for_theme(theme, plain, || changes::looks_like_diff(&output.rows));
            for row in &output.rows {
                rows.push(card.boxed(Line::from(Span::styled(row.clone(), style.of(row))), 0));
            }
            // A row cut at its right edge hides output with no rows to count, so
            // the count alone would say the preview is whole. A call whose text
            // the client no longer holds (it fell out of the window) gets no
            // marker: a count promising a view that is gone is worse than none.
            //
            // The marker is one row in every state it is drawn in. The key label
            // is added only where the whole marker still fits on that one row,
            // because the label moves to a new call when the next output arrives
            // and the card it leaves is never measured again: a label that pushed
            // the marker onto a second row would shift a held view. The key keeps
            // working where the hint is dropped.
            let marker = match (output.hidden_rows(), retained.kept) {
                (0, false) => None,
                (0, true) => Some(String::from("cut")),
                (hidden, _) => Some(format!("+{hidden} lines")),
            };
            let marker = marker.map(|row| match retained.key {
                Some(key) => {
                    let labelled = format!("{row} · {key} expands");
                    if UnicodeWidthStr::width(labelled.as_str()) <= card.inner() {
                        labelled
                    } else {
                        row
                    }
                }
                None => row,
            });
            if let Some(marker) = marker {
                rows.push(card.boxed(Line::from(Span::styled(marker, style.plain())), 0));
            }
        }
    }
    rows.push(foot);
    CallRows::Built(rows)
}

/// The rows of an argument the title had no room for.
///
/// A shell command is drawn under a `$ ` prompt with its later lines under the
/// first one's text; any other argument is drawn as it is. Each line is cut to
/// the preview's width and only the first [`MAX_ARGUMENT_LINES`] are drawn, so
/// a long script never pushes the call's own result off the pane.
fn argument_rows(rows: &mut Vec<Indented>, card: &Card, key: &str, value: &str, theme: &Theme, muted: Style) {
    let text = Style::default().fg(theme.core.text);
    let command = key == COMMAND_KEY;
    let indent = if command { 2 } else { 0 };
    let total = value.lines().count();
    for (index, line) in value.lines().take(MAX_ARGUMENT_LINES).enumerate() {
        let mut spans = Vec::with_capacity(2);
        if command {
            spans.push(Span::styled(if index == 0 { "$ " } else { "  " }, muted));
        }
        spans.push(Span::styled(truncate_cells(line.trim_end(), MAX_PREVIEW_CELLS), text));
        rows.push(card.boxed(Line::from(spans), indent));
    }
    if total > MAX_ARGUMENT_LINES {
        let mut more = String::from("… +");
        push_u64(&mut more, u64::try_from(total - MAX_ARGUMENT_LINES).unwrap_or(u64::MAX));
        more.push_str(" lines");
        rows.push(card.boxed(Line::from(Span::styled(more, muted)), indent));
    }
}

/// When a call started, so a finished line can say how long it took.
///
/// Kept beside the pure functions because the duration wording lives here; the
/// reading itself belongs to the call that recorded it.
pub(crate) fn elapsed_since(started: Option<Instant>) -> Option<u64> {
    started.map(|start| u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX))
}
