//! A read-only report drawn as a panel over the conversation: `/session`.
//!
//! The report arrives as text: a line with no `key: value` shape is a section
//! heading, a blank line separates sections, and every other line is a field
//! whose key is drawn muted. The panel scrolls with the arrow and page keys and
//! the wheel, and `esc`, `q` or `enter` closes it, revealing the conversation
//! exactly as it was.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::theme::Theme;
use crate::wrap::wrap_indented;

/// Widest the panel grows; a wider pane keeps it centred.
const MAX_WIDTH: u16 = 110;

/// What a key on the panel asks the client to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The key was consumed here.
    Handled,
    /// Close the panel.
    Closed,
}

/// One report on screen.
pub struct InfoOverlay {
    title: String,
    text: String,
    /// The first wrapped row drawn.
    top: usize,
    /// Rows the report wrapped to, and the rows the body showed, last frame.
    rows: usize,
    height: usize,
}

impl InfoOverlay {
    pub fn new(title: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            text: text.into(),
            top: 0,
            rows: 0,
            height: 0,
        }
    }

    /// The report as it arrived.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Route one keystroke. Every key is consumed while the panel is open.
    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        let page = self.height.max(1);
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => return Outcome::Closed,
            KeyCode::Up | KeyCode::Char('k') => self.scroll_up(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_down(1),
            KeyCode::PageUp => self.scroll_up(page),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_down(page),
            KeyCode::Home => self.top = 0,
            KeyCode::End => self.top = self.limit(),
            _ => {}
        }
        Outcome::Handled
    }

    pub fn scroll_up(&mut self, rows: usize) {
        self.top = self.top.saturating_sub(rows);
    }

    pub fn scroll_down(&mut self, rows: usize) {
        self.top = self.top.saturating_add(rows).min(self.limit());
    }

    fn limit(&self) -> usize {
        self.rows.saturating_sub(self.height)
    }

    /// The report's lines, styled: headings bold, field keys muted.
    fn lines(&self, theme: &Theme) -> Vec<Line<'static>> {
        let muted = Style::default().fg(theme.core.muted);
        let text = Style::default().fg(theme.core.text);
        self.text
            .lines()
            .map(|line| match line.split_once(": ") {
                Some((key, value)) if !key.is_empty() && !key.contains("  ") => Line::from(vec![
                    Span::styled(format!("{key}: "), muted),
                    Span::styled(value.to_string(), text),
                ]),
                _ if line.trim().is_empty() => Line::default(),
                _ => Line::from(Span::styled(line.to_string(), text.add_modifier(Modifier::BOLD))),
            })
            .collect()
    }
}

/// Draw the report over `area`.
pub fn draw(frame: &mut Frame<'_>, area: Rect, overlay: &mut InfoOverlay, theme: &Theme) {
    frame.render_widget(Clear, area);
    let width = area.width.min(MAX_WIDTH);
    let panel = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y,
        width,
        height: area.height,
    };
    let border = Style::default().fg(theme.core.accent);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(Span::styled(
            format!(" {} ", overlay.title),
            Style::default().fg(theme.core.warning).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(panel);
    frame.render_widget(block, panel);
    if inner.width < 4 || inner.height < 3 {
        return;
    }
    // The text keeps two columns from each edge, like a card's.
    let padded = Rect {
        x: inner.x + 2,
        width: inner.width - 4,
        ..inner
    };
    let [body, rule, footer] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1), Constraint::Length(1)]).areas(padded);

    let rows: Vec<Line<'static>> = overlay
        .lines(theme)
        .iter()
        .flat_map(|line| wrap_indented(line, usize::from(body.width.max(1)), 0))
        .collect();
    overlay.rows = rows.len();
    overlay.height = usize::from(body.height);
    overlay.top = overlay.top.min(overlay.limit());
    let visible: Vec<Line<'static>> = rows.into_iter().skip(overlay.top).take(overlay.height).collect();
    frame.render_widget(Paragraph::new(visible), body);

    let rule_area = Rect {
        x: inner.x,
        width: inner.width,
        ..rule
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled("─".repeat(usize::from(inner.width)), border))),
        rule_area,
    );
    let mut hint = String::from("↑/↓ scroll · esc close");
    if overlay.rows > overlay.height {
        hint.push_str(&format!(
            " · {}-{} of {}",
            overlay.top + 1,
            (overlay.top + overlay.height).min(overlay.rows),
            overlay.rows
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(hint, Style::default().fg(theme.core.muted)))),
        footer,
    );
}
