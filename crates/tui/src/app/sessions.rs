//! `/new`, `/switch`, `/resume` and `/swarm`: what changing the session refuses,
//! and the new session or child the bridge is asked for.
//!
//! The commands that change the session share one refusal rule and one warning:
//! a running turn's events would arrive for a session the client no longer
//! shows, and the queue that turn is still working through belongs to it, so
//! the change waits. A model or an effort still waiting for its turn-end pass
//! waits for the same reason: the change would drop it. A connection that is
//! gone has no session to change to. The strings are built here rather than
//! copied per command, so a refusal always names the command the user typed.
//!
//! `/swarm` only reads until a row is chosen, and the choice is an attach like
//! the others: it waits under the same rule, so a working turn is never left
//! showing a conversation the client no longer follows.

use horon_agent_sdk::RuntimeCommand;

use super::{Action, App};
use crate::overlays::picker::{Kind, Picker};

/// The line a session list says when the directory holds nothing.
const NO_SESSIONS: &str = "no sessions in this directory";

/// The title the `/swarm` list is drawn with.
const SWARM_TITLE: &str = "Swarm children";

/// What the `/swarm` list says when the harness reports no child.
const NO_SWARM: &str = "no swarm children reported for this session";

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

    /// `/swarm`: the children of the attached session, or the harness's report.
    ///
    /// The bare name opens the list the bridge fills with this session's
    /// children and the parent row that returns to it. Anything typed after the
    /// name still reaches the harness's own `swarm` command, which is where the
    /// enabled flag, the swarm id and the role are reported: the built-in owns
    /// the bare name, not the whole command.
    pub(crate) fn run_swarm(&mut self, arguments: &str) -> Option<Action> {
        let arguments = arguments.trim();
        if !arguments.is_empty() {
            return Some(Action::Runtime {
                command: RuntimeCommand::Swarm,
                arguments: arguments.split_whitespace().map(str::to_string).collect(),
            });
        }
        if self.disconnected {
            self.warn("the harness connection is gone; there are no swarm children to list");
            return None;
        }
        self.picker = Some(Picker::loading(Kind::Swarm, SWARM_TITLE, NO_SWARM));
        Some(Action::Swarm)
    }

    pub(crate) fn rename_session(&mut self, arguments: &str) -> Option<Action> {
        let title = arguments.trim();
        if title.is_empty() || title.chars().any(char::is_control) {
            self.warn("enter a title without control characters: /rename <title>");
            return None;
        }
        self.session_change_allowed("rename")
            .then(|| Action::Rename(title.to_owned()))
    }

    /// Take the flag that says the next attach answer started a fresh session.
    pub(crate) fn take_pending_new_session(&mut self) -> bool {
        std::mem::take(&mut self.new_session_pending)
    }

    /// Whether the session may change now, saying why not when it may not.
    ///
    /// Shared with the pickers that choose a session: a row chosen while a turn
    /// is running would leave that turn's events arriving for a conversation the
    /// client no longer shows, which is the same reason a typed `/switch` waits.
    pub(super) fn session_change_allowed(&mut self, name: &str) -> bool {
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
