//! Terminal indications derived from the current app state.

use super::{App, Prompt};

impl App {
    /// What the terminal should say about the client right now.
    ///
    /// One place decides, from the state the status line already reads, so the
    /// tab indicator and the status record can never disagree with each other
    /// or with what the screen says. Precedence follows the user's attention: a
    /// lost connection outranks everything, a prompt outranks a running turn
    /// (an approval is what is blocking it), and a turn that ended outranks the
    /// idle state until the user does something.
    ///
    /// The share is the todo list's: it is the only counted work the client
    /// knows about. A turn with no counted list reports no share, which the
    /// protocols draw as an indeterminate spinner instead of a frozen 0%.
    pub(crate) fn terminal_signal(&self) -> crate::progress::Signal {
        use crate::progress::{Kind, Signal};
        if self.disconnected {
            return Signal::error();
        }
        if let Some(prompt) = &self.prompt {
            let kind = match prompt {
                Prompt::Approval(_) => Kind::Permission,
                Prompt::Ask(_) => Kind::Question,
            };
            return Signal::blocked(kind);
        }
        if self.pending_turn {
            return Signal::working(self.todos.percent());
        }
        match self.outcome {
            Some(outcome) => outcome.signal(),
            None => Signal::idle(),
        }
    }

    /// Forget how the last turn ended, because the user has acted on it.
    ///
    /// A submission, a session change and a key that answers a prompt all mean
    /// the user is looking at the client again, so the mark on the tab has
    /// served its purpose and the next report is the state they are in now.
    pub(crate) fn note_user_attention(&mut self) {
        self.outcome = None;
    }
}
