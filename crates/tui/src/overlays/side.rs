//! The side-question screen: one question asked of a read-only child of the
//! conversation, and the answer as it arrives.
//!
//! It is drawn over the transcript and owns the keyboard while it is open, but
//! it changes nothing underneath: closing it reveals the conversation exactly as
//! it was, and `enter` moves the client to the stored child instead, which is
//! how an answer is read in full or revisited later.
//!
//! The overlay is a pure state machine. The bridge hands it text, progress and a
//! final answer; the keys it understands are only leave and open, plus scrolling
//! so a long answer is readable without leaving.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use std::borrow::Cow;

use crate::theme::{Symbols, Theme};
use crate::wrap::wrap_indented;

/// Narrowest pane the panel is drawn in; below it the overlay says where it is.
const MIN_WIDTH: u16 = 18;
/// Shortest pane the panel is drawn in.
const MIN_HEIGHT: u16 = 8;
/// Columns a wrapped question keeps on its continuation rows.
const QUESTION_INDENT: usize = 3;
/// Lines of notice the panel shows above the footer.
const NOTICE_ROWS: usize = 3;

/// What the side turn has done so far.
#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// The fork has not answered with its id yet.
    Starting,
    /// The child exists and its turn is running.
    Answering,
    /// The answer is complete.
    Done,
    /// The question could not be started or answered.
    Failed(String),
}

/// What a key on the overlay asks the client to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The key was consumed here.
    Handled,
    /// Leave the overlay; the conversation underneath is shown again.
    Closed,
    /// Move the client to this stored session.
    Open(String),
}

struct QuestionWrapCache {
    width: u16,
    accent: Color,
    text: Color,
    lines: Vec<Line<'static>>,
}

struct AnswerWrapCache {
    width: u16,
    text_color: Color,
    muted_color: Color,
    placeholder_kind: u8,
    rendered_rev: u64,
    committed_bytes: usize,
    committed_rows: usize,
    has_placeholder: bool,
    rows: Vec<Line<'static>>,
}

impl Default for AnswerWrapCache {
    fn default() -> Self {
        Self {
            width: 0,
            text_color: Color::Reset,
            muted_color: Color::Reset,
            placeholder_kind: u8::MAX,
            rendered_rev: u64::MAX,
            committed_bytes: 0,
            committed_rows: 0,
            has_placeholder: false,
            rows: Vec::new(),
        }
    }
}

/// One side question, as the screen shows it.
pub struct SideOverlay {
    /// The client's own id for this question.
    ///
    /// Deliveries carry it back, so a thread left running by a question the
    /// reader closed cannot write into a later one.
    id: u64,
    question: String,
    /// The stored message the child stops before, when one was chosen.
    boundary: Option<String>,
    session_id: Option<String>,
    state: State,
    /// The answer, streamed and then replaced by the stored one.
    answer: String,
    answer_rev: u64,
    /// What the child is doing now, on one line.
    progress: Option<String>,
    /// Refusals and stops worth keeping in view.
    notices: Vec<String>,
    /// The top row of the answer when the reader scrolled away from its end.
    held: Option<usize>,
    /// Rows the answer wrapped to in the last frame.
    rows: usize,
    /// Height the answer had in the last frame.
    height: usize,
    question_cache: Option<QuestionWrapCache>,
    answer_cache: AnswerWrapCache,
}

impl SideOverlay {
    /// A question that has just been sent to the bridge.
    pub fn starting(id: u64, question: String, boundary: Option<String>) -> Self {
        Self {
            id,
            question,
            boundary,
            session_id: None,
            state: State::Starting,
            answer: String::new(),
            answer_rev: 0,
            progress: None,
            notices: Vec::new(),
            held: None,
            rows: 0,
            height: 0,
            question_cache: None,
            answer_cache: AnswerWrapCache::default(),
        }
    }

    /// The child exists; its id is what `enter` opens.
    pub fn started(&mut self, session_id: String) {
        self.session_id = Some(session_id);
        self.state = State::Answering;
    }

