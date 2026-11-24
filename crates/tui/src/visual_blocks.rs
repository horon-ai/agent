//! The one diagram syntax the transcript draws, and the limits around it.
//!
//! A model can write a `diagram` fence whose body is one line of labels joined
//! by `->`. A terminal can line that flow up without another process, a fetch
//! or a second grammar: the labels are the text already in the message. This
//! module owns that one fixed shape. Every other diagram or image kind, and
//! every flow past the limits below, is left as the source it was.
//!
//! The drawing is ASCII: brackets around a label, `->` between labels, so it
//! reads the same in a terminal without line-drawing glyphs. Nothing here reads
//! a URL, opens a file, runs a command or follows the text into a second pass.
//! The parsing is a single scan over characters and the size of a flow is
//! bounded by the constants below and by the pane width. A control character in
//! a label makes the whole fence fall back to its source, so a visual block
//! never turns into an escape sequence the model chose.
//!
//! A label is drawn as the text it holds. Marks inside it, a link, a URL and an
//! angle-bracket tag are characters to line up, not something to interpret:
//! this syntax has no second grammar and no resource behind it.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::wrap::Indented;

/// The most rows a diagram may have: one line of labels, nothing else.
pub(crate) const MAX_ROWS: usize = 1;

/// The most labels one flow may have.
pub(crate) const MAX_LABELS: usize = 8;

/// The most characters one label may have.
pub(crate) const MAX_LABEL_CHARS: usize = 24;

/// The most columns one label may take, so one wide word cannot blow the row.
pub(crate) const MAX_LABEL_COLUMNS: usize = 24;

/// The most bytes of a diagram fence's body.
pub(crate) const MAX_SOURCE_BYTES: usize = 4 * 1024;

/// Columns ` -> ` between two labels takes.
const ARROW_COLUMNS: usize = 4;

/// The styles a flow is drawn with.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Ink {
    /// A label and its brackets.
    pub(crate) node: Style,
    /// The arrow between labels.
    pub(crate) arrow: Style,
}

/// A flow drawn as one ASCII row, or `None` when it is not one to draw.
///
/// `body` is the fence body as written. Exactly one non-blank line is the flow;
/// a fence with no line or with more than one line is not this shape and is
/// returned as `None`, which the caller draws as source. The labels are split
/// on `->`, each one is trimmed, and the whole row has to fit `width`: a flow
/// wider than the pane would wrap into a row that no longer reads as a flow, so
/// the source is drawn instead.
pub(crate) fn flow(body: &[&str], width: u16, ink: Ink) -> Option<Vec<Indented>> {
    let mut source_bytes = 0usize;
    let mut rows = 0usize;
    let mut flow_line: Option<&str> = None;
    for raw in body {
        source_bytes += raw.len() + 1;
        if source_bytes > MAX_SOURCE_BYTES {
            return None;
        }
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            rows += 1;
            if rows > MAX_ROWS {
                return None;
            }
            if flow_line.is_none() {
                flow_line = Some(trimmed);
            }
        }
    }
    if rows != MAX_ROWS {
        return None;
    }
    let line = flow_line?;

    let mut labels = [""; MAX_LABELS];
    let mut count = 0usize;
    let mut total_width = 0usize;
    for label in line.split("->").map(str::trim) {
        if count >= MAX_LABELS || label.is_empty() {
            return None;
        }
        let label_cols = if label.bytes().all(|byte| (0x20..=0x7e).contains(&byte)) {
            if label.len() > MAX_LABEL_CHARS || label.len() > MAX_LABEL_COLUMNS {
                return None;
            }
            label.len()
        } else {
            if label.chars().count() > MAX_LABEL_CHARS || label.chars().any(char::is_control) {
                return None;
            }
            let cols = UnicodeWidthStr::width(label);
            if cols > MAX_LABEL_COLUMNS {
                return None;
            }
            cols
        };
        labels[count] = label;
        count += 1;
        total_width += label_cols + 4;
    }
    if count < 2 {
        return None;
    }
    total_width += (count - 1) * ARROW_COLUMNS;
    if total_width > usize::from(width) {
        return None;
    }
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(count * 2 - 1);
    for (index, label) in labels[..count].iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" -> ", ink.arrow));
        }
        let mut node = String::with_capacity(label.len() + 4);
        node.push_str("[ ");
        node.push_str(label);
        node.push_str(" ]");
        spans.push(Span::styled(node, ink.node));
    }
    Some(vec![Indented::plain(Line::from(spans))])
}
