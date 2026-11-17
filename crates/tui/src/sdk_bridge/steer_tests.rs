//! A steer's answer: what an accepted request settles, and what a refusal
//! carries back to the queue.
//!
//! Nothing here talks to a daemon: the SDK call is one request in the caller,
//! and what it feeds into — the messages the client held, and the sentence it
//! shows when the harness refused them — is what these tests pin.

use horon_agent_sdk::{Error, ErrorKind};

use super::Incoming;
use super::steer::answer;

#[test]
fn an_accepted_steer_echoes_the_text_it_carried() {
    assert_eq!(
        answer(Ok(()), "first\n\nsecond"),
        Incoming::Steered {
            text: "first\n\nsecond".to_string()
        }
    );
}

#[test]
fn a_refused_steer_carries_the_text_and_the_reason() {
    let error = Error::new(ErrorKind::Timeout, "the harness did not answer");
    assert_eq!(
        answer(Err(error), "only one"),
        Incoming::SteerFailed {
            text: "only one".to_string(),
            message: "could not send the queued messages into the turn: timeout: the harness did not answer"
                .to_string(),
        }
    );
}
