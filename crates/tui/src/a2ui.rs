//! Read-only A2UI surface fallback card state, replay parser, and terminal renderer.
//!
//! Default `horon-tui` connections keep A2UI negotiation disabled so the daemon
//! never expands its tool catalog or invalidates the prompt cache. When the client
//! inspects or replays a stored session with `ContentBlock::A2ui`, or observes
//! live `a2ui_envelope` events on a shared or configured session, this module
//! turns them into bounded, read-only bordered cards using the active theme and
//! symbol preset.

use horon_agent_tool_types::a2ui::{MAX_A2UI_ENVELOPES_PER_CALL, render_a2ui_text_card};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::app::App;
use crate::item_lines::Look;
use crate::transcript::Transcript;
use crate::wrap::Indented;

/// Maximum rendered body lines kept per A2UI card in the transcript pane.
const MAX_CARD_LINES: usize = 36;

/// Maximum characters kept per line in a rendered card before truncation.
const MAX_CARD_LINE_CHARS: usize = 240;

/// Stored or live read-only A2UI surface fallback card in the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct A2uiCard {
    /// Tool call identifier correlating live envelopes within one emission.
    pub call_id: String,
    /// Ordered `(sequence, envelope)` entries when built from live `a2ui_envelope` frames.
    pub sequences: Vec<(u32, Value)>,
    /// Optional plain-text fallback carried beside the envelopes.
    pub fallback_text: Option<String>,
    /// Pre-formatted plain lines starting with `[A2UI ...]`.
    pub lines: Vec<String>,
}

impl A2uiCard {
    /// Build a card from the first observed live `a2ui_envelope` frame.
    pub fn from_envelope(
        call_id: impl Into<String>,
        sequence: u32,
        envelope: Value,
        fallback_text: Option<String>,
    ) -> Self {
        let mut card = Self {
            call_id: call_id.into(),
            sequences: Vec::new(),
            fallback_text: None,
            lines: Vec::new(),
        };
        card.apply_envelope(sequence, envelope, fallback_text);
        card
    }

    /// Build a card from a stored `[A2UI ...]` text card emitted by history replay.
    pub fn from_rendered_block(raw: &str) -> Self {
        let mut lines = Vec::new();
        for line in raw.lines() {
            let cleaned = clean_card_line(line.trim_end());
            if !cleaned.is_empty() || !lines.is_empty() {
                lines.push(cleaned);
            }
        }
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        if lines.is_empty() {
            lines.push("[A2UI response]".to_string());
        }
        Self {
            call_id: String::new(),
            sequences: Vec::new(),
            fallback_text: None,
            lines,
        }
    }

    /// Merge an arriving envelope into this card in sequence order.
    pub fn apply_envelope(&mut self, sequence: u32, envelope: Value, fallback_text: Option<String>) {
        if let Some(slot) = self
            .sequences
            .iter_mut()
            .find(|(existing_seq, _)| *existing_seq == sequence)
        {
            slot.1 = envelope;
        } else if self.sequences.len() < MAX_A2UI_ENVELOPES_PER_CALL {
            self.sequences.push((sequence, envelope));
            self.sequences.sort_by_key(|(seq, _)| *seq);
        }

        if let Some(text) = fallback_text
            && !text.trim().is_empty()
        {
            self.fallback_text = Some(text);
        }

        let ordered: Vec<Value> = self.sequences.iter().map(|(_, env)| env.clone()).collect();
        let rendered = render_a2ui_text_card(&ordered, self.fallback_text.as_deref());
        self.lines = rendered.lines().map(clean_card_line).collect();
        if self.lines.is_empty() {
            self.lines.push("[A2UI response]".to_string());
        }
    }
}

impl App {
    /// Apply a live `a2ui_envelope` event when it belongs to the active session.
    pub(crate) fn apply_a2ui_envelope(
        &mut self,
        session_id: &str,
        tool_call_id: &str,
        sequence: u32,
        envelope: &Value,
        fallback_text: Option<&str>,
    ) {
        let follows = self.session_id.as_deref().is_none_or(|current| current == session_id);
        if follows
            && self.transcript.apply_a2ui_envelope(
                tool_call_id,
                sequence,
                envelope.clone(),
                fallback_text.map(str::to_string),
            )
        {
            self.scroll.reflow();
        }
    }
}

