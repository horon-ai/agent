//! Application state: what the harness has told us, what the user has typed,
//! and what a keystroke means.
//!
//! Everything here is synchronous and terminal-agnostic, so the tests can drive
//! a whole conversation with fake events and draw the result into a
//! `TestBackend`. Only [`crate::run`] touches a real terminal.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver};

use horon_agent_sdk::api::{ApiEvent, AskSelection, TurnStopReason};

use crate::commands::Registry;
use crate::history::History;
use crate::input::Input;
use crate::keys::Bindings;
use crate::overlays::picker::Picker;
use crate::overlays::settings::{SettingChange, SettingsOverlay};
use crate::prompts::Prompt;
use crate::recording::Event;
use crate::scroll::Scroll;
use crate::sdk_bridge::{Incoming, SkillRow};
use crate::settings::{Motion, Settings};
use crate::statusline::{Layout, TokenTotals, UsageTotals};
use crate::suggestions::Suggestions;
use crate::theme::{Symbols, Theme};
use crate::transcript::{CacheMiss, Decision, ToolState, Transcript};

use self::copy::CopyRequest;

mod commands;
mod copy;
mod draft_suggestions;
mod effort;
mod history_search;
mod keys;
mod notes;
mod picker;
mod pins;
mod prompts;
pub(crate) mod record;
mod sending;
mod sessions;
mod settings;
mod skills;
mod status;
mod terminal_signals;

#[cfg(test)]
mod boundary_tests;

#[cfg(test)]
mod cache_miss_tests;

#[cfg(test)]
mod copy_tests;

#[cfg(test)]
mod effort_cmd_tests;

#[cfg(test)]
mod history_search_tests;

#[cfg(test)]
mod notes_tests;

#[cfg(test)]
mod session_switch_tests;

#[cfg(test)]
mod steer_tests;

/// What the UI asks the bridge to do after a keystroke.
///
/// Not `Eq`: an answer carries protocol selections, which only compare with
/// `PartialEq`.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Send this text as a user message.
    Send(String),
    /// Send this text into the running turn as one soft interrupt.
    ///
    /// It carries the queued messages joined for the harness; what the
    /// transcript shows is each message's own row, settled when the bridge
    /// answers.
    Steer(String),
    /// Cancel the running turn.
    Cancel,
    /// Answer a parked approval.
    Approval {
        request_id: String,
        approved: bool,
        always: bool,
    },
    /// Answer a structured question.
    Ask { ask_id: String, answers: Vec<AskSelection> },
    /// Ask the harness to run one of its own commands.
    Runtime {
        command: horon_agent_sdk::RuntimeCommand,
        arguments: Vec<String>,
    },
    /// Switch to the model named.
    Model(String),
    /// Set the reasoning effort to the level named.
    Effort(String),
    /// Ask for the model list the picker offers.
    ///
    /// The answer fills an open list and is also the catalog a cycle checks
    /// `models.shortlist` against, so a cycle pressed before the list was read
    /// asks for it the same way and opens nothing.
    Models,
    /// Ask for the sessions stored for this working directory.
    Sessions,
    /// Start a new session in this working directory and follow it.
    NewSession,
    /// Attach to another session and replay its stored conversation.
    Attach(String),
    /// Ask the harness for the skills it reports for the session.
    Skills,
    /// Ask which noted sessions were deleted, including checks for archived sessions.
    CheckSessions(Vec<String>),
    /// Hand the draft to the user's editor; the loop answers this one itself,
    /// because the editor needs the terminal the loop is drawing on.
    EditDraft,
    /// Give the terminal to the editor with a private copy of a resolved artifact.
    OpenArtifact(std::path::PathBuf),
    /// Ask the daemon for the values the settings screen displays.
    DaemonSettings,
    /// Leave.
    Quit,
}

/// One message from the user, before the harness has confirmed it.
///
/// The two strings differ only for a command file: the harness receives the
/// body it expands to, while the conversation shows the line that was typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// What the harness is sent.
    pub text: String,
    /// The line the user typed, which is what the conversation shows.
    pub display: String,
}

impl Message {
    /// A message, with `text` doubling as the line when nothing else is given.
    fn new(text: String, display: Option<String>) -> Self {
        let display = display.unwrap_or_else(|| text.clone());
        Self { text, display }
    }
}

/// The queued messages handed to a running turn as one soft interrupt.
///
/// Held until the bridge answers: their rows stay drawn as queued for that
/// long, so a refusal needs no redraw and nothing is drawn twice. The session
/// is the one they were queued for, so an answer that arrives after the client
/// moved cannot put them into another conversation's queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingSteer {
    /// The joined body sent to the harness, echoed back in the answer.
    pub text: String,
    /// The session the messages were queued for, when it was known.
    pub session_id: Option<String>,
    /// The messages, in the order they were queued.
    pub messages: Vec<Message>,
}

