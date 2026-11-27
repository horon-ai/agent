//! Breaking rendered lines at a fixed width.
//!
//! The transcript pane keeps only the rows that fit its height, so the rows
//! must be counted after wrapping, not before: a wrapped line occupies as many
//! rows as its cells need. Doing it here also keeps the wrapper's own wrap out
//! of the pane, where it would wrap after the tail was already chosen.
//!
//! A block is also wrapped only as far as a caller needs it. Wrapping all of a
//! block to show its last rows builds a row for every cell of output that stays
//! off screen, and an expanded tool call drawing megabytes of output is exactly
//! that case: [`Source`] keeps such a block as the text it came from,
//! [`Source::height`] counts its rows without building any, and
//! [`Source::rows_from_end`] builds only the rows the pane shows.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::changes::RowStyle;

/// One character, the columns it takes, and the style of the span it came from.
type Cell = (char, usize, Style);

/// The cells of the line being broken, reused across the lines of a block so
/// counting a block's height does not allocate once per line.
type Cells = Vec<Cell>;

/// Columns a continuation row keeps for its own text at the narrowest width.
///
/// An indent that leaves less than this would hide the text it is aligning, so
/// it gives way instead: the alignment is worth less than the words.
const MIN_TEXT_COLUMNS: usize = 3;

/// Blank columns a boxed row borrows its right-hand padding from, so a pane
/// up to this wide pads every row without allocating.
const PADDING: &str = match std::str::from_utf8(&[b' '; 512]) {
    Ok(spaces) => spaces,
    // Unreachable: ASCII spaces are UTF-8. An empty string would only send
    // every row to the allocating fallback below.
    Err(_) => "",
};

/// `columns` blank columns, borrowed from [`PADDING`] whenever they fit in it.
fn padding(columns: usize) -> Span<'static> {
    match PADDING.get(..columns) {
        Some(spaces) => Span::raw(spaces),
        None => Span::raw(" ".repeat(columns)),
    }
}

#[cfg(test)]
mod built {
    use std::cell::Cell;

    thread_local! {
        /// Wrapped rows built on this thread.
        static COUNT: Cell<usize> = const { Cell::new(0) };
    }

    /// Wrapped rows built on this thread, so a test can check that a frame buys
    /// only the rows it shows.
    pub(crate) fn built_rows() -> usize {
        COUNT.with(Cell::get)
    }

    /// Start counting built rows again.
    pub(crate) fn reset() {
        COUNT.with(|count| count.set(0));
    }

    /// Note one row built.
    pub(crate) fn note() {
        COUNT.with(|count| count.set(count.get() + 1));
    }
}

#[cfg(test)]
pub(crate) use built::{built_rows, reset as reset_built};

/// The two sides a boxed line is drawn between.
///
/// A card's rows are its text with an edge and one blank column on each side.
/// The sides take their columns out of the pane's width before the text is
/// broken, and every row is padded to that width, so the right edge lines up
/// down the whole card however the text inside it wrapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sides {
    /// The glyph drawn at both edges of every row.
    pub edge: &'static str,
    /// The style the edges are drawn in.
    pub style: Style,
}

impl Sides {
    /// Columns the text of a row gets inside a pane `width` columns wide.
    fn inner(&self, width: usize) -> usize {
        width.saturating_sub(2 * (self.edge.width() + 1)).max(1)
    }

    /// `row` between the two edges, padded out to `inner` columns.
    fn enclose(&self, row: Line<'static>, inner: usize) -> Line<'static> {
        let used: usize = row.spans.iter().map(|span| span.content.width()).sum();
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(row.spans.len() + 4);
        spans.push(Span::styled(self.edge, self.style));
        spans.push(Span::raw(" "));
        spans.extend(row.spans);
        spans.push(padding(inner.saturating_sub(used) + 1));
        spans.push(Span::styled(self.edge, self.style));
        Line {
            style: row.style,
            alignment: row.alignment,
            spans,
        }
    }
}

/// The width a line with `sides` is broken at inside a pane `width` wide.
fn bounds(sides: Option<Sides>, width: usize) -> usize {
    sides.map_or(width, |sides| sides.inner(width))
}

/// A rendered line and the columns its continuation rows keep.
///
/// Wrapping alone would start every row at the first column, so a message under
/// a `> ` prefix or a list marker would lose the alignment that shows it is one
/// message. The indent travels with the line and is spent out of the row's
/// width, not added to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Indented {
    line: Line<'static>,
    indent: usize,
    sides: Option<Sides>,
}

