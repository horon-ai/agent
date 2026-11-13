//! The stdio JSONL RPC adapter.
//!
//! Reads one command per line from stdin, drives the daemon over its socket,
//! and writes one frame per line to stdout. While a prompt runs the same loop
//! keeps consuming stdin, so the host can steer, abort, or ask for state, and
//! forwards daemon events to stdout as the turn progresses. Stdin is read by its
//! own task because a read cancelled by `select!` would lose the bytes it had
//! already taken.
//!
//! Exactly one `prompt_result` and one `session_settled` end a prompt, whether
//! the turn completed, was aborted, or failed.

use anyhow::Result;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite};
use tokio::sync::mpsc;

use crate::cli::provider_init::ProviderChoice;
use crate::protocol::{Request, ServerEvent};

use super::command::{self, Command, Kind};
use super::frame::{self, Emitter};
use super::link::{self, Daemon};
use super::mapper::Turn;
use super::state::SessionState;

/// What ended a turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Terminal {
    Completed,
    Aborted,
    Failed,
}

impl Terminal {
    fn status(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Aborted => "aborted",
            Self::Failed => "error",
        }
    }
}

pub(super) struct Adapter {
    pub(super) daemon: Daemon,
    pub(super) foreground: tokio::sync::mpsc::UnboundedReceiver<ServerEvent>,
    pub(super) out: Emitter<Box<dyn AsyncWrite + Unpin + Send>>,
    pub(super) state: SessionState,
    pub(super) prompt_in_flight: bool,
    message_seq: u64,
    /// Commands read from stdin. A separate task owns stdin because a read
    /// cancelled by `select!` would lose the bytes it had already consumed.
    commands: mpsc::UnboundedReceiver<Inbound>,
}

/// What the turn loop is waiting on.
enum Next {
    Event(Option<ServerEvent>),
    Inbound(Option<Inbound>),
}

/// One line read from stdin, or the error that stopped the read.
enum Inbound {
    Line(Vec<u8>),
    Eof,
    Failed(String),
}

pub(super) type Input = Box<dyn AsyncBufRead + Unpin + Send>;

/// Run the adapter until stdin reaches EOF or the daemon connection ends.
pub(super) struct Start<'a> {
    pub(super) provider: &'a ProviderChoice,
    pub(super) model: Option<&'a str>,
    pub(super) provider_profile: Option<&'a str>,
    pub(super) cwd: Option<&'a std::path::Path>,
    pub(super) resume: Option<&'a str>,
    pub(super) connector: Option<link::Connector>,
    pub(super) out: Box<dyn AsyncWrite + Unpin + Send>,
    pub(super) input: Input,
}

pub(super) async fn run(start: Start<'_>) -> Result<()> {
    let (daemon, history, foreground) = link::connect(
        start.provider,
        start.model,
        start.provider_profile,
        start.cwd,
        start.resume,
        start.connector,
    )
    .await?;
    let state = SessionState::from_history(&history, start.model);
    let commands = spawn_reader(start.input);
    let mut adapter = Adapter {
        daemon,
        foreground,
        out: Emitter::new(start.out),
        state,
        prompt_in_flight: false,
        message_seq: 0,
        commands,
    };
    adapter.announce().await?;
    adapter.serve().await
}

/// Read stdin on its own task. A read cancelled mid-line loses the bytes it had
/// consumed, so the adapter must never race a read against anything else.
fn spawn_reader(mut input: Input) -> mpsc::UnboundedReceiver<Inbound> {
    let (sender, receiver) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let mut buffer = Vec::new();
            match input.read_until(b'\n', &mut buffer).await {
                Ok(0) => {
                    // The adapter is gone when this send fails; nothing is lost.
                    drop(sender.send(Inbound::Eof));
                    return;
                }
                Ok(_) => {
                    if sender.send(Inbound::Line(buffer)).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    drop(sender.send(Inbound::Failed(error.to_string())));
                    return;
                }
            }
        }
    });
    receiver
}