    /// The answer as the child's stream has built it so far.
    pub fn stream(&mut self, answer: String) {
        if matches!(&self.state, State::Starting | State::Answering) {
            if self.answer == answer {
                return;
            }
            if !answer.starts_with(&self.answer) {
                self.answer_cache.committed_bytes = 0;
                self.answer_cache.committed_rows = 0;
                self.answer_cache.rows.clear();
            }
            self.answer = answer;
            self.answer_rev = self.answer_rev.wrapping_add(1);
        }
    }

    /// What the child is doing now.
    pub fn progress(&mut self, line: String) {
        if matches!(&self.state, State::Starting | State::Answering) {
            self.progress = Some(line);
        }
    }

    /// Something about the turn to keep in view.
    pub fn notice(&mut self, line: String) {
        self.notices.push(line);
    }

    /// The turn ended; the stored answer is authoritative over the stream.
    pub fn done(&mut self, answer: String) {
        if !answer.trim().is_empty() && self.answer != answer {
            if !answer.starts_with(&self.answer) {
                self.answer_cache.committed_bytes = 0;
                self.answer_cache.committed_rows = 0;
                self.answer_cache.rows.clear();
            }
            self.answer = answer;
            self.answer_rev = self.answer_rev.wrapping_add(1);
        }
        self.state = State::Done;
        self.progress = None;
    }

    /// The question could not be started or answered.
    pub fn failed(&mut self, message: String) {
        self.state = State::Failed(message);
        self.progress = None;
    }

    /// The client's id for this question.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The question as it was asked.
    pub fn question(&self) -> &str {
        &self.question
    }

    /// The stored message the child stops before, when one was chosen.
    pub fn boundary(&self) -> Option<&str> {
        self.boundary.as_deref()
    }

    /// The child's stored id, once the fork answered.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// The answer as it stands: streamed, or the stored one after the turn.
    pub fn answer(&self) -> &str {
        &self.answer
    }

    /// The refusals and stops recorded for this question.
    pub fn notices(&self) -> &[String] {
        &self.notices
    }

    /// Whether the turn finished.
    pub fn is_done(&self) -> bool {
        self.state == State::Done
    }

    /// The failure that ended the question, when there was one.
    pub fn failure(&self) -> Option<&str> {
        match &self.state {
            State::Failed(message) => Some(message.as_str()),
            _ => None,
        }
    }

    /// The line printed under the question.
    fn status_line(&self) -> Cow<'_, str> {
        match &self.state {
            State::Starting => Cow::Borrowed("starting a read-only side session"),
            State::Answering => self
                .progress
                .as_deref()
                .map(Cow::Borrowed)
                .unwrap_or(Cow::Borrowed("the side session is answering")),
            State::Done => Cow::Borrowed("done; the answer is stored in this side session"),
            State::Failed(message) => Cow::Borrowed(message.as_str()),
        }
    }

    /// Route one keystroke. Every key is consumed: the conversation behind the
    /// panel is not being edited while it is open.
    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Esc => Outcome::Closed,
            KeyCode::Enter => match self.session_id.clone() {
                Some(session_id) => Outcome::Open(session_id),
                None => Outcome::Handled,
            },
            KeyCode::Up => {
                self.hold(self.top().saturating_sub(1));
                Outcome::Handled
            }
            KeyCode::Down => {
                let next = self.top().saturating_add(1);
                if next + self.height >= self.rows {
                    self.held = None;
                } else {
                    self.held = Some(next);
                }
                Outcome::Handled
            }
            KeyCode::PageUp => {
                self.hold(self.top().saturating_sub(self.height.max(1)));
                Outcome::Handled
            }
            KeyCode::PageDown => {
                let next = self.top().saturating_add(self.height.max(1));
                if next + self.height >= self.rows {
                    self.held = None;
                } else {
                    self.held = Some(next);
                }
                Outcome::Handled
            }
            _ => Outcome::Handled,
        }
    }

    /// The top row currently drawn.
    fn top(&self) -> usize {
        match self.held {
            Some(top) => top.min(self.limit()),
            None => self.limit(),
        }
    }

    /// The largest top row that still fills the body.
    fn limit(&self) -> usize {
        self.rows.saturating_sub(self.height)
    }

    /// Hold the view at `top`, following the end again when that is the end.
    fn hold(&mut self, top: usize) {
        if top >= self.limit() {
            self.held = None;
        } else {
            self.held = Some(top);
        }
    }
}

