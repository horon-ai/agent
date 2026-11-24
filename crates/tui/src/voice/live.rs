//! Opt-in real-time voice session adapter for `horon-tui`.
//!
//! Live audio is disabled by default (`voice.live_enabled = false`) and ships
//! with no preconfigured third-party endpoint. When enabled with a valid
//! configuration and explicit per-session consent (`/live consent`), the client
//! drives a user-configured external JSONL subprocess (`voice.live_command`) or
//! an injected [`LiveVoiceAdapter`] through strict generation and capture-id
//! fencing: stopping, cancelling, reconnecting or leaving audio mode immediately
//! retires active captures so delayed frames can never submit a stale utterance.
//! Leaving audio mode or falling back leaves the attached text session, draft
//! and conversation history untouched.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use horon_agent_sdk::process;

use crate::settings::Settings;

/// What `/live consent` shows before opening a live audio session.
pub(crate) const CONSENT: &str = "live voice: real-time audio mode streams speech through your configured external adapter/endpoint (`voice.live_command`, `voice.live_endpoint`). Audio stays off by default with no preconfigured third-party endpoint. Type /live consent to allow live audio for this session.";

/// What an unknown `/live` argument shows.
pub(crate) const USAGE: &str = "live: /live toggles live audio mode/capture; also consent, forget, start, talk, end, stop, reconnect, leave, status";

/// Process group of the running external live-voice adapter child, for signal cleanup.
static ACTIVE_LIVE_PID: AtomicU32 = AtomicU32::new(0);

/// Failure message produced by opt-in live voice configuration or adapter operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveError(String);

impl LiveError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for LiveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for LiveError {}

/// Settings governing the opt-in real-time voice adapter (`tui.toml`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveVoiceConfig {
    pub(crate) enabled: bool,
    pub(crate) adapter: String,
    pub(crate) endpoint: String,
    pub(crate) api_key_env: String,
    pub(crate) model: String,
    pub(crate) command: Vec<String>,
    pub(crate) fallback_to_text: bool,
}

impl Default for LiveVoiceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            adapter: String::new(),
            endpoint: String::new(),
            api_key_env: String::new(),
            model: String::new(),
            command: Vec::new(),
            fallback_to_text: true,
        }
    }
}

impl LiveVoiceConfig {
    /// Resolve live voice settings from the layered TUI settings snapshot.
    pub(crate) fn from_settings(settings: &Settings) -> Self {
        Self {
            enabled: settings.boolean("voice.live_enabled").unwrap_or(false),
            adapter: settings.text("voice.live_adapter").unwrap_or("").trim().to_string(),
            endpoint: settings.text("voice.live_endpoint").unwrap_or("").trim().to_string(),
            api_key_env: settings.text("voice.live_api_key_env").unwrap_or("").trim().to_string(),
            model: settings.text("voice.live_model").unwrap_or("").trim().to_string(),
            command: settings.list("voice.live_command").unwrap_or(&[]).to_vec(),
            fallback_to_text: settings.boolean("voice.live_fallback_to_text").unwrap_or(true),
        }
    }

    /// Check that opt-in live voice is enabled, well-formed, and backed by a configured adapter.
    pub(crate) fn check_ready(&self, has_custom_adapter: bool) -> Result<(), LiveError> {
        if !self.enabled {
            return Err(LiveError::new(
                "live voice is off; set voice.live_enabled = true in tui.toml or /settings (text input remains active)",
            ));
        }
        if !self.endpoint.is_empty() {
            validate_endpoint(&self.endpoint)?;
        }
        if !self.api_key_env.is_empty() && self.api_key_env.chars().any(char::is_whitespace) {
            return Err(LiveError::new(
                "voice.live_api_key_env must name a single environment variable without spaces",
            ));
        }
        if has_custom_adapter {
            return Ok(());
        }
        let has_cmd = self.command.first().is_some_and(|program| !program.trim().is_empty());
        if !has_cmd {
            if self.endpoint.is_empty() {
                return Err(LiveError::new(
                    "no live voice adapter or endpoint is configured; set voice.live_command and voice.live_endpoint in tui.toml or /settings (text input remains active)",
                ));
            }
            return Err(LiveError::new(
                "voice.live_endpoint is configured without an adapter executable (`voice.live_command`); text input remains active",
            ));
        }
        Ok(())
    }
}

