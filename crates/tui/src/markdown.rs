//! A small Markdown renderer for assistant text.
//!
//! A model writes its answers in Markdown, and a terminal that shows the source
//! makes the reader parse `**` and `## ` themselves. This module turns the marks
//! a model actually emits into lines of styled spans: headings, bold, italic,
//! inline code, lists, block quotes, fenced code and horizontal rules.
//!
//! Anything it does not claim stays as typed. A table drawn as its own pipe
//! characters is still readable, and dropping the text instead would lose what
//! the model said, which is worse than showing the source it wrote.
//!
//! The message on screen is often still arriving, so the renderer has to cope
//! with half a construct: a fence with no end, a `**` with no partner. Those are
//! drawn as the characters they are, never swallowed and never carried into the
//! next item, and no input may panic here.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::theme::{Symbols, Theme};
use crate::wrap::Indented;

/// Columns a code block's lines are indented by, so the block reads as one.
const CODE_INDENT: usize = 2;

/// How deep emphasis may nest before the rest of the line is drawn plainly.
///
/// A run of markers long enough to nest past this would otherwise recurse once
/// per pair, and the input is model output of whatever shape it happens to be.
const MAX_NESTING: usize = 8;

/// Columns a tab advances inside a code block.
const TAB: usize = 4;

/// Columns one level of list nesting adds before its marker.
const NEST_COLUMNS: usize = 2;

/// The deepest list nesting drawn, so a deep list cannot push its text away.
const MAX_NEST: usize = 6;

/// Columns a row keeps for text beside an indented marker.
const MIN_TEXT_COLUMNS: usize = 3;

/// The styles and marks one message is drawn with.
struct Palette {
    /// Text outside any mark.
    text: Style,
    /// Text between backticks.
    code: Style,
    /// A heading's text.
    heading: Style,
    /// A list marker, a quote bar, or a rule.
    marker: Style,
    /// The bullet before an unordered list item.
    bullet: &'static str,
    /// The bar before a block quote.
    quote: &'static str,
    /// The mark a horizontal rule is drawn with.
    rule: &'static str,
}

impl Palette {
    /// The palette a theme and a glyph set give.
    fn new(theme: &Theme, symbols: Symbols) -> Self {
        let text = Style::default().fg(theme.transcript.assistant_text);
        Self {
            text,
            // The theme's existing colours only: a new token would reject every
            // theme file already written.
            code: Style::default().fg(theme.transcript.tool_done),
            heading: Style::default().fg(theme.core.accent).add_modifier(Modifier::BOLD),
            marker: Style::default().fg(theme.core.muted),
            bullet: symbols.bullet,
            quote: symbols.quote,
            rule: symbols.rule,
        }
    }
}

/// Render `text` as lines, each with the columns its continuation rows keep.
///
/// `width` is the pane width the block is drawn at: a horizontal rule spans it
/// rather than carrying its own fixed length.
pub fn render(text: &str, theme: &Theme, symbols: Symbols, width: u16) -> Vec<Indented> {
    let palette = Palette::new(theme, symbols);
    let mut rows: Vec<Indented> = Vec::new();
    let mut fence: Option<&'static str> = None;
    let mut lines = text.lines().peekable();
    while let Some(raw) = lines.next() {
        // An opening fence that is the last line of a message with no newline
        // after it may still be growing, so nothing is drawn for it yet: a
        // half-received ` ```ru ` must not flash a label that then changes.
        let complete = lines.peek().is_some() || text.ends_with('\n');
        let line = raw.trim_end();
        if let Some(opened) = fence {
            if closes_fence(line, opened) {
                fence = None;
            } else {
                // Inside a fence the text is code as typed, marks and all.
                rows.push(code_line(line, &palette));
            }
            continue;
        }
        let (indent, body) = leading(line);
        if body.is_empty() {
            rows.push(Indented::plain(Line::default()));
            continue;
        }
        if let Some((opened, info)) = fence_info(body) {
            fence = Some(opened);
            if complete && let Some(label) = info.split_whitespace().next() {
                rows.push(fence_label(label, &palette));
            }
            continue;
        }
        if let Some(text) = heading(body) {
            rows.push(with_indent(indent, inline(text, palette.heading, &palette, 0)));
            continue;
        }
        if is_rule(body) {
            rows.push(rule(&palette, width));
            continue;
        }
        if let Some((depth, text)) = quote(body) {
            rows.push(quote_line(depth, text, &palette));
            continue;
        }
        if let Some((marker, text)) = list_marker(body) {
            rows.push(list_line(marker, text, indent, width, &palette));
            continue;
        }
        // Tables and syntax this renderer does not claim stay as typed.
        rows.push(with_indent(indent, inline(body, palette.text, &palette, 0)));
    }
    rows
}

