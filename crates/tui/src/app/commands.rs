//! Built-in slash commands the client answers itself, and the command
//! registry's reload path when a command directory changes.

use super::{Action, App};
use crate::commands::Local;
use crate::links;
use crate::overlays::picker::{Kind, Picker, Row};
use crate::voice::{CONSENT, Outcome, Phase, Start, USAGE};

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
                    match crate::help::detail(self.commands.entry_list(), name) {
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
            Local::Context(command) => self.change_context(command, arguments),
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
            Local::Mcp => Some(Action::Runtime {
                command: horon_agent_sdk::RuntimeCommand::Mcp,
                arguments: arguments
                    .splitn(2, char::is_whitespace)
                    .map(str::trim)
                    .filter(|argument| !argument.is_empty())
                    .map(str::to_owned)
                    .collect(),
            }),
            Local::Settings => self.open_settings(),
            // `/links` reads the answers already on screen. Opening one is the
            // reader's own enter in the list, so this arm answers itself and
            // sends nothing to the harness.
            Local::Links => {
                self.open_links();
                None
            }
            // `/jobs` reads the live list from the daemon: a job that started
            // or ended on another client is not in this transcript.
            Local::Jobs => self.open_jobs(),
            // `/switch` and `/resume` are one flow under two names: the list
            // when no id is given, the session itself when one is.
            Local::Resume => self.change_session("resume", "Resume session", arguments),
            Local::Archived => self.open_archived_sessions(),
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
            // A pin is a client-local preference too: the attached session
            // when no id is typed, the id otherwise. Nothing is sent.
            Local::Pin => self.run_pin(arguments, true),
            Local::Unpin => self.run_pin(arguments, false),
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
            // `/voice` is entirely local: consent, capture, recognition and
            // playback stay in this client, and recognized text is inserted as
            // a draft rather than sent.
            Local::Voice => {
                self.run_voice(arguments);
                None
            }
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

impl App {
    /// `/voice`: consent, local dictation and local playback, all inside this
    /// client.
    pub(crate) fn run_voice(&mut self, arguments: &str) {
        let arguments = arguments.trim();
        match arguments {
            "" => self.toggle_voice(),
            "consent" => {
                self.voice.give_consent();
                self.transcript.push_notice(
                    "voice: consent recorded for this session; switching sessions, losing the connection or leaving forgets it, and nothing leaves this device",
                );
            }
            "forget" => {
                self.voice.forget();
                self.transcript
                    .push_notice("voice: consent forgotten; any capture, recognition or playback was stopped");
            }
            "stop" => self.stop_voice(),
            "cancel" => {
                self.voice.abandon();
                self.transcript.push_notice("voice: cancelled; the draft is unchanged");
            }
            "read" => self.read_voice(),
            "status" => {
                let status = self.voice.status();
                self.transcript.push_notice(status);
            }
            other => match other.strip_prefix("say ") {
                Some(text) => self.say_voice(text),
                None => self.transcript.push_notice(USAGE),
            },
        }
    }

    /// The key and `/voice` share one toggle: start, finish, cancel, or stop
    /// playback, whichever the phase calls for.
    pub(crate) fn toggle_voice(&mut self) {
        match self.voice.phase() {
            Phase::Idle => match self.voice.start_recording() {
                Start::Started => {
                    let key = match self.bindings.first_key(crate::keys::ids::APP_VOICE) {
                        Some(key) => format!("{key} or /voice stop finishes"),
                        None => "/voice stop finishes".to_string(),
                    };
                    self.transcript
                        .push_notice(format!("voice: recording; {key}, esc cancels (2 minute limit)"));
                }
                Start::NeedsConsent => self.transcript.push_notice(CONSENT),
                Start::Unavailable(problem) => self.transcript.push_notice(format!("voice: {problem}")),
                Start::Busy(phase) => {
                    self.transcript
                        .push_notice(format!("voice: {} is already running", phase.label()));
                }
            },
            Phase::Recording => {
                self.voice.stop_recording();
                self.transcript
                    .push_notice("voice: recognizing on this device; the draft is not sent");
            }
            Phase::Transcribing => {
                self.voice.abandon();
                self.transcript.push_notice("voice: cancelled; the draft is unchanged");
            }
            Phase::Speaking => {
                self.voice.abandon();
                self.transcript.push_notice("voice: playback stopped");
            }
        }
    }

    /// `/voice stop`: finish a capture, or stop playback.
    fn stop_voice(&mut self) {
        match self.voice.phase() {
            Phase::Recording => {
                self.voice.stop_recording();
                self.transcript
                    .push_notice("voice: recognizing on this device; the draft is not sent");
            }
            Phase::Transcribing => {
                self.voice.abandon();
                self.transcript.push_notice("voice: cancelled; the draft is unchanged");
            }
            Phase::Speaking => {
                self.voice.abandon();
                self.transcript.push_notice("voice: playback stopped");
            }
            Phase::Idle => self.transcript.push_notice("voice: nothing is running"),
        }
    }

    /// `esc`: put down whatever is running, keeping the draft as it was.
    pub(crate) fn cancel_voice(&mut self) {
        if self.voice.phase() == Phase::Idle {
            return;
        }
        let speaking = self.voice.phase() == Phase::Speaking;
        self.voice.abandon();
        if speaking {
            self.transcript.push_notice("voice: playback stopped");
        } else {
            self.transcript.push_notice("voice: cancelled; the draft is unchanged");
        }
    }

    /// `/voice say <text>`.
    fn say_voice(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            self.transcript.push_notice("voice: /voice say needs the text to speak");
            return;
        }
        match self.voice.start_playback(text) {
            Start::Started => self
                .transcript
                .push_notice("voice: speaking locally; /voice stop or esc stops it"),
            Start::Unavailable(problem) => self.transcript.push_notice(format!("voice: {problem}")),
            Start::Busy(phase) => {
                self.transcript.push_notice(format!(
                    "voice: {} is already running; /voice stop finishes it first",
                    phase.label()
                ));
            }
            Start::NeedsConsent => self.transcript.push_notice(CONSENT),
        }
    }

    /// `/voice read`: speak the newest answer, without asking the model again.
    fn read_voice(&mut self) {
        let answer = self.transcript.recent_assistant_text(1).next().map(str::to_string);
        match answer {
            Some(answer) if !answer.trim().is_empty() => self.say_voice(&answer),
            _ => self.transcript.push_notice("voice: there is no answer to read yet"),
        }
    }

    /// Collect a finished capture or playback, returning whether anything
    /// reached the screen.
    pub(crate) fn tick_voice(&mut self) -> bool {
        // Consent is for the session that gave it. A lost connection ends that
        // session, so the microphone is no longer allowed until the user asks
        // again; `forget` also stops whatever was running.
        if self.disconnected && (self.voice.consent_given() || self.voice.phase() != Phase::Idle) {
            self.voice.forget();
        }
        let outcomes = self.voice.poll();
        if outcomes.is_empty() {
            return false;
        }
        for outcome in outcomes {
            self.apply_voice(outcome);
        }
        true
    }

    /// Put one finished activity on screen: a draft edit or a notice.
    pub(crate) fn apply_voice(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Draft(text) => self.insert_voice_text(&text),
            Outcome::Notice(message) => self.transcript.push_notice(format!("voice: {message}")),
        }
    }

    /// Insert recognized text at the caret as a draft, never as a message.
    fn insert_voice_text(&mut self, text: &str) {
        let cleaned = text.trim();
        if cleaned.is_empty() {
            self.transcript.push_notice("voice: no speech was recognized");
            return;
        }
        if self.voice_separator(cleaned) {
            self.input.insert_text(" ");
        }
        self.input.insert_text(cleaned);
        let words = cleaned.split_whitespace().count();
        let plural = if words == 1 { "" } else { "s" };
        self.transcript.push_notice(format!(
            "voice: {words} word{plural} inserted into the draft (not sent); enter sends it"
        ));
    }

    /// Whether an insertion needs a space so it does not join the word before
    /// the caret.
    fn voice_separator(&self, next: &str) -> bool {
        let text = self.input.text();
        let cursor = self.input.cursor();
        if cursor == 0 {
            return false;
        }
        let before = text.chars().nth(cursor - 1);
        let after = next.chars().next();
        match (before, after) {
            (Some(before), Some(after)) => {
                !before.is_whitespace() && !after.is_whitespace() && !",.;:!?)]}".contains(after)
            }
            _ => false,
        }
    }

    /// The hint under the draft while voice is doing something.
    pub(crate) fn voice_hint(&self) -> Option<String> {
        match self.voice.phase() {
            Phase::Idle => None,
            Phase::Recording => {
                let finish = match self.bindings.first_key(crate::keys::ids::APP_VOICE) {
                    Some(key) => format!("{key} or /voice stop finishes"),
                    None => "/voice stop finishes".to_string(),
                };
                Some(format!("voice: recording; {finish}, esc cancels (2 minute limit)"))
            }
            Phase::Transcribing => Some("voice: recognizing on this device; /voice cancel discards it".to_string()),
            Phase::Speaking => Some("voice: speaking; /voice stop or esc stops it".to_string()),
        }
    }
}
