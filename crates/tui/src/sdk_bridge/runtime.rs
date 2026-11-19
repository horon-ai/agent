//! What the harness reports about the session it serves, and the reasoning
//! effort the client asks it to store.
//!
//! Kept beside the worker loop rather than inside it for the same reason as the
//! session flows: each request here is one call in, one answer out, and the
//! answer is a structure the client draws instead of a line it prints.

use horon_agent_sdk::{Error, HoronAgentClient, RuntimeCommand, RuntimeInfo};

use super::Incoming;

/// Run one harness command and shape its answer for the transcript.
///
/// `/context` keeps its measured panel. A command that repeats its payload as
/// JSON is drawn from that payload, so the terminal shows labels and values
/// instead of a serialization of the answer it already has; one that reported
/// in prose keeps the message it wrote.
pub(super) fn command(
    client: &HoronAgentClient,
    session_id: &str,
    command: RuntimeCommand,
    arguments: Vec<String>,
) -> Result<Option<Incoming>, Error> {
    let output = client.runtime_command(session_id, command, arguments)?;
    if let Some(report) = super::context_report(command, &output.data) {
        return Ok(Some(Incoming::Context(Box::new(report))));
    }
    Ok(Some(
        match crate::report::Report::from_answer(command, &output.data, &output.message) {
            Some(report) => Incoming::Report(Box::new(report)),
            None => Incoming::Info(output.message),
        },
    ))
}

/// The effort a report names, trimmed, with a blank one read as unknown.
///
/// A provider that does not expose an effort answers with an empty string
/// rather than naming a level, and drawing that would leave an empty part in
/// the status line.
pub(super) fn effort(reported: Option<String>) -> Option<String> {
    reported
        .map(|effort| effort.trim().to_string())
        .filter(|effort| !effort.is_empty())
}

/// The model and the effort of a session, for a client that has just moved to
/// it.
///
/// A report the daemon refuses leaves both unknown rather than failing a move
/// that already happened: the identity event that follows an attach or a create
/// carries the same two fields, and the status line fills in from it.
pub(super) fn identity(client: &HoronAgentClient, session_id: &str) -> (Option<String>, Option<String>) {
    let Ok(info) = client.get_runtime_info(session_id) else {
        return (None, None);
    };
    (info.model, effort(info.reasoning_effort))
}

/// The report the client draws, for the session the worker follows.
pub(super) fn read(client: &HoronAgentClient, session_id: &str) -> Result<Option<Incoming>, Error> {
    let info = client.get_runtime_info(session_id)?;
    Ok(reported(info, session_id, None))
}

/// Store one level and report the state the harness answers with.
///
/// The answer travels with the level that was asked for, so the client can say
/// the level was set only after the harness took it: a refusal never reaches
/// this function's `Ok`.
pub(super) fn set(client: &HoronAgentClient, session_id: &str, level: &str) -> Result<Option<Incoming>, Error> {
    client.set_reasoning_effort(session_id, level)?;
    // The level is stored once this request succeeds. A report the daemon then
    // refuses costs the fresh provider and model, not the confirmation: the
    // client would otherwise report a refusal for a change the harness took.
    let Ok(info) = client.get_runtime_info(session_id) else {
        return Ok(confirmed(level));
    };
    Ok(reported(info, session_id, Some(level)))
}

/// The answer for a level the harness stored, with nothing else known.
fn confirmed(level: &str) -> Option<Incoming> {
    Some(Incoming::RuntimeInfo {
        provider: None,
        model: None,
        effort: Some(level.to_string()),
        level: Some(level.to_string()),
    })
}

/// Turn one runtime report into the answer the client draws.
///
/// A report that names another session is dropped: the daemon rewrites a typed
/// prefix, and an answer that describes a conversation the worker has left
/// would put one session's model beside another's transcript.
pub(super) fn reported(info: RuntimeInfo, followed: &str, level: Option<&str>) -> Option<Incoming> {
    if info.session_id != followed {
        return None;
    }
    let effort = effort(info.reasoning_effort).or_else(|| level.map(str::to_string));
    Some(Incoming::RuntimeInfo {
        provider: info.provider,
        model: info.model,
        effort,
        level: level.map(str::to_string),
    })
}