/// Draw the side question over `area`.
///
/// The answer's wrapped row count is recorded on the overlay so the scroll keys
/// move by rows rather than by lines, which is the unit the pane draws in.
pub fn draw(frame: &mut Frame<'_>, area: Rect, overlay: &mut SideOverlay, theme: &Theme, symbols: Symbols) {
    // The panel is modal, so the conversation behind it is not being read; a
    // stray transcript character beside the border would look like part of it.
    frame.render_widget(Clear, area);
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " side question hidden: window too small, esc returns",
                Style::default().fg(theme.core.muted),
            ))),
            area,
        );
        return;
    }
    let width = area.width.saturating_sub(2).min(96);
    let panel = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y,
        width,
        height: area.height,
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.core.border))
        .title(Span::styled(
            format!(" {} Side question ", symbols.bullet),
            Style::default().fg(theme.core.accent),
        ));
    let inner = block.inner(panel);
    frame.render_widget(block, panel);
    if inner.width == 0 || inner.height < 3 {
        return;
    }

    let question_len = ensure_question_lines(overlay, inner.width, theme).len();
    let q_rows = u16::try_from(question_len.clamp(1, 4)).unwrap_or(1);
    let notices = overlay.notices.len().min(NOTICE_ROWS);
    let layout = Layout::vertical([
        Constraint::Length(q_rows),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(u16::try_from(notices).unwrap_or(0)),
        Constraint::Length(1),
    ]);
    let [question_area, status_area, body_area, notice_area, footer_area] = layout.areas(inner);

    let question = ensure_question_lines(overlay, inner.width, theme).to_vec();
    frame.render_widget(Paragraph::new(question), question_area);

    let (status, style) = match &overlay.state {
        State::Failed(_) => (
            overlay.status_line().into_owned(),
            Style::default().fg(theme.core.error),
        ),
        State::Done => (
            overlay.status_line().into_owned(),
            Style::default().fg(theme.core.muted).add_modifier(Modifier::ITALIC),
        ),
        _ => (
            overlay.status_line().into_owned(),
            Style::default().fg(theme.core.warning).add_modifier(Modifier::ITALIC),
        ),
    };
    frame.render_widget(Paragraph::new(Line::from(Span::styled(status, style))), status_area);

    ensure_answer_rows(overlay, body_area.width.max(1), theme);
    let total_rows = overlay.answer_cache.rows.len();
    overlay.rows = total_rows;
    overlay.height = usize::from(body_area.height);
    let top = overlay.top();
    let slice_top = top.min(usize::from(u16::MAX));
    let visible_end = slice_top.saturating_add(overlay.height).min(total_rows);
    let visible = if slice_top < visible_end {
        overlay.answer_cache.rows[slice_top..visible_end].to_vec()
    } else {
        Vec::new()
    };
    frame.render_widget(Paragraph::new(visible), body_area);

    if notices > 0 {
        // The newest notices are the ones that explain the current state, so
        // the oldest are dropped when the list is longer than the rows kept
        // for them.
        let start = overlay.notices.len().saturating_sub(notices);
        let lines: Vec<Line<'static>> = overlay.notices[start..]
            .iter()
            .map(|line| Line::from(Span::styled(line.clone(), Style::default().fg(theme.core.warning))))
            .collect();
        frame.render_widget(Paragraph::new(lines), notice_area);
    }

    let boundary = match overlay.boundary.as_deref() {
        Some(boundary) => format!("from {boundary} · "),
        None => String::new(),
    };
    let footer = match overlay.session_id.as_deref() {
        Some(session_id) => format!(
            "{boundary}enter opens {session_id} · esc returns to the conversation{}",
            if overlay.is_done() && overlay.held.is_some() {
                " · ↑↓ scroll"
            } else {
                ""
            }
        ),
        None => format!("{boundary}esc returns to the conversation"),
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(footer, Style::default().fg(theme.core.muted)))),
        footer_area,
    );
}

