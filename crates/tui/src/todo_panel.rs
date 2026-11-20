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
}

#[derive(Debug, PartialEq, Eq)]
struct TodoRow {
    status: &'static str,
    text: String,
}

impl TodoPanel {
    pub(crate) fn from_items(items: &[TodoItem]) -> Self {
        let mut panel = Self::default();
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
}

fn append_text(out: &mut String, value: &str) {
    out.extend(value.chars().take(512).map(|ch| if ch.is_control() { ' ' } else { ch }));
}