/// The whole client state.
pub struct App {
    /// The conversation so far.
    pub transcript: Transcript,
    /// The reader's own notes on transcript messages, in a file of this
    /// client's own. Never part of a message or a harness request.
    pub(crate) notes: crate::notes::NoteStore,
    pub(crate) todos: crate::todo_panel::TodoPanel,
    /// The line being edited.
    pub input: Input,
    /// Drafts sent before this one, for `up` and `down`.
    pub history: History,
    /// The prompt blocking the turn, if any.
    pub prompt: Option<Prompt>,
    /// Prompts that arrived while one was open, oldest first.
    ///
    /// The daemon parks one approval per call, so two parallel calls need two
    /// decisions. They are kept rather than dropped over the one on screen:
    /// answering shows the next, and the count is drawn with the prompt.
    pub waiting_prompts: VecDeque<Prompt>,
    /// A fatal or connection-level sentence drawn across the top.
    pub banner: Option<String>,
    /// Whether the loop should keep going.
    pub running: bool,
    /// What `run` returns once the loop ends.
    pub exit_code: i32,
    /// Whether a turn is in flight (Ctrl-C cancels instead of leaving).
    pub pending_turn: bool,
    /// Messages typed while a turn was running, oldest first. Each one is sent
    /// when the turn before it ends.
    pub queue: Vec<Message>,
    /// The message handed to the bridge and not yet confirmed or refused.
    ///
    /// It keeps the line the user typed beside the text the harness receives,
    /// so a command file is drawn as the line that was typed rather than as
    /// the body it expands to.
    pub in_flight: Option<Message>,
    /// Whether the bridge or the daemon went away. The client stays up so the
    /// conversation can be read; the status line says why nothing is sent.
    pub disconnected: bool,
    /// Whether a failed send is holding the queue back.
    ///
    /// A message the harness refused goes back to the input line or the queue;
    /// retrying it automatically would loop against a daemon that keeps
    /// refusing, so the queue waits for the next turn end or an explicit
    /// submit instead.
    pub resend_held: bool,
    /// Rows the transcript pane got last frame, used to keep the newest lines.
    pub viewport_height: Option<u16>,
    /// Whether the user asked for the screen to be painted again in full.
    ///
    /// A repaint is display-only: the draw that answers it discards the frame
    /// Ratatui kept, so every cell is written again, while the draft, the
    /// selection and the view stay exactly as the key found them. The request
    /// travels as a flag because the app never touches a terminal; the draw
    /// that owns one takes it.
    repaint_requested: bool,
    /// Where the transcript view sits: at the newest rows, or held above them.
    pub scroll: Scroll,
    /// Whether `transcript.mouse_scroll` hands the wheel to the transcript.
    ///
    /// The setting is read here and acted on where the terminal is: capture
    /// takes the terminal's own text selection away, so it is on only while the
    /// setting says so.
    pub mouse_scroll: bool,
    /// Whether `title.state` appends the turn state to the window title.
    ///
    /// Read here and acted on where the title is written, which also knows
    /// whether the terminal keeps a title worth adding to.
    pub title_state: bool,
    /// What the client is attached to, for the status line.
    pub description: String,
    /// Events from the bridge thread.
    pub events: Receiver<Incoming>,
    /// Model id reported by the daemon.
    pub model: Option<String>,
    /// Provider reported by the daemon.
    pub provider: Option<String>,
    /// Reasoning effort the daemon reported for the model, e.g. `high`.
    pub reasoning_effort: Option<String>,
    /// The effort the client last asked for, until an answer names it.
    ///
    /// The cycle key steps from here rather than from the level last reported,
    /// so two presses in quick succession walk two levels instead of asking for
    /// the same one twice. It is cleared when an answer confirms a level or a
    /// request fails.
    effort_requested: Option<String>,
    /// The reasoning effort asked for while a turn was running.
    ///
    /// The daemon answers an effort request only after the running turn
    /// releases the agent, so a request sent mid-turn would hold the bridge
    /// thread — every event and the cancel key with it — until the turn ended.
    /// The level waits here instead and leaves with the next
    /// [`App::take_queued`] pass. A newer level replaces a waiting one.
    effort_after_turn: Option<String>,
    /// The model asked for while a turn was running.
    ///
    /// Same reason as `effort_after_turn`: the daemon applies a switch after the
    /// turn releases the agent. It leaves before the effort, because the report
    /// that follows a switch clears an effort it does not name.
    model_after_turn: Option<String>,
    /// The model the client last asked for, until an answer names it.
    ///
    /// The cycle keys step from here rather than from the model last reported,
    /// so two presses in quick succession walk two models instead of asking for
    /// the same one twice. It is cleared when a report names that model or a
    /// request fails.
    model_requested: Option<String>,
    /// The models `models.shortlist` names, in file order, without repeats.
    ///
    /// The cycle keys walk this list; the model list key still opens the whole
    /// catalog. It is read from the settings on every refresh, so editing
    /// `tui.toml` changes the next press.
    model_shortlist: Vec<String>,
    /// The `models.shortlist` entries the file actually listed, repeats and
    /// blank entries included.
    ///
    /// Kept only to tell a file that changed from a settings save that re-read
    /// the same file: the warnings for a repeated or blank entry are printed
    /// once per edit, not once per save.
    model_shortlist_source: Vec<String>,
    /// The unsupported shortlist entries already named in the transcript.
    ///
    /// A press that skips the same entries again stays quiet; a change in what
    /// is skipped is reported, so an entry the harness never lists is not
    /// silently unavailable.
    model_shortlist_skipped: Vec<String>,
    /// The catalog the harness last reported for the session's provider.
    ///
    /// Kept from the model list answer so a cycle can tell an entry the harness
    /// supports from one it does not without asking again, and so a switch is
    /// never made on a guess. A session change clears it with the provider it
    /// described.
    model_catalog: Vec<String>,
    /// Whether `model_catalog` is the harness's answer rather than its absence.
    ///
    /// An empty catalog is a real answer, so a separate flag is what keeps the
    /// cycle from asking again on every press and from treating "not read yet"
    /// as "the harness lists nothing".
    model_catalog_loaded: bool,
    /// The direction a cycle press asked for while the catalog was unread.
    ///
    /// The first press reads the list and waits for it; the walk then follows
    /// in the direction that press asked for, which keeps a single keypress
    /// from becoming a request the harness was never shown.
    model_cycle_pending: Option<bool>,
    /// The queued messages a steer handed to the running turn, until the bridge
    /// answers.
    ///
    /// Their rows are drawn as queued while it waits, so the answer settles
    /// them in place: a confirmation turns each row into the message the model
    /// received, and a refusal puts the messages back in front of the queue.
    steer_pending: Option<PendingSteer>,
    /// Steers of sessions the client left that no answer has settled yet,
    /// oldest first.
    ///
    /// The bridge answers steers in the order it received them, so the oldest
    /// one is the one the next answer belongs to. Keeping them beside the slot
    /// lets a steer of the session in force take it without dropping a record
    /// whose answer is still on its way.
    steer_left_behind: VecDeque<PendingSteer>,
    /// Latest session status string, e.g. `idle` or `generating`.
    pub session_status: Option<String>,
    /// Session the client is attached to, from the last event that named it.
    pub session_id: Option<String>,
    /// The title the harness holds for the attached session, when it has one.
    ///
    /// It names the terminal window. A session the daemon never titled is named
    /// after its working directory instead, so this stays `None`.
    pub session_title: Option<String>,
    /// Whether the session change in flight is a `/new` the client asked for.
    ///
    /// The bridge answers an attach and a create with the same message, and the
    /// client has to say which happened: `resumed` claims a stored conversation
    /// was replayed, which a session that was just created does not have.
    new_session_pending: bool,
    /// Working directory the `path` segment shows.
    pub cwd: Option<String>,
    /// Whether path completion may read this session's workspace on this machine.
    pub workspace_is_local: bool,
    /// Whether the daemon supplied the workspace directory.
    pub workspace_known: bool,
    /// The path candidates from the last Tab, for cycling.
    pub(crate) path_cycle: Option<crate::path_completion::Choice>,
    /// The user's home directory, shortened to `~` by the `path` segment.
    pub home: Option<String>,
    /// Latest token totals the daemon reported.
    pub tokens: Option<TokenTotals>,
    /// Context window of the route in force, from the runtime's own report.
    pub capacity: Option<u64>,
    /// Cumulative token totals of the session, when the runtime reported them.
    pub usage: Option<UsageTotals>,
    /// The daemon's own spend estimate, when it publishes one.
    pub cost: Option<f64>,
    /// How the bottom line is arranged, replaced when `tui.toml` changes.
    pub statusline: Layout,
    /// The status line's separator after applying its appearance settings.
    pub(crate) status_separator: Option<&'static str>,
    /// Colours the renderer uses, replaced when the theme loader resolves a
    /// new theme.
    pub theme: Theme,
    /// Glyphs the renderer uses, replaced with the theme.
    pub symbols: Symbols,
    /// What keystrokes mean, replaced when `keybindings.toml` changes.
    pub bindings: Bindings,
    /// How much the screen animates, and how often a streaming turn draws.
    ///
    /// The cadence is the event loop's, but the glyph a running turn shows is
    /// drawn here, so the frame the loop just made is what picks it.
    pub motion: Motion,
    /// The animation frame the frame being drawn uses.
    ///
    /// The loop sets it before each draw; a draw made outside the loop (a test,
    /// or the connection-failure screen) uses frame zero, which is the same
    /// glyph a `reduced` preset freezes on.
    pub motion_frame: usize,
    /// The slash command popup: the commands the draft offers, and its row.
    pub slash: crate::help::SlashPopup,
    /// The local draft suggestion: a ghost suffix offered from drafts already
    /// in memory, shown only for the exact text it was computed from.
    pub(crate) suggestions: Suggestions,
    /// The slash commands in force, re-read when a command directory changes.
    pub commands: Registry,
    /// The skills the harness reported for the session in force.
    ///
    /// The registry holds the name and description for the resolver, while the
    /// picker needs the file and the tool list the harness named, so the report
    /// is kept whole here. It is cleared with the session it belongs to.
    pub skills: Vec<SkillRow>,
    /// The resolved settings last applied, which the settings screen draws.
    pub settings: Settings,
    /// The daemon values the settings screen shows, or why it has none.
    ///
    /// Read only when the screen opens or `r` is pressed, because the daemon
    /// owns these values and this client never changes one: a stale copy is
    /// worse than a fresh read the user asked for.
    pub daemon_settings: crate::settings::DaemonSettings,
    /// Where user theme files are read from, offered by the settings screen.
    pub themes_dir: std::path::PathBuf,
    /// The artifact store root (`$HORON_HOME/artifacts`), read by `/artifact`.
    pub artifacts_dir: std::path::PathBuf,
    /// The open settings screen, if one is showing.
    pub overlay: Option<SettingsOverlay>,
    /// The open list of models or sessions, if one is showing.
    ///
    /// A picker sits above every other surface: while it is open no key reaches
    /// the settings screen, a prompt, the slash popup or the draft.
    pub picker: Option<Picker>,
    /// The sessions this reader pinned, and the rows the last list built.
    pub(crate) pins: pins::SessionPins,
    /// A change the settings screen confirmed, waiting for the caller to save.
    pending_change: Option<SettingChange>,
    /// Which terminal signal channels the settings allow.
    ///
    /// Read from `terminal.progress` and `terminal.program_status` when the
    /// settings are applied, so the loop never reads a setting per frame.
    pub(crate) terminal_signals: crate::progress::Options,
    /// How the last turn ended, until the user acts on it.
    ///
    /// The status line goes back to `idle` the moment a turn ends, which is
    /// right for the person already looking at the screen; a terminal tab behind
    /// another window has no other way to say a turn finished or failed, so the
    /// outcome is held here until the user does something and the mark is safe
    /// to drop.
    pub(crate) outcome: Option<crate::progress::Outcome>,
    /// A copy the app decided on, waiting for the loop to write it.
    ///
    /// It sits here rather than being written from the key handler so that the
    /// app keeps knowing nothing about a terminal: the loop that owns standard
    /// output is the one place a sequence is written.
    pending_copy: Option<CopyRequest>,
    /// The recording the user turned on with `/record`.
    ///
    /// Off until asked for: nothing is written to disk unless a recording was
    /// started in this session, and a recording that reaches its limit stops
    /// itself rather than growing.
    pub(crate) recorder: Option<crate::recording::Recorder>,
    /// The replay playing on screen, which owns the transcript while it runs.
    pub(crate) replay: Option<record::ReplayState>,
    /// The conversation identity and answer count a recording already kept.
    ///
    /// The revision says the conversation was replaced (a clear or a session
    /// switch), and the count says how many of its answers were recorded.
    record_scan: (u64, usize),
}

