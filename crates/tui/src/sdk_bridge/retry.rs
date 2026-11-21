//! Continuing the last failed run of the session this client is attached to.
//!
//! The client names no run id, because it has no trustworthy one: only the
//! daemon records when a run started and how it stopped. So the answer is taken
//! in two steps, both against the same daemon:
//!
//! 1. `get_runtime_state` reports the `last_stop` the daemon wrote, which names
//!    the run that ended last and why.
//! 2. `continue_run` asks the daemon to continue exactly that run. The daemon
//!    re-decides at the point it would begin the turn, so a run that moved on,
//!    is still executing, or ended for a reason no retry can fix is refused
//!    there.
//!
//! This is the same two-step route a reload recovery uses, and it needs no new
//! request: the continuation itself travels as a hidden reminder, which keeps the
//! visible history at the one user prompt the failed turn already stored.

use horon_agent_sdk::{Error, ErrorKind, HoronAgentClient, RuntimeStateSummary};

use super::Incoming;

/// Continue the run the daemon reports as the last one to stop.
///
/// An idle session answers with a notice saying nothing failed, and a stop that
/// no retry may resume is named by the daemon's own reason rather than by a
/// guess here.
pub(super) fn retry(client: &HoronAgentClient, session_id: &str) -> Result<Incoming, Error> {
    let state = client.get_runtime_state(session_id)?;
    let run_id = eligible_run(&state)?;
    // The answer is the daemon's, not an assumption: a stale run, a run still
    // executing, or a stop no retry can fix comes back as an error here, and the
    // client shows it instead of a turn that never started.
    client
        .continue_run(session_id, run_id)
        .map(|()| Incoming::Info("continuing the failed turn".to_string()))
}

/// The run the runtime state says may be continued, or the sentence saying why
/// there is none.
///
/// `last_stop` is what the daemon recorded, so the cause is read from the
/// daemon's own table rather than from anything this client inferred. The set of
/// causes is the same one the daemon enforces when the continuation arrives; the
/// check here only spares a request that the daemon would refuse.
fn eligible_run(state: &RuntimeStateSummary) -> Result<&str, Error> {
    let refusal = |message| Error::new(ErrorKind::InvalidOption, message);
    // An open run means a turn is executing now, so the stop `last_stop` names
    // belongs to earlier work and continuing it would run the wrong thing.
    if !state.runs.is_empty() {
        return Err(refusal("a turn is running, so there is no failed turn to continue".to_string()));
    }
    let Some(stop) = state.last_stop.as_ref() else {
        return Err(refusal("nothing has failed in this session; there is no turn to continue".to_string()));
    };
    // These are the causes the daemon itself accepts for a continuation, read
    // from the stop it recorded. The daemon re-decides the same question when
    // the request arrives, so this only spares a request it would refuse; a
    // cause added later is refused here rather than acted on by mistake.
    match stop.cause.as_str() {
        "provider_error" | "timeout" | "interrupted_unknown" => Ok(stop.run_id.as_str()),
        other => Err(refusal(format!(
            "the last turn ended as `{other}`, which retrying would not change; nothing was started"
        ))),
    }
}