/// Replay a stored assistant answer, splitting out `[A2UI ...]` fallback cards.
///
/// Messages without an A2UI header line outside fenced code blocks pass straight
/// through to [`Transcript::push_assistant_stored`] untouched so note addressing
/// and markdown rendering remain identical for non-A2UI conversations.
pub(crate) fn push_replayed_assistant(transcript: &mut Transcript, content: String, mut stored: Option<String>) {
    if !contains_unquoted_card_header(&content) {
        transcript.push_assistant_stored(content, stored);
        return;
    }

    enum Part {
        Prose(String),
        Card(String),
    }

    let mut parts: Vec<Part> = Vec::new();
    let mut fence: Option<(char, usize)> = None;

    for raw_line in content.lines() {
        let trimmed = raw_line.trim_end();
        if let Some((fence_ch, fence_len)) = fence {
            if is_closing_fence(trimmed, fence_ch, fence_len) {
                fence = None;
            }
            match parts.last_mut() {
                Some(Part::Card(buf)) => push_line(buf, trimmed),
                Some(Part::Prose(buf)) => push_line(buf, raw_line),
                None => parts.push(Part::Prose(raw_line.to_string())),
            }
            continue;
        }

        if let Some((fence_ch, fence_len)) = opening_fence(trimmed) {
            fence = Some((fence_ch, fence_len));
            match parts.last_mut() {
                Some(Part::Card(buf)) => push_line(buf, trimmed),
                Some(Part::Prose(buf)) => push_line(buf, raw_line),
                None => parts.push(Part::Prose(raw_line.to_string())),
            }
            continue;
        }

        if is_card_header(trimmed) {
            parts.push(Part::Card(trimmed.to_string()));
            continue;
        }

        match parts.last_mut() {
            Some(Part::Card(buf)) => push_line(buf, trimmed),
            Some(Part::Prose(buf)) => push_line(buf, raw_line),
            None => parts.push(Part::Prose(raw_line.to_string())),
        }
    }

    for part in parts {
        match part {
            Part::Prose(text) if !text.trim().is_empty() => {
                transcript.push_assistant_stored(text, stored.take());
            }
            Part::Prose(_) => {}
            Part::Card(card_raw) => {
                transcript.push_a2ui_card(A2uiCard::from_rendered_block(&card_raw));
            }
        }
    }
}

/// Render an [`A2uiCard`] as styled, indented transcript rows.
pub(crate) fn lines(card: &A2uiCard, look: &Look<'_>) -> Vec<Indented> {
    let border = Style::default().fg(look.theme.core.border);
    let accent_bold = Style::default().fg(look.theme.core.accent).add_modifier(Modifier::BOLD);
    let accent = Style::default().fg(look.theme.core.accent);
    let muted = Style::default().fg(look.theme.core.muted);
    let plain = Style::default().fg(look.theme.core.text);
    let prefix = format!("{} ", look.symbols.quote);

    let Some((header, rest)) = card.lines.split_first() else {
        return Vec::new();
    };

    let mut out = Vec::with_capacity(card.lines.len().min(MAX_CARD_LINES + 1));
    let mut header_spans = vec![
        Span::styled(prefix.clone(), border),
        Span::styled(header.clone(), accent_bold),
    ];
    if look.width >= 36 && !header.starts_with("[A2UI closed:") {
        header_spans.push(Span::styled(" · read-only", muted));
    }
    out.push(Indented::new(Line::from(header_spans), 2));

    let body_cap = MAX_CARD_LINES.saturating_sub(1);
    let visible = rest.len().min(body_cap);
    for row in &rest[..visible] {
        let mut spans = vec![Span::styled(prefix.clone(), border)];
        if row.starts_with("[Button ") {
            spans.push(Span::styled(row.clone(), accent));
        } else if let Some(after_dash) = row.strip_prefix("- ") {
            spans.push(Span::styled("- ", muted));
            spans.push(Span::styled(after_dash.to_string(), plain));
        } else {
            spans.push(Span::styled(row.clone(), plain));
        }
        out.push(Indented::new(Line::from(spans), 2));
    }

    let hidden = rest.len().saturating_sub(visible);
    if hidden > 0 {
        out.push(Indented::new(
            Line::from(vec![
                Span::styled(prefix, border),
                Span::styled(format!("… +{hidden} more lines"), muted),
            ]),
            2,
        ));
    }

    out
}

fn contains_unquoted_card_header(content: &str) -> bool {
    let mut fence: Option<(char, usize)> = None;
    for raw_line in content.lines() {
        let trimmed = raw_line.trim_end();
        if let Some((fence_ch, fence_len)) = fence {
            if is_closing_fence(trimmed, fence_ch, fence_len) {
                fence = None;
            }
            continue;
        }
        if let Some((fence_ch, fence_len)) = opening_fence(trimmed) {
            fence = Some((fence_ch, fence_len));
            continue;
        }
        if is_card_header(trimmed) {
            return true;
        }
    }
    false
}

fn is_card_header(line: &str) -> bool {
    if line == "[A2UI response]" {
        return true;
    }
    line.strip_prefix("[A2UI surface: ")
        .or_else(|| line.strip_prefix("[A2UI closed: "))
        .and_then(|rest| rest.strip_suffix(']'))
        .is_some_and(|id| !id.trim().is_empty())
}

fn opening_fence(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let ch = trimmed.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let count = trimmed.chars().take_while(|c| *c == ch).count();
    if count >= 3 { Some((ch, count)) } else { None }
}

fn is_closing_fence(line: &str, ch: char, min_len: usize) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= min_len && trimmed.chars().all(|c| c == ch)
}

fn push_line(buf: &mut String, line: &str) {
    if !buf.is_empty() {
        buf.push('\n');
    }
    buf.push_str(line);
}

fn clean_card_line(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(MAX_CARD_LINE_CHARS));
    let mut chars = raw.chars().peekable();
    let mut emitted = 0usize;
    let mut truncated = false;
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        let mapped = if ch == '\t' {
            ' '
        } else if ch.is_control() {
            continue;
        } else {
            ch
        };
        if emitted >= MAX_CARD_LINE_CHARS {
            truncated = true;
            break;
        }
        out.push(mapped);
        emitted += 1;
    }
    if truncated {
        out.push('…');
    }
    out
}

#[cfg(test)]
#[path = "a2ui_tests.rs"]
mod tests;