/// Validate a configured live voice endpoint URL (`http(s)://`, `ws(s)://`, `unix://`).
pub(crate) fn validate_endpoint(endpoint: &str) -> Result<(), LiveError> {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() {
        return Err(LiveError::new("voice.live_endpoint cannot be empty"));
    }
    if trimmed.chars().any(char::is_control) || trimmed.chars().any(char::is_whitespace) {
        return Err(LiveError::new(
            "voice.live_endpoint contains whitespace or control characters",
        ));
    }
    if let Some(socket) = trimmed.strip_prefix("unix://") {
        if socket.is_empty() {
            return Err(LiveError::new("voice.live_endpoint `unix://` needs a socket path"));
        }
        return Ok(());
    }
    let parsed = url::Url::parse(trimmed)
        .map_err(|error| LiveError::new(format!("voice.live_endpoint is not a valid URL: {error}")))?;
    match parsed.scheme() {
        "http" | "https" | "ws" | "wss" => {}
        other => {
            return Err(LiveError::new(format!(
                "voice.live_endpoint scheme `{other}` is unsupported; use ws://, wss://, http://, https:// or unix://"
            )));
        }
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(LiveError::new("voice.live_endpoint URL needs a host"));
    }
    Ok(())
}

/// Lifecycle phase of the live audio session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LivePhase {
    Idle,
    Connecting,
    Ready,
    Capturing,
    Reconnecting,
}

impl LivePhase {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Connecting => "connecting",
            Self::Ready => "ready",
            Self::Capturing => "capturing",
            Self::Reconnecting => "reconnecting",
        }
    }

    pub(crate) fn is_active(self) -> bool {
        self != Self::Idle
    }
}

/// Events produced by a [`LiveVoiceAdapter`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LiveAdapterEvent {
    Ready {
        generation: u64,
    },
    Transcript {
        generation: u64,
        capture_id: u64,
        text: String,
        is_final: bool,
    },
    Disconnected {
        generation: u64,
        reason: String,
    },
    Error {
        generation: u64,
        reason: String,
    },
}

/// Result of attempting to start a live audio session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LiveStart {
    Started,
    NeedsConsent,
    AlreadyActive(LivePhase),
    Fallback(String),
}

/// What one polled adapter event resolved into for the UI/session loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LiveOutcome {
    Submit(String),
    Notice(String),
    Fallback(String),
}

/// Pluggable transport/device adapter for real-time audio sessions.
pub(crate) trait LiveVoiceAdapter: Send {
    fn start(&mut self, config: &LiveVoiceConfig, generation: u64) -> Result<(), LiveError>;
    fn begin_capture(&mut self, generation: u64, capture_id: u64) -> Result<(), LiveError>;
    fn end_capture(&mut self, generation: u64, capture_id: u64, commit: bool) -> Result<(), LiveError>;
    fn stop(&mut self, generation: u64) -> Result<(), LiveError>;
    fn reconnect(&mut self, config: &LiveVoiceConfig, generation: u64) -> Result<(), LiveError>;
    fn poll(&mut self) -> Vec<LiveAdapterEvent>;
    fn shutdown(&mut self);
}

/// Subprocess adapter communicating over newline-delimited JSON on stdin/stdout.
pub(crate) struct CommandLiveAdapter {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    events: Option<Receiver<LiveAdapterEvent>>,
    closed: Arc<AtomicBool>,
}

