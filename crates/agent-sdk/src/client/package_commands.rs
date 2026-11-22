//! The slash commands installed packages supply.
//!
//! Both calls ride the runtime-command request, which is why an older client
//! never sends one: [`HoronAgentClient::list_package_commands`] and
//! [`HoronAgentClient::run_package_command`] first check the
//! `package_commands_v1` capability the bridge advertises when the daemon
//! serves runtime commands. A server without it is refused here, before the
//! request leaves the process, and every other method keeps working.

use horon_agent_harness_api::{
    API_VERSION_MAJOR, CAPABILITY_PACKAGE_COMMANDS_V1, PackageCommandList, PackageCommandOutcome, RuntimeCommand,
};

use super::{Error, ErrorKind, HoronAgentClient, Result};

impl HoronAgentClient {
    /// List the package slash commands this session provides.
    ///
    /// The entries belong to the package generation the session currently
    /// observes, and `generation` says which one. A command listed here and run
    /// later is refused with a stale-entry error if its package left that
    /// generation in between.
    pub fn list_package_commands(&self, session_id: &str) -> Result<PackageCommandList> {
        self.require_capability(CAPABILITY_PACKAGE_COMMANDS_V1, "list_package_commands")?;
        let output = self.runtime_command(session_id, RuntimeCommand::PackageCommands, Vec::new())?;
        serde_json::from_value(output.data).map_err(|error| {
            Error::new(
                ErrorKind::UnexpectedReply,
                format!("expected a package command list from harness API v{API_VERSION_MAJOR}, got: {error}"),
            )
        })
    }

    /// Run one package command and return what the daemon answered.
    ///
    /// `arguments` is the text typed after the command name. A template answers
    /// with `text`, the user message it expands to; an executable command
    /// answers with `output` and the child's `data` after the tool policy
    /// allowed the call.
    pub fn run_package_command(
        &self,
        session_id: &str,
        package_id: &str,
        command: &str,
        arguments: &str,
    ) -> Result<PackageCommandOutcome> {
        self.require_capability(CAPABILITY_PACKAGE_COMMANDS_V1, "run_package_command")?;
        let mut args = Vec::with_capacity(2 + arguments.split_whitespace().count());
        args.push(package_id.to_string());
        args.push(command.to_string());
        args.extend(arguments.split_whitespace().map(str::to_string));
        let output = self.runtime_command(session_id, RuntimeCommand::PackageCommand, args)?;
        serde_json::from_value(output.data).map_err(|error| {
            Error::new(
                ErrorKind::UnexpectedReply,
                format!("expected a package command outcome from harness API v{API_VERSION_MAJOR}, got: {error}"),
            )
        })
    }
}