/// Rows the question occupies, capped so a pasted paragraph cannot take the
/// whole panel away from the answer.
fn ensure_question_lines<'a>(overlay: &'a mut SideOverlay, width: u16, theme: &Theme) -> &'a [Line<'static>] {
    let accent = theme.core.accent;
    let text = theme.core.text;
    let stale = overlay
        .question_cache
        .as_ref()
        .is_none_or(|c| c.width != width || c.accent != accent || c.text != text);
    if stale {
        let question_line = Line::from(vec![
            Span::styled("Q: ", Style::default().fg(accent)),
            Span::styled(overlay.question.clone(), Style::default().fg(text)),
        ]);
        let lines = wrap_indented(&question_line, usize::from(width), QUESTION_INDENT);
        overlay.question_cache = Some(QuestionWrapCache {
            width,
            accent,
            text,
            lines,
        });
    }
    overlay.question_cache.as_ref().map_or(&[], |c| c.lines.as_slice())
}

/// The answer as wrapped rows, with a line saying what there is to read.
fn ensure_answer_rows(overlay: &mut SideOverlay, width: u16, theme: &Theme) {
    let placeholder_kind = match &overlay.state {
        State::Failed(_) => 0,
        State::Done => 1,
        _ => 2,
    };
    let text_color = theme.core.text;
    let muted_color = theme.core.muted;
    let c = &mut overlay.answer_cache;
    if c.rendered_rev == overlay.answer_rev
        && c.width == width
        && c.text_color == text_color
        && c.muted_color == muted_color
        && c.placeholder_kind == placeholder_kind
    {
        return;
    }
    let can_incremental = c.width == width
        && c.text_color == text_color
        && c.muted_color == muted_color
        && !c.has_placeholder
        && c.committed_bytes <= overlay.answer.len()
        && (c.committed_bytes == 0 || overlay.answer.as_bytes().get(c.committed_bytes - 1) == Some(&b'\n'));
    if can_incremental {
        c.rows.truncate(c.committed_rows);
    } else {
        c.rows.clear();
        c.committed_bytes = 0;
        c.committed_rows = 0;
    }
    c.has_placeholder = false;
    let tail = &overlay.answer[c.committed_bytes..];
    if !tail.is_empty() {
        let style = Style::default().fg(text_color);
        let wrap_width = usize::from(width);
        let push_wrapped = |rows: &mut Vec<Line<'static>>, slice: &str| {
            for line in slice.lines() {
                let styled = Line::from(Span::styled(line.to_string(), style));
                let wrapped = wrap_indented(&styled, wrap_width, 0);
                if wrapped.is_empty() {
                    rows.push(Line::default());
                } else {
                    rows.extend(wrapped);
                }
            }
        };
        if let Some(last_nl) = tail.rfind('\n') {
            let settled_rel = last_nl + 1;
            push_wrapped(&mut c.rows, &tail[..settled_rel]);
            c.committed_bytes += settled_rel;
            c.committed_rows = c.rows.len();
            let rem = &tail[settled_rel..];
            if !rem.is_empty() {
                push_wrapped(&mut c.rows, rem);
            }
        } else {
            push_wrapped(&mut c.rows, tail);
        }
    }
    if c.rows.is_empty() {
        let placeholder = match &overlay.state {
            State::Failed(_) => "",
            State::Done => "(the side session ended without an answer)",
            _ => "(waiting for the answer)",
        };
        if !placeholder.is_empty() {
            c.rows.push(Line::from(Span::styled(
                placeholder,
                Style::default().fg(muted_color).add_modifier(Modifier::ITALIC),
            )));
            c.has_placeholder = true;
        }
    }
    c.width = width;
    c.text_color = text_color;
    c.muted_color = muted_color;
    c.placeholder_kind = placeholder_kind;
    c.rendered_rev = overlay.answer_rev;
}