impl Indented {
    /// A line whose continuation rows start at the first column.
    pub fn plain(line: Line<'static>) -> Self {
        Self {
            line,
            indent: 0,
            sides: None,
        }
    }

    /// A line whose continuation rows keep `indent` columns.
    pub fn new(line: Line<'static>, indent: usize) -> Self {
        Self {
            line,
            indent,
            sides: None,
        }
    }

    /// A line drawn inside a card: every row it breaks into sits between
    /// `sides`, and its continuation rows keep `indent` columns inside them.
    pub fn boxed(line: Line<'static>, indent: usize, sides: Sides) -> Self {
        Self {
            line,
            indent,
            sides: Some(sides),
        }
    }

    /// The line before it is wrapped.
    pub fn line(&self) -> &Line<'static> {
        &self.line
    }

    /// Columns the continuation rows keep.
    pub fn indent(&self) -> usize {
        self.indent
    }

    /// The sides every row of the line is drawn between, if it is boxed.
    pub fn sides(&self) -> Option<Sides> {
        self.sides
    }
}

impl From<Line<'static>> for Indented {
    fn from(line: Line<'static>) -> Self {
        Self::plain(line)
    }
}

/// Break `line` into rows of at most `width` cells, keeping each span's style.
///
/// A row that overflows ends after its last whitespace character, which stays
/// at the end of the row: nothing is trimmed, so a word is never split when the
/// row has somewhere else to break. A row with no whitespace is broken at the
/// cell limit instead, and a wide character that does not fit starts the next
/// row rather than being split, so a row is only wider than `width` when one
/// character alone is wider than the pane.
pub fn wrap_line(line: &Line<'static>, width: usize) -> Vec<Line<'static>> {
    wrap_indented(line, width, 0)
}

/// Break `line` into rows, keeping `indent` columns free on continuation rows.
///
/// The first row carries the line's own prefix, so it uses the full width; every
/// row after it spends `indent` columns on staying under the text that prefix
/// introduced. The indent gives way at a width too narrow to hold both it and a
/// few columns of text.
pub fn wrap_indented(line: &Line<'static>, width: usize, indent: usize) -> Vec<Line<'static>> {
    let mut cells: Cells = Vec::new();
    fill(&mut cells, line);
    let mut rows: Vec<Line<'static>> = Vec::new();
    break_rows(&cells, width, indent, |row| {
        rows.push(row_of(&cells[row.start..row.end], line, row.lead));
    });
    rows
}

/// Wrap a rendered block, keeping every line's own indent and sides.
#[cfg(test)]
pub fn wrap_rows(lines: Vec<Indented>, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width);
    let mut rows: Vec<Line<'static>> = Vec::new();
    for line in lines {
        let inner = bounds(line.sides(), width);
        let wrapped = wrap_indented(line.line(), inner, line.indent());
        match line.sides() {
            Some(sides) => rows.extend(wrapped.into_iter().map(|row| sides.enclose(row, inner))),
            None => rows.extend(wrapped),
        }
    }
    rows
}

/// Which rows of a block are wanted, counted back from its last row.
///
/// A pane shows the rows near the end of a block, so what it does not show is
/// the thing to name: `skip` rows at the end are left out and `count` rows above
/// them are wanted. Counting the block first is not needed, which is what makes
/// [`Source::rows_from_end`] able to build a window without measuring the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FromEnd {
    /// Rows at the end of the block the caller does not want.
    pub skip: usize,
    /// Rows wanted above those.
    pub count: usize,
}

impl FromEnd {
    /// `count` rows ending `skip` rows above the block's last row.
    pub fn new(skip: usize, count: usize) -> Self {
        Self { skip, count }
    }
}

