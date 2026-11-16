//! Built-in slash commands the client answers itself, and the command
//! registry's reload path when a command directory changes.

use super::{Action, App};
use crate::commands::Local;
use crate::transcript::Transcript;

impl App {
    /// Answer a built-in command that the client owns.
    pub(crate) fn run_local(&mut self, command: Local, arguments: &str) -> Option<Action> {
        match command {
            Local::Quit => Some(Action::Quit),
            Local::Help => {
                // The listing names the keys in force, not the shipped ones: a
                // rebound key has to be the key the listing shows.
                let listing = self.commands.help_with(&self.bindings);
                self.transcript.push_notice(listing);
                None
            }
            Local::Clear => {
                self.transcript = Transcript::new();
                None
            }
            // No argument asks for the list the picker draws; a name still
            // switches straight away, which is what a script or a habit uses.
            Local::Model => match arguments.trim() {
                "" => self.open_models(),
                name => Some(Action::Model(Some(name.to_string()))),
            },
            Local::Settings => {
                self.open_settings();
                None
            }
            Local::Resume => self.open_sessions(),
        }
    }

    /// Point the command registry at the working directory and read the files.
    pub fn reload_commands(&mut self) {
        let cwd = self.cwd.clone();
        if self.commands.sync(cwd.as_deref().map(std::path::Path::new)) {
            for warning in self.commands.take_warnings() {
                self.warn(format!("commands: {warning}"));
            }
        }
    }

    /// Re-read the command files when a directory changed.
    pub fn tick_commands(&mut self) {
        if self.commands.tick() {
            for warning in self.commands.take_warnings() {
                self.warn(format!("commands: {warning}"));
            }
        }
    }
}
