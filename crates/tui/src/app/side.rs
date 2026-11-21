//! `/side`: ask a read-only child of the conversation a question, read its
//! answer over the conversation, and return.
//!
//! The parent is never written to. The child is a fork the bridge creates and
//! narrows to the read-only consult tools, and every result of it arrives as a
//! `Side*` delivery that lands on the overlay rather than in the transcript, so
//! "returning" is closing the overlay and nothing else.

use std::sync::atomic::{AtomicU64, Ordering};

use super::{Action, App};
use crate::overlays::side::SideOverlay;

/// Source of the id every side question carries.
///
/// Deliveries echo the id, so a thread that outlived the screen it started
/// cannot write into the question that replaced it. A process-wide sequence is
/// enough because a process runs one client.
static NEXT_SIDE_ID: AtomicU64 = AtomicU64::new(1);

/// `--at <stored message id>` selects the boundary the child stops before.
///
/// The default is the latest point: the whole stored conversation. A boundary
/// is the parent's stored user message id, the same identity #506's branch
/// picker publishes; a stale id is refused by the daemon and forks nothing.
fn side_arguments(arguments: &str) -> Result<(Option<String>, &str), &'static str> {
    let rest = arguments.trim();
    let Some(rest) = rest.strip_prefix("--at") else {
        return Ok((None, rest));
    };
    let rest = rest.trim_start();
    let mut parts = rest.splitn(2, char::is_whitespace);
    let boundary = parts.next().unwrap_or("").trim();
    let question = parts.next().unwrap_or("").trim();
    if boundary.is_empty() || question.is_empty() {
        return Err("side: --at needs a stored message id and a question");
    }
    Ok((Some(boundary.to_string()), question))
}

impl App {
    /// `/side <question>`: open the overlay and ask the bridge for an answer.
    pub(crate) fn run_side(&mut self, arguments: &str) -> Option<Action> {
        let (boundary, question) = match side_arguments(arguments) {
            Ok(arguments) => arguments,
            Err(message) => {
                self.warn(message);
                return None;
            }
        };
        if question.is_empty() {
            self.warn("side: ask a question, for example /side what did we decide about the cache?");
            return None;
        }
        if self.disconnected {
            self.warn("side: the harness connection is gone");
            return None;
        }
        if self.side.is_some() {
            self.warn("side: a side question is already open; esc returns to the conversation");
            return None;
        }
        if self.session_id.is_none() {
            self.warn("side: no session is attached");
            return None;
        }
        let id = NEXT_SIDE_ID.fetch_add(1, Ordering::Relaxed);
        self.side = Some(SideOverlay::starting(id, question.to_string(), boundary.clone()));
        Some(Action::SideQuestion {
            id,
            question: question.to_string(),
            boundary,
        })
    }

    /// Whether a delivery belongs to the question on screen.
    fn side_matches(&self, id: u64) -> bool {
        self.side.as_ref().is_some_and(|side| side.id() == id)
    }

    /// The fork answered with its id, after it was narrowed to read-only.
    pub(crate) fn side_started(&mut self, id: u64, session_id: String) {
        if !self.side_matches(id) {
            return;
        }
        if let Some(side) = self.side.as_mut() {
            side.started(session_id);
        }
    }

    /// The answer as the child's stream has built it so far.
    pub(crate) fn side_answer(&mut self, id: u64, answer: String) {
        if !self.side_matches(id) {
            return;
        }
        if let Some(side) = self.side.as_mut() {
            side.stream(answer);
        }
    }

    /// What the child is doing now.
    pub(crate) fn side_progress(&mut self, id: u64, line: String) {
        if !self.side_matches(id) {
            return;
        }
        if let Some(side) = self.side.as_mut() {
            side.progress(line);
        }
    }

    /// A refusal or stop worth keeping in view.
    pub(crate) fn side_notice(&mut self, id: u64, line: String) {
        if !self.side_matches(id) {
            return;
        }
        if let Some(side) = self.side.as_mut() {
            side.notice(line);
        }
    }

    /// The turn ended; the stored answer replaces the stream.
    pub(crate) fn side_done(&mut self, id: u64, answer: String) {
        if !self.side_matches(id) {
            return;
        }
        if let Some(side) = self.side.as_mut() {
            side.done(answer);
        }
    }

    /// The side question failed.
    ///
    /// A failure is shown on the overlay when it is still open. When the reader
    /// closed it before the failure landed there is nowhere to show it but the
    /// transcript, which is the one case a side question is allowed a line
    /// there: silence would hide that the question never got an answer. A
    /// failure for a question the reader already replaced is dropped instead:
    /// it says nothing about the one on screen.
    pub(crate) fn side_failed(&mut self, id: u64, message: String) {
        match self.side.as_mut() {
            Some(side) if side.id() == id => side.failed(message),
            Some(_) => {}
            None => self.warn(format!("side: {message}")),
        }
    }

    /// Route one key to the open side question.
    pub(crate) fn side_key(&mut self, key: crossterm::event::KeyEvent) -> Option<Action> {
        let mut side = self.side.take()?;
        match side.on_key(key) {
            crate::overlays::side::Outcome::Handled => {
                self.side = Some(side);
                None
            }
            crate::overlays::side::Outcome::Closed => None,
            crate::overlays::side::Outcome::Open(session_id) => Some(Action::Attach(session_id)),
        }
    }
}