/// The lines of one block, before they are wrapped.
///
/// Most blocks are a short list of lines and arrive built. A tool call drawing
/// its whole kept output is one line per row of that output, and building them
/// all to show the pane's last few rows is what this type exists to avoid: the
/// output stays the borrowed text and a line is made only where one is drawn.
pub enum Source<'a> {
    /// Every line is built already.
    Built(Vec<Indented>),
    /// `head` rows, then one line per row of `text`, each in its row style and
    /// keeping `indent` columns on its continuation rows.
    Output {
        /// The rows above the output: a tool call's head, and a failure.
        head: Vec<Indented>,
        /// The output itself, one row per line.
        text: &'a str,
        /// The style every row of the output is drawn in, or the styles that
        /// tell an added line and a removed one apart.
        style: RowStyle,
        /// Columns a continuation row of the output keeps.
        indent: usize,
        /// The sides every row of the output is drawn between, when the
        /// output sits inside a card.
        sides: Option<Sides>,
        /// Lines under the output: the card's closing row, and a reader's note.
        tail: Vec<Indented>,
    },
}

impl<'a> Source<'a> {
    /// A block whose lines are all built.
    pub fn built(lines: Vec<Indented>) -> Self {
        Self::Built(lines)
    }

    /// Put `line` above the block.
    pub fn prepend(&mut self, line: Indented) {
        match self {
            Self::Built(lines) => lines.insert(0, line),
            Self::Output { head, .. } => head.insert(0, line),
        }
    }

    /// Put `line` under the block.
    pub fn append(&mut self, line: Indented) {
        match self {
            Self::Built(lines) => lines.push(line),
            Self::Output { tail, .. } => tail.push(line),
        }
    }

    /// Rows the block occupies once wrapped, counted without building any.
    pub fn height(&self, width: u16) -> usize {
        let width = usize::from(width);
        let mut cells: Cells = Vec::new();
        match self {
            Self::Built(lines) => lines.iter().map(|line| line_height(&mut cells, line, width)).sum(),
            Self::Output {
                head,
                text,
                style,
                indent,
                sides,
                tail,
            } => {
                let above: usize = head.iter().map(|line| line_height(&mut cells, line, width)).sum();
                let inner = bounds(*sides, width);
                let rows: usize = text
                    .lines()
                    .map(|row| {
                        fill_row(&mut cells, style.of(row), row);
                        break_rows(&cells, inner, *indent, |_| {})
                    })
                    .sum();
                let below: usize = tail.iter().map(|line| line_height(&mut cells, line, width)).sum();
                above + rows + below
            }
        }
    }

    /// The rows [`FromEnd`] names, top to bottom.
    ///
    /// The block's rows are walked back from its last one, so a window near the
    /// end only reads the lines it draws; the rows skipped by [`FromEnd::skip`]
    /// are counted and dropped, never built.
    pub fn rows_from_end(&self, width: u16, want: FromEnd) -> Vec<Line<'static>> {
        let width = usize::from(width);
        let mut rows: Vec<Line<'static>> = Vec::with_capacity(want.count.min(64));
        let mut take = want.count;
        if take == 0 {
            return rows;
        }
        let mut skip = want.skip;
        let mut cells: Cells = Vec::new();
        match self {
            Self::Built(lines) => {
                for line in lines.iter().rev() {
                    if take == 0 {
                        break;
                    }
                    fill(&mut cells, line.line());
                    let window = take_from_line(&cells, line, width, &mut skip, &mut take);
                    rows.extend(window.into_iter().rev());
                }
            }
            Self::Output {
                head,
                text,
                style,
                indent,
                sides,
                tail,
            } => {
                for line in tail.iter().rev() {
                    if take == 0 {
                        break;
                    }
                    fill(&mut cells, line.line());
                    let window = take_from_line(&cells, line, width, &mut skip, &mut take);
                    rows.extend(window.into_iter().rev());
                }
                // An output row is a line of its own with no style of its own,
                // so the row it breaks into carries the default line style and
                // alignment.
                let output = Indented {
                    line: Line::default(),
                    indent: *indent,
                    sides: *sides,
                };
                for row in text.lines().rev() {
                    if take == 0 {
                        break;
                    }
                    fill_row(&mut cells, style.of(row), row);
                    let window = take_from_line(&cells, &output, width, &mut skip, &mut take);
                    rows.extend(window.into_iter().rev());
                }
                for line in head.iter().rev() {
                    if take == 0 {
                        break;
                    }
                    fill(&mut cells, line.line());
                    let window = take_from_line(&cells, line, width, &mut skip, &mut take);
                    rows.extend(window.into_iter().rev());
                }
            }
        }
        rows.reverse();
        rows
    }
}

