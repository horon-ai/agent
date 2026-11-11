//! Breaking rendered lines at a fixed width.
//!
//! The transcript pane keeps only the rows that fit its height, so the rows
//! must be counted after wrapping, not before: a wrapped line occupies as many
//! rows as its cells need. Doing it here also keeps the wrapper's own wrap out
//! of the pane, where it would wrap after the tail was already chosen.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// Break `line` into rows of at most `width` cells, keeping each span's style.
///
/// A wide character that does not fit on the current row starts the next one
/// instead of being split, and a row is only wider than `width` when one
/// character alone is wider than the pane.
pub fn wrap_line(line: &Line<'static>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut text = String::new();
    let mut style = Style::default();
    let mut used = 0;
    for span in &line.spans {
        if !text.is_empty() && span.style != style {
            spans.push(Span::styled(std::mem::take(&mut text), style));
        }
        style = span.style;
        for character in span.content.chars() {
            let cells = UnicodeWidthChar::width(character).unwrap_or(0);
            if used > 0 && used + cells > width {
                if !text.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut text), style));
                }
                rows.push(row(std::mem::take(&mut spans), line));
                used = 0;
            }
            text.push(character);
            used += cells;
        }
    }
    if !text.is_empty() {
        spans.push(Span::styled(text, style));
    }
    // An empty line still occupies one row; anything else has been flushed
    // only when a character overflowed it.
    if !spans.is_empty() || rows.is_empty() {
        rows.push(row(spans, line));
    }
    rows
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
}
