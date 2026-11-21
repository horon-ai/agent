use horon_agent_sdk::todo::TodoItem;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::theme::Theme;

const MAX_ITEMS: usize = 5;

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
    status: &'static str,
    text: String,
}

impl TodoPanel {
    pub(crate) fn from_items(items: &[TodoItem]) -> Self {
        Self::from_snapshot(items, None)
    }

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
        for (filter, label) in [
            ("in_progress", "Active"),
            ("blocked", "Blocked"),
            ("pending", "Pending"),
        ] {
            for item in items.iter().filter(|item| item.status == filter) {
                if panel.rows.len() == MAX_ITEMS {
                    return panel;
                }
                let mut text = String::new();
                if let Some(group) = item.group.as_deref().filter(|group| !group.trim().is_empty()) {
                    append_text(&mut text, group);
                    text.push_str(" / ");
                }
                append_text(&mut text, &item.content);
                if filter == "blocked"
                    && let Some(reason) = item.blocker.as_deref().filter(|reason| !reason.trim().is_empty())
                {
                    text.push_str(" · ");
                    append_text(&mut text, reason);
                }
                panel.rows.push(TodoRow { status: label, text });
            }
        }
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
        let hidden = self.open.saturating_sub(shown);
        let mut heading = format!("Todos · {}/{} complete", self.completed, self.total);
        if hidden > 0 {
            heading.push_str(&format!(" · {hidden} more"));
        }
        std::iter::once(Line::styled(heading, Style::default().fg(theme.core.accent))).chain(
            self.rows.iter().take(shown).map(move |row| {
                let color = match row.status {
                    "Active" => theme.core.accent,
                    "Blocked" => theme.core.warning,
                    _ => theme.core.muted,
                };
                Line::from(vec![
                    Span::styled(format!("  {} · ", row.status), Style::default().fg(color)),
                    Span::styled(row.text.as_str(), Style::default().fg(theme.core.text)),
                ])
            }),
        )
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