impl CommandLiveAdapter {
    pub(crate) fn new() -> Self {
        Self {
            child: None,
            stdin: None,
            events: None,
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    fn spawn(&mut self, config: &LiveVoiceConfig, generation: u64) -> Result<(), LiveError> {
        self.shutdown();
        let Some((program, args)) = config.command.split_first() else {
            return Err(LiveError::new("voice.live_command is empty"));
        };
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env_remove("HORON_LIVE_ENDPOINT")
            .env_remove("HORON_LIVE_MODEL")
            .env_remove("HORON_LIVE_ADAPTER")
            .env_remove("HORON_LIVE_API_KEY");
        if !config.endpoint.is_empty() {
            command.env("HORON_LIVE_ENDPOINT", &config.endpoint);
        }
        if !config.model.is_empty() {
            command.env("HORON_LIVE_MODEL", &config.model);
        }
        if !config.adapter.is_empty() {
            command.env("HORON_LIVE_ADAPTER", &config.adapter);
        }
        if !config.api_key_env.is_empty()
            && let Ok(secret) = std::env::var(&config.api_key_env)
            && !secret.trim().is_empty()
        {
            command.env("HORON_LIVE_API_KEY", secret);
        }
        process::isolate_process_group(&mut command);
        let mut child = command
            .spawn()
            .map_err(|error| LiveError::new(format!("cannot start live voice adapter `{program}`: {error}")))?;
        let pid = child.id();
        ACTIVE_LIVE_PID.store(pid, Ordering::SeqCst);
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let (sender, receiver) = mpsc::channel();
        let closed = Arc::new(AtomicBool::new(false));
        self.closed = closed.clone();
        if let Some(stdout) = stdout {
            let spawn_result = thread::Builder::new()
                .name("horon-live-voice".to_string())
                .spawn(move || read_adapter_stdout(stdout, sender, generation, closed));
            if let Err(error) = spawn_result {
                drop(process::force_stop_process_group(pid));
                drop(child.wait());
                if ACTIVE_LIVE_PID
                    .compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst)
                    .is_err()
                {
                    // Another live voice adapter already replaced the slot.
                }
                return Err(LiveError::new(format!(
                    "cannot start live voice reader thread: {error}"
                )));
            }
        }
        self.child = Some(child);
        self.stdin = stdin;
        self.events = Some(receiver);
        Ok(())
    }

    fn send_json(&mut self, value: serde_json::Value) -> Result<(), LiveError> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(LiveError::new("live voice adapter stdin is closed"));
        };
        let line = value.to_string();
        writeln!(stdin, "{line}")
            .and_then(|()| stdin.flush())
            .map_err(|error| LiveError::new(format!("cannot send control message to live voice adapter: {error}")))
    }
}

impl LiveVoiceAdapter for CommandLiveAdapter {
    fn start(&mut self, config: &LiveVoiceConfig, generation: u64) -> Result<(), LiveError> {
        self.spawn(config, generation)?;
        if let Err(error) = self.send_json(serde_json::json!({
            "type": "start",
            "generation": generation,
            "adapter": config.adapter,
            "endpoint": config.endpoint,
            "model": config.model,
        })) {
            self.shutdown();
            return Err(error);
        }
        Ok(())
    }

    fn begin_capture(&mut self, generation: u64, capture_id: u64) -> Result<(), LiveError> {
        self.send_json(serde_json::json!({
            "type": "begin_capture",
            "generation": generation,
            "capture_id": capture_id,
        }))
    }

    fn end_capture(&mut self, generation: u64, capture_id: u64, commit: bool) -> Result<(), LiveError> {
        self.send_json(serde_json::json!({
            "type": "end_capture",
            "generation": generation,
            "capture_id": capture_id,
            "commit": commit,
        }))
    }

    fn stop(&mut self, generation: u64) -> Result<(), LiveError> {
        drop(self.send_json(serde_json::json!({
            "type": "stop",
            "generation": generation,
        })));
        self.shutdown();
        Ok(())
    }

    fn reconnect(&mut self, config: &LiveVoiceConfig, generation: u64) -> Result<(), LiveError> {
        self.start(config, generation)
    }

    fn poll(&mut self) -> Vec<LiveAdapterEvent> {
        let mut out = Vec::new();
        if let Some(receiver) = self.events.as_ref() {
            while let Ok(event) = receiver.try_recv() {
                out.push(event);
            }
        }
        out
    }

    fn shutdown(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        self.stdin.take();
        if let Some(mut child) = self.child.take() {
            let pid = child.id();
            drop(process::force_stop_process_group(pid));
            drop(child.wait());
            if ACTIVE_LIVE_PID
                .compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                // Another live voice adapter already replaced the slot.
            }
        }
        self.events.take();
    }
}