/// The rows of one line that a window keeps, after dropping `skip` rows from the
/// line's end, taken off `take`.
///
/// Answers the rows in the line's own order. `skip` and `take` carry the window
/// across lines, so the caller walks lines from the block's end and stops when
/// `take` reaches zero. A boxed line is broken inside its sides, and every row
/// it keeps is drawn between them.
fn take_from_line(
    cells: &[Cell],
    line: &Indented,
    width: usize,
    skip: &mut usize,
    take: &mut usize,
) -> Vec<Line<'static>> {
    let inner = bounds(line.sides, width);
    let height = break_rows(cells, inner, line.indent, |_| {});
    if *skip >= height {
        *skip -= height;
        return Vec::new();
    }
    let end = height - *skip;
    let start = end.saturating_sub(*take);
    let mut rows: Vec<Line<'static>> = Vec::with_capacity(end - start);
    break_rows(cells, inner, line.indent, |row| {
        if (start..end).contains(&row.index) {
            let built = row_of(&cells[row.start..row.end], &line.line, row.lead);
            rows.push(match line.sides {
                Some(sides) => sides.enclose(built, inner),
                None => built,
            });
        }
    });
    *take -= rows.len();
    *skip = 0;
    rows
}

/// Rows one built line occupies, counted without building them.
fn line_height(cells: &mut Cells, line: &Indented, width: usize) -> usize {
    fill(cells, line.line());
    break_rows(cells, bounds(line.sides, width), line.indent(), |_| {})
}

/// Every cell of `line` in the order it is drawn.
fn fill(cells: &mut Cells, line: &Line<'static>) {
    cells.clear();
    cells.extend(line.spans.iter().flat_map(|span| {
        span.content
            .chars()
            .map(move |character| (character, UnicodeWidthChar::width(character).unwrap_or(0), span.style))
    }));
}

/// Every cell of one output row, in one style.
fn fill_row(cells: &mut Cells, style: Style, text: &str) {
    cells.clear();
    cells.extend(
        text.chars()
            .map(|character| (character, UnicodeWidthChar::width(character).unwrap_or(0), style)),
    );
}

/// One row a line breaks into.
///
/// `cells[start..end]` is what the row draws, and `lead` is the columns it
/// spends in front of them, which is how a continuation row stays under the text
/// its prefix introduced.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// The row's place in the line, counted from the top.
    index: usize,
    /// The first cell it draws.
    start: usize,
    /// One past the last cell it draws.
    end: usize,
    /// Columns it keeps in front of the cells.
    lead: usize,
}

/// Walk `cells` as rows of at most `width` columns, answering how many there are.
///
/// `row` is handed each row in turn. Counting a line calls it with nothing to do,
/// and building a window builds only the rows whose index falls in the window, so
/// both always agree on where a row begins and ends.
fn break_rows(cells: &[Cell], width: usize, indent: usize, mut row: impl FnMut(Row)) -> usize {
    let width = width.max(1);
    let indent = indent.min(width.saturating_sub(MIN_TEXT_COLUMNS));
    // An empty line still occupies one row.
    if cells.is_empty() {
        row(Row {
            index: 0,
            start: 0,
            end: 0,
            lead: 0,
        });
        return 1;
    }
    let mut count = 0;
    let mut start = 0;
    while start < cells.len() {
        let lead = if count == 0 { 0 } else { indent };
        let budget = width.saturating_sub(lead).max(1);
        let mut used = 0;
        let mut end = start;
        let mut space = None;
        while end < cells.len() {
            let cell_width = cells[end].1;
            if used > 0 && used + cell_width > budget {
                break;
            }
            used += cell_width;
            if cells[end].0.is_whitespace() {
                space = Some(end + 1);
            }
            end += 1;
        }
        // `end < cells.len()` means the row overflowed. Breaking after the last
        // space keeps whole words; without one the cell limit is the only
        // place left to break.
        let end = if end < cells.len() {
            space.filter(|space| *space > start).unwrap_or(end)
        } else {
            end
        };
        row(Row {
            index: count,
            start,
            end,
            lead,
        });
        count += 1;
        start = end;
    }
    count
}