/// The indentation of a line in columns, and the line without it.
///
/// A tab is a level of its own, which is what an indented list written with tabs
/// means: one tab is the same nesting as two spaces.
fn leading(line: &str) -> (usize, &str) {
    let mut columns = 0;
    let mut end = 0;
    for character in line.chars() {
        match character {
            ' ' => columns += 1,
            '\t' => columns += 2,
            _ => break,
        }
        end += character.len_utf8();
    }
    (columns, &line[end..])
}

/// The fence marker this line opens a code block with, and its info string.
///
/// The info string is the language the block was tagged with; the renderer draws
/// its first word as a label and otherwise leaves the tag alone.
fn fence_info(line: &str) -> Option<(&'static str, &str)> {
    for marker in ["```", "~~~"] {
        if let Some(info) = line.strip_prefix(marker) {
            return Some((marker, info.trim()));
        }
    }
    None
}

/// The row naming a fenced block's language, above its code.
///
/// A fence with no info string draws nothing in its place: an empty row would
/// separate the block from the text around it for no reason.
fn fence_label(label: &str, palette: &Palette) -> Indented {
    let mut spans = vec![Span::raw(" ".repeat(CODE_INDENT))];
    spans.push(Span::styled(label.to_string(), palette.marker));
    Indented::new(Line::from(spans), CODE_INDENT)
}

/// Whether this line ends a block opened with `marker`.
///
/// A closing fence is at least as long as the opening one and holds nothing but
/// its own character, which is what keeps a lone ` ``` ` inside code from
/// ending the block around it.
fn closes_fence(line: &str, marker: &str) -> bool {
    let character = marker.chars().next().unwrap_or('`');
    let trimmed = line.trim();
    trimmed.chars().count() >= marker.len() && trimmed.chars().all(|found| found == character)
}

/// The text of a heading line, without its marks.
fn heading(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|found| *found == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = line.get(hashes..)?;
    // `#tag` is a word, not a heading: Markdown wants the space.
    let text = rest.strip_prefix(' ')?;
    Some(text.trim_end())
}

/// Whether the line is a thematic break rather than text.
fn is_rule(line: &str) -> bool {
    let marks: String = line.chars().filter(|found| !found.is_whitespace()).collect();
    if marks.chars().count() < 3 {
        return false;
    }
    let first = marks.chars().next().unwrap_or('-');
    matches!(first, '-' | '*' | '_') && marks.chars().all(|found| found == first)
}

/// The nesting depth and text of a block quote line.
fn quote(line: &str) -> Option<(usize, &str)> {
    if !line.starts_with('>') {
        return None;
    }
    let mut depth = 0;
    let mut rest = line;
    while let Some(stripped) = rest.strip_prefix('>') {
        depth += 1;
        rest = stripped.strip_prefix(' ').unwrap_or(stripped);
    }
    Some((depth, rest))
}

/// The mark a list item starts with, kept as the writer wrote it.
enum Marker {
    /// An unordered item, drawn with the glyph set's bullet.
    Bullet,
    /// An ordered item, drawn with its own number and delimiter.
    Number(String),
}

/// The marker and text of a list item line.
fn list_marker(line: &str) -> Option<(Marker, &str)> {
    let digits: String = line.chars().take_while(|found| found.is_ascii_digit()).collect();
    if !digits.is_empty() {
        let rest = line.get(digits.len()..)?;
        for delimiter in ['.', ')'] {
            if let Some(text) = rest.strip_prefix(delimiter).and_then(|after| after.strip_prefix(' ')) {
                return Some((Marker::Number(format!("{digits}{delimiter}")), text));
            }
        }
    }
    for bullet in ['-', '*', '+'] {
        if let Some(text) = line.strip_prefix(bullet).and_then(|after| after.strip_prefix(' ')) {
            return Some((Marker::Bullet, text));
        }
    }
    None
}

/// One row of a code block: indented, tabs expanded, marks left alone.
fn code_line(line: &str, palette: &Palette) -> Indented {
    let mut spans = vec![Span::raw(" ".repeat(CODE_INDENT))];
    spans.push(Span::styled(expand_tabs(line), palette.code));
    Indented::new(Line::from(spans), CODE_INDENT)
}

/// Replace each tab with the spaces that keep the next character on its stop.
///
/// The wrapper counts cells, and a tab has no cell width of its own, so an
/// unexpanded tab would make the row wider than the pane it was measured for.
fn expand_tabs(line: &str) -> String {
    let mut expanded = String::with_capacity(line.len());
    for character in line.chars() {
        if character == '\t' {
            let column = UnicodeWidthStr::width(expanded.as_str());
            let pad = TAB - column % TAB;
            expanded.push_str(&" ".repeat(pad));
        } else {
            expanded.push(character);
        }
    }
    expanded
}