impl App {
    /// A connected app.
    pub fn new(description: String, events: Receiver<Incoming>) -> Self {
        Self {
            transcript: Transcript::new(),
            // In memory until `run` gives the client the file it owns: a test
            // or an embedded user of `App` must not read or write `$HORON_HOME`.
            notes: crate::notes::NoteStore::memory(),
            todos: Default::default(),
            input: Input::new(),
            history: History::new(),
            prompt: None,
            waiting_prompts: VecDeque::new(),
            banner: None,
            running: true,
            exit_code: 0,
            pending_turn: false,
            queue: Vec::new(),
            in_flight: None,
            disconnected: false,
            resend_held: false,
            viewport_height: None,
            repaint_requested: false,
            scroll: Scroll::new(),
            // Placeholder until the settings resolve: the terminal is not
            // captured yet, and `refresh_settings` decides from the registry's
            // default once the files are read.
            mouse_scroll: false,
            // Off unless a settings layer asks for it: a window named after its
            // session is what every setup saw before the key existed.
            title_state: false,
            description,
            events,
            model: None,
            provider: None,
            reasoning_effort: None,
            effort_requested: None,
            effort_after_turn: None,
            model_after_turn: None,
            model_requested: None,
            model_shortlist: Vec::new(),
            model_shortlist_source: Vec::new(),
            model_shortlist_skipped: Vec::new(),
            model_catalog: Vec::new(),
            model_catalog_loaded: false,
            model_cycle_pending: None,
            steer_pending: None,
            steer_left_behind: VecDeque::new(),
            session_status: None,
            session_id: None,
            session_title: None,
            new_session_pending: false,
            cwd: None,
            workspace_is_local: false,
            // A new session starts in the launch directory until it reports otherwise.
            workspace_known: true,
            path_cycle: None,
            home: crate::path_completion::home_directory(),
            tokens: None,
            capacity: None,
            usage: None,
            cost: None,
            statusline: Layout::default(),
            status_separator: None,
            theme: Theme::default(),
            symbols: Symbols::default(),
            bindings: Bindings::default(),
            motion: Motion::default(),
            motion_frame: 0,
            slash: crate::help::SlashPopup::new(),
            suggestions: Suggestions::new(),
            commands: Registry::default(),
            skills: Vec::new(),
            settings: Settings::defaults(),
            daemon_settings: crate::settings::DaemonSettings::default(),
            themes_dir: horon_agent_sdk::user_horon_agent_home().join("themes"),
            artifacts_dir: horon_agent_sdk::user_horon_agent_home().join("artifacts"),
            overlay: None,
            picker: None,
            pins: pins::SessionPins::default(),
            pending_change: None,
            terminal_signals: crate::progress::Options::default(),
            outcome: None,
            pending_copy: None,
            recorder: None,
            replay: None,
            record_scan: (0, 0),
        }
    }

