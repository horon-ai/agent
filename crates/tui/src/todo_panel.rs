use horon_agent_sdk::todo::TodoItem;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;

/// The most items the panel draws under its heading.
const MAX_ITEMS: usize = 8;
/// Cells of the progress bar in the heading.
const BAR_CELLS: usize = 12;

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct TodoPanel {
    total: usize,
    completed: usize,
    open: usize,
    rows: Vec<TodoRow>,
    /// The complete list the rows summarize, in stored order.
    ///
    /// Kept beside the rows so the editor and the passive display are built
    /// from the same read: an edit refreshes both, and neither can show rows
    /// from a revision the other does not hold.
    items: Vec<TodoItem>,
    /// The stored list's revision, absent on a harness that does not report
    /// one. Without it an edit cannot be checked, so the list stays read-only.
    revision: Option<String>,
    /// The status change sent and not yet answered, if any.
    ///
    /// One at a time: a second edit sent before the first answer would carry a
    /// revision that the first is about to replace.
    pending: Option<TodoPending>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TodoPending {
    pub id: String,
    pub status: String,
}

/// One revision-checked status change the todo view asks the bridge to commit.
///
/// It is a named value rather than a four-field variant so the action stays one
/// line in the app's vocabulary and the fields are documented where the todo
/// state lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoStatusRequest {
    pub session_id: String,
    pub revision: String,
    pub id: String,
    pub status: String,
}

#[derive(Debug, PartialEq, Eq)]
struct TodoRow {
    /// The item's stored status: `pending`, `in_progress`, `blocked`,
    /// `completed` or `cancelled`.
    status: String,
    text: String,
}

impl TodoPanel {
    pub(crate) fn from_snapshot(items: &[TodoItem], revision: Option<String>) -> Self {
        let mut panel = Self {
            items: items.to_vec(),
            revision,
            ..Self::default()
        };
        for item in items {
            if item.status != "cancelled" {
                panel.total += 1;
                if item.status == "completed" {
                    panel.completed += 1;
                }
            }
        }
        panel.open = panel.total - panel.completed;
        // The whole list in stored order, so the reader sees the plan as it was
        // written with finished steps struck through. A list longer than the
        // panel gives up its oldest finished rows first: the open work is what
        // the panel is for.
        let mut rows: Vec<TodoRow> = items
            .iter()
            .map(|item| {
                let mut text = String::new();
                if let Some(group) = item.group.as_deref().filter(|group| !group.trim().is_empty()) {
                    append_text(&mut text, group);
                    text.push_str(" / ");
                }
                append_text(&mut text, &item.content);
                if item.status == "blocked"
                    && let Some(reason) = item.blocker.as_deref().filter(|reason| !reason.trim().is_empty())
                {
                    text.push_str(" · ");
                    append_text(&mut text, reason);
                }
                TodoRow {
                    status: item.status.clone(),
                    text,
                }
            })
            .collect();
        while rows.len() > MAX_ITEMS {
            let finished = rows
                .iter()
                .position(|row| matches!(row.status.as_str(), "completed" | "cancelled"));
            match finished {
                Some(index) => {
                    rows.remove(index);
                }
                None => rows.truncate(MAX_ITEMS),
            }
        }
        panel.rows = rows;
        panel
    }

    /// Share of the list that is done, or none when there is nothing to count.
    ///
    /// A cancelled item is not in `total`, so the share describes the work that
    /// is still expected; an empty list has no share at all, which is what makes
    /// a progress report from it indeterminate rather than 0%.
    pub(crate) fn percent(&self) -> Option<u8> {
        if self.total == 0 {
            return None;
        }
        Some((self.completed * 100 / self.total) as u8)
    }

    pub(crate) fn height(&self, available: u16) -> u16 {
        if self.total == 0 || (available < 2 && self.open > 0) {
            return 0;
        }
        (self.rows.len() as u16 + 1).min(available)
    }

