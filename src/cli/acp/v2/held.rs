//! IS-12: prompts this connection sends while its previous prompt is still
//! being processed.
//!
//! The protocol forbids rejecting a prompt only because foreground work is
//! active (prompt-lifecycle "Prompts during active work"). The daemon already
//! queues a v2 `message` behind another surface's turn and lets it be
//! withdrawn; it refuses only a second `message` from the same connection
//! while that connection's own prompt is processing. So the adapter holds such
//! a prompt itself: it waits, in the order the prompts arrived, until the
//! session takes its next prompt, and only then sends its `message`. Its
//! response stays pending until insertion, as for any prompt.
//!
//! A held prompt has not reached the daemon, so a `$/cancel_request`, a
//! `session/cancel` or a `session/close` answers it `-32800` and it is never
//! sent.

use super::super::*;
use super::state::V2Session;

/// How long a held prompt waits before it re-checks on its own. Every release
/// of the session's prompt wakes the waiters; this only bounds a release that
/// forgot to.
const RECHECK: Duration = Duration::from_millis(250);

/// How a held prompt left the queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hold {
    /// The session took this prompt: it now owns `prompt_running`.
    Taken,
    /// A cancel reached the prompt before it was sent.
    Cancelled,
    /// The session closed before the prompt was sent.
    Closed,
}

/// The prompts waiting for their turn, oldest first.
#[derive(Debug, Default)]
pub(crate) struct HeldQueue {
    waiting: std::collections::VecDeque<u64>,
    next_ticket: u64,
}

impl V2Session {
    /// Take the session's prompt for this request, waiting behind every prompt
    /// that arrived earlier. Returns at once when nothing waits and no prompt
    /// of this connection is being processed.
    pub(crate) async fn take_prompt(&self, session: &DaemonSession, cancel: &CancelSignal) -> Hold {
        let ticket = {
            let mut held = self.held();
            if held.waiting.is_empty() && try_take(session) {
                return Hold::Taken;
            }
            held.next_ticket += 1;
            let ticket = held.next_ticket;
            held.waiting.push_back(ticket);
            ticket
        };
        let mut changes = self.subscribe_changes();
        loop {
            changes.mark_unchanged();
            if let Some(outcome) = self.settle(session, cancel, ticket) {
                return outcome;
            }
            tokio::select! {
                _ = changes.changed() => {}
                () = cancel.recorded() => {}
                () = tokio::time::sleep(RECHECK) => {}
            }
        }
    }

    /// Whether `ticket` leaves the queue now, and how.
    fn settle(&self, session: &DaemonSession, cancel: &CancelSignal, ticket: u64) -> Option<Hold> {
        let mut held = self.held();
        let outcome = if cancel.is_recorded() {
            Hold::Cancelled
        } else if self.is_closed() {
            Hold::Closed
        } else if held.waiting.front() == Some(&ticket) && try_take(session) {
            Hold::Taken
        } else {
            return None;
        };
        held.waiting.retain(|waiting| *waiting != ticket);
        drop(held);
        // The next prompt in line may go now, or re-check its own cancel.
        self.changed();
        Some(outcome)
    }

    /// The prompt this connection was processing is done: the next held
    /// prompt may take the session.
    pub(crate) fn prompt_released(&self, session: &DaemonSession) {
        session.prompt_running.store(false, Ordering::SeqCst);
        self.changed();
    }
}

fn try_take(session: &DaemonSession) -> bool {
    session
        .prompt_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}