/// One row of cells, with adjacent cells of the same style in one span.
///
/// `lead` columns are spent in front of the cells, which is where a continuation
/// row stays under the text its prefix introduced.
fn row_of(cells: &[Cell], line: &Line<'static>, lead: usize) -> Line<'static> {
    #[cfg(test)]
    built::note();
    let mut spans: Vec<Span<'static>> = Vec::new();
    if lead > 0 {
        spans.push(Span::raw(" ".repeat(lead)));
    }
    let mut text = String::new();
    let mut style: Option<Style> = None;
    for &(character, _, cell_style) in cells {
        if style != Some(cell_style) {
            if let Some(previous) = style {
                spans.push(Span::styled(std::mem::take(&mut text), previous));
            }
            style = Some(cell_style);
        }
        text.push(character);
    }
    if let Some(previous) = style {
        spans.push(Span::styled(text, previous));
    }
    Line {
        style: line.style,
        alignment: line.alignment,
        spans,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn text_of(line: &Line<'_>) -> String {
        line.spans.iter().map(|span| span.content.as_ref()).collect()
    }

    fn narrow() -> Line<'static> {
        Line::from(Span::styled("abcdefghij", Style::default().fg(Color::Red)))
    }

    #[test]
    fn short_line_stays_on_one_row() {
        let rows = wrap_line(&narrow(), 20);
        assert_eq!(rows.len(), 1);
        assert_eq!(text_of(&rows[0]), "abcdefghij");
        assert_eq!(rows[0].spans[0].style.fg, Some(Color::Red));
    }

    #[test]
    fn long_line_breaks_at_the_width_and_keeps_order() {
        let rows = wrap_line(&narrow(), 4);
        let texts: Vec<String> = rows.iter().map(text_of).collect();
        assert_eq!(texts, vec!["abcd", "efgh", "ij"]);
        assert_eq!(rows[0].spans[0].style.fg, Some(Color::Red));
    }

    #[test]
    fn exact_multiple_leaves_no_empty_row() {
        let rows = wrap_line(&narrow(), 5);
        assert_eq!(rows.len(), 2);
        assert_eq!(text_of(&rows[1]), "fghij");
    }

    #[test]
    fn wide_characters_are_never_split() {
        // Each Hangul syllable is two cells wide, so a three-cell pane holds
        // one per row.
        let rows = wrap_line(&Line::from("한글"), 3);
        let texts: Vec<String> = rows.iter().map(text_of).collect();
        assert_eq!(texts, vec!["한", "글"]);
    }

    #[test]
    fn styles_are_kept_across_a_break() {
        let line = Line::from(vec![
            Span::styled("ab", Style::default().fg(Color::Green)),
            Span::styled("cd", Style::default().fg(Color::Blue)),
        ]);
        let rows = wrap_line(&line, 2);
        assert_eq!(rows.len(), 2);
        assert_eq!(text_of(&rows[0]), "ab");
        assert_eq!(rows[0].spans[0].style.fg, Some(Color::Green));
        assert_eq!(text_of(&rows[1]), "cd");
        assert_eq!(rows[1].spans[0].style.fg, Some(Color::Blue));
    }

    #[test]
    fn empty_line_yields_one_row() {
        let rows = wrap_line(&Line::default(), 10);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].spans.is_empty());
    }

    #[test]
    fn a_sentence_breaks_between_words() {
        // Word boundaries are where a reader expects the break; the trailing
        // space stays on the row it ended, as `Wrap { trim: false }` did.
        let rows = wrap_line(&Line::from("hello world"), 8);
        let texts: Vec<String> = rows.iter().map(text_of).collect();
        assert_eq!(texts, vec!["hello ", "world"]);
    }

    #[test]
    fn a_word_wider_than_the_pane_hard_breaks() {
        // No whitespace to break at, so the cell limit is the only option and
        // the word still reaches the next rows instead of being lost.
        let rows = wrap_line(&Line::from("supercalifragilistic"), 6);
        let texts: Vec<String> = rows.iter().map(text_of).collect();
        assert_eq!(texts, vec!["superc", "alifra", "gilist", "ic"]);
    }

    #[test]
    fn continuation_rows_keep_the_indent() {
        // The first row carries the prefix a caller already drew; every row
        // after it stays under the text that prefix introduced.
        let rows = wrap_indented(&Line::from("abcdefgh"), 5, 2);
        let texts: Vec<String> = rows.iter().map(text_of).collect();
        assert_eq!(texts, vec!["abcde", "  fgh"]);
    }

    #[test]
    fn the_indent_is_spent_from_the_width() {
        // A word still fits a continuation row because the row's budget is the
        // pane width minus the indent, not the width itself.
        let rows = wrap_indented(&Line::from("hello world"), 8, 2);
        let texts: Vec<String> = rows.iter().map(text_of).collect();
        assert_eq!(texts, vec!["hello ", "  world"]);
    }

    #[test]
    fn the_indent_gives_way_at_a_narrow_width() {
        // Two columns of indent in a four-cell pane would leave two cells for
        // the text, so the indent shrinks instead of hiding it.
        let rows = wrap_indented(&Line::from("abcdefgh"), 4, 2);
        let texts: Vec<String> = rows.iter().map(text_of).collect();
        assert_eq!(texts, vec!["abcd", " efg", " h"]);
    }

    #[test]
    fn wrap_rows_keeps_each_lines_indent() {
        let rows = wrap_rows(
            vec![
                Indented::plain(Line::from("plain")),
                Indented::new(Line::from("abcdef"), 2),
            ],
            5,
        );
        let texts: Vec<String> = rows.iter().map(text_of).collect();
        assert_eq!(texts, vec!["plain", "abcde", "  f"]);
    }

    #[test]
    fn a_space_break_keeps_both_styles() {
        let line = Line::from(vec![
            Span::styled("ab", Style::default().fg(Color::Green)),
            Span::styled(" cd", Style::default().fg(Color::Blue)),
        ]);
        let rows = wrap_line(&line, 3);
        assert_eq!(rows.len(), 2);
        assert_eq!(text_of(&rows[0]), "ab ");
        assert_eq!(rows[0].spans[0].style.fg, Some(Color::Green));
        assert_eq!(text_of(&rows[1]), "cd");
        assert_eq!(rows[1].spans[0].style.fg, Some(Color::Blue));
    }

    /// Lines that exercise every break rule at once: words, long words, wide
    /// characters, blank rows and a row that fits exactly.
    fn mixed() -> Vec<Indented> {
        vec![
            Indented::plain(Line::from("a line that is longer than the pane so it wraps")),
            Indented::plain(Line::from("")),
            Indented::new(Line::from("  a continuation row that keeps two columns of indent"), 2),
            Indented::plain(Line::from("한글한글한글한글한글")),
            Indented::plain(Line::from("abcdefghij")),
            Indented::plain(Line::from("supercalifragilisticexpialidocious and then words")),
            Indented::new(Line::from("tail"), 2),
        ]
    }

    #[test]
    fn a_window_is_the_tail_of_the_full_wrap() {
        let lines = mixed();
        let full = wrap_rows(lines.clone(), 20);
        for skip in [0, 1, 3, 7, 19, 20, 21, 500] {
            for count in [0, 1, 2, 6, 20, 21, 500] {
                let window = Source::built(lines.clone()).rows_from_end(20, FromEnd::new(skip, count));
                let end = full.len().saturating_sub(skip);
                let start = end.saturating_sub(count);
                assert_eq!(
                    window,
                    full[start..end],
                    "skip {skip} count {count}: the window is not the tail of the full wrap"
                );
            }
        }
    }

    #[test]
    fn counting_rows_matches_wrapping_them() {
        let lines = mixed();
        assert_eq!(Source::built(lines.clone()).height(20), wrap_rows(lines, 20).len());
    }

    #[test]
    fn an_output_window_is_the_tail_of_its_own_lines() {
        let style = Style::default().fg(Color::Yellow);
        let sides = Sides {
            edge: "│",
            style: Style::default().fg(Color::Gray),
        };
        let text = String::from("first row\n\n  a row that wraps over the pane's width\n한글한글한글\nlast");
        let head = Indented::plain(Line::from("head"));
        let foot = Indented::plain(Line::from("foot"));
        let source = Source::Output {
            head: vec![head.clone()],
            text: &text,
            style: RowStyle::Plain(style),
            indent: 2,
            sides: Some(sides),
            tail: vec![foot.clone()],
        };
        // The same block with its output rows built: what the pane used to draw.
        let mut eager = vec![head];
        for row in text.lines() {
            let line = Line::from(Span::styled(row.to_string(), style));
            eager.push(Indented::boxed(line, 2, sides));
        }
        eager.push(foot);
        for width in [4, 14, 20, 40] {
            let full = wrap_rows(eager.clone(), width);
            assert_eq!(source.height(width), full.len(), "height at width {width}");
            for skip in [0, 3, 40] {
                for count in [0, 1, 5, 40] {
                    let window = source.rows_from_end(width, FromEnd::new(skip, count));
                    let end = full.len().saturating_sub(skip);
                    let start = end.saturating_sub(count);
                    assert_eq!(window, full[start..end], "width {width} skip {skip} count {count}");
                }
            }
        }
    }

    #[test]
    fn an_appended_line_draws_under_the_block() {
        // What the transcript pane uses for a note under a message.
        let mut built = Source::built(vec![Indented::plain(Line::from("message"))]);
        built.append(Indented::plain(Line::from("note: mine")));
        assert_eq!(built.height(40), 2);
        let rows = built.rows_from_end(40, FromEnd::new(0, 2));
        assert_eq!(
            rows.iter().map(text_of).collect::<Vec<_>>(),
            vec!["message", "note: mine"]
        );

        // A note never lands on an output block, but the tail keeps the same
        // offer: after the output, not before it.
        let text = String::from("output");
        let mut output = Source::Output {
            head: vec![Indented::plain(Line::from("head"))],
            text: &text,
            sides: None,
            style: RowStyle::Plain(Style::default()),
            indent: 0,
            tail: Vec::new(),
        };
        output.append(Indented::plain(Line::from("note: mine")));
        assert_eq!(output.height(40), 3);
        let rows = output.rows_from_end(40, FromEnd::new(0, 3));
        assert_eq!(
            rows.iter().map(text_of).collect::<Vec<_>>(),
            vec!["head", "output", "note: mine"]
        );
    }

    #[test]
    fn an_empty_block_has_no_rows() {
        let source = Source::built(Vec::new());
        assert_eq!(source.height(40), 0);
        assert!(source.rows_from_end(40, FromEnd::new(0, 40)).is_empty());
    }

    #[test]
    fn a_window_builds_only_the_rows_it_returns() {
        let lines: Vec<Indented> = (0..2000)
            .map(|row| Indented::plain(Line::from(format!("output row {row}"))))
            .collect();
        reset_built();
        let source = Source::built(lines);
        let window = source.rows_from_end(120, FromEnd::new(0, 40));
        assert_eq!(window.len(), 40, "the window is the pane's own height");
        assert_eq!(built_rows(), 40, "a frame built rows it cannot show");
        assert_eq!(window.iter().map(text_of).next().as_deref(), Some("output row 1960"));
    }

    #[test]
    fn counting_a_block_builds_no_rows() {
        let text: String = (0..2000).map(|row| format!("output row {row}\n")).collect();
        reset_built();
        let source = Source::Output {
            head: vec![Indented::plain(Line::from("head"))],
            text: &text,
            style: RowStyle::Plain(Style::default()),
            indent: 2,
            sides: None,
            tail: Vec::new(),
        };
        assert_eq!(source.height(120), 2001);
        assert_eq!(built_rows(), 0, "counting a block built rows");
        let window = source.rows_from_end(120, FromEnd::new(0, 40));
        assert_eq!(window.len(), 40);
        assert_eq!(built_rows(), 40, "a window built rows it cannot show");
    }
}
