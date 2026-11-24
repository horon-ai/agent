//! The lines one transcript item is made of, before they are wrapped.
//!
//! The pane draws the newest items first and stops when it fills, so an item's
//! lines are built only when they are about to be shown. Each line carries the
//! columns its continuation rows keep ([`Indented`]), because a wrapped item has
//! to stay under the text its prefix introduced rather than sliding back to the
//! first column.
//!
//! Assistant text is the one kind that is rendered as Markdown, and only while
//! `transcript.markdown` says so: a user's own words, a tool line and a notice
//! are shown the way they were produced.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::App;
use crate::markdown;
use crate::settings::Settings;
use crate::theme::{Symbols, Theme};
use crate::tool_expand;
use crate::tool_line;
use crate::transcript::{Item, TurnMetrics};
use crate::wrap::{self, Indented};

/// Columns a wrapped continuation row keeps under a prefixed item.
///
/// It is the width of a one-glyph prefix and its space, which is what `> ` and
/// a notice bullet both are, and it is also the pad a second logical line gets,
/// so both kinds of continuation line up on the same column.
pub(crate) const INDENT: usize = 2;

/// Columns a note's continuation rows keep, under the text after `note: `.
pub(crate) const NOTE_INDENT: usize = 6;

/// What one item's lines are drawn with, gathered once per block.
pub(crate) struct Look<'a> {
    pub(crate) theme: &'a Theme,
    pub(crate) symbols: Symbols,
    pub(crate) width: u16,
    pub(crate) markdown: bool,
    pub(crate) visual_blocks: bool,
}

impl<'a> Look<'a> {
    /// The look of a frame drawing `app` at `width`.
    pub(crate) fn new(app: &'a App, width: u16) -> Self {
        Self {
            theme: &app.theme,
            symbols: app.symbols,
            width,
            markdown: markdown_enabled(&app.settings),
            visual_blocks: visual_blocks_enabled(&app.settings),
        }
    }
}

/// Whether assistant text is rendered as Markdown.
///
/// Read from the resolved settings rather than cached in the app: the settings
/// screen flips it while the transcript is on screen, so every frame that has to
/// agree with the measurement it just took reads it fresh.
pub(crate) fn markdown_enabled(settings: &Settings) -> bool {
    settings.boolean("transcript.markdown").unwrap_or(true)
}

/// Whether a supported visual block is drawn rather than left as source.
///
/// Read fresh for the same reason as `markdown_enabled`: the settings screen
/// flips it while the transcript is on screen, and the frame that draws a block
/// has to agree with the height the scrollback measured for it.
pub(crate) fn visual_blocks_enabled(settings: &Settings) -> bool {
    settings.boolean("transcript.visual_blocks").unwrap_or(true)
}

/// The lines of one item, in display order.
///
/// `expand` is consulted for tool items only: the pane passes the frame it built
/// for this draw, so a call whose output was cut is drawn in full without the
/// look having to carry state that belongs to the transcript.
pub(crate) fn lines(item: &Item, look: &Look<'_>, expand: Option<&tool_expand::Frame<'_>>) -> Vec<Indented> {
    match item {
        Item::User(text) => prefixed(
            Span::styled("> ", Style::default().fg(look.theme.core.accent)),
            text,
            Style::default().fg(look.theme.transcript.user_text),
        ),
        // A queued message is accepted but not sent yet, so it is drawn dim
        // and worded as waiting: the same color as a notice, because nothing
        // is wrong with it.
        Item::Queued(text) => prefixed(
            Span::styled("queued: ", Style::default().fg(look.theme.transcript.notice)),
            text,
            Style::default()
                .fg(look.theme.transcript.user_text)
                .add_modifier(Modifier::DIM),
        ),
        Item::Assistant(text) => answer(text, look),
        Item::Tool(call) => {
            if hidden(call, expand) {
                return Vec::new();
            }
            let retained = match expand {
                Some(expand) => expand.retained(&call.call_id),
                None => tool_line::Retained::default(),
            };
            indented(tool_line::lines_with(
                call,
                look.theme,
                look.symbols,
                look.width,
                retained,
            ))
        }
        Item::Notice(text) => notice(text, look),
        Item::Warning(text) => prefixed(Span::raw("! "), text, Style::default().fg(look.theme.core.error)),
        // Reasoning is drawn dimmed and indented, and the theme gains no token:
        // a new required token would reject every theme file already written,
        // and the notice color is already the dimmest text the client has.
        Item::Reasoning(text) => reasoning(text, look),
        Item::Context(report) => crate::context::lines(report, look),
        Item::Report(report) => crate::report::lines(report, look),
        Item::A2ui(card) => crate::a2ui::lines(card, look),
        Item::TurnUsage(metrics) => turn_usage(metrics, look),
        Item::CacheMiss(miss) => cache_miss(miss, look),
    }
}

