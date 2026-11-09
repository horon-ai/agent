//! Shared real-process fixture: per-HOME daemon and bridge children, a loopback
//! scripted provider, and direct reads of the durable store. Included by
//! `tests/session_event_lifecycle.rs` via `#[path]`.
#![allow(dead_code)]

#[path = "change_watch.rs"]
mod change_watch;

/// Re-exported so a suite that includes both this fixture and the waits needs
/// only one `mod` of the shared file (clippy::duplicate_mod otherwise refuses the
/// second `#[path]` include of the same source). Each includer uses a different
/// subset, so an unused name here is not dead code.
#[allow(unused_imports)]
pub use change_watch::{ChangeWatch, wait_for_pid_exit};

use horon_agent::session::event_store_api as store;
use horon_agent_sdk::{ConnectOptions, HoronAgentClient};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

pub const BINARY: &str = env!("CARGO_BIN_EXE_horon-agent");
pub const TOOL_PROMPT: &str = "T17_TOOL_TURN please inspect the fixture";
pub const TOOL_COMMAND: &str = "printf t17-tool-ok";
pub const TOOL_FINAL_TEXT: &str = "t17 scripted tool turn complete";
pub const PLAIN_TEXT: &str = "t17 scripted reply";
pub const HOLD_TURN_PROMPT: &str = "T17_HOLD_TURN hold this turn open";
pub const SUMMARY_ROUTE_KEY: &str = "next_action";
pub const SUMMARY_PROMPT_MARKER: &str = "Compact the conversation below";

/// Bounded wait slice, so an event-driven probe never stalls on one OS wait.
const SLICE: Duration = Duration::from_millis(50);
pub const SUMMARY_GOAL: &str = "lifecycle fixture";

struct Gate {
    armed: AtomicBool,
    state: Mutex<bool>,
    released: Condvar,
}

impl Default for Gate {
    fn default() -> Self {
        Self {
            armed: AtomicBool::new(false),
            state: Mutex::new(false),
            released: Condvar::new(),
        }
    }
}

impl Gate {
    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
        *self.state.lock().expect("gate lock") = false;
    }

    fn release(&self) {
        *self.state.lock().expect("gate lock") = true;
        self.released.notify_all();
    }

    fn wait(&self) {
        let mut state = self.state.lock().expect("gate lock");
        while !*state {
            state = self
                .released
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

pub struct ScriptedProvider {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
    arrivals: Mutex<mpsc::Receiver<String>>,
    summary_gate: Arc<Gate>,
    turn_gate: Arc<Gate>,
    scripts: Arc<ScriptRegistry>,
}

impl ScriptedProvider {
    pub fn start() -> Arc<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind scripted provider");
        let port = listener.local_addr().expect("addr").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (arrival_tx, arrival_rx) = mpsc::channel();
        let summary_gate = Arc::new(Gate::default());
        let turn_gate = Arc::new(Gate::default());
        let scripts = Arc::new(ScriptRegistry::default());
        let provider = Arc::new(Self {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            requests: Arc::clone(&requests),
            arrivals: Mutex::new(arrival_rx),
            summary_gate: Arc::clone(&summary_gate),
            turn_gate: Arc::clone(&turn_gate),
            scripts: Arc::clone(&scripts),
        });
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let requests = Arc::clone(&requests);
                let arrivals = arrival_tx.clone();
                let summary_gate = Arc::clone(&summary_gate);
                let turn_gate = Arc::clone(&turn_gate);
                let scripts = Arc::clone(&scripts);
                std::thread::spawn(move || {
                    let _ = serve_request(stream, &requests, &arrivals, &summary_gate, &turn_gate, &scripts);
                });
            }
        });
        provider
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().expect("requests lock").len()
    }

    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().expect("requests lock").clone()
    }

    /// Wait for a request body containing `needle`. Requests that already
    /// arrived are scanned first, then the wait blocks on the provider's own
    /// arrival signal until the deadline.
    pub fn wait_for_request(&self, needle: &str, deadline: Duration) -> Result<String, String> {
        if let Some(body) = find_request(&self.requests, needle) {
            return Ok(body);
        }
        let arrivals = self.arrivals.lock().expect("arrivals lock");
        let end = Instant::now() + deadline;
        loop {
            let remaining = end.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "no provider request containing {needle:?} within {deadline:?}; saw {} requests",
                    self.request_count()
                ));
            }
            match arrivals.recv_timeout(remaining) {
                Ok(body) if body.contains(needle) => return Ok(body),
                Ok(_) => {}
                Err(error) => {
                    return Err(format!(
                        "no provider request containing {needle:?} within {deadline:?} ({error}); saw {} requests",
                        self.request_count()
                    ));
                }
            }
        }
    }

    pub fn hold_summary(&self) {
        self.summary_gate.arm();
    }

    pub fn release_summary(&self) {
        self.summary_gate.release();
    }

    pub fn hold_turn(&self) {
        self.turn_gate.arm();
    }

    pub fn release_turn(&self) {
        self.turn_gate.release();
    }

    /// Register a scripted turn for every request whose last user message
    /// contains `needle`. The script stays registered, so a second prompt with
    /// the same needle replays it.
    pub fn script(&self, needle: impl Into<String>, script: Script) {
        self.scripts.register(needle.into(), script);
    }

    pub fn wait_for_hold_open_opened(&self, deadline: Duration) -> Result<(), String> {
        self.scripts.wait_for_hold_open(true, deadline)
    }

    pub fn wait_for_hold_open_closed(&self, deadline: Duration) -> Result<(), String> {
        self.scripts.wait_for_hold_open(false, deadline)
    }

    pub fn hold_open_is_still_open(&self) -> bool {
        self.scripts.hold_open_opened() && !self.scripts.hold_open_closed()
    }
}

