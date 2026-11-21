//! Built-in slash commands the client answers itself, and the command
//! registry's reload path when a command directory changes.

use super::{Action, App};
use crate::commands::Local;
use crate::links;
use crate::overlays::picker::{Kind, Picker, Row};

/// Assistant answers `/links` reads back from the transcript.
const LINK_ANSWERS: usize = 8;

/// The title the link list is drawn with.
const LINK_TITLE: &str = "Links";

/// What `/links` says, and the list draws, when no answer held a link.
const NO_LINKS: &str = "no http links in the recent answers";

impl App {
    /// Answer a built-in command that the client owns.
    pub(crate) fn run_local(&mut self, command: Local, arguments: &str) -> Option<Action> {
        match command {
            // `/exit` is `/quit` under the name a shell user already has in
            // their fingers; both leave through the same action, so the exit
            // code is the same.
            Local::Quit | Local::Exit => Some(Action::Quit),
            // Listing is a notice and opening is an action the frame loop
            // answers, so neither reaches the bridge thread.
            Local::Artifact => self.run_artifact(arguments),
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
            // Copying is answered here and written by the loop: the app decides
            // what a copy carries and never touches the terminal.
            Local::Copy => self.copy_command(arguments),
            // No argument asks for the list the picker draws; a name still
            // switches straight away, or waits for the turn to end, which is
            // what a script or a habit uses.
            Local::Model => match arguments.trim() {
                "" => self.open_models(),
                name => self.ask_model(name),
            },
            // `/effort` answers itself: the ladder is the client's, so an
            // unknown level is refused before a request is made, and the only
            // thing that leaves the client is a level it holds.
            Local::Effort => self.run_effort(arguments),
            // `/new` asks the bridge for a session in the directory the client
            // is in; nothing is claimed on screen until the daemon answers,
            // because a create can be refused.
            Local::New => self.start_new_session(),
            // `/skills` shows the list the harness reports: the rows it already
            // knows are drawn at once, and the answer to this request replaces
            // them when it comes.
            Local::Skills => self.open_skills(),
            Local::Settings => self.open_settings(),
            // `/links` reads the answers already on screen. Opening one is the
            // reader's own enter in the list, so this arm answers itself and
            // sends nothing to the harness.
            Local::Links => {
                self.open_links();
                None
            }
            // `/switch` and `/resume` are one flow under two names: the list
            // when no id is given, the session itself when one is.
            Local::Resume => self.change_session("resume", "Resume session", arguments),
            Local::Switch => self.change_session("switch", "Switch session", arguments),
            Local::Rename => self.rename_session(arguments),
            // `/swarm` opens this session's children; an argument after it still
            // reaches the harness's own report, so the built-in takes the bare
            // name rather than the command.
            Local::Swarm => self.run_swarm(arguments),
            // A note is the reader's own text about a message: it never joins
            // the conversation or a request, so this arm answers entirely in
            // the client except when it asks which noted sessions are gone.
            Local::Note => self.run_note(arguments),
            // `!command` and `!!command` are resolved by the registry, never
            // by a slash name, and are run by this client alone.
            Local::Shell => {
                self.run_local_shell(arguments, false);
                None
            }
            Local::ShellExcluded => {
                self.run_local_shell(arguments, true);
                None
            }
            // Neither asks the harness for anything: a recording is a local
            // file, and a replay is local playback over it.
            Local::Record => {
                self.run_record(arguments);
                None
            }
            Local::Replay => {
                self.run_replay(arguments);
                None
            }
            // `/queue` opens the list of messages waiting for the running turn
            // to end; nothing about them is sent from here.
            Local::Queue => self.open_queue(),
        }
    }