impl Drop for CommandLiveAdapter {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn read_adapter_stdout(
    stdout: impl std::io::Read,
    sender: Sender<LiveAdapterEvent>,
    initial_generation: u64,
    closed: Arc<AtomicBool>,
) {
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let Ok(line) = line else {
            break;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(event) = parse_adapter_line(trimmed, initial_generation) {
            drop(sender.send(event));
        }
    }
    if !closed.load(Ordering::SeqCst) {
        drop(sender.send(LiveAdapterEvent::Disconnected {
            generation: initial_generation,
            reason: "live voice adapter closed its stream".to_string(),
        }));
    }
}

fn parse_adapter_line(line: &str, default_generation: u64) -> Option<LiveAdapterEvent> {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line) else {
        return None;
    };
    let kind = parsed.get("type")?.as_str()?;
    let generation = parsed
        .get("generation")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(default_generation);
    match kind {
        "ready" => Some(LiveAdapterEvent::Ready { generation }),
        "transcript" => {
            let capture_id = parsed
                .get("capture_id")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let text = parsed.get("text")?.as_str()?.to_string();
            let is_final = parsed.get("final").and_then(serde_json::Value::as_bool).unwrap_or(true);
            Some(LiveAdapterEvent::Transcript {
                generation,
                capture_id,
                text,
                is_final,
            })
        }
        "disconnected" => Some(LiveAdapterEvent::Disconnected {
            generation,
            reason: parsed
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("adapter disconnected")
                .to_string(),
        }),
        "error" => Some(LiveAdapterEvent::Error {
            generation,
            reason: parsed
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("adapter error")
                .to_string(),
        }),
        _ => None,
    }
}

/// Stop any active external live-voice child process when the TUI exits or receives a signal.
pub(crate) fn kill_active_process() {
    let pid = ACTIVE_LIVE_PID.swap(0, Ordering::SeqCst);
    if pid != 0 {
        drop(process::force_stop_process_group(pid));
    }
}

/// Client-side state machine for opt-in real-time voice sessions.
pub(crate) struct LiveVoice {
    consent: bool,
    phase: LivePhase,
    generation: u64,
    next_capture_id: u64,
    active_capture_id: Option<u64>,
    committed_capture: Option<(u64, u64)>,
    dropped_stale: u64,
    config: LiveVoiceConfig,
    adapter: Option<Box<dyn LiveVoiceAdapter>>,
    custom_adapter: bool,
    pending_send: Option<String>,
}

impl Default for LiveVoice {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveVoice {
    pub(crate) fn new() -> Self {
        Self {
            consent: false,
            phase: LivePhase::Idle,
            generation: 0,
            next_capture_id: 0,
            active_capture_id: None,
            committed_capture: None,
            dropped_stale: 0,
            config: LiveVoiceConfig::default(),
            adapter: None,
            custom_adapter: false,
            pending_send: None,
        }
    }

    pub(crate) fn set_pending_send(&mut self, text: String) {
        self.pending_send = Some(text);
    }

    pub(crate) fn take_pending_send(&mut self) -> Option<String> {
        self.pending_send.take()
    }

    pub(crate) fn clear_pending_send(&mut self) {
        self.pending_send = None;
    }

    /// Inject a custom adapter implementation (used by tests and embedded hosts).
    #[cfg(test)]
    pub(crate) fn set_adapter(&mut self, adapter: Box<dyn LiveVoiceAdapter>) {
        if let Some(mut old) = self.adapter.take() {
            old.shutdown();
        }
        self.adapter = Some(adapter);
        self.custom_adapter = true;
    }

    /// Refresh configuration from the layered TUI settings snapshot.
    pub(crate) fn apply_settings(&mut self, settings: &Settings) {
        self.config = LiveVoiceConfig::from_settings(settings);
        if !self.config.enabled && self.phase.is_active() {
            self.stop();
        }
    }

    pub(crate) fn consent_given(&self) -> bool {
        self.consent
    }

    pub(crate) fn give_consent(&mut self) {
        self.consent = true;
    }

    /// Revoke session consent and stop any active live audio capture or session.
    pub(crate) fn forget(&mut self) {
        self.consent = false;
        self.stop();
        if !self.custom_adapter
            && let Some(mut adapter) = self.adapter.take()
        {
            adapter.shutdown();
        }
    }

