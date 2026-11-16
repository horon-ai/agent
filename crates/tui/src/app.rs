//! Application state: what the harness has told us, what the user has typed,
//! and what a keystroke means.
//!
//! Everything here is synchronous and terminal-agnostic, so the tests can drive
//! a whole conversation with fake events and draw the result into a
//! `TestBackend`. Only [`crate::run`] touches a real terminal.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use horon_agent_sdk::api::{ApiEvent, AskSelection, TurnStopReason};

use crate::commands::{Invocation, Local, Registry};
use crate::input::Input;
use crate::keys::{self, Bindings, Key};
use crate::overlays::settings::{Outcome, SettingChange, SettingsOverlay};
use crate::prompts::{Prompt, Response};
use crate::sdk_bridge::Incoming;
use crate::settings::Settings;
use crate::statusline::{Layout, StatusData, TokenTotals};
use crate::theme::{Symbols, Theme};
use crate::transcript::Transcript;

/// What the UI asks the bridge to do after a keystroke.
///
/// Not `Eq`: an answer carries protocol selections, which only compare with
/// `PartialEq`.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Send this text as a user message.
    Send(String),
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
    /// List the models, or switch to the one named.
    Model(Option<String>),
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

/// The whole client state.
pub struct App {
    /// The conversation so far.
    pub transcript: Transcript,
    /// The line being edited.
    pub input: Input,
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
    /// What the client is attached to, for the status line.
    pub description: String,
    /// Events from the bridge thread.
    pub events: Receiver<Incoming>,
    /// Model id reported by the daemon.
    pub model: Option<String>,
    /// Provider reported by the daemon.
    pub provider: Option<String>,
    /// Latest session status string, e.g. `idle` or `generating`.
    pub session_status: Option<String>,
    /// Session the client is attached to, from the last event that named it.
    pub session_id: Option<String>,
    /// Working directory the `path` segment shows.
    pub cwd: Option<String>,
    /// The user's home directory, shortened to `~` by the `path` segment.
    pub home: Option<String>,
    /// Latest token totals the daemon reported.
    pub tokens: Option<TokenTotals>,
    /// How the bottom line is arranged, replaced when `tui.toml` changes.
    pub statusline: Layout,
    /// Colours the renderer uses, replaced when the theme loader resolves a
    /// new theme.
    pub theme: Theme,
    /// Glyphs the renderer uses, replaced with the theme.
    pub symbols: Symbols,
    /// What keystrokes mean, replaced when `keybindings.toml` changes.
    pub bindings: Bindings,
    /// The slash commands in force, re-read when a command directory changes.
    pub commands: Registry,
    /// The resolved settings last applied, which the settings screen draws.
    pub settings: Settings,
    /// Where user theme files are read from, offered by the settings screen.
    pub themes_dir: std::path::PathBuf,
    /// The open settings screen, if one is showing.
    pub overlay: Option<SettingsOverlay>,
    /// A change the settings screen confirmed, waiting for the caller to save.
    pending_change: Option<SettingChange>,
}

impl App {
    /// A connected app.
    pub fn new(description: String, events: Receiver<Incoming>) -> Self {
        Self {
            transcript: Transcript::new(),
            input: Input::new(),
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
            description,
            events,
            model: None,
            provider: None,
            session_status: None,
            session_id: None,
            cwd: None,
            home: home_directory(),
            tokens: None,
            statusline: Layout::default(),
            theme: Theme::default(),
            symbols: Symbols::default(),
            bindings: Bindings::default(),
            commands: Registry::default(),
            settings: Settings::defaults(),
            themes_dir: horon_agent_sdk::user_horon_agent_home().join("themes"),
            overlay: None,
            pending_change: None,
        }
    }