    /// An app that could not connect: the banner carries the reason and the
    /// process exits non-zero once it has been drawn.
    pub fn failure(message: &str) -> Self {
        let (_sender, receiver) = mpsc::channel();
        Self {
            transcript: Transcript::new(),
            notes: crate::notes::NoteStore::memory(),
            todos: Default::default(),
            input: Input::new(),
            history: History::new(),
            prompt: None,
            waiting_prompts: VecDeque::new(),
            banner: Some(message.to_string()),
            running: false,
            exit_code: 1,
            pending_turn: false,
            queue: Vec::new(),
            in_flight: None,
            disconnected: true,
            resend_held: false,
            viewport_height: None,
            repaint_requested: false,
            scroll: Scroll::new(),
            // Placeholder until the settings resolve: the terminal is not
            // captured yet, and `refresh_settings` decides from the registry's
            // default once the files are read.
            mouse_scroll: false,
            // Off unless a settings layer asks for it: a window named after its
            // session is what every setup saw before the key existed.
            title_state: false,
            description: "not connected".to_string(),
            events: receiver,
            model: None,
            provider: None,
            reasoning_effort: None,
            effort_requested: None,
            effort_after_turn: None,
            model_after_turn: None,
            model_requested: None,
            model_shortlist: Vec::new(),
            model_shortlist_source: Vec::new(),
            model_shortlist_skipped: Vec::new(),
            model_catalog: Vec::new(),
            model_catalog_loaded: false,
            model_cycle_pending: None,
            steer_pending: None,
            steer_left_behind: VecDeque::new(),
            session_status: None,
            session_id: None,
            session_title: None,
            new_session_pending: false,
            cwd: None,
            workspace_is_local: false,
            workspace_known: true,
            path_cycle: None,
            home: crate::path_completion::home_directory(),
            tokens: None,
            capacity: None,
            usage: None,
            cost: None,
            statusline: Layout::default(),
            status_separator: None,
            theme: Theme::default(),
            symbols: Symbols::default(),
            bindings: Bindings::default(),
            motion: Motion::default(),
            motion_frame: 0,
            slash: crate::help::SlashPopup::new(),
            suggestions: Suggestions::new(),
            commands: Registry::default(),
            skills: Vec::new(),
            settings: Settings::defaults(),
            daemon_settings: crate::settings::DaemonSettings::default(),
            themes_dir: horon_agent_sdk::user_horon_agent_home().join("themes"),
            artifacts_dir: horon_agent_sdk::user_horon_agent_home().join("artifacts"),
            overlay: None,
            picker: None,
            pins: pins::SessionPins::default(),
            pending_change: None,
            terminal_signals: crate::progress::Options::default(),
            outcome: None,
            pending_copy: None,
            recorder: None,
            replay: None,
            record_scan: (0, 0),
        }
    }