    pub(crate) fn phase(&self) -> LivePhase {
        self.phase
    }

    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    #[cfg(test)]
    pub(crate) fn active_capture_id(&self) -> Option<u64> {
        self.active_capture_id
    }

    #[cfg(test)]
    pub(crate) fn dropped_stale(&self) -> u64 {
        self.dropped_stale
    }

    /// Start the live audio session if consent and valid configuration are present.
    pub(crate) fn start(&mut self) -> LiveStart {
        if !self.consent {
            return LiveStart::NeedsConsent;
        }
        if self.phase.is_active() {
            return LiveStart::AlreadyActive(self.phase);
        }
        if let Err(reason) = self.config.check_ready(self.custom_adapter) {
            return LiveStart::Fallback(reason.to_string());
        }
        if self.adapter.is_none() {
            self.adapter = Some(Box::new(CommandLiveAdapter::new()));
            self.custom_adapter = false;
        }
        self.generation += 1;
        self.active_capture_id = None;
        self.committed_capture = None;
        let generation = self.generation;
        let config = self.config.clone();
        let Some(adapter) = self.adapter.as_mut() else {
            return LiveStart::Fallback("no live voice adapter available".to_string());
        };
        if let Err(reason) = adapter.start(&config, generation) {
            if !self.custom_adapter {
                adapter.shutdown();
            }
            self.generation += 1;
            self.phase = LivePhase::Idle;
            return LiveStart::Fallback(reason.to_string());
        }
        self.phase = LivePhase::Connecting;
        LiveStart::Started
    }

    /// Explicitly open audio capture on an already-ready live session.
    pub(crate) fn begin_capture(&mut self) -> Result<u64, LiveError> {
        if self.phase != LivePhase::Ready {
            return Err(LiveError::new(format!(
                "live voice must be ready before starting capture (currently {})",
                self.phase.label()
            )));
        }
        let Some(adapter) = self.adapter.as_mut() else {
            return Err(LiveError::new("no live voice adapter attached"));
        };
        self.next_capture_id += 1;
        let capture_id = self.next_capture_id;
        self.committed_capture = None;
        adapter.begin_capture(self.generation, capture_id)?;
        self.active_capture_id = Some(capture_id);
        self.phase = LivePhase::Capturing;
        Ok(capture_id)
    }

    /// Explicitly end active capture; `commit = true` allows its final transcript to submit.
    pub(crate) fn end_capture(&mut self, commit: bool) -> Result<(), LiveError> {
        let Some(capture_id) = self.active_capture_id else {
            return Err(LiveError::new("no live audio capture is active"));
        };
        let Some(adapter) = self.adapter.as_mut() else {
            self.active_capture_id = None;
            self.phase = LivePhase::Idle;
            return Err(LiveError::new("no live voice adapter attached"));
        };
        self.active_capture_id = None;
        self.phase = LivePhase::Ready;
        self.committed_capture = commit.then_some((self.generation, capture_id));
        adapter.end_capture(self.generation, capture_id, commit)
    }

    /// Stop the live audio session and immediately retire any unsubmitted captures.
    pub(crate) fn stop(&mut self) {
        let current_gen = self.generation;
        self.active_capture_id = None;
        self.committed_capture = None;
        if let Some(adapter) = self.adapter.as_mut() {
            drop(adapter.stop(current_gen));
        }
        self.generation += 1;
        self.phase = LivePhase::Idle;
    }

    /// Reconnect the live adapter under a fresh generation, fencing off any in-flight capture.
    pub(crate) fn reconnect(&mut self) -> Result<u64, LiveError> {
        if !self.consent {
            return Err(LiveError::new(CONSENT));
        }
        if let Err(reason) = self.config.check_ready(self.custom_adapter) {
            self.stop();
            return Err(reason);
        }
        if self.adapter.is_none() {
            self.adapter = Some(Box::new(CommandLiveAdapter::new()));
            self.custom_adapter = false;
        }
        self.active_capture_id = None;
        self.committed_capture = None;
        self.generation += 1;
        let generation = self.generation;
        let config = self.config.clone();
        let Some(adapter) = self.adapter.as_mut() else {
            return Err(LiveError::new("no live voice adapter attached"));
        };
        if let Err(reason) = adapter.reconnect(&config, generation) {
            if !self.custom_adapter {
                adapter.shutdown();
            }
            self.generation += 1;
            self.phase = LivePhase::Idle;
            return Err(reason);
        }
        self.phase = LivePhase::Reconnecting;
        Ok(generation)
    }