fn find_request(requests: &Arc<Mutex<Vec<String>>>, needle: &str) -> Option<String> {
    requests
        .lock()
        .expect("requests lock")
        .iter()
        .find(|body| body.contains(needle))
        .cloned()
}

fn serve_request(
    stream: TcpStream,
    requests: &Arc<Mutex<Vec<String>>>,
    arrivals: &mpsc::Sender<String>,
    summary_gate: &Arc<Gate>,
    turn_gate: &Arc<Gate>,
    scripts: &Arc<ScriptRegistry>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    let mut length = 0usize;
    let mut chunked = false;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let trimmed = line.trim_end();
        let header = trimmed.to_ascii_lowercase();
        if let Some(rest) = header.strip_prefix("content-length:") {
            length = rest.trim().parse().unwrap_or(0);
        }
        if let Some(rest) = header.strip_prefix("transfer-encoding:")
            && rest.contains("chunked")
        {
            chunked = true;
        }
        if trimmed.is_empty() {
            break;
        }
    }
    let body = if chunked {
        read_chunked_body(&mut reader)?
    } else {
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body)?;
        body
    };
    let body = String::from_utf8_lossy(&body).to_string();
    requests.lock().expect("requests lock").push(body.clone());
    let _ = arrivals.send(body.clone());

    if body.contains(SUMMARY_PROMPT_MARKER) {
        if summary_gate.armed.load(Ordering::SeqCst) {
            summary_gate.wait();
        }
        return write_text(stream, &summary_artifact());
    }
    if body.contains("tool_call_id") && last_user_text(&body).contains(TOOL_PROMPT) {
        return write_text(stream, TOOL_FINAL_TEXT);
    }
    let prompt = last_user_text(&body);
    if prompt.contains(HOLD_TURN_PROMPT) {
        if turn_gate.armed.load(Ordering::SeqCst) {
            turn_gate.wait();
        }
        return write_text(stream, PLAIN_TEXT);
    }
    if prompt.contains(TOOL_PROMPT) {
        return write_tool_call(stream);
    }
    if let Some(follow_up) = scripts.follow_up_for(&body) {
        return write_text(stream, &follow_up);
    }
    if let Some(script) = scripts.matching(&prompt) {
        return serve_script(stream, &script, scripts);
    }
    write_text(stream, PLAIN_TEXT)
}

/// Decode a chunked request body: hex-sized chunks, an optional trailer, then the terminator.
fn read_chunked_body(reader: &mut impl BufRead) -> std::io::Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let mut size_line = String::new();
        if reader.read_line(&mut size_line)? == 0 {
            break;
        }
        let size_text = size_line.trim();
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or_default().trim(), 16).unwrap_or(0);
        if size == 0 {
            loop {
                let mut trailer = String::new();
                if reader.read_line(&mut trailer)? == 0 || trailer.trim_end().is_empty() {
                    break;
                }
            }
            break;
        }
        let mut chunk = vec![0u8; size];
        reader.read_exact(&mut chunk)?;
        body.extend_from_slice(&chunk);
        let mut terminator = String::new();
        reader.read_line(&mut terminator)?;
    }
    Ok(body)
}