    /// Ask for the screen to be painted again in full.
    ///
    /// Nothing else changes: the request says the terminal's picture cannot be
    /// trusted, not that the session moved. The draw that owns a terminal
    /// consumes it and writes every cell once, so a held view, an open settings
    /// screen and the draft under it stay where they were.
    fn request_repaint(&mut self) {
        self.repaint_requested = true;
    }

    /// Take the pending repaint request, leaving none behind.
    ///
    /// One draw answers one request, which is what keeps the key from turning
    /// the idle loop into a loop that repaints.
    pub(crate) fn take_repaint_request(&mut self) -> bool {
        std::mem::take(&mut self.repaint_requested)
    }

    /// Add a warning line to the transcript.
    ///
    /// The fact that a warning was shown is recorded when a recording is
    /// running: a replay that dropped it would show a turn that failed as one
    /// that simply produced nothing. The text stays out of the file, because a
    /// warning can name a private path or echo a harness message.
    pub fn warn(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.transcript.push_warning(text);
        // The push closes a streaming answer, and that answer is recorded first,
        // so the file reads in the order the screen did. The recording keeps that
        // a warning was shown, not what it said: its text can name a private path
        // or echo a harness message.
        self.record_new_assistants();
        self.record_event(Event::Warning);
    }

    /// The connection ended.
    ///
    /// The client stays up: the conversation is still worth reading, and
    /// exiting on a dropped socket means the last error is wiped by the
    /// terminal restore before anyone can read it. `Ctrl-C` then leaves, and
    /// the exit code still says the session failed.
    pub fn disconnect(&mut self, message: &str) {
        if self.disconnected {
            return;
        }
        self.banner = Some(message.to_string());
        self.lose_connection();
    }

    /// Stop pretending the session is usable, without claiming a new banner.
    ///
    /// Used when the loss is already being reported in the transcript: a send
    /// that came back as a closed socket proves the connection is gone, and
    /// the client has to say so in the status line without repeating itself at
    /// the top of the screen.
    fn lose_connection(&mut self) {
        self.exit_code = 1;
        self.disconnected = true;
        self.pending_turn = false;
        // No end-of-turn frame can arrive over a dead socket, so the turn being
        // measured has no completed turn to describe.
        self.transcript.discard_turn();
        // A change that was waiting for the turn can never leave: there is no
        // connection left to send it over.
        self.effort_after_turn = None;
        self.model_after_turn = None;
        self.model_cycle_pending = None;
        // A switch that was already on its way can never be confirmed now, so
        // the cycle keys step from the model the status line still shows.
        self.model_requested = None;
        // A list that is still waiting for rows will never get them: with the
        // connection gone it can only swallow keys, so it goes with it.
        if self.picker.as_ref().is_some_and(Picker::is_loading) {
            self.picker = None;
        }
        self.session_status = Some("idle".to_string());
        self.withdraw_prompts("the connection was lost");
        self.transcript.end_running_tools();
        // A steer the turn never took is a message the harness will not take
        // now either, so it waits with the rest of the queue the notice below
        // counts.
        self.restore_steer();
        if !self.queue.is_empty() {
            let waiting = self.queue.len();
            self.transcript
                .push_notice(format!("{waiting} queued message(s) cannot be sent"));
        }
    }