    /// An app that could not connect: the banner carries the reason and the
    /// process exits non-zero once it has been drawn.
    pub fn failure(message: &str) -> Self {
        let (_sender, receiver) = mpsc::channel();
        Self {
            transcript: Transcript::new(),
            input: Input::new(),
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
            description: "not connected".to_string(),
            events: receiver,
            model: None,
            provider: None,
            session_status: None,
            session_id: None,
            cwd: None,
            home: home_directory(),
            tokens: None,
            statusline: Layout::default(),
            theme: Theme::default(),
            symbols: Symbols::default(),
            bindings: Bindings::default(),
            commands: Registry::default(),
            settings: Settings::defaults(),
            themes_dir: horon_agent_sdk::user_horon_agent_home().join("themes"),
            overlay: None,
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
        self.session_status = Some("idle".to_string());
        self.withdraw_prompts("the connection was lost");
        self.transcript.end_running_tools();
        if !self.queue.is_empty() {
            let waiting = self.queue.len();
            self.transcript
                .push_notice(format!("{waiting} queued message(s) cannot be sent"));
        }
    }

    /// A message the bridge could not deliver.
    ///
    /// Nothing was sent, so nothing stays in the conversation claiming it was:
    /// the waiting line is dropped and the text goes back to the input line, so
    /// a dropped send costs a key press rather than the message. The turn never
    /// started, so the status returns to idle instead of staying on `running`.
    /// A failure the bridge traced to a closed socket also marks the connection
    /// lost: otherwise the client would keep saying it is ready to send.
    pub fn send_failed(&mut self, text: &str, message: &str, lost: bool) {
        self.pending_turn = false;
        self.session_status = Some("idle".to_string());
        let display = self.take_in_flight(text);
        // Only the line this send drew is taken back: another message with the
        // same text may still be waiting its turn.
        self.transcript.retract_user(&display);
        self.keep_text(&display);
        // The queue must not fire this message again on the next pass: a daemon
        // that keeps refusing would turn the retry into a loop.
        self.resend_held = true;
        if lost {
            self.lose_connection();
        }
        self.warn(message);
    }

    /// The line to retract or keep for a refused send, consuming the slot.
    ///
    /// One message is in flight at a time, so the slot belongs to exactly the
    /// text the bridge reports; the fallback keeps a stray refusal from
    /// retracting nothing.
    fn take_in_flight(&mut self, text: &str) -> String {
        match self.in_flight.take() {
            Some(message) if message.text == text => message.display,
            Some(message) => {
                self.in_flight = Some(message);
                text.to_string()
            }
            None => text.to_string(),
        }
    }

    /// The next queued message, as the action that sends it.
    ///
    /// Called after a turn-end event: a message typed during a turn waits for
    /// that turn rather than racing it, and several of them keep their order.
    /// A message the harness refused holds the queue until the next turn ends
    /// or the user submits again, so a refused send cannot loop.
    pub fn take_queued(&mut self) -> Option<Action> {
        if self.pending_turn || self.disconnected || self.resend_held {
            return None;
        }
        let message = self.queue.first()?.clone();
        self.queue.remove(0);
        self.transcript.promote_queued(&message.display);
        self.in_flight = Some(message.clone());
        Some(self.start_turn(message.text))
    }

    /// Put a message that was not sent back where the user can see it.
    ///
    /// The input line is the natural place when it is free. When the user has
    /// already typed something else, the message is queued instead of
    /// overwriting that: losing either one silently is worse than a queue.
    fn keep_text(&mut self, text: &str) {
        if self.input.is_empty() {
            for character in text.chars() {
                self.input.insert(character);
            }
            return;
        }
        self.transcript.push_queued(text);
        self.queue.push(Message::new(text.to_string(), None));
    }

    /// Drop the questions a turn was parked on.
    ///
    /// The `ask` tool waits on the connection for as long as its turn runs, so a
    /// question outliving its turn can no longer be answered: the daemon has
    /// already stopped waiting. Approvals are different and are left alone —
    /// their request stays queued with its own deadline.
    fn withdraw_asks(&mut self, reason: &str) {
        let mut withdrawn = 0;
        if matches!(self.prompt, Some(Prompt::Ask(_))) {
            self.prompt = None;
            withdrawn += 1;
        }
        let before = self.waiting_prompts.len();
        self.waiting_prompts.retain(|prompt| !matches!(prompt, Prompt::Ask(_)));
        withdrawn += before - self.waiting_prompts.len();
        if self.prompt.is_none() {
            self.prompt = self.waiting_prompts.pop_front();
        }
        if withdrawn == 0 {
            return;
        }
        let plural = if withdrawn == 1 { "" } else { "s" };
        self.transcript
            .push_notice(format!("{reason}; {withdrawn} question{plural} withdrawn"));
    }

    /// The lines the active prompt wants drawn, if any.
    ///
    /// The hints come from the key map in force, so a rebound answer key is the
    /// key the prompt names. A line under the prompt counts the decisions still
    /// waiting behind it: answering the one on screen shows the next, and
    /// without the count a second approval would look like a repeat of the
    /// first.
    pub fn prompt_lines(&self) -> Vec<String> {
        let Some(prompt) = &self.prompt else {
            return Vec::new();
        };
        let mut lines = prompt.lines_with(&self.bindings);
        // The arguments come from the one line whose failure names this request,
        // so a prompt can only ever show the call it is asking about. No match
        // means the name alone: a guess here already put one call's command on
        // another call's decision.
        if let Prompt::Approval(approval) = prompt
            && let Some(target) = self.transcript.argument_summary_for_request(&approval.request_id)
        {
            lines[0] = format!("! approval needed: {}: {target}", approval.action);
        }
        let waiting = self.waiting_prompts.len();
        if waiting > 0 {
            let plural = if waiting == 1 { "" } else { "s" };
            lines.push(format!("  {waiting} more prompt{plural} waiting"));
        }
        lines
    }

    /// Put a prompt in front of the user, or behind the one already there.
    fn show_prompt(&mut self, prompt: Prompt) {
        match &self.prompt {
            Some(_) => self.waiting_prompts.push_back(prompt),
            None => self.prompt = Some(prompt),
        }
    }

    /// Take the prompt on screen down and show the next one, if any.
    ///
    /// Returns the prompt that was on screen, which is what the confirmation
    /// line names.
    fn take_prompt(&mut self) -> Option<Prompt> {
        let taken = self.prompt.take();
        self.prompt = self.waiting_prompts.pop_front();
        taken
    }

    /// Drop the prompts a turn was parked on.
    ///
    /// A prompt belongs to the turn that asked for it. Once that turn is gone
    /// (answered, cancelled, failed, or a lost connection) the decision has no
    /// waiting call behind it, and answering it would be a claim about a run
    /// that no longer exists. The queue goes with it for the same reason.
    fn withdraw_prompts(&mut self, reason: &str) {
        if self.prompt.is_none() && self.waiting_prompts.is_empty() {
            return;
        }
        let shown = usize::from(self.prompt.is_some()) + self.waiting_prompts.len();
        self.prompt = None;
        self.waiting_prompts.clear();
        let plural = if shown == 1 { "" } else { "s" };
        self.transcript
            .push_notice(format!("{reason}; {shown} prompt{plural} withdrawn"));
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
            session_id: self.session_id.as_deref(),
            pending_turn: self.pending_turn,
            disconnected: self.disconnected,
            session_status: self.session_status.as_deref(),
            prompt: self.prompt_label(),
            cwd: self.cwd.as_deref(),
            home: self.home.as_deref(),
            tokens: self.tokens,
            now: None,
        }
    }

