//! The slash commands installed packages supply: the list the harness reports,
//! and what one of its answers does to the client.
//!
//! The list belongs to the session, like the skills beside it: a switch clears
//! it, and the bridge sends the new session's list right after the attach, so a
//! name the client left behind is never offered for the session on screen. The
//! resolver keeps the ordering that makes packages additive — a package command
//! answers only a name no built-in, command file or harness command spells.
//!
//! A template's answer is a user message and goes through the ordinary submit
//! path, which queues it during a turn exactly as typed text is queued. An
//! executable command's answer is a report. Its refusals — a stale entry whose
//! package left the generation, or a tool call the policy denied — arrive as
//! the worker's ordinary failure, drawn as the sentence the daemon wrote.

use super::App;
use crate::commands::PackageCommand;
use crate::sdk_bridge::PackageOutcome;

impl App {
    /// The harness's answer: the package commands the session supplies.
    ///
    /// The replacement is whole, because the answer names one session's
    /// installed packages. It only ever adds names to the client: the registry
    /// resolves a package command after every other source.
    pub fn show_package_commands(&mut self, commands: Vec<PackageCommand>) {
        self.commands.set_package_commands(commands);
    }

    /// What one package command answered.
    ///
    /// A template expands to the message a user would have typed, so it is
    /// submitted through [`App::send_or_queue`] and drawn as the line that was
    /// typed. The action that sends it waits in `package_send` because this
    /// runs on the delivery the bridge sent, where no command sender is held.
    ///
    /// An executable command has already run on the daemon under the tool
    /// policy; its output is a report, so nothing is sent and the transcript
    /// shows what it returned.
    pub(crate) fn package_command(&mut self, outcome: PackageOutcome) {
        let PackageOutcome { display, text, output } = outcome;
        if let Some(text) = text {
            if text.trim().is_empty() {
                self.warn(format!("`{display}` expanded to nothing; nothing was sent"));
                return;
            }
            if let Some(action) = self.send_or_queue(text, Some(display)) {
                self.package_send = Some(action);
            }
            return;
        }
        match output.filter(|output| !output.trim().is_empty()) {
            Some(output) => self.transcript.push_notice(output),
            None => self.transcript.push_notice(format!("`{display}` returned nothing")),
        }
    }
}