    /// Fold one protocol event into the state.
    ///
    /// A live event that changes the conversation ends a replay rather than
    /// landing on top of the recording: the pane shows one conversation, and the
    /// recording is only accurate until the session moves on.
    pub fn apply_event(&mut self, event: &ApiEvent) {
        if self.replay.is_some() && record::ends_replay(event) {
            self.stop_replay("ended by live activity");
        }
        match event {
            ApiEvent::TextDelta { text, message_id, .. } => {
                self.transcript.text_delta(message_id.as_deref(), text);
            }
            // Reasoning is kept only when `transcript.show_reasoning` is on; the
            // setting is applied to the transcript when it loads, so the event
            // itself needs no branch here.
            ApiEvent::ReasoningDelta { text, .. } => self.transcript.reasoning_delta(text),
            ApiEvent::ReasoningDone { .. } => self.transcript.reasoning_done(),
            ApiEvent::TextDone { .. } => self.transcript.text_done(),
            ApiEvent::TextReplace { text, message_id, .. } => {
                self.transcript.text_replace(message_id.as_deref(), text);
            }
            ApiEvent::ToolStart { call_id, name, .. } => {
                self.transcript.tool_start(call_id, name);
            }
            // Input streams as JSON fragments, joined onto the call's own line:
            // the id is what keeps two parallel calls from mixing arguments.
            ApiEvent::ToolInputDelta { call_id, delta, .. } => {
                self.transcript.tool_input(call_id, delta);
            }
            // `ToolExec` follows `ToolStart` for the same call, so its line is
            // already in the transcript; a call that starts execution without
            // a start event still gets a line when it finishes.
            ApiEvent::ToolExec { .. } => {}
            ApiEvent::ToolDone {
                call_id,
                name,
                output,
                error,
                ..
            } => {
                // A result can reach the client twice after a reconnect: the
                // transcript keeps one line, and the recording has to keep one
                // event, so only a call that was not already settled is
                // recorded.
                let already_settled = matches!(
                    self.transcript.tool_state(call_id, name),
                    Some(ToolState::Done | ToolState::Failed(_) | ToolState::Waiting(_) | ToolState::Settled(_, _))
                );
                // Keeping this call's output folds the call that was open, which
                // shrinks a block nothing else measures again: a held view would
                // otherwise keep the height it counted while that call was open.
                // The key label moving onto this call costs the call before it no
                // rows, so it alone is not a reason to recount.
                if self
                    .transcript
                    .tool_done(call_id, name, Some(output.as_str()), error.as_deref())
                {
                    // A fold is a block changing height with no output
                    // arriving; the held view keeps the row it was reading.
                    self.scroll.reflow();
                }
                // A refusal can reach the client before the event that parks
                // it, so the line is parked from this side too: any request
                // already waiting that the message names makes the call a
                // waiting one rather than a failed one. The prompt arriving
                // later parks the same line, and parking twice changes nothing.
                if let Some(message) = error {
                    self.park_line_naming(message);
                }
                // The recording keeps the call's name and outcome, never its
                // arguments or output. A call the gate held or decided is an
                // approval matter, and the default recording leaves it out.
                if !already_settled
                    && self.recorder.is_some()
                    && let Some(state) = self.transcript.tool_state(call_id, name)
                    && let Some(outcome) = record::recorded_outcome(&state)
                {
                    self.record_event(Event::Tool {
                        name: name.clone(),
                        outcome,
                    });
                }
            }
            ApiEvent::TurnDone {
                pending_soft_interrupts,
                ..
            } => {
                self.transcript.text_done();
                self.transcript.end_running_tools();
                self.pending_turn = false;
                self.resend_held = false;
                // The turn finished; the tab carries the mark until the user
                // looks at the client again. A failed turn sends `TurnStopped`
                // before this event, so a failure already on record wins: the
                // tab must not say the turn finished when it broke.
                if self.outcome != Some(crate::progress::Outcome::Failed) {
                    self.outcome = Some(crate::progress::Outcome::Done);
                }
                self.session_status = Some("idle".to_string());
                // An approval outlives the turn that asked for it: the request
                // stays queued in the daemon (with its own deadline) and the
                // recorded answer is what the model's retry reads, so dropping
                // the prompt here would throw away the only way to answer it. A
                // question does not: the `ask` tool stops waiting when the turn
                // it belongs to ends.
                self.withdraw_asks("the turn ended");
                // The turn ended before it reached everything the daemon was
                // still holding for the session. The count is a number of queue
                // items: a steer of several messages is one item, and a
                // background result or another client's interrupt is one too.
                // It says nothing about how many messages were typed or how
                // they were sent, so the notice names neither a number nor a
                // key — only that the next turn carries the rest.
                if let Some(count) = *pending_soft_interrupts
                    && count > 0
                {
                    self.transcript.push_notice(
                        "the turn ended before reading everything sent into it; the next turn carries the rest",
                    );
                }
                // Last, so the footer sits under everything the turn produced
                // and is not pushed above a notice that arrives with the end.
                self.transcript.end_turn();
            }
            ApiEvent::TurnStopped { reason, message, .. } => {
                self.transcript.end_running_tools();
                self.pending_turn = false;
                // A turn boundary is a fresh chance to send: a message a
                // refused send left waiting is not held for ever.
                self.resend_held = false;
                self.session_status = Some("idle".to_string());
                self.withdraw_asks("the turn stopped");
                // A cancel is the user deciding to stop, so nothing may stay on
                // screen asking them for a decision about the run they just
                // stopped.
                if matches!(reason, TurnStopReason::Interrupted) {
                    self.withdraw_prompts("the turn was cancelled");
                }
                // A cancel is the user's own decision, so it is a notice. Any
                // other abnormal stop is news about the harness and stays a
                // warning.
                //
                // A cancel is also the user looking at the client, so the tab
                // has nothing to flag and no mark is left; every other stop is
                // a failure the user may not have seen yet.
                self.outcome =
                    (!matches!(reason, TurnStopReason::Interrupted)).then_some(crate::progress::Outcome::Failed);
                if matches!(reason, TurnStopReason::Interrupted) {
                    self.transcript.push_notice(message.clone());
                } else {
                    self.warn(message.clone());
                }
                // A stopped turn is still a turn that ran: the footer says how
                // long and how much, and comes after the reason it stopped.
                self.transcript.end_turn();
            }
            ApiEvent::SoftInterruptInjected {
                notice: Some(notice), ..
            } => {
                self.transcript.push_notice(notice.clone());
            }
            ApiEvent::MessageAccepted { .. } => {}
            ApiEvent::BackgroundOutput { session_id, text } => {
                // Process output belongs to the session that ran the job. The
                // SDK stream and this filter both check the session, because a
                // frame already queued when the client reattached can still
                // arrive: showing it would put one job's output under the
                // conversation that replaced it.
                if self.session_id.as_deref() == Some(session_id.as_str()) {
                    // Keep process notices distinct from the answer and let
                    // the current model message continue streaming below.
                    self.transcript.push_live_notice(text.clone());
                }
            }
            ApiEvent::ApprovalPending { request_id, action, .. } => {
                // The event names the tool only. The prompt pairs with a line
                // when it is drawn, using the request id the refusal wrote into
                // that line, so a decision can never be shown another call's
                // arguments.
                self.show_prompt(Prompt::approval(request_id.clone(), action.clone()));
                // The call the request names is drawn as waiting from here on:
                // it was refused because the mode asks first, and the failure
                // wording is for a call that broke.
                self.transcript.park_request(request_id);
                // The line changed rows without output arriving; a held view
                // keeps the row it was reading.
                self.scroll.reflow();
                self.transcript.push_notice("a tool call needs your approval");
            }
            ApiEvent::ApprovalResolved {
                request_id,
                approved,
                via,
                ..
            } => {
                // The decision may have been made on another surface, so the
                // prompt is found rather than assumed: it can be the one on
                // screen or one still waiting its turn.
                let holds_request = matches!(
                    &self.prompt,
                    Some(Prompt::Approval(approval)) if &approval.request_id == request_id
                );
                if holds_request {
                    self.take_prompt();
                } else {
                    let queued = self.waiting_prompts.iter().position(
                        |prompt| matches!(prompt, Prompt::Approval(approval) if &approval.request_id == request_id),
                    );
                    if let Some(index) = queued {
                        self.waiting_prompts.remove(index);
                    }
                }
                // A decision made on another surface settles the row the same
                // way one made here does.
                self.transcript
                    .decide_request(request_id, if *approved { Decision::Allowed } else { Decision::Denied });
                self.scroll.reflow();
                let outcome = if *approved { "approved" } else { "denied" };
                self.transcript.push_notice(format!("approval {outcome} via {via}"));
            }
            ApiEvent::AskRequested { ask_id, questions, .. } => {
                self.show_prompt(Prompt::ask(ask_id.clone(), questions.clone()));
            }
            ApiEvent::ModelInfo {
                session_id,
                provider,
                model,
                reasoning_effort,
                ..
            } => {
                self.session_id = Some(session_id.clone());
                if let Some(model) = model {
                    // The identity event names the model the daemon now serves,
                    // so it is the confirmation a switch was waiting for: the
                    // cycle keys step from here again.
                    if self.model_requested.as_deref() == Some(model.as_str()) {
                        self.model_requested = None;
                    }
                    self.model = Some(model.clone());
                }
                if let Some(provider) = provider {
                    // Same reason as in `App::runtime_info`: the model catalog
                    // is the old provider's list.
                    if self.provider.as_deref() != Some(provider.as_str()) {
                        self.model_catalog.clear();
                        self.model_catalog_loaded = false;
                        self.model_shortlist_skipped.clear();
                    }
                    self.provider = Some(provider.clone());
                }
                // The effort describes the model this report names, so a
                // report without one clears it rather than leaving the
                // previous model's effort beside the new model.
                self.reasoning_effort = reasoning_effort
                    .as_deref()
                    .map(str::trim)
                    .filter(|effort| !effort.is_empty())
                    .map(str::to_string);
            }
            ApiEvent::SessionStatus { session_id, status, .. } => {
                self.session_id = Some(session_id.clone());
                // `cancelled` is the daemon restating the turn that just
                // stopped, not a state the session is in: left as it is, the
                // status line kept saying `cancelled` after the notice that
                // already reported it, and the client was idle all along.
                self.session_status = Some(if status == "cancelled" {
                    "idle".to_string()
                } else {
                    status.clone()
                });
            }
            ApiEvent::TokenUsage {
                session_id,
                input,
                output,
                cache_read_input,
                cache_creation_input,
                ..
            } => {
                self.session_id = Some(session_id.clone());
                self.tokens = Some(TokenTotals {
                    input: *input,
                    output: *output,
                    cache_read: optional_count(cache_read_input),
                    cache_creation: optional_count(cache_creation_input),
                });
                // The event is one provider call, so the cumulative figure
                // moves with it. The running total is kept here rather than
                // read again: a request per call would be a request per tool
                // round, and a requested report replaces this baseline with the
                // daemon's own totals whenever the session or model moves.
                self.usage = Some(match self.usage {
                    Some(total) => total.plus(*input, *output, *cache_read_input, *cache_creation_input),
                    None => UsageTotals {
                        input: Some(*input),
                        output: Some(*output),
                        cache_read: *cache_read_input,
                        cache_creation: *cache_creation_input,
                    },
                });
                // The turn footer totals all provider calls in this turn.
                self.transcript.note_usage(*input, *output);
            }
            // The daemon classifies one miss per completed provider request and
            // reports it right after that request's usage, so the mark lands
            // under the answer that cost the resend. Only an avoidable miss is
            // kept: a provider or model switch, an expiry, a documented reload,
            // or a reason id this client does not know is not something the
            // reader can act on, and `CacheMiss::from_report` says which.
            ApiEvent::KvCacheMiss {
                session_id,
                reason,
                harness_caused,
                missed_tokens,
                documented_cause,
                ..
            } => {
                // A frame already queued when the client reattached can still
                // arrive: showing it would put one conversation's miss under the
                // one that replaced it, the same rule `BackgroundOutput` follows.
                let follows = self
                    .session_id
                    .as_deref()
                    .is_none_or(|current| current == session_id.as_str());
                if follows
                    && let Some(miss) =
                        CacheMiss::from_report(reason, *harness_caused, documented_cause.as_deref(), *missed_tokens)
                {
                    self.transcript.push_cache_miss(miss);
                }
            }
            ApiEvent::Error { message, .. } => {
                // The turn is not over: the daemon follows a failed turn's
                // error with `TurnStopped` and `TurnDone`, which close the turn
                // state. Clearing the flag here made the next Ctrl-C quit the
                // client while the turn was still live, which is the one thing
                // the key must not do; closing running tool lines here would
                // mark a call that may still finish as already ended.
                self.warn(message.clone());
            }
            _ => {}
        }
        self.record_new_assistants();
    }