    /// The pending-decision wording for the `pending` segment.
    fn prompt_label(&self) -> Option<&'static str> {
        match &self.prompt {
            Some(Prompt::Approval(_)) => Some("approval needed"),
            Some(Prompt::Ask(_)) => Some("question waiting"),
            None => None,
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
                self.transcript
                    .tool_done(call_id, name, Some(output.as_str()), error.as_deref());
            }
            ApiEvent::TurnDone { .. } => {
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
            ApiEvent::ApprovalPending { request_id, action, .. } => {
                // The event names the tool only. The prompt pairs with a line
                // when it is drawn, using the request id the refusal wrote into
                // that line, so a decision can never be shown another call's
                // arguments.
                self.show_prompt(Prompt::approval(request_id.clone(), action.clone()));
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
                ..
            } => {
                self.session_id = Some(session_id.clone());
                if let Some(model) = model {
                    self.model = Some(model.clone());
                }
                if let Some(provider) = provider {
                    self.provider = Some(provider.clone());
                }
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

    /// Translate one keystroke into an action, if it produced one.
    ///
    /// A live prompt takes every key first: a half-answered approval must not
    /// let a stray character into the message being drafted. The key map
    /// decides what each key means; a printable character that no action claims
    /// is inserted, so a text field cannot lose a letter to a binding.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        // An open overlay owns every key: a letter meant for a value must not
        // reach the message being drafted underneath it.
        if let Some(mut overlay) = self.overlay.take() {
            let outcome = overlay.on_key(key);
            match outcome {
                Outcome::Handled => self.overlay = Some(overlay),
                Outcome::Closed => self.pending_change = None,
                Outcome::Change(change) => {
                    self.pending_change = Some(change);
                    self.overlay = Some(overlay);
                }
            }
            return None;
        }
        if let Some(prompt) = self.prompt.as_mut() {
            match prompt.on_key_with(&self.bindings, key) {
                Some(Response::Approval {
                    request_id,
                    approved,
                    always,
                }) => {
                    // The prompt comes down before the answer is sent: the
                    // daemon refuses a second answer to the same request, and a
                    // prompt still on screen invites one.
                    let covered = self.take_prompt();
                    self.transcript
                        .push_notice(approval_answer_line(&covered, approved, always));
                    return Some(Action::Approval {
                        request_id,
                        approved,
                        always,
                    });
                }
                Some(Response::Ask { ask_id, answers }) => {
                    let covered = answers.len();
                    self.take_prompt();
                    self.transcript.push_notice(ask_answer_line(covered));
                    return Some(Action::Ask { ask_id, answers });
                }
                None => {
                    // A prompt owns every key, but not the two that end the
                    // session. An approval outlives its turn, so a swallowed
                    // Ctrl-C would leave a prompt on screen with no way out of
                    // the client; the prompt still answers its own keys first,
                    // which is what keeps `esc` on the approval.
                    let bound = Key::from_event(key).and_then(|pressed| self.bindings.resolve_editing(&pressed));
                    match bound {
                        Some(keys::ids::APP_CANCEL) if self.pending_turn => {
                            self.withdraw_prompts("the turn was cancelled");
                            return self.cancel_turn();
                        }
                        Some(keys::ids::APP_CANCEL | keys::ids::APP_QUIT) => return Some(Action::Quit),
                        _ => return None,
                    }
                }
            }
        }

        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let bound = Key::from_event(key).and_then(|pressed| self.bindings.resolve_editing(&pressed));
        match bound {
            Some(keys::ids::APP_SUBMIT) => {
                // `editor.submit_on_enter = false` disconnects the send key.
                // The editor is single-line, so there is nowhere to insert a
                // newline: the key does nothing rather than sending, since
                // sending is exactly what the user turned off.
                let submits = self.settings.boolean("editor.submit_on_enter").unwrap_or(true);
                if !submits {
                    return None;
                }
                let text = self.input.take();
                if text.trim().is_empty() {
                    return None;
                }
                return self.submit(&text);
            }
            Some(keys::ids::EDITOR_COMPLETE) => {
                self.complete();
                return None;
            }
            Some(keys::ids::APP_CANCEL) => {
                // Cancel whenever a turn is actually running, which includes
                // the stretch after a mid-turn error: the daemon has not
                // finished the turn yet, so leaving here would drop it.
                if self.pending_turn {
                    return self.cancel_turn();
                }
                return Some(Action::Quit);
            }
            Some(keys::ids::APP_INTERRUPT) => {
                // `esc` interrupts and never leaves: a stray key on an idle
                // client must not end the session.
                if self.pending_turn {
                    return self.cancel_turn();
                }
                return None;
            }
            Some(keys::ids::APP_QUIT) => return Some(Action::Quit),
            Some(keys::ids::EDITOR_BACKSPACE) => self.input.backspace(),
            Some(keys::ids::EDITOR_DELETE) => self.input.delete(),
            Some(keys::ids::EDITOR_LEFT) => self.input.left(),
            Some(keys::ids::EDITOR_RIGHT) => self.input.right(),
            Some(keys::ids::EDITOR_HOME) => self.input.home(),
            Some(keys::ids::EDITOR_END) => self.input.end(),
            _ => {}
        }
        match key.code {
            KeyCode::Char(character) if !control => self.input.insert(character),
            _ => {}
        }
        None
    }

    /// Resolve one submitted line and turn it into an action.
    ///
    /// A slash line either names a built-in UI command, a command file or one
    /// of the harness's commands; a line that names none of them becomes an
    /// error line and never reaches the model.
    fn submit(&mut self, text: &str) -> Option<Action> {
        match self.commands.resolve(text) {
            Invocation::Message(message) => self.send_or_queue(message, None),
            // A command file sends the body it expands to, while the line the
            // user typed stays in the transcript as the message they sent.
            Invocation::Expanded { display, text } => self.send_or_queue(text, Some(display)),
            Invocation::Local { command, arguments } => self.run_local(command, &arguments),
            Invocation::Server { command, arguments } => {
                if self.disconnected {
                    self.refuse(text);
                    return None;
                }
                Some(Action::Runtime { command, arguments })
            }
            Invocation::Error(message) => {
                self.warn(message);
                None
            }
        }
    }

    /// Send a message now, or queue it while a turn is running.
    ///
    /// Sending during a turn is what the daemon answers with "already
    /// processing a message", which loses the text and shows the user an
    /// error for doing something ordinary. The message is kept instead and
    /// sent when the turn ends.
    fn send_or_queue(&mut self, text: String, display: Option<String>) -> Option<Action> {
        // An explicit submit is the user asking again, so a refusal that was
        // holding the queue back no longer blocks this message.
        self.resend_held = false;
        let message = Message::new(text, display);
        if self.disconnected {
            self.refuse(&message.display);
            return None;
        }
        if self.pending_turn {
            self.transcript.push_queued(message.display.clone());
            self.queue.push(message);
            return None;
        }
        // Drawn now, and taken back by `send_failed` if the harness refuses it:
        // the conversation reads in the order things were typed, and a line
        // that was never delivered does not survive the refusal.
        self.transcript.push_user(message.display.clone());
        self.in_flight = Some(message.clone());
        Some(self.start_turn(message.text))
    }

    /// Ask the harness to stop the running turn.
    fn cancel_turn(&mut self) -> Option<Action> {
        self.transcript.push_notice("cancelling the turn");
        Some(Action::Cancel)
    }

    /// Say that a message went nowhere, and put it back in front of the user.
    fn refuse(&mut self, text: &str) {
        self.keep_text(text);
        self.warn("the harness connection is gone; the message was not sent");
    }

    /// Mark a turn as running and hand the text to the bridge.
    fn start_turn(&mut self, text: String) -> Action {
        self.pending_turn = true;
        self.session_status = Some("generating".to_string());
        Action::Send(text)
    }

    /// Answer a built-in command that the client owns.
    fn run_local(&mut self, command: Local, arguments: &str) -> Option<Action> {
        match command {
            Local::Quit => Some(Action::Quit),
            Local::Help => {
                let listing = self.commands.help();
                self.transcript.push_notice(listing);
                None
            }
            Local::Clear => {
                self.transcript = Transcript::new();
                None
            }
            Local::Model => match arguments.trim() {
                "" => Some(Action::Model(None)),
                name => Some(Action::Model(Some(name.to_string()))),
            },
            Local::Settings => {
                self.open_settings();
                None
            }
        }
    }

    /// Show the settings screen, seeded from the settings in force.
    pub fn open_settings(&mut self) {
        let mut overlay = SettingsOverlay::new(self.themes_dir.clone());
        overlay.refresh(&self.settings);
        self.overlay = Some(overlay);
    }

    /// Take the change the settings screen confirmed, for the caller to write.
    pub fn take_setting_change(&mut self) -> Option<SettingChange> {
        self.pending_change.take()
    }

    /// Replace the settings snapshot the screen draws, keeping the selection.
    ///
    /// The two settings that change behavior are applied to the parts that read
    /// them here rather than in each drawing call: the transcript keeps or drops
    /// reasoning as it streams, and the editor asks before it sends.
    pub fn refresh_settings(&mut self, settings: &Settings) {
        self.settings = settings.clone();
        let show_reasoning = settings.boolean("transcript.show_reasoning").unwrap_or(false);
        self.transcript.set_show_reasoning(show_reasoning);
        let output_lines = settings
            .number("transcript.tool_output_lines")
            .and_then(|lines| usize::try_from(lines).ok())
            .unwrap_or(crate::tool_line::DEFAULT_OUTPUT_LINES);
        self.transcript.set_tool_output_lines(output_lines);
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.refresh(settings);
        }
    }

