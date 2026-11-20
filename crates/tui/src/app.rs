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
use crate::scroll::Scroll;
use crate::sdk_bridge::{Incoming, SkillRow};
use crate::settings::Settings;
use crate::statusline::{Layout, StatusData, TokenTotals, UsageSnapshot, UsageTotals};
use crate::theme::{Symbols, Theme};
use crate::transcript::Decision;
use crate::transcript::Transcript;

mod commands;
mod effort;
mod history_search;
mod keys;
mod picker;
mod prompts;
mod sending;
mod sessions;
mod settings;
mod skills;

#[cfg(test)]
mod boundary_tests;

#[cfg(test)]
mod effort_cmd_tests;

#[cfg(test)]
mod history_search_tests;

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
    Models,
    /// Ask for the sessions stored for this working directory.
    Sessions,
    /// Start a new session in this working directory and follow it.
    NewSession,
    /// Attach to another session and replay its stored conversation.
    Attach(String),
    /// Ask the harness for the skills it reports for the session.
    Skills,
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
    /// Where the transcript view sits: at the newest rows, or held above them.
    pub scroll: Scroll,
    /// Whether `transcript.mouse_scroll` hands the wheel to the transcript.
    ///
    /// The setting is read here and acted on where the terminal is: capture
    /// takes the terminal's own text selection away, so it is on only while the
    /// setting says so.
    pub mouse_scroll: bool,
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
    /// Colours the renderer uses, replaced when the theme loader resolves a
    /// new theme.
    pub theme: Theme,
    /// Glyphs the renderer uses, replaced with the theme.
    pub symbols: Symbols,
    /// What keystrokes mean, replaced when `keybindings.toml` changes.
    pub bindings: Bindings,
    /// The slash command popup: the commands the draft offers, and its row.
    pub slash: crate::help::SlashPopup,
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
    /// Where user theme files are read from, offered by the settings screen.
    pub themes_dir: std::path::PathBuf,
    /// The open settings screen, if one is showing.
    pub overlay: Option<SettingsOverlay>,
    /// The open list of models or sessions, if one is showing.
    ///
    /// A picker sits above every other surface: while it is open no key reaches
    /// the settings screen, a prompt, the slash popup or the draft.
    pub picker: Option<Picker>,
    /// A change the settings screen confirmed, waiting for the caller to save.
    pending_change: Option<SettingChange>,
}

impl App {
    /// A connected app.
    pub fn new(description: String, events: Receiver<Incoming>) -> Self {
        Self {
            transcript: Transcript::new(),
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
            scroll: Scroll::new(),
            // Placeholder until the settings resolve: the terminal is not
            // captured yet, and `refresh_settings` decides from the registry's
            // default once the files are read.
            mouse_scroll: false,
            description,
            events,
            model: None,
            provider: None,
            reasoning_effort: None,
            effort_requested: None,
            effort_after_turn: None,
            model_after_turn: None,
            steer_pending: None,
            steer_left_behind: VecDeque::new(),
            session_status: None,
            session_id: None,
            session_title: None,
            new_session_pending: false,
            cwd: None,
            home: home_directory(),
            tokens: None,
            capacity: None,
            usage: None,
            cost: None,
            statusline: Layout::default(),
            theme: Theme::default(),
            symbols: Symbols::default(),
            bindings: Bindings::default(),
            slash: crate::help::SlashPopup::new(),
            commands: Registry::default(),
            skills: Vec::new(),
            settings: Settings::defaults(),
            themes_dir: horon_agent_sdk::user_horon_agent_home().join("themes"),
            overlay: None,
            picker: None,
            pending_change: None,
        }
    }