/// Whether a finished call is left out of the pane.
///
/// The setting hides the record of what ran. A call still in progress and one
/// waiting for approval are the work the reader has to see or answer, so they
/// are drawn either way.
fn hidden(call: &crate::transcript::ToolCall, expand: Option<&tool_expand::Frame<'_>>) -> bool {
    expand.is_some_and(|frame| frame.hides_finished()) && call.finished_activity()
}

/// A completed turn's footer: how long it ran and what it spent.
///
/// Only the fields the turn reported are drawn, and the separator appears only
/// between two parts: a turn whose calls reported no usage shows its time
/// alone, and a zero never stands in for a missing count.
fn turn_usage(metrics: &TurnMetrics, look: &Look<'_>) -> Vec<Indented> {
    let mut text = String::with_capacity(if metrics.tokens.is_some() { 36 } else { 12 });
    tool_line::push_elapsed(&mut text, metrics.elapsed_ms);
    if let Some((input, output)) = metrics.tokens {
        let separator = if look.symbols.separator.is_empty() {
            "·"
        } else {
            look.symbols.separator
        };
        text.push(' ');
        text.push_str(separator);
        text.push_str(" ↑");
        crate::statusline::push_compact(&mut text, input);
        text.push_str(" ↓");
        crate::statusline::push_compact(&mut text, output);
    }
    let style = Style::default().fg(look.theme.transcript.notice);
    vec![Indented::plain(Line::from(Span::styled(text, style)))]
}

/// The marker a request leaves when the provider had to serve it without its
/// cached prefix.
///
/// Drawn as one dim row: it belongs to the request above it and says what the
/// resend cost, and it is neither the model's answer nor an error the reader can
/// act on. The wording is the client's own, so it names the cause in words
/// rather than echoing the daemon's reason id, and it carries no part of the
/// prompt: the daemon never sends one.
fn cache_miss(miss: &crate::transcript::CacheMiss, look: &Look<'_>) -> Vec<Indented> {
    let style = Style::default().fg(look.theme.transcript.notice);
    let mut text = String::with_capacity(look.symbols.bullet.len() + 64);
    text.push_str(look.symbols.bullet);
    text.push_str(" cache miss: ");
    miss.push_summary(&mut text);
    vec![Indented::new(Line::from(Span::styled(text, style)), INDENT)]
}

/// The lines of one item, keeping a call's whole output as the text it came
/// from.
///
/// Everything but a tool call drawn expanded is a short list of lines and is
/// built here. That one case is one line per row of the output, and the pane
/// shows only its last rows, so the output is handed over as the borrowed text
/// and the pane makes the rows it draws; see [`wrap::Source`].
pub(crate) fn source<'a>(item: &Item, look: &Look<'_>, expand: Option<&tool_expand::Frame<'a>>) -> wrap::Source<'a> {
    let Item::Tool(call) = item else {
        return wrap::Source::built(lines(item, look, expand));
    };
    if hidden(call, expand) {
        return wrap::Source::built(Vec::new());
    }
    let retained = match expand {
        Some(expand) => expand.retained(&call.call_id),
        None => tool_line::Retained::default(),
    };
    match tool_line::rows_with(call, look.theme, look.symbols, look.width, retained) {
        tool_line::CallRows::Built(rows) => wrap::Source::built(indented(rows)),
        tool_line::CallRows::Expanded { head, text, style } => wrap::Source::Output {
            head: indented(head),
            text,
            prefix: tool_line::OUTPUT_PREFIX,
            style,
            indent: INDENT,
            // A note never lands on a tool row: only user and assistant
            // messages can be addressed, so there is nothing below the output.
            tail: Vec::new(),
        },
    }
}

/// A tool call's rows as lines.
///
/// The head row introduces the call; every row under it (the failure text, the
/// output preview, the count of rows the preview hides) is written after a
/// two-column pad, so a wrapped row has to stay under that text rather than
/// slide back to the first column.
fn indented(lines: Vec<Line<'static>>) -> Vec<Indented> {
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                Indented::plain(line)
            } else {
                Indented::new(line, INDENT)
            }
        })
        .collect()
}

/// The reader's note on one message, drawn under it.
///
/// A note is one line by construction (`crate::notes::NoteStore::set`), so this
/// makes one [`Indented`] rather than a list. The label is the accent colour so
/// the note reads as the reader's mark rather than as part of the message, and
/// the text is dim so it never competes with the conversation it describes.
pub(crate) fn note(text: &str, look: &Look<'_>) -> Indented {
    let line = Line::from(vec![
        Span::styled("note: ", Style::default().fg(look.theme.core.accent)),
        Span::styled(
            text.to_string(),
            Style::default()
                .fg(look.theme.transcript.notice)
                .add_modifier(Modifier::ITALIC),
        ),
    ]);
    Indented::new(line, NOTE_INDENT)
}

