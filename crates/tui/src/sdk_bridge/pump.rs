//! One pass over the event stream: forward what is ready, and say what the
//! connection did.
//!
//! Kept apart from the worker loop because the pass is the part with the
//! timing rules — how long one read blocks, how many frames one pass drains —
//! and it is exercised with a fake stream, while the loop around it needs a
//! daemon.

use std::sync::mpsc::Sender;
use std::time::Duration;

use horon_agent_sdk::api::{ApiEvent, ServerPhase};

use super::{EVENT_BATCH, EVENT_POLL, Incoming, sessions};

/// What the client is told when the transport is gone but the stream never
/// reported an error.
pub(super) const CONNECTION_CLOSED: &str = "the harness connection closed";

/// What one pump pass decided for the worker loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Step {
    /// Nothing was ready; the caller may block waiting for a command.
    Idle,
    /// At least one event was forwarded, so more may be ready right now.
    Flowing,
    /// The connection ended, with the sentence that says why.
    ///
    /// Nothing has been sent to the client yet: a daemon that announced a
    /// restart ends its connection the same way a daemon that stopped does, and
    /// only the worker knows which one it is looking at.
    Ended(String),
    /// The UI dropped its receiver, so nobody is left to tell.
    UiGone,
}

/// Forward the events that are already ready, up to [`EVENT_BATCH`].
///
/// One event per pass was the old cost: every pass then waited
/// [`super::COMMAND_POLL`] on the command queue, so a stream faster than about
/// fifty events a second fell further behind for as long as it lasted, and the
/// SDK's bounded subscription eventually closed with `event_buffer_overflow`.
/// Here the first read may block, which is what keeps an idle client off the
/// CPU, and every read after a hit is a non-blocking poll, so a burst drains at
/// the speed the queue can be written.
///
/// The batch bound is what keeps the pass finite: without it a stream that
/// never pauses would starve the command queue, including the `Cancel` that a
/// user presses during exactly that burst. A stream error is still terminal and
/// still reported as a [`Step::Ended`].
///
/// A closed connection is not a stream error: the SDK answers `Ok(None)` on a
/// closed stream for ever, so `closed` is asked after every empty read. Without
/// it an idle client would keep saying it is ready and poll a dead channel as
/// fast as the loop goes round.
///
/// The stream is subscribed unfiltered because the daemon rewrites a typed
/// session prefix to the full id: a subscription filtered by what the user typed
/// would drop every event of the session it attached to. The filter therefore
/// lives here, against `session_id`, the id the answer named — borrowed, so an
/// event that is forwarded costs no copy and the check itself allocates
/// nothing.
///
/// A restarting announcement is recorded in `restarting` as it passes: it is
/// what tells the worker that the connection ending next is a restart to wait
/// out rather than a session to end.
///
/// `next` is the stream read, generic so the batch can be exercised without a
/// daemon: `Ok(Some(event))` is a frame, `Ok(None)` is a timeout.
pub(super) fn pump<F, E>(
    events: &Sender<Incoming>,
    mut next: F,
    closed: impl Fn() -> bool,
    session_id: &str,
    restarting: &mut bool,
) -> Step
where
    F: FnMut(Duration) -> Result<Option<ApiEvent>, E>,
    E: std::fmt::Display,
{
    let mut timeout = EVENT_POLL;
    let mut forwarded = false;
    for _ in 0..EVENT_BATCH {
        match next(timeout) {
            Ok(Some(event)) => {
                // The read succeeded, so the next one may poll rather than
                // block, whether or not this event is kept.
                timeout = Duration::ZERO;
                if !sessions::belongs(&event, session_id) {
                    continue;
                }
                if let ApiEvent::ServerLifecycle {
                    phase: ServerPhase::Restarting,
                    ..
                } = &event
                {
                    *restarting = true;
                }
                if events.send(Incoming::Event(Box::new(event))).is_err() {
                    return Step::UiGone;
                }
                forwarded = true;
            }
            Ok(None) => {
                if closed() {
                    return Step::Ended(CONNECTION_CLOSED.to_string());
                }
                break;
            }
            Err(error) => {
                // A dropped receiver means the UI already exited, which is the
                // one case where nobody needs to be told why the stream ended.
                return Step::Ended(format!("the event stream ended: {error}"));
            }
        }
    }
    if forwarded { Step::Flowing } else { Step::Idle }
}
