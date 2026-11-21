//! `/new`, `/switch` and `/resume`: what changing the session refuses, and the
//! new session the bridge is asked for.
//!
//! The three commands share one refusal rule and one warning: a running turn's
//! events would arrive for a session the client no longer shows, and the queue
//! that turn is still working through belongs to it, so the change waits. A
//! model or an effort still waiting for its turn-end pass waits for the same
//! reason: the change would drop it. A connection that is gone has no session to
//! change to. The strings are built here rather than copied per command, so a
//! refusal always names the command the user typed.

use super::{Action, App};
use crate::overlays::picker::{Kind, Picker};

/// The line a session list says when the directory holds nothing.
const NO_SESSIONS: &str = "no sessions in this directory";

impl App {
    /// Start a new session in the directory the client is working in.
    pub(crate) fn start_new_session(&mut self) -> Option<Action> {
        if !self.session_change_allowed("new") {
            return None;
        }
        // The answer's notice depends on this: an attach answer says the stored
        // session was resumed, while a fresh one says it was started, and its
        // empty history is the state the daemon just created rather than a
        // stored conversation with nothing in it.
        self.new_session_pending = true;
        Some(Action::NewSession)
    }

    /// `/switch` and `/resume`: the list, or the session id typed after them.
    ///
    /// `name` is what the refusal names and `title` is what the panel says, so
    /// two commands that do the same thing read as the two different things a
    /// user typed.
    pub(crate) fn change_session(&mut self, name: &str, title: &str, arguments: &str) -> Option<Action> {
        if !self.session_change_allowed(name) {
            return None;
        }
        let id = arguments.trim();
        if !id.is_empty() {
            return Some(Action::Attach(id.to_string()));
        }
        self.picker = Some(Picker::loading(Kind::Sessions, title, NO_SESSIONS));
        Some(Action::Sessions)
    }

    pub(crate) fn rename_session(&mut self, arguments: &str) -> Option<Action> {
        let title = arguments.trim();
        if title.is_empty() || title.chars().any(char::is_control) {
            self.warn("enter a title without control characters: /rename <title>");
            return None;
        }
        self.session_change_allowed("rename").then(|| Action::Rename(title.to_owned()))
    }

    /// Take the flag that says the next attach answer started a fresh session.
    pub(crate) fn take_pending_new_session(&mut self) -> bool {
        std::mem::take(&mut self.new_session_pending)
    }

    /// Whether the session may change now, saying why not when it may not.
    fn session_change_allowed(&mut self, name: &str) -> bool {
        if self.pending_turn {
            self.transcript
                .push_notice(format!("the turn is still running; /{name} works once it ends"));
            return false;
        }
        // The turn flag is already down when a held change leaves, and one
        // leaves per pass: a session change here would drop the other, or send
        // it to a session it was never asked for.
        if self.model_after_turn.is_some() || self.effort_after_turn.is_some() {
            self.transcript.push_notice(format!(
                "a held model or effort change is still being applied; /{name} works in a moment"
            ));
            return false;
        }
        if self.disconnected {
            // A warning, not a notice: the client is unusable for sending, and a
            // refused session change is a consequence of that rather than an
            // ordinary answer.
            self.warn("the harness connection is gone; there is no session to change to");
            return false;
        }
        true
    }
}
