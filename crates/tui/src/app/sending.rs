//! Getting a submitted line to the harness, and what happens when it
//! cannot be sent: the queue, the in-flight slot, refusals and the turn start.

use super::{Action, App, Message, PendingSteer};
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

    /// The next action a turn boundary releases, one per call.
    ///
    /// A model or an effort asked for during the turn goes first, the model
    /// before the effort: the report that follows a switch clears an effort it
    /// does not name, so an effort sent first could be wiped off the status line
    /// by the answer to the model. Then comes the oldest queued message. The
    /// waiting values are not held back by `resend_held`: a refused message
    /// waits for the user, while these were asked for during the turn and are
    /// the answer to that request.
    ///
    /// Called after a turn-end event: a message typed during a turn waits for
    /// that turn rather than racing it, and several of them keep their order.
    /// A message the harness refused holds the queue until the next turn ends
    /// or the user submits again, so a refused send cannot loop.
    pub fn take_queued(&mut self) -> Option<Action> {
        if self.pending_turn || self.disconnected {
            return None;
        }
        if let Some(model) = self.model_after_turn.take() {
            return Some(Action::Model(model));
        }
        if let Some(level) = self.effort_after_turn.take() {
            // The level steers the cycle key until an answer names it, exactly
            // as one sent straight away does.
            self.effort_requested = Some(level.clone());
            return Some(Action::Effort(level));
        }
        if self.resend_held {
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
        // A message the user just sent is what they want to read next, so a
        // view held above the end follows it again: otherwise the line and its
        // answer are drawn below the window until `ctrl+end`. The commands that
        // hold the view on purpose (`/keys`) never reach this function.
        self.scroll.to_bottom();
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

    /// Hand the queued messages to the running turn instead of stopping it.
    ///
    /// `esc` means "this turn should hear what I have typed"; with nothing
    /// queued there is nothing to send, so it cancels exactly as it always did.
    /// The queue empties here, while the rows stay drawn as queued until the
    /// bridge answers: a refusal then needs no redraw, and the text is never on
    /// screen twice. One steer is in flight per session, so a second `esc` while
    /// one waits cancels rather than stacking messages the client could not put
    /// back together; a steer the session change left behind is answered
    /// elsewhere and does not hold this session's key.
    pub(crate) fn steer_turn(&mut self) -> Option<Action> {
        if self.disconnected || self.steer_outstanding_here() {
            return self.cancel_turn();
        }
        let text = joined_text(&self.queue);
        if text.is_empty() {
            return self.cancel_turn();
        }
        // The check above leaves the slot empty or holding a steer of a session
        // the client has left. That record waits in the list for the answer it
        // was sent for, and this steer takes the slot.
        if let Some(pending) = self.steer_pending.take() {
            self.steer_left_behind.push_back(pending);
        }
        self.steer_pending = Some(PendingSteer {
            text: text.clone(),
            session_id: self.session_id.clone(),
            messages: std::mem::take(&mut self.queue),
        });
        Some(Action::Steer(text))
    }

    /// The running turn took the messages a steer carried.
    ///
    /// Each queued row becomes the message the model received, in the order the
    /// messages were typed; the notice says what happened, so a reply that
    /// answers text the user sent mid-turn reads as one.
    pub(crate) fn steered(&mut self, text: &str) {
        let Some(pending) = self.take_steer(text) else {
            return;
        };
        if !self.carried_into(&pending) {
            // The rows went with the session these messages were queued for.
            return;
        }
        for message in &pending.messages {
            self.transcript.promote_queued(&message.display);
        }
        self.transcript.push_notice("sent into the running turn");
    }

    /// The harness refused a steer: put the messages back where the user can
    /// see them and say so.
    ///
    /// They go in front of the queue, in their own order, so messages typed
    /// while the request was in flight keep their place behind them.
    pub(crate) fn steer_failed(&mut self, text: &str, message: &str) {
        let Some(pending) = self.take_steer(text) else {
            return;
        };
        if !self.carried_into(&pending) {
            // Restoring them would send one conversation's text into another:
            // the queue was cleared when the session changed, and these belong
            // to the session that was left.
            self.warn(message);
            return;
        }
        self.restore(pending);
        self.warn(message);
    }

    /// The steer an answer about `text` settles.
    ///
    /// The bridge answers steers in the order it received them, so the oldest
    /// record still outstanding is the one the next answer belongs to: a listed
    /// steer of a session the client left is taken first, and the slot only
    /// when nothing older is listed. A listed steer is taken only by an answer
    /// naming its own text; other text is not this client's answer to it, so it
    /// stays listed and the answer is matched against the slot as usual. The
    /// slot keeps its own text check, so a steer naming other text stays where
    /// it is.
    fn take_steer(&mut self, text: &str) -> Option<PendingSteer> {
        let oldest_names_it = self
            .steer_left_behind
            .front()
            .is_some_and(|pending| pending.text == text);
        if oldest_names_it {
            return self.steer_left_behind.pop_front();
        }
        let pending = self.steer_pending.take()?;
        if pending.text != text {
            self.steer_pending = Some(pending);
            return None;
        }
        Some(pending)
    }

    /// Whether a steer in flight belongs to the session in force.
    ///
    /// A steer still unanswered when the client moves is answered for the
    /// session it was made in: its refusal is still shown, but it says nothing
    /// about the turn running under the session shown now, and holding the key
    /// back for it would turn the first `esc` there into a cancel. Unknown ids
    /// count as this session, so the ordinary case stays one steer at a time.
    fn steer_outstanding_here(&self) -> bool {
        self.steer_pending
            .as_ref()
            .is_some_and(|pending| self.carried_into(pending))
    }

    /// Whether a steer's messages still belong to the session in force.
    ///
    /// The ids are compared only when both are known: before the first identity
    /// event the client names no session, and an unknown id never proves a
    /// change.
    fn carried_into(&self, pending: &PendingSteer) -> bool {
        match (&pending.session_id, &self.session_id) {
            (Some(queued_for), Some(current)) => queued_for == current,
            _ => true,
        }
    }

    /// Put a steer's messages back in front of the queue, in their own order.
    fn restore(&mut self, pending: PendingSteer) {
        let mut restored = pending.messages;
        restored.append(&mut self.queue);
        self.queue = restored;
    }

    /// Put a steer the connection lost back in front of the queue.
    ///
    /// The messages were never taken by the harness, so a connection that dies
    /// with one in flight reports them with the rest of the queue: those of any
    /// listed steer that belongs to the session in force and then the slot's,
    /// oldest first, so the queue keeps the order they were typed. A steer made
    /// for a session the client has already left is not one of them: its queue
    /// went with that session, and putting its text back here would send one
    /// conversation's words into another.
    pub(super) fn restore_steer(&mut self) {
        let mut restored: Vec<Message> = Vec::new();
        while let Some(pending) = self.steer_left_behind.pop_front() {
            if self.carried_into(&pending) {
                restored.extend(pending.messages);
            }
        }
        if let Some(pending) = self.steer_pending.take()
            && self.carried_into(&pending)
        {
            restored.extend(pending.messages);
        }
        if restored.is_empty() {
            return;
        }
        restored.append(&mut self.queue);
        self.queue = restored;
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

/// The queued messages as one body, one blank line between them.
///
/// A message that is empty once trimmed contributes no paragraph: the harness
/// would read an empty one as a message of its own, and the turn would hear
/// nothing the user said.
fn joined_text(messages: &[Message]) -> String {
    let mut body = String::new();
    for message in messages {
        if message.text.trim().is_empty() {
            continue;
        }
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(&message.text);
    }
    body
}