fn last_user_text(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return String::new();
    };
    let Some(messages) = value["messages"].as_array() else {
        return String::new();
    };
    let mut last = String::new();
    for message in messages {
        if message["role"] != "user" {
            continue;
        }
        match &message["content"] {
            Value::String(text) => last = text.clone(),
            Value::Array(blocks) => {
                for block in blocks {
                    if let Some(text) = block["text"].as_str() {
                        last = text.to_string();
                    }
                }
            }
            _ => {}
        }
    }
    last
}

fn summary_artifact() -> String {
    json!({
        "goal": SUMMARY_GOAL,
        "done": ["scripted summary"],
        "incomplete": [],
        "cancelled": [],
        "files": [{"path": "tests/session_event_lifecycle.rs"}],
        "evidence": ["scripted provider"],
        "decisions": ["originals are append-only"],
        "next_action": "replicate the session"
    })
    .to_string()
}

fn write_tool_call(stream: TcpStream) -> std::io::Result<()> {
    let arguments = json!({"command": TOOL_COMMAND, "intent": "lifecycle fixture tool call"});
    write_tool_call_frames(stream, "call_t17", "bash", &arguments.to_string())
}

pub fn chat_chunk(delta: Value, finish_reason: Option<&str>) -> Value {
    json!({
        "id": "chatcmpl-t17",
        "object": "chat.completion.chunk",
        "model": "scripted-model",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}]
    })
}

fn write_tool_call_frames(mut stream: TcpStream, call_id: &str, name: &str, arguments: &str) -> std::io::Result<()> {
    let frames = vec![
        chat_chunk(
            json!({"role": "assistant", "tool_calls": [{
                "index": 0, "id": call_id, "type": "function",
                "function": {"name": name, "arguments": ""}}]}),
            None,
        ),
        chat_chunk(
            json!({"tool_calls": [{"index": 0, "function": {"arguments": arguments}}]}),
            None,
        ),
        chat_chunk(json!({}), Some("tool_calls")),
    ];
    write_frames(&mut stream, &frames)
}

fn write_reasoning(mut stream: TcpStream, summary: &str, text: &str) -> std::io::Result<()> {
    // The scripted endpoint speaks the DeepSeek-style `reasoning_content`
    // delta. A distinct `summary` is streamed first, then the reasoning text.
    let mut frames = Vec::new();
    if !summary.trim().is_empty() && summary != text {
        frames.push(chat_chunk(
            json!({"role": "assistant", "reasoning_content": summary}),
            None,
        ));
    }
    frames.push(chat_chunk(
        json!({"role": "assistant", "reasoning_content": text}),
        None,
    ));
    frames.push(chat_chunk(json!({}), Some("stop")));
    write_frames(&mut stream, &frames)
}

fn write_text(mut stream: TcpStream, text: &str) -> std::io::Result<()> {
    let frames = vec![
        chat_chunk(json!({"role": "assistant", "content": text}), None),
        chat_chunk(json!({}), Some("stop")),
    ];
    write_frames(&mut stream, &frames)
}