    /// The one-line completion hint drawn under the input, if one applies.
    pub fn completion_hint(&self) -> Option<String> {
        let text = self.input.text();
        let completion = self.commands.complete(&text)?;
        Some(completion.matches.join("  "))
    }

    /// Complete the line being edited to the longest common prefix, or the
    /// single match when there is one.
    pub fn complete(&mut self) {
        let text = self.input.text();
        let Some(completion) = self.commands.complete(&text) else {
            return;
        };
        if completion.insert == text {
            return;
        }
        self.input.clear();
        for character in completion.insert.chars() {
            self.input.insert(character);
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

/// The line the transcript keeps after an approval is answered.
///
/// An answer is recorded, not resumed. A tool call outside an ACP permission
/// bridge is refused with "Nothing was executed", and only the model calling the
/// same tool again runs it: the gate reads the recorded decision on the retry
/// and then removes the request (`crates/app-core/src/tool/permission_bridge.rs`,
/// module doc; `Registry::approval_refusal` in `crates/app-core/src/tool/mod.rs`).
/// A line that just said "approved" would read as "it ran".
///
/// `allow_always` is not a rule either: the daemon has no persistent always
/// store, and records the answer as one approval
/// (`crates/app-core/src/server/client_ask.rs`).
fn approval_answer_line(covered: &Option<Prompt>, approved: bool, always: bool) -> String {
    let action = match covered {
        Some(Prompt::Approval(approval)) => approval.action.as_str(),
        Some(Prompt::Ask(_)) | None => "the call",
    };
    match (approved, always) {
        (true, true) => format!(
            "you allowed {action}: recorded once (the daemon keeps no always-rule), so the model has to call it again"
        ),
        (true, false) => format!("you allowed {action}: the model has to call it again before it runs"),
        (false, _) => format!("you denied {action}: the call will not run"),
    }
}

/// The line the transcript keeps after a question set is answered.
///
/// The ask tool waits for the answer on the connection that advertised
/// `supports_ask_requests`, so an answer releases the call rather than being
/// remembered for a retry (`crates/app-core/src/server/client_ask.rs`).
fn ask_answer_line(answers: usize) -> String {
    let counted = if answers == 1 {
        "1 answer".to_string()
    } else {
        format!("{answers} answers")
    };
    format!("you answered the ask tool ({counted}): the call resumes")
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
