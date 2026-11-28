//! The pieces drawn around the conversation: the scrollbar in its rightmost
//! column, and the messages waiting for the running turn above the status line.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::{App, Message};
use crate::theme::Theme;

/// The most queued messages drawn; the rest are counted on the last row.
const MAX_QUEUE_ROWS: usize = 3;

/// Rows the queued messages take: one per message up to the cap, and none
/// without a queue.
pub(crate) fn queue_rows(queued: usize) -> u16 {
    u16::try_from(queued.min(MAX_QUEUE_ROWS)).unwrap_or(0)
}

/// One row per waiting message, oldest first, the last row naming how many
/// more wait behind the ones shown.
pub(crate) fn queue_lines(queue: &[Message], rows: usize, theme: &Theme) -> Vec<Line<'static>> {
    let shown = if queue.len() > rows {
        rows.saturating_sub(1)
    } else {
        rows
    };
    let label = Style::default().fg(theme.transcript.notice);
    let text = Style::default()
        .fg(theme.transcript.user_text)
        .add_modifier(Modifier::DIM);
    let mut lines: Vec<Line<'static>> = queue
        .iter()
        .take(shown)
        .map(|message| {
            let first = message.display.lines().next().unwrap_or("");
            let more = if message.display.lines().nth(1).is_some() {
                " …"
            } else {
                ""
            };
            Line::from(vec![
                Span::styled("⏳ queued  ", label),
                Span::styled(format!("{first}{more}"), text),
            ])
        })
        .collect();
    if queue.len() > shown && rows > 0 {
        lines.push(Line::from(Span::styled(
            format!("⏳ +{} more queued", queue.len() - shown),
            label,
        )));
    }
    lines
}

/// The cells of a scrollbar `rows` tall: `true` where the thumb is.
///
/// `total` rows of conversation are seen through `rows` of them, `offset` rows
/// above the end. Nothing is drawn when everything fits.
pub(crate) fn scrollbar_cells(total: usize, rows: usize, offset: usize) -> Option<Vec<bool>> {
    if rows == 0 || total <= rows {
        return None;
    }
    let thumb = (rows * rows / total).clamp(1, rows);
    let travel = rows - thumb;
    let hidden = total - rows;
    let from_top = hidden - offset.min(hidden);
    let start = from_top * travel / hidden;
    Some((0..rows).map(|row| row >= start && row < start + thumb).collect())
}

/// Draw the scrollbar in the rightmost column of `area`, over its first `rows`.
///
/// The height comes from the last measurement of the conversation; a view that
/// follows the end and was never measured has nothing to place a thumb by, so
/// it is left without one rather than given a guess.
pub(crate) fn draw_scrollbar(buffer: &mut Buffer, area: Rect, rows: u16, app: &App) {
    let Some(heights) = app.scroll.heights() else {
        return;
    };
    let total = heights.iter().sum::<usize>() + app.scroll.open_heights().iter().sum::<usize>();
    let Some(cells) = scrollbar_cells(total, usize::from(rows), app.scroll.offset()) else {
        return;
    };
    let x = area.right().saturating_sub(1);
    for (row, thumb) in cells.into_iter().enumerate() {
        let (symbol, color) = if thumb {
            ("┃", app.theme.core.muted)
        } else {
            ("│", app.theme.core.border)
        };
        let y = area.y.saturating_add(u16::try_from(row).unwrap_or(u16::MAX));
        buffer[(x, y)].set_symbol(symbol).set_fg(color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_thumb_follows_the_view_and_hides_when_everything_fits() {
        assert_eq!(scrollbar_cells(5, 10, 0), None);
        // At the end the thumb sits at the bottom; at the top, at the top.
        let end = scrollbar_cells(40, 10, 0).expect("bar");
        assert_eq!(end.iter().filter(|thumb| **thumb).count(), 2);
        assert!(end[9] && end[8] && !end[7]);
        let top = scrollbar_cells(40, 10, 30).expect("bar");
        assert!(top[0] && top[1] && !top[2]);
        // A huge conversation still gets a thumb of one row.
        assert_eq!(
            scrollbar_cells(10_000, 10, 0)
                .expect("bar")
                .iter()
                .filter(|thumb| **thumb)
                .count(),
            1
        );
    }

    #[test]
    fn the_queue_shows_its_oldest_messages_and_counts_the_rest() {
        let theme = Theme::default();
        let queue: Vec<Message> = ["first", "second\nline", "third", "fourth"]
            .iter()
            .map(|text| Message {
                text: (*text).to_string(),
                display: (*text).to_string(),
            })
            .collect();
        let rows = usize::from(queue_rows(queue.len()));
        let text: Vec<String> = queue_lines(&queue, rows, &theme)
            .iter()
            .map(|line| line.spans.iter().map(|span| span.content.as_ref()).collect())
            .collect();
        assert_eq!(text, ["⏳ queued  first", "⏳ queued  second …", "⏳ +2 more queued"]);
        assert_eq!(queue_rows(0), 0);
    }
}