/// An assistant message, committed or still streaming.
pub(crate) fn answer(text: &str, look: &Look<'_>) -> Vec<Indented> {
    if look.markdown {
        let options = markdown::Options {
            visual_blocks: look.visual_blocks,
        };
        return markdown::render(text, look.theme, look.symbols, look.width, options);
    }
    plain_lines(text, Style::default().fg(look.theme.transcript.assistant_text), 0)
}

/// A message's reasoning, dimmed and indented under its answer.
pub(crate) fn reasoning(text: &str, look: &Look<'_>) -> Vec<Indented> {
    let style = Style::default()
        .fg(look.theme.transcript.notice)
        .add_modifier(Modifier::ITALIC);
    plain_lines(text, style, INDENT)
}

/// Every line of `text` in one style, with `indent` kept on continuation rows.
fn plain_lines(text: &str, style: Style, indent: usize) -> Vec<Indented> {
    text.lines()
        .map(|line| {
            let body = if indent == 0 {
                line.to_string()
            } else {
                format!("{}{line}", " ".repeat(indent))
            };
            Indented::new(Line::from(Span::styled(body, style)), indent)
        })
        .collect()
}

const INDENT_PAD: &str = const {
    assert!(INDENT == 2);
    "  "
};

/// One line per logical line, the first under `prefix` and the rest under it.
fn prefixed(prefix: Span<'static>, text: &str, style: Style) -> Vec<Indented> {
    if text.is_empty() {
        return Vec::new();
    }
    if !text.contains('\n') {
        let line = text.strip_suffix('\r').unwrap_or(text);
        return vec![Indented::new(
            Line::from(vec![prefix, Span::styled(line.to_string(), style)]),
            INDENT,
        )];
    }
    text.lines()
        .enumerate()
        .map(|(index, line)| {
            let built = if index == 0 {
                Line::from(vec![prefix.clone(), Span::styled(line.to_string(), style)])
            } else {
                Line::from(vec![Span::raw(INDENT_PAD), Span::styled(line.to_string(), style)])
            };
            Indented::new(built, INDENT)
        })
        .collect()
}

/// Cells of text a description column has to leave on the pane to be worth
/// aligning to.
const MIN_READABLE: usize = 12;

/// The lines of a notice, each wrapped under what its row introduces.
///
/// Most notices are prose and hang under the bullet like every other item. A
/// `/help` listing is two columns, and a description that wraps back to the
/// row's first column reads as a new entry, so those rows hang under their own
/// description instead.
fn notice(text: &str, look: &Look<'_>) -> Vec<Indented> {
    let style = Style::default().fg(look.theme.transcript.notice);
    let prefix = Span::styled(format!("{} ", look.symbols.bullet), style);
    let prefix_width = UnicodeWidthStr::width(look.symbols.bullet) + 1;
    let width = usize::from(look.width);
    text.lines()
        .enumerate()
        .map(|(index, line)| {
            let (built, offset) = if index == 0 {
                (
                    Line::from(vec![prefix.clone(), Span::styled(line.to_string(), style)]),
                    prefix_width,
                )
            } else {
                (
                    Line::from(vec![Span::raw(INDENT_PAD), Span::styled(line.to_string(), style)]),
                    INDENT,
                )
            };
            Indented::new(built, hang_column(line, offset, width))
        })
        .collect()
}

/// The column a wrapped notice row keeps.
///
/// It is the description's own column when that leaves a readable line, since
/// that is what makes a wrapped row read as the continuation of one entry. A
/// description too far right for the pane gives way to the row's own text,
/// which is never left of the row's first word.
fn hang_column(line: &str, offset: usize, width: usize) -> usize {
    let Some((text, description)) = row_columns(line) else {
        return offset;
    };
    if offset + description + MIN_READABLE <= width {
        offset + description
    } else {
        offset + text
    }
}

/// Where a listing row's own text and its description start, in cells.
///
/// A listing row is the shape `/help` prints: an indented row whose left column
/// is followed by a run of two or more spaces and then the description. Prose
/// that happens to hold two spaces is not one, because it is not indented.
fn row_columns(line: &str) -> Option<(usize, usize)> {
    let body = line.trim_start_matches(' ');
    if body.is_empty() || body.len() == line.len() {
        return None;
    }
    let text = UnicodeWidthStr::width(&line[..line.len() - body.len()]);
    let mut width = text;
    let mut spaces = 0usize;
    let mut seen = false;
    for character in body.chars() {
        let cells = UnicodeWidthChar::width(character).unwrap_or(0);
        width += cells;
        if character == ' ' {
            spaces += cells;
            continue;
        }
        if seen && spaces >= 2 {
            return Some((text, width - cells));
        }
        seen = true;
        spaces = 0;
    }
    None
}