    /// The one-line completion hint drawn under the input, if one applies.
    ///
    /// The popup carries the same names with their descriptions, so the hint is
    /// the fallback for a popup the user closed.
    pub fn completion_hint(&self) -> Option<String> {
        let text = self.input.text();
        let completion = self.commands.complete(&text)?;
        // A line that already names the command has nothing left to complete,
        // and naming it again under the draft is noise.
        if completion.insert == text {
            return None;
        }
        Some(completion.matches.join("  "))
    }

    /// Draw the waiting line a refusal message names, when its request is still
    /// parked.
    fn park_line_naming(&mut self, message: &str) {
        let request = self
            .prompt
            .iter()
            .chain(self.waiting_prompts.iter())
            .find_map(|prompt| match prompt {
                Prompt::Approval(approval)
                    if crate::transcript::Transcript::names_request(message, &approval.request_id) =>
                {
                    Some(approval.request_id.clone())
                }
                _ => None,
            });
        if let Some(request) = request {
            self.transcript.park_request(&request);
            self.scroll.reflow();
        }
    }

    /// Bring the slash popup in line with the draft.
    ///
    /// Called before the keys are read and before a frame is drawn: both need
    /// the matches of the draft that is on screen, not of the one before it.
    pub fn sync_slash(&mut self) {
        // The list only ever offers the first word of a one-line draft, and a
        // message being written is neither. The fingerprint is what says the
        // draft is new: without it this built the whole draft twice on every
        // keystroke, once before the key and once after, to learn nothing.
        let fingerprint = self.input.fingerprint();
        if self.slash.is_synced(fingerprint) {
            return;
        }
        if !self.input.starts_with_slash() || self.input.line_count() != 1 {
            self.slash.clear_for(fingerprint);
            return;
        }
        let text = self.input.text();
        self.slash.sync(&self.commands, &text);
    }