    /// An app that could not connect: the banner carries the reason and the
    /// process exits non-zero once it has been drawn.
    pub fn failure(message: &str) -> Self {
        let (_sender, receiver) = mpsc::channel();
        Self {
            transcript: Transcript::new(),
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
            scroll: Scroll::new(),
            // Placeholder until the settings resolve: the terminal is not
            // captured yet, and `refresh_settings` decides from the registry's
            // default once the files are read.
            mouse_scroll: false,
            description: "not connected".to_string(),
            events: receiver,
            model: None,
            provider: None,
            reasoning_effort: None,
            effort_requested: None,
            effort_after_turn: None,
            model_after_turn: None,
            steer_pending: None,
            steer_left_behind: VecDeque::new(),
            session_status: None,
            session_id: None,
            session_title: None,
            new_session_pending: false,
            cwd: None,
            home: home_directory(),
            tokens: None,
            capacity: None,
            usage: None,
            cost: None,
            statusline: Layout::default(),
            theme: Theme::default(),
            symbols: Symbols::default(),
            bindings: Bindings::default(),
            slash: crate::help::SlashPopup::new(),
            commands: Registry::default(),
            skills: Vec::new(),
            settings: Settings::defaults(),
            themes_dir: horon_agent_sdk::user_horon_agent_home().join("themes"),
            overlay: None,
            picker: None,
            pending_change: None,
        }
    }

    /// Add a warning line to the transcript.
    pub fn warn(&mut self, text: impl Into<String>) {
        self.transcript.push_warning(text);
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
        // A change that was waiting for the turn can never leave: there is no
        // connection left to send it over.
        self.effort_after_turn = None;
        self.model_after_turn = None;
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

    /// The one-line status: what we are attached to and what it is doing.
    pub fn status_line(&self) -> String {
        self.status_parts().join(" · ")
    }

    /// The status line's parts, for a renderer that chooses its own separator
    /// and colours.
    pub fn status_parts(&self) -> Vec<String> {
        let mut parts = vec![self.description.clone()];
        if let Some(model) = &self.model {
            parts.push(model.clone());
        }
        if let Some(provider) = &self.provider {
            parts.push(provider.clone());
        }
        let state = if self.disconnected {
            "disconnected"
        } else if self.pending_turn {
            "running"
        } else {
            self.session_status.as_deref().unwrap_or("idle")
        };
        parts.push(state.to_string());
        parts
    }

    /// The state the status-line segments read.
    ///
    /// The clock is left empty here: the renderer fills it once per frame, so
    /// the app state never holds a time that goes stale.
    pub fn status_data(&self) -> StatusData<'_> {
        StatusData {
            provider: self.provider.as_deref(),
            model: self.model.as_deref(),
            effort: self.reasoning_effort.as_deref(),
            session_id: self.session_id.as_deref(),
            pending_turn: self.pending_turn,
            disconnected: self.disconnected,
            session_status: self.session_status.as_deref(),
            prompt: self.prompt_label(),
            cwd: self.cwd.as_deref(),
            home: self.home.as_deref(),
            tokens: self.tokens,
            capacity: self.capacity,
            usage: self.usage,
            cost: self.cost,
            now: None,
        }
    }

    /// Fold one usage report into the state the status line reads.
    ///
    /// The window and the estimate replace what was there, including with
    /// `None`: a report that names no window or no price is saying the daemon
    /// knows neither for the route in force, and an old figure would describe a
    /// model the client already left. The totals replace the running sum the
    /// token-usage events maintain whenever the report has any, because that
    /// report is the daemon's own accounting of the same calls; a report with
    /// none keeps the sum, since it is the newer of the two readings.
    pub fn usage(&mut self, snapshot: UsageSnapshot) {
        self.capacity = snapshot.context_window;
        self.cost = snapshot.cost;
        if snapshot.totals.is_some() {
            self.usage = snapshot.totals;
        }
    }

