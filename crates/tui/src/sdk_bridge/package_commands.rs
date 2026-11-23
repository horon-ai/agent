//! The slash commands installed packages supply: the list the client reads for
//! itself, and the one command it runs.
//!
//! Both requests ride the runtime-command surface, and the SDK checks the
//! `package_commands_v1` capability before either leaves the process, so a
//! daemon without it answers `Unsupported` here and the client reads that as a
//! session that supplies no package command. The rest of the client keeps
//! working: `/help` simply lists one section fewer.
//!
//! The list is read on the client's own behalf, which is why it is quiet. A
//! command the user typed is not: its refusals — a stale entry whose package
//! left the session's generation, a tool-policy denial — are the sentences the
//! daemon wrote, and they travel back through the worker's ordinary failure
//! path.

use horon_agent_sdk::api::{PackageCommandInfo, PackageCommandList, PackageCommandOutcome};
use horon_agent_sdk::{Error, ErrorKind, HoronAgentClient};

use super::Incoming;
use super::skills::one_line;
use crate::commands::PackageCommand;

/// What one package command answered, ready for the transcript.
///
/// A template answers with the message it expands to, which the client submits
/// as a user message and draws as the line the user typed; an executable
/// command answers with the rendering of what its child returned. Exactly one
/// of the two is set, because the daemon decides which kind the command is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageOutcome {
    /// The line the user typed, drawn as the message a template sends.
    pub display: String,
    /// The message a template expands to.
    pub text: Option<String>,
    /// The rendered answer of an executable command.
    pub output: Option<String>,
}

/// The commands the session supplies, or nothing for a harness without the
/// capability.
pub(super) fn read(client: &HoronAgentClient, session_id: &str) -> Result<Option<Incoming>, Error> {
    match client.list_package_commands(session_id) {
        Ok(list) => Ok(Some(Incoming::PackageCommands(listed(&list)))),
        Err(error) if unsupported(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Whether the harness answered that it does not serve this surface.
///
/// The SDK refuses both calls here before sending when the capability is
/// missing, so this is the one error an older daemon produces for them.
pub(super) fn unsupported(error: &Error) -> bool {
    matches!(&error.kind, ErrorKind::Unsupported)
}

/// Run one package command, echoing the line the user typed back with its
/// answer.
pub(super) fn run(
    client: &HoronAgentClient,
    session_id: &str,
    package_id: &str,
    command: &str,
    arguments: &str,
    display: &str,
) -> Result<Incoming, Error> {
    let answer = client.run_package_command(session_id, package_id, command, arguments)?;
    Ok(Incoming::PackageCommand(Box::new(outcome(display, answer))))
}

/// The list as the resolver and `/help` need it, in the order reported.
///
/// The daemon sends it sorted. A row that names no command is skipped: it is
/// the one entry the client could list but never run, and a listing that
/// offered it would name a command no line answers.
pub(super) fn listed(list: &PackageCommandList) -> Vec<PackageCommand> {
    list.commands.iter().filter_map(entry).collect()
}

/// One row, or nothing when it cannot name a command.
fn entry(info: &PackageCommandInfo) -> Option<PackageCommand> {
    let name = info.name.trim();
    if name.is_empty() || info.command.trim().is_empty() {
        return None;
    }
    Some(PackageCommand {
        name: name.to_string(),
        package_id: info.package_id.clone(),
        command: info.command.clone(),
        argument_hint: info
            .argument_hint
            .as_deref()
            .map(str::trim)
            .filter(|hint| !hint.is_empty())
            .map(str::to_string),
        description: one_line(&info.description),
    })
}

/// One answer as the client draws it.
pub(super) fn outcome(display: &str, answer: PackageCommandOutcome) -> PackageOutcome {
    PackageOutcome {
        display: display.to_string(),
        text: answer.text,
        output: answer.output,
    }
}