/// The slash commands this adapter advertises. Horon's tools are model-side, so
/// the list is the set the adapter itself answers.
fn available_commands() -> Vec<Value> {
    vec![
        json!({
            "name": "compact",
            "source": "builtin",
            "description": "Compact the conversation context",
        }),
        json!({
            "name": "model",
            "source": "builtin",
            "description": "Switch the active model",
        }),
    ]
}

impl Adapter {
    /// The frames a host expects before it sends a command.
    async fn announce(&mut self) -> Result<()> {
        self.out.send(&frame::ready()).await?;
        // The host's startup handshake includes a widget request. This adapter
        // has no widget surface, so it acknowledges it with an empty widget
        // instead of leaving a host that waits for it stalled.
        self.out
            .send(&json!({
                "type": "extension_ui_request",
                "id": "widget-1",
                "method": "setWidget",
                "widgetKey": "horon",
                "widgetLines": [],
            }))
            .await?;
        self.out
            .send(&json!({
                "type": "available_commands_update",
                "commands": available_commands(),
            }))
            .await?;
        Ok(())
    }

    async fn serve(&mut self) -> Result<()> {
        while let Some(inbound) = self.commands.recv().await {
            match inbound {
                Inbound::Line(buffer) => self.handle_line(&buffer).await?,
                Inbound::Eof => return Ok(()),
                Inbound::Failed(error) => anyhow::bail!("stdin read failed: {error}"),
            }
        }
        Ok(())
    }

    async fn handle_line(&mut self, buffer: &[u8]) -> Result<()> {
        if buffer.len() > command::MAX_INBOUND_FRAME_BYTES {
            let error = format!(
                "inbound frame of {} bytes exceeds maxFrameBytes ({})",
                buffer.len(),
                command::MAX_INBOUND_FRAME_BYTES
            );
            let parsed = command::parse(&String::from_utf8_lossy(buffer));
            self.out
                .send(&frame::response_error(
                    parsed.as_ref().and_then(|command| command.id.as_ref()),
                    parsed.as_ref().map_or("unknown", |command| command.kind.name()),
                    &error,
                    Some("frame_too_large"),
                ))
                .await?;
            return Ok(());
        }
        let text = String::from_utf8_lossy(buffer);
        let Some(command) = command::parse(&text) else {
            return Ok(());
        };
        self.dispatch(&command).await
    }