/// A horizontal rule, drawn across the pane.
fn rule(palette: &Palette, width: u16) -> Indented {
    let columns = usize::from(width).max(1);
    let glyph = UnicodeWidthStr::width(palette.rule).max(1);
    Indented::plain(Line::from(Span::styled(
        palette.rule.repeat(columns / glyph),
        palette.marker,
    )))
}

/// One row of a block quote, with a bar per level of nesting.
fn quote_line(depth: usize, text: &str, palette: &Palette) -> Indented {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for _ in 0..depth {
        spans.push(Span::styled(format!("{} ", palette.quote), palette.marker));
    }
    spans.extend(inline(text, palette.text, palette, 0));
    Indented::new(Line::from(spans), depth * 2)
}

/// One row of a list item, aligned under its own text and nested by its indent.
///
/// The item's source indentation is drawn as nesting rather than dropped: a list
/// written with two spaces per level keeps the levels a reader needs to see which
/// item a sub-list belongs to. Wrapped rows hang under the item's text, not under
/// its marker, and the nesting gives way before the text does at a narrow width.
fn list_line(marker: Marker, text: &str, indent: usize, width: u16, palette: &Palette) -> Indented {
    let mark = match marker {
        Marker::Bullet => format!("{} ", palette.bullet),
        Marker::Number(number) => format!("{number} "),
    };
    let mark_width = UnicodeWidthStr::width(mark.as_str());
    let room = usize::from(width).saturating_sub(mark_width + MIN_TEXT_COLUMNS);
    let depth = (indent / NEST_COLUMNS).min(MAX_NEST).min(room / NEST_COLUMNS);
    let lead = format!("{}{mark}", " ".repeat(depth * NEST_COLUMNS));
    let mut spans = vec![Span::styled(lead, palette.marker)];
    spans.extend(inline(text, palette.text, palette, 0));
    Indented::new(Line::from(spans), depth * NEST_COLUMNS + mark_width)
}

/// Text that starts `indent` columns in, so an indented paragraph stays so.
fn with_indent(indent: usize, spans: Vec<Span<'static>>) -> Indented {
    if indent == 0 {
        return Indented::plain(Line::from(spans));
    }
    let mut all = vec![Span::raw(" ".repeat(indent))];
    all.extend(spans);
    Indented::new(Line::from(all), indent)
}

/// Render one line's inline marks into styled spans.
///
/// Emphasis may contain more emphasis, so a paired marker renders its own body
/// through this function again, one level deeper and one modifier heavier. A
/// marker with no partner is the text it looks like: the half of it that has
/// arrived while a message streams is still text the model sent.
fn inline(text: &str, style: Style, palette: &Palette, depth: usize) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    let mut previous: Option<char> = None;
    if depth > MAX_NESTING {
        return vec![Span::styled(text.to_string(), style)];
    }
    while let Some(character) = rest.chars().next() {
        if let Some((marker, inner)) = emphasis(rest, previous, style, palette) {
            let body = &rest[marker.len()..];
            match body.find(marker) {
                Some(end) if end > 0 => {
                    push_plain(&mut spans, &mut plain, style);
                    spans.extend(inline(&body[..end], inner, palette, depth + 1));
                    rest = &body[end + marker.len()..];
                    previous = Some(character);
                    continue;
                }
                // Markers with nothing between them, or none at all: the
                // marker characters are drawn rather than losing the text.
                _ => {
                    plain.push_str(marker);
                    rest = body;
                    previous = marker.chars().last();
                    continue;
                }
            }
        }
        plain.push(character);
        rest = &rest[character.len_utf8()..];
        previous = Some(character);
    }
    push_plain(&mut spans, &mut plain, style);
    spans
}

/// The marker `rest` starts with, and the style its body takes.
///
/// `_` inside a word is part of the word — `snake_case` is an identifier, not
/// emphasis — so it only opens one at a word boundary.
fn emphasis(rest: &str, previous: Option<char>, style: Style, palette: &Palette) -> Option<(&'static str, Style)> {
    // Emphasis adds a modifier rather than replacing the style: a heading's
    // own colour stays its own when a word inside it is bold.
    let bold = style.add_modifier(Modifier::BOLD);
    let italic = style.add_modifier(Modifier::ITALIC);
    if rest.starts_with("**") {
        Some(("**", bold))
    } else if rest.starts_with("__") {
        Some(("__", bold))
    } else if rest.starts_with('`') {
        Some(("`", style.patch(palette.code)))
    } else if rest.starts_with('*') {
        Some(("*", italic))
    } else if rest.starts_with('_') && !previous.is_some_and(char::is_alphanumeric) {
        Some(("_", italic))
    } else {
        None
    }
}

/// Move the gathered plain text into the spans under its style.
fn push_plain(spans: &mut Vec<Span<'static>>, plain: &mut String, style: Style) {
    if plain.is_empty() {
        return;
    }
    spans.push(Span::styled(std::mem::take(plain), style));
}
