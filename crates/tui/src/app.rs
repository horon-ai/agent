//! Application state: what the harness has told us, what the user has typed,
//! and what a keystroke means.
//!
//! Everything here is synchronous and terminal-agnostic, so the tests can drive
//! a whole conversation with fake events and draw the result into a
//! `TestBackend`. Only [`crate::run`] touches a real terminal.

use std::sync::mpsc::{self, Receiver, Sender};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use horon_agent_sdk::api::{ApiEvent, AskSelection};

use crate::input::Input;
use crate::prompts::{Prompt, Response};
use crate::sdk_bridge::Incoming;
use crate::transcript::Transcript;

/// What the UI asks the bridge to do after a keystroke.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Kept so a headless app's event channel stays open instead of
    /// disconnecting on the first drain.
    keepalive: Option<Sender<Incoming>>,
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
            keepalive: None,
        }
    }

    /// An app that could not connect: the banner carries the reason and the
    /// process exits non-zero once it has been drawn.
    pub fn failure(message: &str) -> Self {
        let (sender, receiver) = mpsc::channel();
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
            keepalive: Some(sender),
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
        parts.join(" · ")
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
            ApiEvent::ToolExec { name, .. } => self.transcript.tool_start(name),
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
                self.transcript
                    .push_notice(format!("a tool call needs approval: {action}"));
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
            ApiEvent::ModelInfo { provider, model, .. } => {
                if let Some(model) = model {
                    self.model = Some(model.clone());
                }
                if let Some(provider) = provider {
                    self.provider = Some(provider.clone());
                }
            }
            ApiEvent::SessionStatus { status, .. } => {
                self.session_status = Some(status.clone());
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
    /// let a stray character into the message being drafted.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        if let Some(prompt) = self.prompt.as_mut() {
            match prompt.on_key(key) {
                Some(Response::Approval {
                    request_id,
                    approved,
                    always,
                }) => {
                    if !always {
                        self.prompt = None;
                    }
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
        match key.code {
            KeyCode::Char('c') if control => {
                if self.pending_turn {
                    self.transcript.push_notice("cancelling the turn");
                    Some(Action::Cancel)
                } else {
                    Some(Action::Quit)
                }
            }
            KeyCode::Char('d') if control => Some(Action::Quit),
            KeyCode::Enter => {
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
                Some(Action::Send(text))
            }
            KeyCode::Backspace => {
                self.input.backspace();
                None
            }
            KeyCode::Delete => {
                self.input.delete();
                None
            }
            KeyCode::Left => {
                self.input.left();
                None
            }
            KeyCode::Right => {
                self.input.right();
                None
            }
            KeyCode::Home => {
                self.input.home();
                None
            }
            KeyCode::End => {
                self.input.end();
                None
            }
            KeyCode::Char(character) if !control => {
                self.input.insert(character);
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let (_sender, receiver) = mpsc::channel();
        App::new("socket · session".to_string(), receiver)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
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