    /// Fold one protocol event into the state.
    pub fn apply_event(&mut self, event: &ApiEvent) {
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
                // Keeping this call's output folds the call that was open, which
                // shrinks a block nothing else measures again: a held view would
                // otherwise keep the height it counted while that call was open.
                // The key label moving onto this call costs the call before it no
                // rows, so it alone is not a reason to recount.
                if self
                    .transcript
                    .tool_done(call_id, name, Some(output.as_str()), error.as_deref())
                {
                    self.scroll.invalidate();
                }
                // A refusal can reach the client before the event that parks
                // it, so the line is parked from this side too: any request
                // already waiting that the message names makes the call a
                // waiting one rather than a failed one. The prompt arriving
                // later parks the same line, and parking twice changes nothing.
                if let Some(message) = error {
                    self.park_line_naming(message);
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
                if matches!(reason, TurnStopReason::Interrupted) {
                    self.transcript.push_notice(message.clone());
                } else {
                    self.warn(message.clone());
                }
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
                    self.model = Some(model.clone());
                }
                if let Some(provider) = provider {
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
        self.input.set_text(&format!("/{name}"));
        // The filled line names the command outright, which is the state the
        // popup has nothing to offer for; syncing here keeps that true for the
        // next frame and the next key alike.
        self.sync_slash();
        true
    }

    /// Complete the line being edited to the longest common prefix, or the
    /// single match when there is one.
    pub fn complete(&mut self) {
        if self.slash.is_open() && self.complete_selected() {
            return;
        }
        let text = self.input.text();
        let Some(completion) = self.commands.complete(&text) else {
            return;
        };
        if completion.insert == text {
            return;
        }
        self.history.reset();
        self.input.set_text(&completion.insert);
        self.sync_slash();
    }
}

/// The home directory the `path` segment shortens to `~`.
///
/// `HOME` is read first and `USERPROFILE` second, which covers Unix and
/// Windows without an OS-specific block.
fn home_directory() -> Option<String> {
    let value = match std::env::var_os("HOME") {
        Some(home) => home,
        None => std::env::var_os("USERPROFILE")?,
    };
    Some(value.to_string_lossy().into_owned())
}

/// A token count the protocol left optional.
fn optional_count(value: &Option<u64>) -> u64 {
    match value {
        Some(value) => *value,
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let (_sender, receiver) = mpsc::channel();
        App::new("socket · session".to_string(), receiver)
    }

    #[test]
    fn status_line_shows_model_and_state() {
        let mut app = app();
        app.apply_event(&ApiEvent::ModelInfo {
            session_id: "s1".into(),
            provider: Some("anthropic".into()),
            model: Some("claude-sonnet-4".into()),
            reasoning_effort: None,
            auth_method: None,
        });
        assert_eq!(
            app.status_line(),
            "socket · session · claude-sonnet-4 · anthropic · idle".to_string()
        );
        app.pending_turn = true;
        assert!(app.status_line().ends_with("running"));
    }

    #[test]
    fn approval_resolves_only_for_its_own_request() {
        let mut app = app();
        app.apply_event(&ApiEvent::ApprovalPending {
            session_id: "s1".into(),
            request_id: "r1".into(),
            action: "bash".into(),
            expires_at: None,
        });
        app.apply_event(&ApiEvent::ApprovalResolved {
            session_id: "s1".into(),
            request_id: "other".into(),
            approved: true,
            via: "file".into(),
            resolved_by: "tool".into(),
        });
        assert!(app.prompt.is_some(), "an unrelated resolution closed the prompt");
        app.apply_event(&ApiEvent::ApprovalResolved {
            session_id: "s1".into(),
            request_id: "r1".into(),
            approved: false,
            via: "inline".into(),
            resolved_by: "tool".into(),
        });
        assert!(app.prompt.is_none());
    }

    #[test]
    fn turn_stopped_clears_the_pending_flag() {
        let mut app = app();
        app.pending_turn = true;
        app.apply_event(&ApiEvent::TurnStopped {
            session_id: "s1".into(),
            reason: horon_agent_sdk::api::TurnStopReason::Failure,
            message: "provider failed".into(),
            provider_stop_reason: None,
        });
        assert!(!app.pending_turn);
        assert!(
            app.transcript
                .items()
                .iter()
                .any(|item| item == &crate::transcript::Item::Warning("provider failed".to_string()))
        );
    }
}
