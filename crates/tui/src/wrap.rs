//! Breaking rendered lines at a fixed width.
//!
//! The transcript pane keeps only the rows that fit its height, so the rows
//! must be counted after wrapping, not before: a wrapped line occupies as many
//! rows as its cells need. Doing it here also keeps the wrapper's own wrap out
//! of the pane, where it would wrap after the tail was already chosen.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// One character, its cell width, and the style of the span it came from.
type Cell = (char, usize, Style);

/// Columns a continuation row keeps for its own text at the narrowest width.
///
/// An indent that leaves less than this would hide the text it is aligning, so
/// it gives way instead: the alignment is worth less than the words.
const MIN_TEXT_COLUMNS: usize = 3;

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
}

impl Indented {
    /// A line whose continuation rows start at the first column.
    pub fn plain(line: Line<'static>) -> Self {
        Self { line, indent: 0 }
    }

    /// A line whose continuation rows keep `indent` columns.
    pub fn new(line: Line<'static>, indent: usize) -> Self {
        Self { line, indent }
    }

    /// The line before it is wrapped.
    pub fn line(&self) -> &Line<'static> {
        &self.line
    }

    /// Columns the continuation rows keep.
    pub fn indent(&self) -> usize {
        self.indent
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
    let width = width.max(1);
    let indent = indent.min(width.saturating_sub(MIN_TEXT_COLUMNS));
    let cells: Vec<Cell> = line
        .spans
        .iter()
        .flat_map(|span| {
            span.content
                .chars()
                .map(move |character| (character, UnicodeWidthChar::width(character).unwrap_or(0), span.style))
        })
        .collect();
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut start = 0;
    // An empty line still occupies one row.
    if cells.is_empty() {
        return vec![row(Vec::new(), line)];
    }
    while start < cells.len() {
        let lead = if rows.is_empty() { 0 } else { indent };
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
        let mut built = row_of(&cells[start..end], line);
        if lead > 0 {
            built.spans.insert(0, Span::raw(" ".repeat(lead)));
        }
        rows.push(built);
        start = end;
    }
    rows
}

/// Wrap a rendered block, keeping every line's own indent.
pub fn wrap_rows(lines: Vec<Indented>, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width);
    let mut rows: Vec<Line<'static>> = Vec::new();
    for line in lines {
        rows.extend(wrap_indented(line.line(), width, line.indent()));
    }
    rows
}

/// One row of cells, with adjacent cells of the same style in one span.
fn row_of(cells: &[Cell], line: &Line<'static>) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
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
    row(spans, line)
}

/// One row keeps the line's alignment and style but only its own spans.
fn row(spans: Vec<Span<'static>>, line: &Line<'static>) -> Line<'static> {
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
}
