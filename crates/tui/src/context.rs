//! The `/context` report: what fills the context window, as the daemon priced it.
//!
//! The command answers with the next request's own material — the system prompt,
//! the tool catalog and the visible conversation — and with token estimates for
//! each part. That payload is far too large to read as text: this module keeps
//! the numbers and drops the text, so the transcript draws a breakdown sized to
//! the pane instead of a screenful of JSON.
//!
//! Every number here is an estimate from the daemon's estimator, not the
//! provider's usage report, so the header and the docs say so.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::item_lines::Look;
use crate::statusline::compact;
use crate::wrap::Indented;

/// Columns between the left edge and a row's label.
const INDENT: usize = 2;
/// Columns kept empty between the label, the count and the bar.
const GAP: usize = 1;
/// The narrowest bar worth drawing; below it the rows carry counts only.
const MIN_BAR: usize = 4;

/// What the daemon reported about the next request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextReport {
    /// Estimated tokens of the system prompt.
    pub system: u64,
    /// Estimated tokens of the tool catalog the request carries.
    pub tools: u64,
    /// Tools in that catalog.
    pub tool_count: usize,
    /// Tools deferred out of the cached prefix, which still cost tokens.
    pub deferred_count: usize,
    /// Estimated tokens of the visible conversation.
    pub messages: u64,
    /// Messages in that conversation.
    pub message_count: usize,
    /// Tokens the model can hold, or zero when the build does not know.
    pub window: u64,
}

impl ContextReport {
    /// Read the numbers out of a runtime command's `data` object.
    ///
    /// `None` means the payload carries no breakdown, which is what a daemon
    /// older than the field sends: the caller then shows the command's message
    /// rather than an empty panel.
    pub fn from_data(data: &serde_json::Value) -> Option<Self> {
        let tokens = data.get("tokens")?;
        let count = |value: &serde_json::Value| value.as_u64().unwrap_or(0);
        let count_usize = |value: &serde_json::Value| usize::try_from(count(value)).unwrap_or(usize::MAX);
        let report = Self {
            system: count(&tokens["system"]["total"]),
            tools: count(&tokens["tools"]["total"]),
            tool_count: count_usize(&tokens["tools"]["count"]),
            deferred_count: count_usize(&tokens["tools"]["deferred_count"]),
            messages: count(&tokens["messages"]["total"]),
            message_count: count_usize(&tokens["messages"]["count"]),
            // Read from the breakdown so one object answers every question; the
            // window the command reports beside it is the same number.
            window: count(&tokens["context_window"]),
        };
        Some(report)
    }

    /// Estimated tokens the next request spends before the model answers.
    pub fn used(&self) -> u64 {
        self.system.saturating_add(self.tools).saturating_add(self.messages)
    }

    /// The share of the window in use, or `None` when the window is unknown.
    pub fn percent(&self) -> Option<u64> {
        if self.window == 0 {
            return None;
        }
        Some(self.used().saturating_mul(100) / self.window)
    }

    /// The rows under the header, in the order the pane draws them.
    fn rows(&self) -> Vec<Row> {
        let mut rows = vec![
            Row {
                label: "system prompt".to_string(),
                tokens: self.system,
                free: false,
            },
            Row {
                label: match self.tool_count {
                    0 => "tools".to_string(),
                    count => format!("tools · {count}"),
                },
                tokens: self.tools,
                free: false,
            },
            Row {
                label: match self.message_count {
                    0 => "messages".to_string(),
                    count => format!("messages · {count}"),
                },
                tokens: self.messages,
                free: false,
            },
        ];
        if let Some(deferred) = Some(self.deferred_count).filter(|count| *count > 0) {
            rows[1].label.push_str(&format!(" ({deferred} deferred)"));
        }
        if self.window > 0 {
            rows.push(Row {
                label: "free".to_string(),
                tokens: self.window.saturating_sub(self.used()),
                free: true,
            });
        }
        rows
    }
}

/// One measured line of the report.
struct Row {
    label: String,
    tokens: u64,
    free: bool,
}

/// The lines the report is drawn with.
pub(crate) fn lines(report: &ContextReport, look: &Look<'_>) -> Vec<Indented> {
    let muted = look.theme.core.muted;
    let rows = report.rows();
    let label_width = rows.iter().map(|row| row.label.chars().count()).max().unwrap_or(0);
    let counts: Vec<String> = rows.iter().map(|row| compact(row.tokens)).collect();
    let count_width = counts.iter().map(|count| count.chars().count()).max().unwrap_or(0);
    let width = usize::from(look.width);
    let bar_width = width.saturating_sub(INDENT + label_width + GAP + count_width + GAP);
    let bar_width = if bar_width >= MIN_BAR { bar_width } else { 0 };

    let mut lines: Vec<Indented> = Vec::with_capacity(rows.len() + 1);
    lines.push(Indented::plain(header(report, look)));
    for (row, count) in rows.iter().zip(counts) {
        let mut spans = vec![
            Span::raw(" ".repeat(INDENT)),
            Span::styled(
                format!("{:<label_width$}{}", row.label, " ".repeat(GAP)),
                Style::default().fg(muted),
            ),
            Span::styled(
                format!("{count:>count_width$}"),
                Style::default().fg(look.theme.core.text),
            ),
        ];
        if bar_width > 0 {
            spans.push(Span::raw(" ".repeat(GAP)));
            spans.push(bar(report, row, bar_width, look));
        }
        lines.push(Indented::plain(Line::from(spans)));
    }
    lines
}

/// The line above the rows: what is in use, out of what, and how close that is.
fn header(report: &ContextReport, look: &Look<'_>) -> Line<'static> {
    let accent = Style::default().fg(look.theme.core.accent).add_modifier(Modifier::BOLD);
    let used = compact(report.used());
    let mut spans = vec![Span::styled("context", accent)];
    let summary = match report.percent() {
        Some(percent) => format!(" · ~{used} of {} tokens ({percent}%)", compact(report.window)),
        None => format!(" · ~{used} tokens in play"),
    };
    spans.push(Span::styled(summary, Style::default().fg(look.theme.core.muted)));
    if report.percent().is_some_and(|percent| percent >= 80) {
        spans.push(Span::styled(
            " · near the limit",
            Style::default().fg(look.theme.core.warning),
        ));
    }
    Line::from(spans)
}

/// The proportional bar of one row, scaled to the whole window.
///
/// A section that costs anything always shows one cell, so a small part of a
/// large window is still visible; the free row shows the cells nobody claimed.
fn bar(report: &ContextReport, row: &Row, width: usize, look: &Look<'_>) -> Span<'static> {
    let window = report.window.max(1);
    let cells = |tokens: u64| -> usize {
        if tokens == 0 {
            return 0;
        }
        let scaled = (tokens.saturating_mul(width as u64) / window) as usize;
        scaled.clamp(1, width)
    };
    let (glyph, count, style) = if row.free {
        (
            look.symbols.bar_empty,
            cells(row.tokens),
            Style::default().fg(look.theme.core.border),
        )
    } else {
        (
            look.symbols.bar_full,
            cells(row.tokens),
            Style::default().fg(look.theme.core.accent),
        )
    };
    Span::styled(glyph.repeat(count), style)
}
