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
    if body.iter().map(|line| line.len() + 1).sum::<usize>() > MAX_SOURCE_BYTES {
        return None;
    }
    let rows = body
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .count();
    if rows != MAX_ROWS {
        return None;
    }
    let Some(line) = body.iter().map(|line| line.trim()).find(|line| !line.is_empty()) else {
        return None;
    };
    let label_count = line.split("->").count();
    if !(2..=MAX_LABELS).contains(&label_count) {
        return None;
    }
    let mut nodes: Vec<String> = Vec::with_capacity(label_count);
    for label in line.split("->").map(str::trim) {
        if label.is_empty()
            || label.chars().count() > MAX_LABEL_CHARS
            || UnicodeWidthStr::width(label) > MAX_LABEL_COLUMNS
            || label.chars().any(char::is_control)
        {
            return None;
        }
        nodes.push(format!("[ {label} ]"));
    }
    let total: usize = nodes
        .iter()
        .map(|node| UnicodeWidthStr::width(node.as_str()))
        .sum::<usize>()
        + (nodes.len() - 1) * ARROW_COLUMNS;
    if total > usize::from(width) {
        return None;
    }
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(nodes.len() * 2);
    for (index, node) in nodes.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" -> ", ink.arrow));
        }
        spans.push(Span::styled(node, ink.node));
    }
    Some(vec![Indented::plain(Line::from(spans))])
}
