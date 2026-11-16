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

use crate::app::App;
use crate::markdown;
use crate::settings::Settings;
use crate::theme::{Symbols, Theme};
use crate::tool_line;
use crate::transcript::Item;
use crate::wrap::Indented;

/// Columns a wrapped continuation row keeps under a prefixed item.
///
/// It is the width of a one-glyph prefix and its space, which is what `> ` and
/// a notice bullet both are, and it is also the pad a second logical line gets,
/// so both kinds of continuation line up on the same column.
pub(crate) const INDENT: usize = 2;

/// What one item's lines are drawn with, gathered once per block.
pub(crate) struct Look<'a> {
    pub(crate) theme: &'a Theme,
    pub(crate) symbols: Symbols,
    pub(crate) width: u16,
    pub(crate) markdown: bool,
}

impl<'a> Look<'a> {
    /// The look of a frame drawing `app` at `width`.
    pub(crate) fn new(app: &'a App, width: u16) -> Self {
        Self {
            theme: &app.theme,
            symbols: app.symbols,
            width,
            markdown: markdown_enabled(&app.settings),
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

/// The lines of one item, in display order.
pub(crate) fn lines(item: &Item, look: &Look<'_>) -> Vec<Indented> {
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
        Item::Tool(call) => tool_line::lines(call, look.theme, look.symbols, look.width)
            .into_iter()
            .map(Indented::plain)
            .collect(),
        Item::Notice(text) => prefixed(
            Span::styled(
                format!("{} ", look.symbols.bullet),
                Style::default().fg(look.theme.transcript.notice),
            ),
            text,
            Style::default().fg(look.theme.transcript.notice),
        ),
        Item::Warning(text) => prefixed(Span::raw("! "), text, Style::default().fg(look.theme.core.error)),
        // Reasoning is drawn dimmed and indented, and the theme gains no token:
        // a new required token would reject every theme file already written,
        // and the notice color is already the dimmest text the client has.
        Item::Reasoning(text) => reasoning(text, look),
    }
}

/// An assistant message, committed or still streaming.
pub(crate) fn answer(text: &str, look: &Look<'_>) -> Vec<Indented> {
    if look.markdown {
        return markdown::render(text, look.theme, look.symbols, look.width);
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

/// One line per logical line, the first under `prefix` and the rest under it.
fn prefixed(prefix: Span<'static>, text: &str, style: Style) -> Vec<Indented> {
    let pad = " ".repeat(INDENT);
    text.lines()
        .enumerate()
        .map(|(index, line)| {
            let built = if index == 0 {
                Line::from(vec![prefix.clone(), Span::styled(line.to_string(), style)])
            } else {
                Line::from(vec![Span::raw(pad.clone()), Span::styled(line.to_string(), style)])
            };
            Indented::new(built, INDENT)
        })
        .collect()
}
