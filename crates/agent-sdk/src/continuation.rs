//! Continuing a run that failed, with the daemon's answer in hand.
//!
//! A continuation is not fire-and-forget like an ordinary message. The daemon is
//! the one that decides whether the named run may still be continued, and a
//! request that is quietly ignored would be the duplicate turn this whole
//! feature exists to prevent. So this call:
//!
//! * refuses locally unless the daemon advertises [`CAPABILITY_TURN_RETRY_V1`],
//!   because a daemon without it ignores `expected_run_id` and would run an
//!   ordinary turn against a stale run;
//! * waits for the daemon's answer on the same correlation id as the request, so
//!   a refusal reaches the caller as an error instead of a silent success;
//! * reads that answer as the fact it is. An ordinary message is acknowledged
//!   when the daemon receives it, but a continuation is answered `ok` only once
//!   the daemon has decided to begin a new run for the run it named. `ok` is
//!   therefore the whole answer, and this call does not wait for the turn.
//!
//! The notification path used by [`HoronAgentClient::send_message`] is unchanged:
//! an ordinary turn's acceptance is still reported through the event stream.

use horon_agent_harness_api::{ApiEvent, ApiRequest};

use crate::client::HoronAgentClient;
use crate::{Error, ErrorKind, Result};

/// The hidden instruction a continuation carries.
///
/// It is not user prose: the transcript already holds the prompt this continues,
/// and a visible second copy would read as the user asking twice. The wording
/// names the two things a resumed attempt must not get wrong — repeat finished
/// work, or stop early to ask what to do. The reminder-only turn path adds the
/// `[auto] ` prefix when it stores the message.
pub const CONTINUATION_REMINDER: &str = "The previous model request failed with a transient error and this run \
was resumed automatically. Continue the task from where it left off. Do not repeat tool calls or edits that already \
completed, and do not ask the user what to do next.";

impl HoronAgentClient {
    /// Continue the failed run `expected_run_id`, reporting the daemon's answer.
    ///
    /// The failed prompt is already in the transcript, so nothing user-visible is
    /// sent: the continuation is a hidden instruction, and the visible history
    /// keeps the one user prompt the failed turn already stored. The removal of
    /// that prompt from history is not this call's business; the daemon refuses
    /// the request before storing anything when the run is stale, in flight, or
    /// ineligible, so a refusal leaves the conversation untouched.
    ///
    /// Read the id from [`HoronAgentClient::get_runtime_state`]'s `last_stop`:
    /// the daemon recorded it, so it names the run that actually ended.
    pub fn continue_run(&self, session_id: &str, expected_run_id: &str) -> Result<()> {
        self.require_capability(
            horon_agent_harness_api::CAPABILITY_TURN_RETRY_V1,
            "continue_run",
        )?;
        if expected_run_id.trim().is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidOption,
                "continue_run needs the id of the run to continue; read it from get_runtime_state's last_stop",
            ));
        }
        let frame = self.request(ApiRequest::SendMessage {
            session_id: session_id.to_string(),
            content: String::new(),
            system_reminder: Some(CONTINUATION_REMINDER.to_string()),
            images: Vec::new(),
            no_reply: false,
            expected_run_id: Some(expected_run_id.to_string()),
        })?;
        match frame.event {
            // A refusal is the daemon's verdict, and it is the answer: the run
            // moved on, never stopped, or ended for a reason a retry cannot fix.
            ApiEvent::Error { code, message, details } => Err(Error::from_frame(
                code,
                message,
                details,
                Some(session_id),
            )),
            // `ok` is the answer to *this* question, not a receipt: the daemon
            // holds it until it has begun the new run, so a continuation that
            // returns here has a run behind it. Nothing further is awaited —
            // waiting for the turn's own events would make a call that means
            // "continue this" block for the length of the continuation.
            ApiEvent::Ok => Ok(()),
            other => Err(Error::new(
                ErrorKind::UnexpectedReply,
                format!("continue_run expected `ok` or an error, got {other:?}"),
            )),
        }
    }
}