fn write_frames(stream: &mut TcpStream, frames: &[Value]) -> std::io::Result<()> {
    let mut payload = String::new();
    for frame in frames {
        payload.push_str("data: ");
        payload.push_str(&frame.to_string());
        payload.push_str("\n\n");
    }
    payload.push_str("data: [DONE]\n\n");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        payload.len(),
        payload
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

/// A scripted turn, selected by [`ScriptedProvider::script`].
#[derive(Clone, Debug)]
pub enum Script {
    Text(String),
    /// One delta frame, then the response stays open until the peer closes.
    HoldOpen(String),
    ToolCall {
        name: String,
        arguments_json: String,
        /// Reply to the request that carries the matching tool result.
        follow_up: String,
    },
    Reasoning {
        summary: String,
        text: String,
    },
    /// These Chat Completions chunk objects stream verbatim. The request that
    /// carries the tool result of call `follow_up.0` is answered with the text
    /// `follow_up.1`.
    Frames {
        frames: Vec<Value>,
        follow_up: (String, String),
    },
}

#[derive(Default)]
struct ScriptRegistry {
    entries: Mutex<Vec<(String, Script)>>,
    follow_ups: Mutex<HashMap<String, String>>,
    call_counter: AtomicU32,
    /// (opened, closed) behind one mutex: a condvar must only ever be paired
    /// with a single mutex.
    hold_open: Mutex<(bool, bool)>,
    hold_open_changed: Condvar,
}

impl ScriptRegistry {
    fn register(&self, needle: String, script: Script) {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((needle, script));
    }

    fn matching(&self, prompt: &str) -> Option<Script> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .find(|(needle, _)| prompt.contains(needle))
            .map(|(_, script)| script.clone())
    }

    fn follow_up_for(&self, body: &str) -> Option<String> {
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return None;
        };
        let messages = value["messages"].as_array()?;
        let follow_ups = self.follow_ups.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        messages.iter().find_map(|message| {
            if message["role"] != "tool" {
                return None;
            }
            let call_id = message["tool_call_id"].as_str()?;
            follow_ups.get(call_id).cloned()
        })
    }

    fn next_call_id(&self) -> String {
        format!("call_t17_{}", self.call_counter.fetch_add(1, Ordering::Relaxed) + 1)
    }

    fn remember_follow_up(&self, call_id: &str, follow_up: &str) {
        self.follow_ups
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(call_id.to_string(), follow_up.to_string());
    }

    fn hold_open_state(&self) -> std::sync::MutexGuard<'_, (bool, bool)> {
        self.hold_open.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn mark_hold_open(&self, opened: bool) {
        let mut state = self.hold_open_state();
        if opened {
            state.0 = true;
        } else {
            state.1 = true;
        }
        self.hold_open_changed.notify_all();
    }

    fn hold_open_opened(&self) -> bool {
        self.hold_open_state().0
    }

    fn hold_open_closed(&self) -> bool {
        self.hold_open_state().1
    }

    /// `opened=true` waits for the hold-open response to be written; `false`
    /// waits for the peer to close it. Both waits are driven by the socket
    /// event that sets the flag, never by a timer.
    fn wait_for_hold_open(&self, opened: bool, deadline: Duration) -> Result<(), String> {
        let end = Instant::now() + deadline;
        let mut state = self.hold_open_state();
        while !(if opened { state.0 } else { state.1 }) {
            let remaining = end.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "the hold-open stream never {}",
                    if opened { "opened" } else { "closed" }
                ));
            }
            state = self
                .hold_open_changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
        Ok(())
    }
}

fn serve_script(stream: TcpStream, script: &Script, scripts: &ScriptRegistry) -> std::io::Result<()> {
    match script {
        Script::Text(text) => write_text(stream, text),
        Script::HoldOpen(delta) => serve_hold_open(stream, delta, scripts),
        Script::ToolCall {
            name,
            arguments_json,
            follow_up,
        } => {
            let call_id = scripts.next_call_id();
            scripts.remember_follow_up(&call_id, follow_up);
            write_tool_call_frames(stream, &call_id, name, arguments_json)
        }
        Script::Reasoning { summary, text } => write_reasoning(stream, summary, text),
        Script::Frames {
            frames,
            follow_up: (call_id, text),
        } => {
            scripts.remember_follow_up(call_id, text);
            let mut stream = stream;
            write_frames(&mut stream, frames)
        }
    }
}

fn serve_hold_open(mut stream: TcpStream, delta: &str, scripts: &ScriptRegistry) -> std::io::Result<()> {
    let frame = chat_chunk(json!({"role": "assistant", "content": delta}), None);
    let payload = format!("data: {frame}\n\n");
    // No Content-Length: the body is close-delimited, which is what lets the
    // daemon start reading the delta while the response is still open.
    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n")?;
    stream.write_all(payload.as_bytes())?;
    stream.flush()?;
    scripts.mark_hold_open(true);
    wait_for_peer_close(&mut stream);
    scripts.mark_hold_open(false);
    Ok(())
}

fn wait_for_peer_close(stream: &mut TcpStream) {
    let mut buffer = [0u8; 256];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return,
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
        }
    }
}

pub struct Instance {
    pub root: PathBuf,
    pub home: PathBuf,
    pub runtime: PathBuf,
    pub api_socket: PathBuf,
    pub daemon: Child,
    pub bridge: Child,
    provider: Arc<ScriptedProvider>,
}