    /// Leave live audio mode cleanly, forgetting session consent while preserving the text session.
    pub(crate) fn leave(&mut self) {
        self.forget();
    }

    /// Poll adapter events and filter out any stale generation or retired capture.
    pub(crate) fn poll(&mut self) -> Vec<LiveOutcome> {
        let Some(adapter) = self.adapter.as_mut() else {
            return Vec::new();
        };
        let events = adapter.poll();
        let mut out = Vec::new();
        for event in events {
            if let Some(outcome) = self.accept(event) {
                out.push(outcome);
            }
        }
        out
    }

    pub(crate) fn accept(&mut self, event: LiveAdapterEvent) -> Option<LiveOutcome> {
        match event {
            LiveAdapterEvent::Ready { generation } => {
                if generation != self.generation {
                    self.dropped_stale += 1;
                    return None;
                }
                if matches!(self.phase, LivePhase::Connecting | LivePhase::Reconnecting) {
                    self.phase = LivePhase::Ready;
                    return Some(LiveOutcome::Notice(
                        "live voice: ready; run /live talk to capture or /live leave to return to text".to_string(),
                    ));
                }
                None
            }
            LiveAdapterEvent::Transcript {
                generation,
                capture_id,
                text,
                is_final,
            } => {
                let matches_active = generation == self.generation && self.active_capture_id == Some(capture_id);
                let matches_committed = self.committed_capture == Some((generation, capture_id));
                if !matches_active && !matches_committed {
                    self.dropped_stale += 1;
                    return None;
                }
                if !is_final {
                    return None;
                }
                if self.committed_capture == Some((generation, capture_id)) {
                    self.committed_capture = None;
                }
                if self.active_capture_id == Some(capture_id) {
                    self.active_capture_id = None;
                    if self.phase == LivePhase::Capturing {
                        self.phase = LivePhase::Ready;
                    }
                }
                let cleaned = sanitize_live_transcript(&text);
                if cleaned.is_empty() {
                    return Some(LiveOutcome::Notice("live voice: no speech captured".to_string()));
                }
                Some(LiveOutcome::Submit(cleaned))
            }
            LiveAdapterEvent::Disconnected { generation, reason } | LiveAdapterEvent::Error { generation, reason } => {
                if generation != self.generation {
                    self.dropped_stale += 1;
                    return None;
                }
                self.active_capture_id = None;
                self.committed_capture = None;
                self.generation += 1;
                self.phase = LivePhase::Idle;
                if !self.custom_adapter
                    && let Some(mut adapter) = self.adapter.take()
                {
                    adapter.shutdown();
                }
                let fallback_note = if self.config.fallback_to_text {
                    "; text input remains active"
                } else {
                    ""
                };
                Some(LiveOutcome::Fallback(format!("{reason}{fallback_note}")))
            }
        }
    }

    /// One-line summary for `/live status` (never prints secrets).
    pub(crate) fn status(&self) -> String {
        let consent = if self.consent {
            "consent given"
        } else {
            "no consent yet; /live consent allows live audio"
        };
        let endpoint = if self.config.endpoint.is_empty() {
            "endpoint: unset (text fallback)"
        } else {
            self.config.endpoint.as_str()
        };
        let enabled = if self.config.enabled { "enabled" } else { "disabled" };
        format!(
            "live voice: {} ({enabled}, gen {}); {consent}; {endpoint}; stale dropped: {}",
            self.phase.label(),
            self.generation,
            self.dropped_stale
        )
    }
}

impl Drop for LiveVoice {
    fn drop(&mut self) {
        self.stop();
        if let Some(mut adapter) = self.adapter.take() {
            adapter.shutdown();
        }
    }
}

fn sanitize_live_transcript(raw: &str) -> String {
    let mut out = String::new();
    for line in raw.lines() {
        let clean: String = line
            .chars()
            .filter(|character| *character == '\t' || !character.is_control())
            .collect();
        let clean = clean.trim();
        if clean.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(clean);
    }
    out
}
