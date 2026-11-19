//! Connecting to the harness again after the daemon restarted.
//!
//! A reload replaces the daemon process and takes the bridge's daemon
//! connection with it; the bridge itself keeps listening, so the client dials
//! the same API socket it used before. What it cannot do is dial at a fixed
//! moment: the reloaded daemon builds and execs itself, and until it answers a
//! connect there either fails at the handshake or finds nothing behind the
//! bridge. The ladder here waits that out, and stops waiting when it is clear
//! nothing is coming.

use std::fmt;
use std::thread::sleep;
use std::time::Duration;

use horon_agent_sdk::{EventStream, HoronAgentClient};

use super::sessions::Dial;

/// How many times one restart is waited out.
///
/// A reload that rebuilds the daemon takes minutes, so the ladder is generous;
/// it is still finite, because a client that waits for ever on a daemon that
/// will not come back is a client that never says the server is gone.
const ATTEMPTS: usize = 40;

/// The first wait between attempts.
const FIRST_WAIT: Duration = Duration::from_millis(250);

/// The longest wait between attempts.
const LONGEST_WAIT: Duration = Duration::from_secs(2);

/// Why the harness could not be reached again, in words the client can show.
///
/// A sentence rather than an error kind because the only consumer is the
/// banner: the user needs to know what the last attempt saw, not which layer
/// refused it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Unreachable(String);

impl Unreachable {
    fn new(sentence: impl Into<String>) -> Self {
        Self(sentence.into())
    }
}

impl fmt::Display for Unreachable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Connect to the harness again and re-attach `session_id`.
///
/// The client and its stream come back together because the worker has to move
/// to both: the new connection serves one session, and the stream opened before
/// the attach is what catches the identity event the attach publishes.
pub(super) fn session(settings: &Dial, session_id: &str) -> Result<(HoronAgentClient, EventStream), Unreachable> {
    with_retries(ATTEMPTS, sleep, || connect_once(settings, session_id))
}

/// One attempt: connect, subscribe, attach.
fn connect_once(settings: &Dial, session_id: &str) -> Result<(HoronAgentClient, EventStream), Unreachable> {
    // `ensure_runtime` is what makes this survive more than a daemon restart:
    // the bridge is started again when nothing answers on its socket, which is
    // the case when the old one exited with the last client.
    let client = HoronAgentClient::connect(settings.options())
        .map_err(|error| Unreachable::new(format!("the harness did not come back: {error}")))?;
    // Subscribed before the request: the daemon publishes the identity event
    // (provider, model, reasoning effort) while the attach is in flight, and a
    // stream opened after the answer would drop it.
    let stream = client.events(None);
    client.attach_session(session_id).map_err(|error| {
        Unreachable::new(format!(
            "the harness came back but session {session_id} could not be attached: {error}"
        ))
    })?;
    Ok((client, stream))
}

/// Run `attempt` until it answers, waiting between tries.
///
/// The last failure is what is reported, because it is the one that describes
/// the harness as it is now. `wait` is a parameter so the ladder can be
/// exercised without spending its real time.
fn with_retries<T>(
    attempts: usize,
    mut wait: impl FnMut(Duration),
    mut attempt: impl FnMut() -> Result<T, Unreachable>,
) -> Result<T, Unreachable> {
    let mut last = Unreachable::new("the harness did not answer");
    for index in 0..attempts {
        match attempt() {
            Ok(value) => return Ok(value),
            Err(error) => last = error,
        }
        if index + 1 < attempts {
            wait(pause(index));
        }
    }
    Err(last)
}

/// How long to wait after attempt `index` before the next one: doubling from
/// [`FIRST_WAIT`] and capped at [`LONGEST_WAIT`].
///
/// The ladder is deliberately short at the start. Most reloads either answer
/// within a second or take long enough that a quarter second more or less does
/// not matter.
fn pause(index: usize) -> Duration {
    let doublings = u32::try_from(index.min(3)).unwrap_or(3);
    FIRST_WAIT.saturating_mul(1 << doublings).min(LONGEST_WAIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reconnect_that_answers_first_time_does_not_wait() {
        let mut waits = Vec::new();
        let answer = with_retries(3, |wait| waits.push(wait), || Ok::<_, Unreachable>("connected"));
        assert_eq!(answer.as_deref(), Ok("connected"));
        assert!(waits.is_empty(), "a first-attempt success waited: {waits:?}");
    }

    #[test]
    fn a_restart_in_progress_is_waited_out_until_the_harness_answers() {
        let mut waits = Vec::new();
        let mut attempts = 0;
        let answer = with_retries(
            5,
            |wait| waits.push(wait),
            || {
                attempts += 1;
                if attempts < 3 {
                    Err(Unreachable::new("nothing is listening"))
                } else {
                    Ok("connected")
                }
            },
        );
        assert_eq!(answer.as_deref(), Ok("connected"));
        assert_eq!(waits, vec![pause(0), pause(1)], "the ladder waited differently");
    }

    #[test]
    fn a_harness_that_never_comes_back_ends_the_ladder_with_its_last_reason() {
        let mut waits = Vec::new();
        let mut attempts = 0;
        let answer: Result<(), Unreachable> = with_retries(
            3,
            |wait| waits.push(wait),
            || {
                attempts += 1;
                Err(Unreachable::new(format!("attempt {attempts}")))
            },
        );
        assert_eq!(answer, Err(Unreachable::new("attempt 3")));
        assert_eq!(attempts, 3, "the ladder ran past its bound");
        assert_eq!(waits.len(), 2, "the last attempt is not followed by a wait");
    }

    #[test]
    fn the_wait_grows_and_then_stays_at_the_cap() {
        let ladder: Vec<Duration> = (0..6).map(pause).collect();
        assert_eq!(ladder[0], FIRST_WAIT);
        for pair in ladder.windows(2) {
            assert!(pair[1] >= pair[0], "the ladder shrank: {ladder:?}");
        }
        assert_eq!(ladder[5], LONGEST_WAIT, "the ladder grew past its cap");
        assert!(
            ladder.contains(&LONGEST_WAIT),
            "the ladder never reached its cap: {ladder:?}"
        );
    }

    #[test]
    fn the_whole_ladder_is_bounded_and_long_enough_for_a_rebuild() {
        let total: Duration = (0..ATTEMPTS - 1).map(pause).sum();
        assert!(
            total >= Duration::from_secs(60),
            "a restart is given up too early: {total:?}"
        );
        assert!(
            total <= Duration::from_secs(120),
            "a stopped daemon is waited on too long: {total:?}"
        );
    }
}