    async fn dispatch(&mut self, command: &Command) -> Result<()> {
        match &command.kind {
            Kind::Prompt => {
                let Some(text) = command.message() else {
                    return self
                        .fail(command, "prompt requires a message", Some("invalid_request"))
                        .await;
                };
                if self.prompt_in_flight {
                    return self
                        .fail(command, "Agent is already processing a prompt", Some("busy"))
                        .await;
                }
                let text = text.to_string();
                self.run_prompt(command, &text).await
            }
            Kind::NegotiateProtocol => {
                self.ok(command, Some(json!({"protocolVersion": frame::PROTOCOL_VERSION})))
                    .await
            }
            // Nothing is running, so there is nothing to abort.
            Kind::Abort => self.ok(command, None).await,
            Kind::Steer | Kind::FollowUp | Kind::AbortAndPrompt => {
                self.unsupported(command, "only valid while a prompt is running").await
            }
            Kind::NewSession => self.unsupported(command, "this adapter serves one session").await,
            Kind::GetState => {
                let data = self.state_frame();
                self.ok(command, Some(data)).await
            }
            Kind::GetAvailableModels => match self.available_models().await {
                Ok(data) => self.ok(command, Some(data)).await,
                Err(error) => {
                    self.fail(command, &error.to_string(), Some("model_catalog_unavailable"))
                        .await
                }
            },
            Kind::GetAvailableCommands => self.ok(command, Some(json!({"commands": available_commands()}))).await,
            Kind::GetSessionStats => match self.session_stats().await {
                Ok(data) => self.ok(command, Some(data)).await,
                Err(error) => {
                    self.fail(command, &error.to_string(), Some("history_unavailable"))
                        .await
                }
            },
            Kind::GetMessages => match self.messages_frame().await {
                Ok(data) => self.ok(command, Some(data)).await,
                Err(error) => {
                    self.fail(command, &error.to_string(), Some("history_unavailable"))
                        .await
                }
            },
            Kind::SetModel => {
                let Some(model) = command.string_field("modelId") else {
                    return self
                        .fail(command, "set_model requires modelId", Some("invalid_request"))
                        .await;
                };
                match self.set_model(model).await {
                    Ok(data) => self.ok(command, Some(data)).await,
                    Err(error) => self.fail(command, &error.to_string(), Some("model_unavailable")).await,
                }
            }
            Kind::SetThinkingLevel => {
                let Some(level) = command.string_field("level") else {
                    return self
                        .fail(command, "set_thinking_level requires level", Some("invalid_request"))
                        .await;
                };
                match self.set_thinking_level(level).await {
                    Ok(()) => self.ok(command, None).await,
                    Err(error) => self.fail(command, &error.to_string(), Some("effort_unavailable")).await,
                }
            }
            Kind::SetTodos => {
                let phases = command.raw.get("phases").cloned().unwrap_or_else(|| json!([]));
                match self.set_todos(&phases) {
                    Ok(data) => self.ok(command, Some(data)).await,
                    Err(error) => self.fail(command, &error.to_string(), Some("todo_write_failed")).await,
                }
            }
            Kind::SetSessionName => {
                let Some(name) = command.string_field("name") else {
                    return self
                        .fail(command, "set_session_name requires name", Some("invalid_request"))
                        .await;
                };
                match self.set_session_name(name).await {
                    Ok(()) => self.ok(command, None).await,
                    Err(error) => self.fail(command, &error.to_string(), Some("rename_failed")).await,
                }
            }
            Kind::Compact => match self.compact().await {
                Ok(data) => self.ok(command, Some(data)).await,
                Err(error) => self.fail(command, &error.to_string(), Some("compaction_failed")).await,
            },
            Kind::Bash => {
                let Some(text) = command.string_field("command") else {
                    return self
                        .fail(command, "bash requires a command", Some("invalid_request"))
                        .await;
                };
                match self.run_bash(text).await {
                    Ok(data) => self.ok(command, Some(data)).await,
                    Err(error) => self.fail(command, &error.to_string(), Some("bash_failed")).await,
                }
            }
            Kind::AbortBash => self.unsupported(command, "no bash command is running").await,
            Kind::Unsupported(name) => {
                let error = format!("unsupported command: {name}");
                self.fail(command, &error, Some("unsupported_command")).await
            }
        }
    }

    async fn ok(&mut self, command: &Command, data: Option<Value>) -> Result<()> {
        let frame = frame::response_ok(command.id.as_ref(), command.kind.name(), data);
        self.out.send(&frame).await?;
        Ok(())
    }

    async fn fail(&mut self, command: &Command, error: &str, code: Option<&str>) -> Result<()> {
        let frame = frame::response_error(command.id.as_ref(), command.kind.name(), error, code);
        self.out.send(&frame).await?;
        Ok(())
    }

    async fn unsupported(&mut self, command: &Command, reason: &str) -> Result<()> {
        let error = format!("unsupported command: {} ({reason})", command.kind.name());
        self.fail(command, &error, Some("unsupported_command")).await
    }

