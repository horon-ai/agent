//! Built-in slash commands the client answers itself, and the command
//! registry's reload path when a command directory changes.

use super::{Action, App};
use crate::commands::Local;

impl App {
    /// Answer a built-in command that the client owns.
    pub(crate) fn run_local(&mut self, command: Local, arguments: &str) -> Option<Action> {
        match command {
            // `/exit` is `/quit` under the name a shell user already has in
            // their fingers; both leave through the same action, so the exit
            // code is the same.
            Local::Quit | Local::Exit => Some(Action::Quit),
            Local::Help => match arguments.trim() {
                "" => {
                    // The listing names the keys in force, not the shipped ones: a
                    // rebound key has to be the key the listing shows.
                    let listing = self.commands.help_with(&self.bindings);
                    self.transcript.push_notice(listing);
                    None
                }
                name => {
                    // One leading slash is accepted, so `/help /model` reads the
                    // same as `/help model`; a second one is part of the name.
                    let name = name.strip_prefix('/').unwrap_or(name);
                    let entries = self.commands.entries();
                    match crate::help::detail(&entries, name) {
                        Some(detail) => self.transcript.push_notice(detail),
                        None => self.warn(crate::help::unknown(name)),
                    }
                    None
                }
            },
            Local::Keys => {
                let listing = crate::help::key_text(&self.bindings);
                self.transcript.push_notice(listing);
                // A listing of every key is taller than most panes and would be
                // drawn from its end; the next frame measures it and puts its
                // first row at the top, which the app cannot count for itself
                // because wrapping depends on the pane width.
                self.scroll.hold_newest();
                None
            }
            Local::Clear => {
                // Cleared, not replaced: the transcript owns the display
                // settings (`transcript.show_reasoning`,
                // `transcript.tool_output_lines`), and a fresh one would put
                // the defaults back while /settings kept showing the saved
                // values. Picking a session already clears it this way.
                self.transcript.clear();
                None
            }
            // No argument asks for the list the picker draws; a name still
            // switches straight away, which is what a script or a habit uses.
            Local::Model => match arguments.trim() {
                "" => self.open_models(),
                name => Some(Action::Model(name.to_string())),
            },
            // `/new` asks the bridge for a session in the directory the client
            // is in; nothing is claimed on screen until the daemon answers,
            // because a create can be refused.
            Local::New => self.start_new_session(),
            Local::Settings => {
                self.open_settings();
                None
            }
            // `/switch` and `/resume` are one flow under two names: the list
            // when no id is given, the session itself when one is.
            Local::Resume => self.change_session("resume", "Resume session", arguments),
            Local::Switch => self.change_session("switch", "Switch session", arguments),
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
    /// Re-read the command directories when a file's stamp moved, reporting
    /// whether anything reached the screen.
    pub fn tick_commands(&mut self) -> bool {
        if !self.commands.tick() {
            return false;
        }
        for warning in self.commands.take_warnings() {
            self.warn(format!("commands: {warning}"));
        }
        true
    }
}