    /// Fill the draft from the command the popup has selected.
    ///
    /// The whole line is the command, so nothing of the old prefix survives to
    /// be sent as an argument the user did not type.
    fn complete_selected(&mut self) -> bool {
        let Some(name) = self.slash.selected_name().map(str::to_string) else {
            return false;
        };
        self.history.reset();
        self.path_cycle = None;
        self.input.set_text(&format!("/{name}"));
        // The filled line names the command outright, which is the state the
        // popup has nothing to offer for; syncing here keeps that true for the
        // next frame and the next key alike.
        self.sync_slash();
        true
    }

    /// Complete a slash command first, then a path in the local workspace.
    pub fn complete(&mut self) {
        if self.slash.is_open() && self.complete_selected() {
            return;
        }
        let text = self.input.text();
        if let Some(completion) = self.commands.complete(&text) {
            if completion.insert == text {
                return;
            }
            self.history.reset();
            self.path_cycle = None;
            self.input.set_text(&completion.insert);
            self.sync_slash();
            return;
        }
        // A path completion replaces the last token and leaves the cursor at its end.
        if self.input.cursor() != text.chars().count() {
            return;
        }
        if text.trim_start().starts_with('/') {
            return;
        }
        // Neither a session that runs elsewhere nor one that never named its
        // directory leaves a directory here that the session's paths belong to.
        if !self.workspace_is_local || !self.workspace_known {
            return;
        }
        let filled = crate::path_completion::complete(&text, self.cwd.as_deref(), self.path_cycle.as_ref());
        let Some((filled, choice)) = filled else {
            self.path_cycle = None;
            return;
        };
        self.history.reset();
        self.input.set_text(&filled);
        self.path_cycle = Some(choice);
        self.sync_slash();
    }
}

/// A token count the protocol left optional.
fn optional_count(value: &Option<u64>) -> u64 {
    match value {
        Some(value) => *value,
        None => 0,
    }
}

#[cfg(test)]
#[path = "app/app_tests.rs"]
mod tests;
