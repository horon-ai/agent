//! Getting a submitted line to the harness, and what happens when it
//! cannot be sent: the queue, the in-flight slot, refusals and the turn start.

use super::{Action, App, Message};
use crate::commands::Invocation;

impl App {
    /// A message the bridge could not deliver.
    ///
    /// Nothing was sent, so nothing stays in the conversation claiming it was:
    /// the waiting line is dropped and the text goes back to the input line, so
    /// a dropped send costs a key press rather than the message. The turn never
    /// started, so the status returns to idle instead of staying on `running`.
    /// A failure the bridge traced to a closed socket also marks the connection
    /// lost: otherwise the client would keep saying it is ready to send.
    pub fn send_failed(&mut self, text: &str, message: &str, lost: bool) {
        self.pending_turn = false;
        self.session_status = Some("idle".to_string());
        let display = self.take_in_flight(text);
        // Only the line this send drew is taken back: another message with the
        // same text may still be waiting its turn.
        self.transcript.retract_user(&display);
        self.keep_text(&display);
        // The queue must not fire this message again on the next pass: a daemon
        // that keeps refusing would turn the retry into a loop.
        self.resend_held = true;
        if lost {
            self.lose_connection();
        }
        self.warn(message);
    }

    /// The line to retract or keep for a refused send, consuming the slot.
    ///
    /// One message is in flight at a time, so the slot belongs to exactly the
    /// text the bridge reports; the fallback keeps a stray refusal from
    /// retracting nothing.
    fn take_in_flight(&mut self, text: &str) -> String {
        match self.in_flight.take() {
            Some(message) if message.text == text => message.display,
            Some(message) => {
                self.in_flight = Some(message);
                text.to_string()
            }
            None => text.to_string(),
        }
    }

    /// The next queued message, as the action that sends it.
    ///
    /// Called after a turn-end event: a message typed during a turn waits for
    /// that turn rather than racing it, and several of them keep their order.
    /// A message the harness refused holds the queue until the next turn ends
    /// or the user submits again, so a refused send cannot loop.
    pub fn take_queued(&mut self) -> Option<Action> {
        if self.pending_turn || self.disconnected || self.resend_held {
            return None;
        }
        let message = self.queue.first()?.clone();
        self.queue.remove(0);
        self.transcript.promote_queued(&message.display);
        self.in_flight = Some(message.clone());
        Some(self.start_turn(message.text))
    }

    /// Put a message that was not sent back where the user can see it.
    ///
    /// The input line is the natural place when it is free. When the user has
    /// already typed something else, the message is queued instead of
    /// overwriting that: losing either one silently is worse than a queue.
    fn keep_text(&mut self, text: &str) {
        if self.input.is_empty() {
            self.input.set_text(text);
            return;
        }
        self.transcript.push_queued(text);
        self.queue.push(Message::new(text.to_string(), None));
    }

    /// Resolve one submitted line and turn it into an action.
    ///
    /// A slash line either names a built-in UI command, a command file or one
    /// of the harness's commands; a line that names none of them becomes an
    /// error line and never reaches the model.
    pub(crate) fn submit(&mut self, text: &str) -> Option<Action> {
        match self.commands.resolve(text) {
            Invocation::Message(message) => self.send_or_queue(message, None),
            // A command file sends the body it expands to, while the line the
            // user typed stays in the transcript as the message they sent.
            Invocation::Expanded { display, text } => self.send_or_queue(text, Some(display)),
            Invocation::Local { command, arguments } => self.run_local(command, &arguments),
            Invocation::Server { command, arguments } => {
                if self.disconnected {
                    self.refuse(text);
                    return None;
                }
                Some(Action::Runtime { command, arguments })
            }
            Invocation::Error(message) => {
                self.warn(message);
                None
            }
        }
    }

    /// Send a message now, or queue it while a turn is running.
    ///
    /// Sending during a turn is what the daemon answers with "already
    /// processing a message", which loses the text and shows the user an
    /// error for doing something ordinary. The message is kept instead and
    /// sent when the turn ends.
    fn send_or_queue(&mut self, text: String, display: Option<String>) -> Option<Action> {
        // An explicit submit is the user asking again, so a refusal that was
        // holding the queue back no longer blocks this message.
        self.resend_held = false;
        let message = Message::new(text, display);
        if self.disconnected {
            self.refuse(&message.display);
            return None;
        }
        if self.pending_turn {
            self.transcript.push_queued(message.display.clone());
            self.queue.push(message);
            return None;
        }
        // Drawn now, and taken back by `send_failed` if the harness refuses it:
        // the conversation reads in the order things were typed, and a line
        // that was never delivered does not survive the refusal.
        self.transcript.push_user(message.display.clone());
        self.in_flight = Some(message.clone());
        Some(self.start_turn(message.text))
    }

    /// Ask the harness to stop the running turn.
    pub(crate) fn cancel_turn(&mut self) -> Option<Action> {
        self.transcript.push_notice("cancelling the turn");
        Some(Action::Cancel)
    }

    /// Say that a message went nowhere, and put it back in front of the user.
    fn refuse(&mut self, text: &str) {
        self.keep_text(text);
        self.warn("the harness connection is gone; the message was not sent");
    }

    /// Mark a turn as running and hand the text to the bridge.
    fn start_turn(&mut self, text: String) -> Action {
        self.pending_turn = true;
        self.session_status = Some("generating".to_string());
        Action::Send(text)
    }
}