    /// `/artifact`: list this session's retained artifacts, or open one as a copy.
    ///
    /// The connection decides first: only the client's own harness has a store
    /// on this machine, so a session behind an explicit socket or a remote
    /// bridge is refused by the store access itself, before any file is looked
    /// at. Then only the attached session's store is reachable, and only through
    /// an address [`crate::artifact::parse`] reads as an id. A URL, a path or a
    /// session-prefixed line is refused here, before the store is listed or the
    /// terminal is given up, so a typo cannot become a local read of a file this
    /// client has no business opening. A missing artifact is refused the same
    /// way: the action carries a resolved path or nothing.
    fn run_artifact(&mut self, arguments: &str) -> Option<Action> {
        let Some(session) = self.session_id.clone() else {
            let refusal = crate::artifact::Refusal::NoSession;
            self.warn(format!("artifact: {refusal}"));
            return None;
        };
        // The store access checks `workspace_is_local` itself: an explicit
        // socket or a remote bridge can report an id that coincides with a local
        // one, and the store function is what refuses to read this machine's
        // disk for it. The same evidence #500 uses for local workspace paths.
        if arguments.trim().is_empty() {
            match crate::artifact::entries(&self.artifacts_dir, &session, self.workspace_is_local) {
                Ok(entries) if entries.is_empty() => {
                    self.transcript
                        .push_notice("artifact: this session has no retained artifacts");
                }
                Ok(entries) => self.transcript.push_notice(artifact_listing(&entries)),
                Err(refusal) => self.warn(format!("artifact: {refusal}")),
            }
            return None;
        }
        match crate::artifact::parse(arguments) {
            Ok(id) => match crate::artifact::resolve(&self.artifacts_dir, &session, id, self.workspace_is_local) {
                Ok(path) => Some(Action::OpenArtifact(path)),
                Err(refusal) => {
                    self.warn(format!("artifact: {refusal}"));
                    None
                }
            },
            Err(refusal) => {
                self.warn(format!("artifact: {refusal}"));
                None
            }
        }
    }

    /// `/links`: the http(s) URLs in the newest assistant answers.
    ///
    /// The list is built from what the transcript already holds, so nothing is
    /// fetched and the model is not asked again. The newest answer comes first,
    /// because that is the one a reader just read.
    pub(crate) fn open_links(&mut self) {
        let urls: Vec<String> = {
            let answers = self.transcript.recent_assistant_text(LINK_ANSWERS);
            let mut urls: Vec<String> = Vec::new();
            for text in answers {
                for url in links::extract(text) {
                    if !urls.contains(&url) {
                        urls.push(url);
                    }
                }
            }
            urls
        };
        if urls.is_empty() {
            self.transcript.push_notice(NO_LINKS);
            return;
        }
        let rows = urls
            .into_iter()
            .map(|url| {
                Row::new(url.clone(), url).with_preview(vec!["enter opens this in the default browser".to_string()])
            })
            .collect();
        self.picker = Some(Picker::new(Kind::Links, LINK_TITLE, rows, NO_LINKS));
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

/// Rows a `/artifact` listing prints before it summarizes the rest.
///
/// A session's store is capped in bytes, not in entries, so a long run of small
/// artifacts could otherwise fill the pane with names and push the conversation
/// off the screen.
const ARTIFACT_ROWS: usize = 20;

/// The text `/artifact` prints: one row per artifact, newest last.
fn artifact_listing(entries: &[crate::artifact::Stored]) -> String {
    let mut lines = Vec::with_capacity(entries.len().min(ARTIFACT_ROWS) + 2);
    lines.push("artifacts: open one with /artifact <id>, or paste its artifact://<id> pointer".to_string());
    for entry in entries.iter().take(ARTIFACT_ROWS) {
        lines.push(format!(
            "  {:>4}  {:>9}  {}",
            entry.id,
            size_text(entry.bytes),
            entry.tool
        ));
    }
    if entries.len() > ARTIFACT_ROWS {
        lines.push(format!("  … {} more", entries.len() - ARTIFACT_ROWS));
    }
    lines.join("\n")
}

/// A size in the unit a list can compare at a glance.
fn size_text(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    let bytes = bytes as f64;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes / MIB)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes / KIB)
    } else {
        format!("{bytes:.0} B")
    }
}
