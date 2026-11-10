//! Application state: what the harness has told us, what the user has typed,
//! and what a keystroke means.
//!
//! Everything here is synchronous and terminal-agnostic, so the tests can drive
//! a whole conversation with fake events and draw the result into a
//! `TestBackend`. Only [`crate::run`] touches a real terminal.

use std::sync::mpsc::{self, Receiver};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use horon_agent_sdk::api::{ApiEvent, AskSelection};

use crate::input::Input;
use crate::keys::{self, Bindings, Key};
use crate::prompts::{Prompt, Response};
use crate::sdk_bridge::Incoming;
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
    /// Leave.
    Quit,
}

/// The whole client state.
pub struct App {
    /// The conversation so far.
    pub transcript: Transcript,
    /// The line being edited.
    pub input: Input,
    /// The prompt blocking the turn, if any.
    pub prompt: Option<Prompt>,
    /// A fatal or connection-level sentence drawn across the top.
    pub banner: Option<String>,
    /// Whether the loop should keep going.
    pub running: bool,
    /// What `run` returns once the loop ends.
    pub exit_code: i32,
    /// Whether a turn is in flight (Ctrl-C cancels instead of leaving).
    pub pending_turn: bool,
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
}

impl App {
    /// A connected app.
    pub fn new(description: String, events: Receiver<Incoming>) -> Self {
        Self {
            transcript: Transcript::new(),
            input: Input::new(),
            prompt: None,
            banner: None,
            running: true,
            exit_code: 0,
            pending_turn: false,
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
            banner: Some(message.to_string()),
            running: false,
            exit_code: 1,
            pending_turn: false,
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
        }
    }

    /// Add a warning line to the transcript.
    pub fn warn(&mut self, text: impl Into<String>) {
        self.transcript.push_warning(text);
    }

    /// The connection ended.
    pub fn disconnected(&mut self, message: &str) {
        self.banner = Some(message.to_string());
        self.running = false;
        self.exit_code = 1;
        self.pending_turn = false;
        self.prompt = None;
    }

    /// The lines the active prompt wants drawn, if any.
    pub fn prompt_lines(&self) -> Vec<String> {
        match &self.prompt {
            Some(prompt) => prompt.lines(),
            None => Vec::new(),
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
        let state = if self.pending_turn {
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
            ApiEvent::TextDone { .. } => self.transcript.text_done(),
            ApiEvent::TextReplace { text, message_id, .. } => {
                self.transcript.text_replace(message_id.as_deref(), text);
            }
            ApiEvent::ToolStart { name, .. } => self.transcript.tool_start(name),
            // `ToolExec` follows `ToolStart` for the same call, so its line is
            // already in the transcript; a call that starts execution without
            // a start event still gets a line when it finishes.
            ApiEvent::ToolExec { .. } => {}
            ApiEvent::ToolDone { name, error, .. } => {
                self.transcript.tool_done(name, error.as_deref());
            }
            ApiEvent::TurnDone { .. } => {
                self.transcript.text_done();
                self.pending_turn = false;
                self.session_status = Some("idle".to_string());
            }
            ApiEvent::TurnStopped { message, .. } => {
                self.pending_turn = false;
                self.warn(message.clone());
            }
            ApiEvent::MessageAccepted { .. } => {}
            ApiEvent::ApprovalPending { request_id, action, .. } => {
                self.prompt = Some(Prompt::approval(request_id.clone(), action.clone()));
                self.transcript.push_notice("a tool call needs your approval");
            }
            ApiEvent::ApprovalResolved {
                request_id,
                approved,
                via,
                ..
            } => {
                let holds_request = matches!(
                    &self.prompt,
                    Some(Prompt::Approval(approval)) if &approval.request_id == request_id
                );
                if holds_request {
                    self.prompt = None;
                }
                let outcome = if *approved { "approved" } else { "denied" };
                self.transcript.push_notice(format!("approval {outcome} via {via}"));
            }
            ApiEvent::AskRequested { ask_id, questions, .. } => {
                self.prompt = Some(Prompt::ask(ask_id.clone(), questions.clone()));
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
                self.session_status = Some(status.clone());
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
                self.pending_turn = false;
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
        if let Some(prompt) = self.prompt.as_mut() {
            match prompt.on_key_with(&self.bindings, key) {
                Some(Response::Approval {
                    request_id,
                    approved,
                    always,
                }) => {
                    self.prompt = None;
                    return Some(Action::Approval {
                        request_id,
                        approved,
                        always,
                    });
                }
                Some(Response::Ask { ask_id, answers }) => {
                    self.prompt = None;
                    return Some(Action::Ask { ask_id, answers });
                }
                None => return None,
            }
        }

        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let bound = Key::from_event(key).and_then(|pressed| self.bindings.resolve_editing(&pressed));
        match bound {
            Some(keys::ids::APP_SUBMIT) => {
                let text = self.input.take();
                if text.trim().is_empty() {
                    return None;
                }
                if text.trim() == "/quit" {
                    return Some(Action::Quit);
                }
                self.transcript.push_user(text.clone());
                self.pending_turn = true;
                self.session_status = Some("generating".to_string());
                return Some(Action::Send(text));
            }
            Some(keys::ids::APP_CANCEL) => {
                if self.pending_turn {
                    self.transcript.push_notice("cancelling the turn");
                    return Some(Action::Cancel);
                }
                return Some(Action::Quit);
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
}

/// The home directory the `path` segment shortens to `~`.
///
/// `HOME` is read first and `USERPROFILE` second, which covers Unix and
/// Windows without an OS-specific block.
fn home_directory() -> Option<String> {
    let value = match std::env::var_os("HOME") {
        Some(home) => home,
        None => match std::env::var_os("USERPROFILE") {
            Some(home) => home,
            None => return None,
        },
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