    pub(crate) fn lines<'a>(&'a self, height: u16, theme: &'a Theme) -> impl Iterator<Item = Line<'a>> {
        let shown = usize::from(height.saturating_sub(1));
        // A panel shorter than the list gives up its finished rows first, in
        // stored order, so the open work stays on screen.
        let mut kept: Vec<&TodoRow> = self.rows.iter().collect();
        while kept.len() > shown {
            match kept
                .iter()
                .position(|row| matches!(row.status.as_str(), "completed" | "cancelled"))
            {
                Some(index) => {
                    kept.remove(index);
                }
                None => kept.truncate(shown),
            }
        }
        let hidden = self.items.len().saturating_sub(kept.len());
        let filled = (self.completed * BAR_CELLS).checked_div(self.total).unwrap_or(0);
        let mut heading = vec![
            Span::styled(
                "Todos  ",
                Style::default().fg(theme.core.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled("━".repeat(filled), Style::default().fg(theme.core.success)),
            Span::styled("─".repeat(BAR_CELLS - filled), Style::default().fg(theme.core.border)),
            Span::styled(
                format!(" {}/{} complete", self.completed, self.total),
                Style::default().fg(theme.core.muted),
            ),
        ];
        if hidden > 0 {
            heading.push(Span::styled(
                format!(" · {hidden} more"),
                Style::default().fg(theme.core.muted),
            ));
        }
        std::iter::once(Line::from(heading)).chain(kept.into_iter().map(move |row| {
            let text = Style::default().fg(theme.core.text);
            let struck = Style::default()
                .fg(theme.core.muted)
                .add_modifier(Modifier::CROSSED_OUT);
            let (glyph, glyph_style, text_style) = match row.status.as_str() {
                "completed" => ("✓", Style::default().fg(theme.core.success), struck),
                "cancelled" => ("✕", Style::default().fg(theme.core.muted), struck),
                "in_progress" => (
                    "◐",
                    Style::default().fg(theme.core.accent),
                    text.fg(theme.core.accent).add_modifier(Modifier::BOLD),
                ),
                "blocked" => (
                    "⊘",
                    Style::default().fg(theme.core.warning),
                    text.fg(theme.core.warning),
                ),
                _ => ("○", Style::default().fg(theme.core.muted), text),
            };
            Line::from(vec![
                Span::styled(format!("  {glyph} "), glyph_style),
                Span::styled(row.text.as_str(), text_style),
            ])
        }))
    }

    /// The complete list the rows summarize, in stored order.
    pub(crate) fn items(&self) -> &[TodoItem] {
        &self.items
    }

    /// The revision of the stored list these items were read from.
    pub(crate) fn revision(&self) -> Option<&str> {
        self.revision.as_deref()
    }

    /// The status change already sent and not yet answered.
    pub(crate) fn pending(&self) -> Option<&TodoPending> {
        self.pending.as_ref()
    }

    /// Remember one status change as in flight.
    ///
    /// Refused when another change is already in flight: both would carry the
    /// same expected revision, and the answer to the first would make the
    /// second stale after the user had already been told it was sent.
    pub(crate) fn begin_edit(&mut self, id: String, status: String) -> bool {
        if self.pending.is_some() {
            return false;
        }
        self.pending = Some(TodoPending { id, status });
        true
    }

    /// Forget the in-flight change; the answer (applied or refused) settled it.
    pub(crate) fn clear_edit(&mut self) {
        self.pending = None;
    }

    /// Put an in-flight change back after the panel was rebuilt from a fresh
    /// read.
    ///
    /// A `TodoChanged` refresh can arrive while the answer to an edit is still
    /// on its way. Dropping the pending marker there would let a second press
    /// carry the revision the first one is about to replace.
    pub(crate) fn restore_pending(&mut self, pending: Option<TodoPending>) {
        self.pending = pending;
    }
}

fn append_text(out: &mut String, value: &str) {
    out.extend(value.chars().take(512).map(|ch| if ch.is_control() { ' ' } else { ch }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(content: &str, status: &str) -> TodoItem {
        TodoItem {
            content: content.into(),
            status: status.into(),
            ..TodoItem::default()
        }
    }

    fn drawn(panel: &TodoPanel, height: u16) -> Vec<(String, Vec<Modifier>)> {
        let theme = Theme::default();
        panel
            .lines(height, &theme)
            .map(|line| {
                let text = line.spans.iter().map(|span| span.content.as_ref()).collect();
                let modifiers = line.spans.iter().map(|span| span.style.add_modifier).collect();
                (text, modifiers)
            })
            .collect()
    }

    #[test]
    fn finished_items_stay_in_order_ticked_and_struck_through() {
        let panel = TodoPanel::from_snapshot(
            &[
                item("plan", "completed"),
                item("build", "in_progress"),
                item("ship", "pending"),
                item("old idea", "cancelled"),
            ],
            None,
        );
        let rows = drawn(&panel, 10);
        let text: Vec<&str> = rows.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(
            text,
            [
                "Todos  ━━━━──────── 1/3 complete",
                "  ✓ plan",
                "  ◐ build",
                "  ○ ship",
                "  ✕ old idea",
            ]
        );
        assert!(
            rows[1].1[1].contains(Modifier::CROSSED_OUT),
            "a finished item is not struck through"
        );
        assert!(
            rows[4].1[1].contains(Modifier::CROSSED_OUT),
            "a cancelled item is not struck through"
        );
        assert!(!rows[2].1[1].contains(Modifier::CROSSED_OUT));
        assert!(
            rows[2].1[1].contains(Modifier::BOLD),
            "the active item does not stand out"
        );
    }

    #[test]
    fn a_short_panel_gives_finished_rows_to_the_open_work() {
        let panel = TodoPanel::from_snapshot(
            &[
                item("plan", "completed"),
                item("build", "in_progress"),
                item("ship", "pending"),
            ],
            None,
        );
        let rows = drawn(&panel, 3);
        let text: Vec<&str> = rows.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(
            text,
            ["Todos  ━━━━──────── 1/3 complete · 1 more", "  ◐ build", "  ○ ship"]
        );
    }
}