    /// Drive one prompt to its terminal, then report it exactly once.
    async fn run_prompt(&mut self, command: &Command, text: &str) -> Result<()> {
        let id = command.id.clone();
        self.ok(command, None).await?;

        let request = Request::Message {
            id: self.daemon.next_id(),
            content: text.to_string(),
            images: command.images(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        };
        let request_id = request.id();
        let mut turn = Turn::new(self.state.provider.clone(), self.state.model.clone(), self.message_seq);
        self.out.send_all(turn.begin(text)).await?;
        self.prompt_in_flight = true;

        let terminal = match self.daemon.send(&request).await {
            Ok(()) => self.drive_turn(&mut turn, request_id).await?,
            Err(error) => {
                crate::logging::warn(&format!("rpc: prompt write failed: {error}"));
                Terminal::Failed
            }
        };
        self.prompt_in_flight = false;
        self.message_seq = turn.message_seq();
        self.out.send_all(turn.finish()).await?;
        self.out
            .send(&frame::prompt_result(id.as_ref(), terminal.status(), true))
            .await?;
        self.out.send(&frame::session_settled()).await?;
        Ok(())
    }

    /// Forward events until the turn's terminal arrives, handling the commands a
    /// host sends while a turn runs.
    async fn drive_turn(&mut self, turn: &mut Turn, request_id: u64) -> Result<Terminal> {
        loop {
            // The select borrows two fields for as long as its futures live, so
            // it yields a value and every use of `self` happens afterwards.
            let next = tokio::select! {
                event = self.foreground.recv() => Next::Event(event),
                inbound = self.commands.recv() => Next::Inbound(inbound),
            };
            match next {
                Next::Event(Some(event)) => {
                    let terminal = self.classify(&event, request_id);
                    self.out.send_all(turn.event(event)).await?;
                    if let Some(terminal) = terminal {
                        return Ok(terminal);
                    }
                }
                // The daemon connection ended mid-turn, so no terminal arrives.
                Next::Event(None) => return Ok(Terminal::Failed),
                Next::Inbound(Some(Inbound::Line(buffer))) => {
                    let text = String::from_utf8_lossy(&buffer);
                    let Some(command) = command::parse(&text) else {
                        continue;
                    };
                    self.in_turn_command(&command).await?;
                }
                Next::Inbound(Some(Inbound::Failed(error))) => {
                    anyhow::bail!("stdin read failed: {error}");
                }
                Next::Inbound(Some(Inbound::Eof)) | Next::Inbound(None) => {
                    return Ok(Terminal::Aborted);
                }
            }
        }
    }

    /// A command that arrived while the turn was running. The turn itself ends
    /// only on a daemon event, so nothing here returns a terminal.
    async fn in_turn_command(&mut self, command: &Command) -> Result<()> {
        match &command.kind {
            Kind::Abort => {
                self.ok(command, None).await?;
                self.daemon
                    .send(&Request::Cancel {
                        id: self.daemon.next_id(),
                    })
                    .await?;
                Ok(())
            }
            Kind::Steer | Kind::FollowUp | Kind::AbortAndPrompt => {
                let Some(text) = command.message() else {
                    self.fail(command, "message required", Some("invalid_request")).await?;
                    return Ok(());
                };
                self.ok(command, None).await?;
                if matches!(command.kind, Kind::AbortAndPrompt) {
                    self.daemon
                        .send(&Request::Cancel {
                            id: self.daemon.next_id(),
                        })
                        .await?;
                }
                let interrupt = Request::SoftInterrupt {
                    id: self.daemon.next_id(),
                    content: text.to_string(),
                    images: command.images(),
                    // A follow-up may skip the remaining tools at its injection
                    // point; a steer waits for the next safe point.
                    urgent: matches!(command.kind, Kind::FollowUp),
                    client_id: None,
                };
                self.daemon.send(&interrupt).await?;
                Ok(())
            }
            Kind::GetState => {
                let data = self.state_frame();
                self.ok(command, Some(data)).await?;
                Ok(())
            }
            Kind::NegotiateProtocol => {
                self.ok(command, Some(json!({"protocolVersion": frame::PROTOCOL_VERSION})))
                    .await?;
                Ok(())
            }
            _ => {
                // Anything that would change the session waits: a queued
                // `message` would interleave with the running turn.
                let error = format!("{} is not available while a prompt runs", command.kind.name());
                self.fail(command, &error, Some("busy")).await?;
                Ok(())
            }
        }
    }

    /// Whether this event ends the turn, and how.
    fn classify(&self, event: &ServerEvent, request_id: u64) -> Option<Terminal> {
        match event {
            ServerEvent::Done { id } if *id == request_id => Some(Terminal::Completed),
            ServerEvent::Error { id, .. } if *id == request_id => Some(Terminal::Failed),
            // A cancelled turn ends with the cancel's `Interrupted` and the
            // turn's own `Done`; the interrupt is the earlier and unambiguous
            // one, so the prompt result reports `aborted`.
            ServerEvent::Interrupted => Some(Terminal::Aborted),
            _ => None,
        }
    }
}