/// The environment one isolated instance's daemon and CLI children run with.
pub fn instance_env(root: &Path, home: &Path, runtime: &Path, profile: Option<&str>) -> Vec<(String, String)> {
    let mut env = vec![
        ("HORON_HOME".to_string(), home.display().to_string()),
        ("HORON_AGENT_RUNTIME_DIR".to_string(), runtime.display().to_string()),
        ("HOME".to_string(), root.display().to_string()),
        ("XDG_CONFIG_HOME".to_string(), root.join("config").display().to_string()),
        ("XDG_CACHE_HOME".to_string(), root.join("cache").display().to_string()),
        ("OPENAI_API_KEY".to_string(), "t17-scripted-key".to_string()),
        ("HORON_AGENT_OPENAI_TRANSPORT".to_string(), "https".to_string()),
        ("HORON_AGENT_NO_AUTO_UPDATE".to_string(), "1".to_string()),
        ("HORON_AGENT_MEMORY_ENABLED".to_string(), "0".to_string()),
        ("HORON_AGENT_MEMORY_SIDECAR_ENABLED".to_string(), "0".to_string()),
        ("HORON_AGENT_NON_INTERACTIVE".to_string(), "1".to_string()),
    ];
    if let Some(profile) = profile {
        env.push(("HORON_AGENT_TOOL_PROFILE".to_string(), profile.to_string()));
    }
    env
}

impl Instance {
    pub fn start(root: &Path, label: &str, provider: Arc<ScriptedProvider>) -> Self {
        let base = root.join(label);
        let home = base.join("home");
        let runtime = base.join("rt");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&runtime).expect("runtime");
        set_mode(&base, 0o700);
        set_mode(&home, 0o700);
        set_mode(&runtime, 0o700);
        write_config(&home);
        let mut instance = Self {
            api_socket: base.join("api.sock"),
            root: base,
            home,
            runtime,
            daemon: Command::new(BINARY)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("probe"),
            bridge: Command::new(BINARY)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("probe"),
            provider,
        };
        instance.spawn_children();
        instance
    }

    fn spawn_children(&mut self) {
        let _ = std::fs::remove_file(self.runtime.join("horon-agent.sock"));
        let _ = std::fs::remove_file(&self.api_socket);
        write_scripted_provider_config(&self.home, self.provider.base_url());
        self.daemon = self.spawn(&["serve", "--no-update"], "daemon.log");
        wait_for_socket(&self.runtime.join("horon-agent.sock"), Duration::from_secs(60));
        let api = self.api_socket.to_str().expect("utf-8 socket").to_string();
        self.bridge = self.spawn(
            &["api-bridge", "--no-update", "--api-socket", api.as_str()],
            "bridge.log",
        );
        wait_for_socket(&self.api_socket, Duration::from_secs(60));
    }

    pub fn env_pairs(&self) -> Vec<(String, String)> {
        instance_env(&self.root, &self.home, &self.runtime, None)
    }

    fn spawn(&self, args: &[&str], log_name: &str) -> Child {
        let log = std::fs::File::create(self.runtime.join(log_name)).expect("log file");
        let errors = log.try_clone().expect("log clone");
        let mut command = Command::new(BINARY);
        command.args(args).env_clear();
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        for (key, value) in self.env_pairs() {
            command.env(key, value);
        }
        command
            .env_remove("HORON_AGENT_SOCKET")
            .env_remove("HORON_AGENT_API_SOCKET")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(errors))
            .spawn()
            .expect("spawn horon-agent")
    }

    pub fn client(&self) -> HoronAgentClient {
        HoronAgentClient::connect(ConnectOptions {
            socket_path: Some(self.api_socket.clone()),
            client_name: "lifecycle-qa/1.0".into(),
            request_timeout: Some(Duration::from_secs(90)),
            ensure_runtime: false,
            supports_user_response: false,
        })
        .unwrap_or_else(|error| panic!("connect to the bridge: {error}\n{}", self.logs()))
    }

    pub fn paths(&self) -> store::SessionPaths {
        store::SessionPaths::new(self.home.join("sessions"))
    }

    pub fn logs(&self) -> String {
        let read = |name: &str| std::fs::read_to_string(self.runtime.join(name)).unwrap_or_default();
        let mut home_logs = String::new();
        if let Ok(entries) = std::fs::read_dir(self.home.join("logs")) {
            for entry in entries.flatten() {
                if let Ok(text) = std::fs::read_to_string(entry.path()) {
                    home_logs.push_str(&format!("\n--- {} ---\n{text}", entry.path().display()));
                }
            }
        }
        format!(
            "daemon.log:\n{}\nbridge.log:\n{}\nhome.logs:{home_logs}",
            read("daemon.log"),
            read("bridge.log")
        )
    }

    pub fn restart(&mut self) {
        self.stop();
        self.spawn_children();
    }

    pub fn kill_hard(&mut self) {
        let _ = self.bridge.kill();
        let _ = self.daemon.kill();
        let _ = self.bridge.wait();
        let _ = self.daemon.wait();
    }

    pub fn stop(&mut self) {
        self.kill_hard();
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct Lifecycle {
    pub root: PathBuf,
    pub provider: Arc<ScriptedProvider>,
    pub a: Instance,
    pub b: Instance,
    guard: std::sync::MutexGuard<'static, ()>,
}

fn instance_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Lifecycle {
    pub fn start(label: &str) -> Self {
        let guard = instance_lock();
        let root = scratch_root(label);
        let provider = ScriptedProvider::start();
        let a = Instance::start(&root, "a", Arc::clone(&provider));
        let b = Instance::start(&root, "b", Arc::clone(&provider));
        Self {
            root,
            provider,
            a,
            b,
            guard,
        }
    }
}

impl Drop for Lifecycle {
    fn drop(&mut self) {
        self.a.stop();
        self.b.stop();
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = &self.guard;
    }
}

pub fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

pub fn write_config(home: &Path) {
    let config = "[compaction]\nbackground = false\nmax_context_tokens = 20000\nrecent_tokens = 512\n";
    std::fs::write(home.join("config.toml"), config).expect("write config");
}

/// Write the isolated home's provider profile: one direct OpenAI-compatible
/// endpoint, selected as the default under the profile id `scripted`.
pub fn write_provider_config(home: &Path, provider_base_url: &str, model: &str) {
    let config = format!(
        "[compaction]\nbackground = false\nmax_context_tokens = 20000\nrecent_tokens = 512\n\n\
[provider]\ndefault_provider = \"scripted\"\ndefault_model = \"{model}\"\n\n\
[providers.scripted]\ntype = \"openai-compatible\"\nbase_url = \"{provider_base_url}\"\nauth = \"none\"\n\
default_model = \"{model}\"\nmodels = [{{ id = \"{model}\" }}]\n"
    );
    std::fs::write(home.join("config.toml"), config).expect("write provider config");
}

pub fn write_scripted_provider_config(home: &Path, provider_base_url: &str) {
    write_provider_config(home, provider_base_url, "scripted-model");
}

pub fn scratch_root(label: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let short: String = label.chars().filter(|ch| ch.is_ascii_alphanumeric()).take(6).collect();
    let root = PathBuf::from(format!("/private/tmp/tl{}{short}{n}", std::process::id()));
    std::fs::create_dir_all(&root).expect("scratch root");
    root
}

/// Wait until the socket accepts a connection. The wait is driven by the
/// directory-change event for the socket's parent, so a live server is picked up
/// on its creation rather than on a timer; a target without that event source
/// polls the connect once per slice under the same deadline.
pub fn wait_for_socket(path: &Path, deadline: Duration) {
    let end = Instant::now() + deadline;
    let parent = path.parent().expect("socket parent");
    let watch = ChangeWatch::arm(&[parent]);
    let outcome = loop {
        if UnixStream::connect(path).is_ok() {
            break Ok(());
        }
        let remaining = end.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let log = std::fs::read_to_string(parent.join("daemon.log")).unwrap_or_default();
            break Err(format!(
                "socket {} never became connectable within {deadline:?}; daemon log:\n{log}",
                path.display()
            ));
        }
        watch.wait(remaining.min(SLICE));
    };
    outcome.unwrap_or_else(|error| panic!("{error}"));
}

