//! Sending queued messages into the running turn.
//!
//! One request in, one answer out. The answer is not the injection event: the
//! daemon acknowledges a soft interrupt when it queues it, so an accepted
//! request is what lets the client settle the messages it held, and a refusal
//! hands the same text back for the queue. The text travels in the answer so a
//! client can tell a steer it is still holding from one it never sent.

use horon_agent_sdk::{Error, HoronAgentClient};

use super::Incoming;

/// What a refusal reads as before the harness's own sentence.
const REFUSAL: &str = "could not send the queued messages into the turn";

/// Inject `text` into the turn this connection is running.
///
/// The session id and the client are the ones the worker holds when the command
/// is served, never ones captured by the client that asked: a `/new` replaces
/// the connection, and a steer queued before it must not reach the session it
/// left.
pub(super) fn send(client: &HoronAgentClient, session_id: &str, text: &str) -> Incoming {
    answer(client.soft_interrupt(session_id, text, true), text)
}

/// The answer one soft-interrupt request produces.
///
/// The failure is carried inside the answer rather than up to the worker: the
/// client has messages to put back, which the worker's own failure path would
/// report as a request with nothing to restore.
pub(super) fn answer(result: Result<(), Error>, text: &str) -> Incoming {
    match result {
        Ok(()) => Incoming::Steered { text: text.to_string() },
        Err(error) => Incoming::SteerFailed {
            text: text.to_string(),
            message: format!("{REFUSAL}: {error}"),
        },
    }
}
