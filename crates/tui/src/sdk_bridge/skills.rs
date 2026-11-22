//! The skills the harness reports, and the list the skill picker draws.
//!
//! One request in, one answer out, and the answer is a JSON document rather
//! than a type the SDK names: the parsing lives here, where a malformed row is
//! one skipped row and a missing list is an empty one. The command is also sent
//! once after the connection and after every session change, where a harness
//! too old to know it must stay silent.

use horon_agent_sdk::api::ErrorCode;
use horon_agent_sdk::{Error, ErrorKind, HoronAgentClient, RuntimeCommand, RuntimeCommandOutput};

use super::{Incoming, SkillRow};

/// The skills the harness reports for `session_id`.
///
/// `announce` is the difference between a list the user asked for and one the
/// client reads for itself: `/skills` shows every failure, while the read after
/// a connect or a session change stays quiet for a harness that does not know
/// the command yet and reports any other failure once.
pub(super) fn read(client: &HoronAgentClient, session_id: &str, announce: bool) -> Result<Option<Incoming>, Error> {
    match client.runtime_command(session_id, RuntimeCommand::Skills, Vec::new()) {
        Ok(output) => Ok(listed(&output, session_id)),
        Err(error) if !announce && unknown_command(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Whether the harness answered that it does not serve this command.
pub(super) fn unknown_command(error: &Error) -> bool {
    matches!(&error.kind, ErrorKind::Harness(ErrorCode::UnknownRequest))
}

/// The answer as a list, or nothing when it belongs to another session.
///
/// The daemon rewrites a typed prefix and answers about the session it resolved,
/// so an answer that names a session the worker has left would put one
/// conversation's skills beside another's transcript.
pub(super) fn listed(output: &RuntimeCommandOutput, followed: &str) -> Option<Incoming> {
    if output.session_id != followed {
        return None;
    }
    Some(Incoming::Skills(rows(&output.data)))
}

/// The rows of one `data` document.
///
/// The document is additive, so only `skills` is read and a value of another
/// shape there means the harness reported no list this client can read.
pub(super) fn rows(data: &serde_json::Value) -> Vec<SkillRow> {
    let Some(list) = data.get("skills").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    list.iter().filter_map(row).collect()
}

/// One row, or nothing when the entry cannot name a skill.
///
/// A name is the only field a row needs: it is what `/skill:<name>` addresses
/// and what the row is chosen by. Every other field is optional in the payload,
/// and one of an unexpected type is read as absent rather than as a row the
/// user loses.
fn row(entry: &serde_json::Value) -> Option<SkillRow> {
    let name = one_line(entry.get("name")?.as_str()?);
    if name.is_empty() {
        return None;
    }
    let description = match entry.get("description").and_then(serde_json::Value::as_str) {
        Some(description) => one_line(description),
        None => String::new(),
    };
    let path = match entry.get("path").and_then(serde_json::Value::as_str) {
        Some(path) if !path.is_empty() => Some(path.to_string()),
        _ => None,
    };
    let allowed_tools = match entry.get("allowed_tools").and_then(serde_json::Value::as_array) {
        Some(tools) => tools
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_string)
            .collect(),
        None => Vec::new(),
    };
    Some(SkillRow {
        name,
        description,
        path,
        allowed_tools,
    })
}

/// The first line of a text field, trimmed.
///
/// A description travels into a listing row and a picker column, both one line
/// tall: a newline left in it would break the columns they share.
pub(super) fn one_line(text: &str) -> String {
    match text.lines().next() {
        Some(line) => line.trim().to_string(),
        None => String::new(),
    }
}