pub fn export_all(client: &HoronAgentClient, session_id: &str) -> horon_agent_sdk::SessionEventPage {
    let mut page = client
        .export_session_events(session_id, Vec::new(), Vec::new(), None)
        .unwrap_or_else(|error| panic!("export first page: {error}"));
    let heads = page.heads.clone();
    let mut walked = 0;
    while let Some(cursor) = page.next_cursor.clone() {
        walked += 1;
        assert!(walked <= 64, "export cursor walk did not terminate");
        let next = client
            .export_session_events(session_id, heads.clone(), Vec::new(), Some(cursor))
            .unwrap_or_else(|error| panic!("export next page: {error}"));
        page.events.extend(next.events.clone());
        page.record_refs.extend(next.record_refs.clone());
        for reference in &next.blob_refs {
            if !page
                .blob_refs
                .iter()
                .any(|existing| existing.sha256 == reference.sha256)
            {
                page.blob_refs.push(reference.clone());
            }
        }
        page.next_cursor = next.next_cursor.clone();
    }
    page
}

pub fn import_all(
    target: &HoronAgentClient,
    source: &HoronAgentClient,
    page: &horon_agent_sdk::SessionEventPage,
) -> horon_agent_sdk::SessionImportResult {
    let session_id = page.session_id.clone();
    for reference in &page.blob_refs {
        let mut offset = 0u64;
        while offset < reference.size {
            let chunk = source
                .export_session_blob(&session_id, &reference.sha256, reference.size, offset)
                .unwrap_or_else(|error| panic!("export blob at {offset}: {error}"));
            target
                .import_session_blob(&session_id, &chunk)
                .unwrap_or_else(|error| panic!("import blob at {offset}: {error}"));
            offset = chunk.next_offset;
            if chunk.complete {
                break;
            }
        }
    }
    match target.import_session_events(page, None) {
        Ok(result) => result,
        Err(error) => {
            let details = error
                .details
                .clone()
                .unwrap_or_else(|| panic!("import failed without a store detail for {}: {error}", page.session_id));
            assert_eq!(
                details.store_code, "conflict_heads",
                "a multi-head page is refused for a reason other than retained heads: {details:?}"
            );
            assert!(
                !details.accepted_ids.is_empty(),
                "the retained-head report must carry the accepted prefix: {details:?}"
            );
            horon_agent_sdk::SessionImportResult {
                session_id: page.session_id.clone(),
                accepted_ids: details.accepted_ids.clone(),
                heads: details.conflict_heads.clone(),
                retry_cursor: details.retry_cursor.clone(),
                conflict_heads: details.conflict_heads.clone(),
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    pub event_id: String,
    pub role: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreProjection {
    pub heads: Vec<String>,
    pub title: Option<String>,
    pub archived: bool,
    pub compaction: Option<String>,
    pub blocked: Vec<String>,
    pub messages: Vec<StoredMessage>,
}

pub fn store_projection(paths: &store::SessionPaths, session_id: &str, head: Option<&str>) -> StoreProjection {
    let session = store::SessionId::parse(session_id).expect("session id");
    let graph = store::EventGraph::open(paths, &session).expect("open graph");
    let selected = head.map(|head| store::EventId::parse(head).expect("head id"));
    let projection = graph.replay(selected.as_ref()).expect("replay");
    let mut blocked = Vec::new();
    for (_, payload) in projection.blocked() {
        let value = serde_json::to_value(payload).expect("blocked json");
        blocked.push(value["reason"].as_str().unwrap_or("?").to_owned());
    }
    StoreProjection {
        heads: graph.heads().iter().map(|id| id.as_str().to_owned()).collect(),
        title: projection
            .metadata()
            .custom_title
            .clone()
            .or_else(|| projection.metadata().title.clone()),
        archived: projection.archive().map(|archive| archive.archived).unwrap_or(false),
        compaction: projection.compaction().map(|(_, payload)| payload.summary_text.clone()),
        blocked,
        messages: projection
            .messages()
            .iter()
            .map(|item| StoredMessage {
                event_id: item.event_id.as_str().to_owned(),
                role: format!("{:?}", item.message.role).to_ascii_lowercase(),
                text: message_text(&item.message),
            })
            .collect(),
    }
}

/// Every retained head of a session with its own projection, in head order.
pub fn branches(paths: &store::SessionPaths, session_id: &str) -> Vec<(String, StoreProjection)> {
    let session = store::SessionId::parse(session_id).expect("session id");
    let graph = store::EventGraph::open(paths, &session).expect("open graph");
    graph
        .heads()
        .iter()
        .map(|head| {
            let id = head.as_str().to_owned();
            (id.clone(), store_projection(paths, session_id, Some(&id)))
        })
        .collect()
}

fn message_text(message: &impl serde::Serialize) -> String {
    let value = serde_json::to_value(message).unwrap_or(Value::Null);
    let mut parts = Vec::new();
    if let Some(blocks) = value["content"].as_array() {
        for block in blocks {
            if let Some(text) = block["text"].as_str() {
                parts.push(text.to_string());
            } else if block["type"] == "tool_use" {
                parts.push(format!(
                    "tool_use:{}:{}",
                    block["name"].as_str().unwrap_or("?"),
                    block["input"]
                ));
            } else if block["type"] == "tool_result" {
                parts.push(format!("tool_result:{}", block["content"].as_str().unwrap_or("?")));
            } else {
                parts.push(block.to_string());
            }
        }
    }
    parts.join("|")
}

pub fn event_ids(paths: &store::SessionPaths, session_id: &str) -> Vec<String> {
    let session = store::SessionId::parse(session_id).expect("session id");
    let graph = store::EventGraph::open(paths, &session).expect("open graph");
    graph.ordered_ids().iter().map(|id| id.as_str().to_owned()).collect()
}

pub fn page_kinds(page: &horon_agent_sdk::SessionEventPage) -> Vec<String> {
    page.events
        .iter()
        .map(|record| {
            let value: Value = serde_json::from_str(&record.canonical_json).expect("canonical record json");
            value["kind"].as_str().unwrap_or("?").to_owned()
        })
        .collect()
}

pub fn page_pairs(page: &horon_agent_sdk::SessionEventPage) -> Vec<(String, String)> {
    page.events
        .iter()
        .map(|record| (record.event_id.clone(), record.canonical_json.clone()))
        .collect()
}

pub fn raw_log_lines(paths: &store::SessionPaths, session_id: &str) -> Vec<String> {
    let session = store::SessionId::parse(session_id).expect("session id");
    let path = paths.events_log(&session).expect("log path");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    text.lines().map(str::to_owned).collect()
}

pub fn parsable_log_lines(paths: &store::SessionPaths, session_id: &str) -> usize {
    raw_log_lines(paths, session_id)
        .iter()
        .filter(|line| serde_json::from_str::<Value>(line).is_ok())
        .count()
}

pub fn contains_all(before: &[String], after: &[String]) -> bool {
    before.iter().all(|id| after.iter().any(|kept| kept == id))
}

pub fn text_message(id: &str, role: &str, text: &str) -> store::MessageAppendedPayload {
    store::MessageAppendedPayload {
        message: serde_json::from_value(json!({
            "id": id,
            "role": role,
            "content": [{"type": "text", "text": text}]
        }))
        .expect("stored message"),
    }
}

/// Block on the OS file-change event for the session log and report the durable
/// compaction summary from whichever retained head carries it. A record already
/// on disk returns immediately; otherwise the wait is driven by the filesystem
/// event, never by a timer, and a target without that event source polls the
/// projection once per slice under the same deadline.
pub fn wait_for_compaction(
    paths: &store::SessionPaths,
    session_id: &str,
    deadline: Duration,
) -> Result<String, String> {
    let session = store::SessionId::parse(session_id).map_err(|error| error.to_string())?;
    if let Some(summary) = compacted_summary(paths, &session) {
        return Ok(summary);
    }
    let log = paths.events_log(&session).map_err(|error| error.to_string())?;
    let watch = ChangeWatch::arm(&[&log]);
    let end = Instant::now() + deadline;
    loop {
        let remaining = end.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "no durable Compacted record in {} within {deadline:?}",
                log.display()
            ));
        }
        watch.wait(remaining.min(SLICE));
        if let Some(summary) = compacted_summary(paths, &session) {
            return Ok(summary);
        }
    }
}

fn compacted_summary(paths: &store::SessionPaths, session: &store::SessionId) -> Option<String> {
    let graph = store::EventGraph::open(paths, session).ok()?;
    for head in graph.heads() {
        if let Ok(projection) = graph.replay(Some(head))
            && let Some((_, payload)) = projection.compaction()
        {
            return Some(payload.summary_text.clone());
        }
    }
    None
}

pub fn append_root(writer: &mut store::BlockingWriter, session: &store::SessionId) -> store::EventId {
    let root = store::EventDraft::new(
        session.clone(),
        store::EventId::new_v4(),
        None,
        store::OriginId::new_v4(),
        store::SessionMetadata::default().created_at,
        store::EventKind::new(store::KnownEventKind::SessionCreated),
        json!(store::SessionCreatedPayload {
            branch_root: false,
            reset_provenance: None,
            metadata: store::SessionMetadata::default(),
        }),
    )
    .seal()
    .expect("root");
    writer.append(&root).expect("append root");
    root.envelope().event_id.clone()
}

pub fn append_message(
    writer: &mut store::BlockingWriter,
    session: &store::SessionId,
    parent: &store::EventId,
    id: &str,
    role: &str,
    text: &str,
) -> store::EventId {
    let event = store::EventDraft::new(
        session.clone(),
        store::EventId::new_v4(),
        Some(parent.clone()),
        store::OriginId::new_v4(),
        store::SessionMetadata::default().created_at,
        store::EventKind::new(store::KnownEventKind::MessageAppended),
        json!(text_message(id, role, text)),
    )
    .seal()
    .expect("message");
    writer.append(&event).expect("append message");
    event.envelope().event_id.clone()
}
