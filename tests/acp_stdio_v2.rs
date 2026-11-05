#![cfg(unix)]
//! ACP v2 stdio conformance suite and dual-stack validation (ACP v2 plan,
//! todo 12): one test per IS-1..IS-15 row plus the six dual-stack checks of
//! the migration guide and the v1-session-resumed-over-v2 check.
//!
//! Every test boots its own isolated `horon-agent serve` (the `acp` tool
//! profile and a provider profile pointed at the scripted upstream) and drives the
//! shipped `horon-agent acp` binary over stdio through the mock client in its
//! v2 mode. The client validates every frame it reads against the vendored
//! `schema-v2.0.0-alpha.10.json` as it arrives, and each test re-validates
//! every frame of the connection in both directions at the end and asserts
//! zero violations. Every wait is for an exact frame with a bounded timeout.
//!
//! Two kinds of frame are accounted for by name rather than validated:
//! requests this suite writes on purpose with params the schema refuses (the
//! v1-only methods on a v2 connection, a `null` id, an `sse` MCP server), and
//! the agent's `id: null` error answer to the `null`-id probe. The alpha.10
//! `Error` response branch documents `null` as the id "only when the request's
//! id could not be determined, such as for a parse error or an invalid
//! request", but its `anyOf` lists only the integer and string forms, so the
//! one frame the description itself prescribes cannot pass the `$def`. That
//! frame's `/id` finding is counted as `null_id_error_answers` and every
//! other check still runs on it.

#[path = "support/acp_client.rs"]
mod acp_client;
#[path = "support/acp_daemon.rs"]
mod acp_daemon;
#[path = "support/acp_schema.rs"]
mod acp_schema;
#[path = "support/lifecycle.rs"]
mod lifecycle;

use acp_client::{AcpClient, AcpClientError, AcpClientOptions, ClientCapabilities, Frame, PermissionOutcome};
use acp_daemon::AcpDaemon;
use acp_schema::{Direction, FrameValidator, Violation};
use lifecycle::Script;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// Bound for one response or one awaited frame. Model turns run through the
/// scripted provider, so a turn is seconds; the bound only keeps a broken run from
/// hanging.
const WAIT: Duration = Duration::from_secs(120);

const V2_OFF: (&str, &str) = ("HORON_AGENT_ACP_PROTOCOL_V2", "0");

/// One mock-client connection plus the bookkeeping the end-of-test schema
/// tally needs.
struct Conn {
    client: AcpClient,
    label: String,
    version: u64,
    /// (id, method) of every request this client wrote.
    sent: Vec<(Value, String)>,
    /// Ids of requests written on purpose with params or a method the schema refuses.
    invalid_ids: Vec<Value>,
    /// Raw lines written on purpose that the schema refuses as a whole.
    invalid_lines: Vec<String>,
    /// Notifications already read while waiting for a specific one, oldest first.
    pulled: Vec<Value>,
}

impl Conn {
    fn v2(daemon: &AcpDaemon, label: &str) -> Self {
        Self::open(daemon.env_pairs(), AcpClientOptions::new(Vec::new()), 2, label)
    }

    fn v2_with(daemon: &AcpDaemon, capabilities: ClientCapabilities, auth_terminal: bool, label: &str) -> Self {
        Self::open(
            daemon.env_pairs(),
            AcpClientOptions::new(Vec::new())
                .capabilities(capabilities)
                .auth_terminal(auth_terminal),
            2,
            label,
        )
    }

    /// A client whose reader validates against `version`'s schema. A v1 reader
    /// is what a v2 client sees from an agent that answered version 1.
    fn open(env: Vec<(String, String)>, options: AcpClientOptions, version: u64, label: &str) -> Self {
        let options = AcpClientOptions { env, ..options }
            .protocol_version(version)
            .deadline(WAIT);
        Self {
            client: AcpClient::spawn(options),
            label: label.to_string(),
            version,
            sent: Vec::new(),
            invalid_ids: Vec::new(),
            invalid_lines: Vec::new(),
            pulled: Vec::new(),
        }
    }

    fn v1(daemon: &AcpDaemon, label: &str) -> Self {
        Self::open(daemon.env_pairs(), AcpClientOptions::new(Vec::new()), 1, label)
    }

    /// The handshake this client's version writes; asserts the version answered.
    fn initialize(&mut self) -> Value {
        let params = self.client.initialize_params();
        let result = self.ok("initialize", params);
        assert_eq!(result["protocolVersion"], self.version, "{}: {result}", self.label);
        result
    }

    fn send(&mut self, method: &str, params: Value) -> Value {
        let id = json!(self.client.next_id());
        self.sent.push((id.clone(), method.to_string()));
        self.client
            .send_request(id.clone(), method, params)
            .unwrap_or_else(|error| panic!("{}: send {method}: {error}", self.label));
        id
    }

    fn send_invalid(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params);
        self.invalid_ids.push(id.clone());
        id
    }

    fn wait(&mut self, method: &str, id: &Value) -> Result<Value, Value> {
        match self.client.await_response(method, id, WAIT) {
            Ok(result) => Ok(result),
            Err(AcpClientError::Rpc { error, .. }) => Err(error),
            Err(error) => panic!("{}: {method} (id {id}): {error}", self.label),
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, Value> {
        let id = self.send(method, params);
        self.wait(method, &id)
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        self.call(method, params)
            .unwrap_or_else(|error| panic!("{}: {method} failed: {error}", self.label))
    }

    fn err(&mut self, method: &str, params: Value) -> Value {
        match self.call(method, params) {
            Err(error) => error,
            Ok(result) => panic!("{}: {method} succeeded unexpectedly: {result}", self.label),
        }
    }

    fn err_invalid(&mut self, method: &str, params: Value) -> Value {
        let id = self.send_invalid(method, params);
        match self.wait(method, &id) {
            Err(error) => error,
            Ok(result) => panic!("{}: {method} succeeded unexpectedly: {result}", self.label),
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.client
            .notify(method, params)
            .unwrap_or_else(|error| panic!("{}: notify {method}: {error}", self.label));
    }

    fn write_invalid_line(&mut self, line: &str) {
        self.invalid_lines.push(line.to_string());
        self.client
            .write_line(line)
            .unwrap_or_else(|error| panic!("{}: write: {error}", self.label));
    }

    /// `_horon/barrier` is unknown, so it is answered -32601 in order after
    /// everything written before it.
    fn barrier(&mut self) {
        let error = self.err("_horon/barrier", json!({}));
        assert_eq!(error["code"], -32601, "{error}");
    }

    fn new_session(&mut self, cwd: &Path) -> String {
        self.new_session_with(json!({"cwd": cwd_str(cwd)}))
    }

    fn new_session_with(&mut self, params: Value) -> String {
        let created = self.ok("session/new", params);
        created["sessionId"].as_str().expect("a session id").to_string()
    }

    fn cursor(&self, session_id: &str) -> usize {
        self.client.state().cursor(session_id)
    }

    /// The state transitions of `session_id` recorded at or after `cursor`.
    fn states_since(&self, session_id: &str, cursor: usize) -> Vec<String> {
        self.client.state().transitions(session_id)[cursor..]
            .iter()
            .map(|update| match &update.stop_reason {
                Some(reason) => format!("{}:{reason}", update.state),
                None => update.state.clone(),
            })
            .collect()
    }

    fn await_idle_after(&mut self, session_id: &str, cursor: usize) -> String {
        self.client
            .await_idle_after(session_id, cursor, WAIT)
            .unwrap_or_else(|| panic!("{}: no idle for {session_id} within {WAIT:?}", self.label))
    }

    /// One v2 prompt turn: the `messageId` the prompt was answered with, the
    /// session updates of `session_id` up to and including its idle state, and
    /// the idle stop reason.
    fn turn(&mut self, session_id: &str, text: &str) -> (String, Vec<Value>, String) {
        self.take_notes();
        let cursor = self.cursor(session_id);
        let message_id = self.prompt(session_id, text);
        let stop = self.await_idle_after(session_id, cursor);
        let updates = updates_of(&self.take_notes(), session_id);
        (message_id, updates, stop)
    }

    /// Every notification read so far and not yet taken, oldest first.
    fn take_notes(&mut self) -> Vec<Value> {
        let mut notes = std::mem::take(&mut self.pulled);
        notes.extend(self.client.drain_notifications());
        notes
    }

    fn prompt(&mut self, session_id: &str, text: &str) -> String {
        let result = self.ok(
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": text}]}),
        );
        assert_eq!(
            result
                .as_object()
                .map(|object| object.keys().cloned().collect::<Vec<_>>()),
            Some(vec!["messageId".to_string()]),
            "a v2 prompt answers only its messageId: {result}"
        );
        result["messageId"].as_str().expect("a messageId").to_string()
    }

    fn send_prompt(&mut self, session_id: &str, text: &str) -> Value {
        self.send(
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": text}]}),
        )
    }

    /// Reads (answering agent requests as usual) until a session/update for
    /// `session_id` satisfies `matches`, searching what was already read
    /// first; returns it. Every notification read stays available to
    /// [`Self::take_notes`].
    fn await_update(&mut self, session_id: &str, what: &str, matches: impl Fn(&Value) -> bool) -> Value {
        let hit = |note: &Value| {
            note["method"] == "session/update"
                && note["params"]["sessionId"] == session_id
                && matches(&note["params"]["update"])
        };
        let queued = self.client.drain_notifications();
        self.pulled.extend(queued);
        if let Some(note) = self.pulled.iter().find(|note| hit(note)) {
            return note["params"]["update"].clone();
        }
        let end = Instant::now() + WAIT;
        loop {
            let remaining = end.saturating_duration_since(Instant::now());
            let note = self
                .client
                .next_notification(remaining)
                .unwrap_or_else(|error| panic!("{}: waiting for {what}: {error}", self.label));
            self.pulled.push(note.clone());
            if hit(&note) {
                return note["params"]["update"].clone();
            }
        }
    }

    /// The agent's answer to the `id: null` probe. The reader refuses it
    /// (alpha.10's `RequestId` has no `null` form), so the refusal is checked
    /// to be about `/id` and nothing else, and the error object is returned.
    fn read_null_id_error(&mut self) -> Value {
        let end = Instant::now() + WAIT;
        loop {
            match self.client.next_frame(end) {
                Err(AcpClientError::Schema { line, violations }) => {
                    assert!(
                        violations.iter().all(|violation| violation.path == "/id"),
                        "{}: the null-id answer breaks more than its id: {violations:?} in {line}",
                        self.label
                    );
                    let frame: Value = serde_json::from_str(&line).expect("a JSON frame");
                    assert_eq!(frame["id"], Value::Null, "{line}");
                    return frame["error"].clone();
                }
                Ok(Frame::Notification { method, params }) => {
                    self.pulled.push(json!({"method": method, "params": params}));
                }
                Ok(other) => panic!("{}: expected the null-id error, saw {other:?}", self.label),
                Err(error) => panic!("{}: waiting for the null-id error: {error}", self.label),
            }
        }
    }

    fn stdout(&self) -> Vec<String> {
        self.client.stdout_lines()
    }

    /// Validates every frame of this connection in both directions, prints the
    /// tally and returns the number of violations.
    fn schema_violations(&self) -> usize {
        let mut agent = FrameValidator::for_version(self.version, Direction::AgentToClient);
        for (id, method) in &self.sent {
            agent.note_request(id, method);
        }
        let mut client = FrameValidator::for_version(self.version, Direction::ClientToAgent);
        let mut violations = 0;
        let mut null_id_errors = 0;
        let mut batch_lines = 0;
        let stdout = self.client.stdout_lines();
        for line in &stdout {
            let value: Value = serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("{}: stdout line is not JSON ({error}): {line}", self.label));
            let entries = match &value {
                Value::Array(entries) => {
                    batch_lines += 1;
                    entries.clone()
                }
                other => vec![other.clone()],
            };
            for entry in &entries {
                if let (Some(method), Some(id)) = (entry.get("method").and_then(Value::as_str), entry.get("id")) {
                    client.note_request(id, method);
                }
                let null_id_error = entry.get("id") == Some(&Value::Null) && entry.get("error").is_some();
                let found: Vec<Violation> = agent
                    .validate(entry)
                    .into_iter()
                    .filter(|violation| !(null_id_error && violation.path == "/id"))
                    .collect();
                if null_id_error {
                    null_id_errors += 1;
                }
                for violation in found {
                    violations += 1;
                    eprintln!("{}-VIOLATION agent->client {violation} in {line}", self.label);
                }
            }
        }
        let stdin = self.client.stdin_lines();
        let mut deliberate = 0;
        for line in &stdin {
            if self.invalid_lines.contains(line) {
                deliberate += 1;
                continue;
            }
            let value: Value = serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("{}: stdin line is not JSON ({error}): {line}", self.label));
            let entries = match &value {
                Value::Array(entries) => entries.clone(),
                other => vec![other.clone()],
            };
            for entry in &entries {
                let probe =
                    entry.get("method").is_some() && entry.get("id").is_some_and(|id| self.invalid_ids.contains(id));
                if probe {
                    deliberate += 1;
                    continue;
                }
                for violation in client.validate(entry) {
                    violations += 1;
                    eprintln!("{}-VIOLATION client->agent {violation} in {line}", self.label);
                }
            }
        }
        eprintln!(
            "IS-SCHEMA-V{} {} agent_frames={} client_frames={} batch_lines={batch_lines} deliberate_error_probes={deliberate} null_id_error_answers={null_id_errors} violations={violations}",
            self.version,
            self.label,
            stdout.len(),
            stdin.len()
        );
        violations
    }

    fn assert_schema_clean(&self) {
        assert_eq!(self.schema_violations(), 0, "{}: schema violations", self.label);
    }
}

fn cwd_str(path: &Path) -> String {
    path.to_str().expect("a UTF-8 path").to_string()
}

fn work_dir(daemon: &AcpDaemon) -> PathBuf {
    let work = daemon.root.join("work");
    std::fs::create_dir_all(&work).expect("work dir");
    work
}

fn updates_of(notifications: &[Value], session_id: &str) -> Vec<Value> {
    notifications
        .iter()
        .filter(|note| note["method"] == "session/update" && note["params"]["sessionId"] == session_id)
        .map(|note| note["params"]["update"].clone())
        .collect()
}

fn kinds(updates: &[Value]) -> Vec<String> {
    updates
        .iter()
        .filter_map(|update| update["sessionUpdate"].as_str().map(str::to_string))
        .collect()
}

fn of_kind<'a>(updates: &'a [Value], kind: &str) -> Vec<&'a Value> {
    updates
        .iter()
        .filter(|update| update["sessionUpdate"] == kind)
        .collect()
}

fn chunk_text(updates: &[Value], kind: &str) -> String {
    of_kind(updates, kind)
        .iter()
        .filter_map(|update| update["content"]["text"].as_str())
        .collect()
}

fn mentions(values: &[Value], needle: &str) -> bool {
    values.iter().any(|value| value.to_string().contains(needle))
}

fn tool_turn(name: &str, arguments: Value, follow_up: &str) -> Script {
    Script::ToolCall {
        name: name.to_string(),
        arguments_json: arguments.to_string(),
        follow_up: follow_up.to_string(),
    }
}

static TOOL_CALL_INDEX: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// One function call as the Chat Completions stream announces it.
fn function_call_frames(call_id: &str, name: &str, arguments: &Value) -> Vec<Value> {
    let index = TOOL_CALL_INDEX.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let arguments = arguments.to_string();
    vec![
        lifecycle::chat_chunk(
            json!({"role": "assistant", "tool_calls": [{
                "index": index, "id": call_id, "type": "function",
                "function": {"name": name, "arguments": ""}}]}),
            None,
        ),
        lifecycle::chat_chunk(
            json!({"tool_calls": [{"index": index, "function": {"arguments": arguments}}]}),
            None,
        ),
    ]
}

fn completed() -> Value {
    lifecycle::chat_chunk(json!({}), Some("tool_calls"))
}

/// The provider request body that contains every needle.
fn provider_body(daemon: &AcpDaemon, needles: &[&str]) -> String {
    daemon
        .provider
        .requests()
        .into_iter()
        .find(|body| needles.iter().all(|needle| body.contains(needle)))
        .unwrap_or_else(|| panic!("no provider request contains all of {needles:?}\n{}", daemon.logs()))
}

/// The `content` of the tool message answering the `tool` call, from the
/// provider request whose messages mention `needle`. The request also replays
/// the call's own arguments, so only the tool message says what the tool
/// returned.
fn function_call_output(daemon: &AcpDaemon, tool: &str, needle: &str) -> String {
    let body = provider_body(daemon, &["tool_call_id", needle]);
    let request: Value = serde_json::from_str(&body).expect("a JSON provider request");
    let messages: &[Value] = request["messages"].as_array().expect("the request messages");
    let call_id = messages
        .iter()
        .filter(|message| message["role"] == "assistant")
        .flat_map(|message| message["tool_calls"].as_array().into_iter().flatten())
        .find(|call| call["function"]["name"] == tool)
        .and_then(|call| call["id"].as_str())
        .unwrap_or_else(|| panic!("the {tool} call in the request: {body}"));
    let output = messages
        .iter()
        .find(|message| message["role"] == "tool" && message["tool_call_id"] == call_id)
        .unwrap_or_else(|| panic!("the {tool} call's output in the request: {body}"));
    match &output["content"] {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The index of the first stdout line that satisfies `matches`.
fn line_index(lines: &[String], what: &str, matches: impl Fn(&Value) -> bool) -> usize {
    lines
        .iter()
        .position(|line| serde_json::from_str::<Value>(line).is_ok_and(|value| matches(&value)))
        .unwrap_or_else(|| panic!("no stdout line is {what}: {lines:#?}"))
}

fn is_response_to(frame: &Value, id: &Value) -> bool {
    frame.get("method").is_none() && frame.get("id") == Some(id)
}

/// The decoded output of every final `terminal_update` in `updates`.
fn terminal_output(updates: &[Value]) -> String {
    of_kind(updates, "terminal_update")
        .iter()
        .filter(|update| update.get("exitStatus").is_some())
        .filter_map(|update| update["output"]["data"].as_str())
        .map(decode_base64)
        .collect()
}

/// Decodes standard-alphabet, padded base64. The root crate has no base64
/// dependency and this suite adds none (no Cargo.toml change).
fn decode_base64(data: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let digits = data.trim_end_matches('=');
    assert_eq!(data.len() % 4, 0, "padded base64: {data}");
    let mut bits: u32 = 0;
    let mut count = 0;
    let mut bytes = Vec::new();
    for byte in digits.bytes() {
        let value = ALPHABET
            .iter()
            .position(|&digit| digit == byte)
            .unwrap_or_else(|| panic!("not standard base64: {data}")) as u32;
        bits = (bits << 6) | value;
        count += 6;
        if count >= 8 {
            count -= 8;
            bytes.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    String::from_utf8(bytes).expect("terminal output is UTF-8")
}

/// Dumps the daemon's logs when the test panics, before the daemon guard
/// removes its scratch root.
struct LogDump<'a>(&'a AcpDaemon);

impl Drop for LogDump<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            dump_logs(&self.0.root, &self.0.home);
        }
    }
}

fn dump_logs(root: &Path, home: &Path) {
    let mut logs: Vec<PathBuf> = std::fs::read_dir(home.join("logs"))
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    logs.sort();
    for log in logs {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        let lines: Vec<&str> = text.lines().filter(|line| !line.contains("TIMING")).collect();
        let start = lines.len().saturating_sub(80);
        eprintln!("--- {} ---\n{}", log.display(), lines[start..].join("\n"));
    }
    let daemon_log = std::fs::read_to_string(root.join("rt/daemon.log")).unwrap_or_default();
    let tail: Vec<&str> = daemon_log.lines().rev().take(40).collect();
    eprintln!("--- daemon.log (newest first) ---\n{}", tail.join("\n"));
}

/// A plain daemon client (no ACP adapter): writes daemon requests without
/// waiting, and collects every event it receives.
struct Peer {
    writer: UnixStream,
    lines: Receiver<Value>,
    session_id: String,
    events: Vec<Value>,
    next_id: u64,
}

impl Peer {
    /// `acp_v2` subscribes with `acp.protocol_version = 2`, which is the only
    /// way the daemon reports stored message ids in History.
    fn connect(socket: &Path, working_dir: &Path, target_session_id: Option<&str>, acp_v2: bool) -> Self {
        let writer = UnixStream::connect(socket).expect("connect to the daemon");
        let reader = writer.try_clone().expect("clone the daemon socket");
        let (sender, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(reader).lines() {
                let Ok(line) = line else { break };
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if sender.send(value).is_err() {
                    break;
                }
            }
        });
        let mut peer = Self {
            writer,
            lines,
            session_id: String::new(),
            events: Vec::new(),
            next_id: 1,
        };
        let mut subscribe = json!({"type": "subscribe", "id": 1, "working_dir": cwd_str(working_dir)});
        if let Some(target) = target_session_id {
            subscribe["target_session_id"] = json!(target);
        }
        if acp_v2 {
            subscribe["acp"] = json!({"protocol_version": 2});
        }
        peer.write(subscribe);
        let start = peer.events.len();
        peer.await_event("the subscribe done", |event| {
            event["type"] == "done" && event["id"] == 1
        });
        peer.session_id = peer.events[start..]
            .iter()
            .rev()
            .find_map(|event| match event["type"].as_str() {
                Some("session") | Some("history") => event["session_id"].as_str().map(str::to_string),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the subscribe names its session: {:?}", peer.events));
        peer
    }

    fn write(&mut self, request: Value) -> u64 {
        let id = request["id"].as_u64().expect("a request id");
        self.next_id = self.next_id.max(id + 1);
        writeln!(self.writer, "{request}").expect("write to the daemon");
        self.writer.flush().expect("flush to the daemon");
        id
    }

    /// Writes a `message` request and returns its id without waiting.
    fn send_message(&mut self, content: &str) -> u64 {
        let id = self.next_id;
        self.write(json!({"type": "message", "id": id, "content": content}))
    }

    /// Writes `request` and waits for its `done`.
    fn request(&mut self, mut request: Value) -> Vec<Value> {
        let id = self.next_id;
        request["id"] = json!(id);
        self.write(request);
        let start = self.events.len();
        self.await_event("the request's done", |event| {
            event["id"] == id && matches!(event["type"].as_str(), Some("done") | Some("error"))
        });
        let events = self.events[start..].to_vec();
        assert!(
            events.last().is_some_and(|event| event["type"] == "done"),
            "the daemon refused request {id}: {events:?}"
        );
        events
    }

    /// Waits for (and returns) the first event received from now on, or
    /// already received since the last wait, that satisfies `matches`.
    fn await_event(&mut self, what: &str, matches: impl Fn(&Value) -> bool) -> Value {
        let end = Instant::now() + WAIT;
        loop {
            let remaining = end.saturating_duration_since(Instant::now());
            let event = match self.lines.recv_timeout(remaining) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => panic!("no {what} within {WAIT:?}: {:?}", self.events),
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("the daemon closed the peer connection: {:?}", self.events)
                }
            };
            self.events.push(event.clone());
            if matches(&event) {
                return event;
            }
        }
    }
}

/// `CREDENTIAL_FILES` of crates/agent-sdk/src/launch.rs: the stores the
/// product keeps directly under HORON_AGENT_HOME. The list is private there, so
/// it is copied and `credential_lists_match_the_shipped_code` fails the moment
/// the two drift apart.
const SHIPPED_HOME_CREDENTIAL_FILES: [&str; 7] = [
    "auth.json",
    "openai-auth.json",
    "antigravity_oauth.json",
    "gemini_oauth.json",
    "google_oauth.json",
    "google_credentials.json",
    "config.toml",
];

/// Further stores under HORON_AGENT_HOME that are not in launch.rs: the MCP
/// OAuth store (`STORE_FILE` of crates/base/src/mcp/oauth.rs).
const EXTRA_HOME_CREDENTIAL_FILES: [&str; 1] = ["mcp-oauth.json"];

/// `EXTERNAL_CREDENTIAL_FILES` of crates/agent-sdk/src/launch.rs, rooted at
/// HOME (the scratch root), copied for the same reason.
const SHIPPED_EXTERNAL_CREDENTIAL_FILES: [&str; 20] = [
    ".claude/.credentials.json",
    ".codex/auth.json",
    ".gemini/oauth_creds.json",
    ".cursor/auth.json",
    ".config/cursor/auth.json",
    "AppData/Roaming/Cursor/auth.json",
    ".config/Cursor/User/globalStorage/state.vscdb",
    ".config/cursor/User/globalStorage/state.vscdb",
    "Library/Application Support/Cursor/User/globalStorage/state.vscdb",
    "Library/Application Support/cursor/User/globalStorage/state.vscdb",
    "AppData/Roaming/Cursor/User/globalStorage/state.vscdb",
    "AppData/Roaming/cursor/User/globalStorage/state.vscdb",
    ".config/github-copilot/hosts.json",
    ".config/github-copilot/apps.json",
    ".copilot/config.json",
    ".hermes/auth.json",
    ".pi/agent/auth.json",
    ".openclaw/agent/auth.json",
    ".openclaw/credentials/oauth.json",
    ".local/share/opencode/auth.json",
];

/// The per-agent OpenClaw store names launch.rs reads one agent-id level deep:
/// `$HOME/.openclaw/agents/<id>/agent/<name>`.
const OPENCLAW_AGENT_CREDENTIAL_FILES: [&str; 2] = ["auth-profiles.json", "auth.json"];

/// Every credential location the product reads or writes for a daemon whose
/// HORON_AGENT_HOME is `home`, HOME is `user_home` and XDG_CONFIG_HOME is
/// `xdg_config`, with its bytes or `None` when absent, so a credential that
/// appears, disappears or changes alters the snapshot. Two copies of each
/// external store and each provider env file are watched:
/// - where the daemon itself resolves them under HORON_AGENT_HOME, which the
///   test daemon always sets: `crate::storage::user_home_path(rel)` is
///   `$HORON_AGENT_HOME/external/<rel>` and `app_config_dir()` is
///   `$HORON_AGENT_HOME/config/horon-agent` (crates/storage/src/lib.rs), which
///   are also the instance paths launch.rs links the stores to;
/// - where launch.rs reads them from the user's HOME: `$HOME/<rel>`, and
///   `user_app_config_dir()`, which is `$HOME/Library/Application Support/horon-agent`
///   on macOS and `$XDG_CONFIG_HOME/horon-agent` on other unix systems.
///
/// For each root: the 20 external stores, the per-agent OpenClaw stores one
/// agent-id level deep, and every `*.env` provider key file. Under `home`
/// also: the launch.rs `CREDENTIAL_FILES`, the MCP OAuth store and every
/// `*.env` provider key file.
///
/// The set is closed: runtime state such as `active_pids/`, session files or
/// the model catalog caches in the config dir never enters it, whatever its name.
fn credential_files(home: &Path, user_home: &Path, xdg_config: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn external_stores(root: &Path, out: &mut Vec<PathBuf>) {
        out.extend(SHIPPED_EXTERNAL_CREDENTIAL_FILES.iter().map(|name| root.join(name)));
        for agent in std::fs::read_dir(root.join(".openclaw/agents"))
            .into_iter()
            .flatten()
            .flatten()
        {
            if agent.path().is_dir() {
                for name in OPENCLAW_AGENT_CREDENTIAL_FILES {
                    out.push(agent.path().join("agent").join(name));
                }
            }
        }
    }
    fn env_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if entry.file_name().to_string_lossy().ends_with(".env") {
                out.push(entry.path());
            }
        }
    }
    let mut paths: Vec<PathBuf> = SHIPPED_HOME_CREDENTIAL_FILES
        .iter()
        .chain(EXTRA_HOME_CREDENTIAL_FILES.iter())
        .map(|name| home.join(name))
        .collect();
    external_stores(&home.join("external"), &mut paths);
    env_files(&home.join("config").join("horon-agent"), &mut paths);
    external_stores(user_home, &mut paths);
    let user_app_config = if cfg!(target_os = "macos") {
        user_home.join("Library/Application Support/horon-agent")
    } else {
        xdg_config.join("horon-agent")
    };
    env_files(&user_app_config, &mut paths);
    paths
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(&path).ok();
            (path, bytes)
        })
        .collect()
}

/// [`credential_files`] for one test daemon.
fn daemon_credentials(daemon: &AcpDaemon) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    credential_files(&daemon.home, &daemon.root, &daemon.root.join("config"))
}

/// IS-1: per-request negotiation. 2 selects v2, 1 the v1 surface, 0 is
/// -32022 naming both supported versions and leaves the connection
/// uninitialized, a missing protocolVersion is -32602, and a repeated
/// initialize after another request is -32600.
#[test]
fn is1_negotiation() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);

    let mut v2 = Conn::v2(&daemon, "is1-v2");
    let result = v2.initialize();
    assert_eq!(result["protocolVersion"], 2, "{result}");
    v2.new_session(&work);
    let repeat = v2.client.initialize_params();
    let late = v2.err("initialize", repeat);
    assert_eq!(late["code"], -32600, "a repeat after session/new: {late}");
    v2.assert_schema_clean();

    let mut v1 = Conn::v1(&daemon, "is1-v1");
    let result = v1.initialize();
    assert_eq!(result["protocolVersion"], 1, "{result}");
    assert!(result["agentCapabilities"].is_object(), "the v1 surface: {result}");
    v1.assert_schema_clean();

    let mut probe = Conn::v2(&daemon, "is1-probe");
    let mut zero = probe.client.initialize_params();
    zero["protocolVersion"] = json!(0);
    let unsupported = probe.err("initialize", zero);
    assert_eq!(unsupported["code"], -32022, "{unsupported}");
    assert_eq!(
        unsupported["data"],
        json!({"requested": 0, "supported": [1, 2]}),
        "{unsupported}"
    );
    let early = probe.err("session/list", json!({}));
    assert_eq!(early["code"], -32600, "0 leaves the connection uninitialized: {early}");
    let mut missing = probe.client.initialize_params();
    missing.as_object_mut().expect("params").remove("protocolVersion");
    let invalid = probe.err_invalid("initialize", missing);
    assert_eq!(invalid["code"], -32602, "missing protocolVersion: {invalid}");
    assert_eq!(probe.initialize()["protocolVersion"], 2);
    probe.assert_schema_clean();
}

/// IS-2: the v2 initialize result: `info`, object capability markers, no
/// `loadSession`, and one auth method even for a client that advertised
/// `capabilities.auth.terminal`.
#[test]
fn is2_initialize_shape() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);

    let mut plain = Conn::v2(&daemon, "is2a");
    let result = plain.initialize();
    assert_eq!(result["info"]["name"], "horon-agent", "{result}");
    assert_eq!(result["info"]["title"], "HoronAgent", "{result}");
    assert!(result["info"]["version"].is_string(), "{result}");
    assert_eq!(
        result["capabilities"]["session"]["prompt"]["image"],
        json!({}),
        "{result}"
    );
    assert_eq!(
        result["capabilities"]["session"]["prompt"]["embeddedContext"],
        json!({}),
        "{result}"
    );
    assert!(!result.to_string().contains("loadSession"), "{result}");
    assert!(result.get("agentCapabilities").is_none(), "{result}");
    let methods: Vec<&str> = result["authMethods"]
        .as_array()
        .expect("authMethods")
        .iter()
        .filter_map(|method| method["methodId"].as_str())
        .collect();
    assert_eq!(methods, ["horon-agent-credentials"], "{result}");
    plain.assert_schema_clean();

    let mut terminal = Conn::v2_with(&daemon, ClientCapabilities::default(), true, "is2b");
    let result = terminal.initialize();
    let methods = result["authMethods"].as_array().expect("authMethods");
    assert_eq!(methods.len(), 1, "no client capability adds a method: {result}");
    let ids: Vec<&str> = methods
        .iter()
        .filter_map(|method| method["methodId"].as_str())
        .collect();
    assert_eq!(ids, ["horon-agent-credentials"], "{result}");
    assert!(methods[0].get("args").is_none(), "{result}");
    terminal.assert_schema_clean();
}

/// IS-3: the v2 method surface: v1-only methods are -32601, a `null` id is
/// -32600 answered with `id: null`, a batch of requests is answered by one
/// array line holding both answers, and a batch of notifications is never
/// answered.
#[test]
fn is3_method_surface() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    let mut conn = Conn::v2(&daemon, "is3");
    conn.initialize();
    let session = conn.new_session(&work);

    let load = conn.err_invalid(
        "session/load",
        json!({"sessionId": session, "cwd": cwd_str(&work), "mcpServers": []}),
    );
    assert_eq!(load["code"], -32601, "{load}");
    let set_mode = conn.err_invalid("session/set_mode", json!({"sessionId": session, "modeId": "auto"}));
    assert_eq!(set_mode["code"], -32601, "{set_mode}");
    let authenticate = conn.err_invalid("authenticate", json!({"methodId": "horon-agent-credentials"}));
    assert_eq!(authenticate["code"], -32601, "{authenticate}");

    conn.write_invalid_line(r#"{"jsonrpc":"2.0","id":null,"method":"session/list","params":{}}"#);
    let null_id = conn.read_null_id_error();
    assert_eq!(null_id["code"], -32600, "{null_id}");

    let entries = [
        json!({"jsonrpc": "2.0", "id": "is3-b1", "method": "session/list", "params": {}}),
        json!({"jsonrpc": "2.0", "id": "is3-b2", "method": "session/list", "params": {"cwd": cwd_str(&work)}}),
    ];
    for entry in &entries {
        conn.sent.push((entry["id"].clone(), "session/list".to_string()));
    }
    let answers = conn
        .client
        .send_batch(&entries)
        .unwrap_or_else(|error| panic!("{error}"));
    let mut ids: Vec<&str> = answers.iter().filter_map(|answer| answer["id"].as_str()).collect();
    ids.sort();
    assert_eq!(ids, ["is3-b1", "is3-b2"], "one array line answers both: {answers:?}");
    assert!(
        answers.iter().all(|answer| answer["result"]["sessions"].is_array()),
        "{answers:?}"
    );

    conn.client
        .write_value(&json!([
            {"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session}},
            {"jsonrpc": "2.0", "method": "$/cancel_request", "params": {"requestId": "is3-nothing"}},
        ]))
        .unwrap_or_else(|error| panic!("{error}"));
    conn.barrier();
    conn.ok("session/list", json!({}));
    let arrays = conn
        .stdout()
        .iter()
        .filter(|line| line.trim_start().starts_with('['))
        .count();
    assert_eq!(
        arrays,
        1,
        "a notifications-only batch writes nothing: {:#?}",
        conn.stdout()
    );
    conn.assert_schema_clean();
}

/// IS-4: a v2 prompt is answered `{messageId}` before the first agent chunk
/// of its turn, and a prompt refused before insertion (the session is busy)
/// is -32602 and writes no update carrying its content.
#[test]
fn is4_prompt_lifecycle() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    daemon
        .provider
        .script("IS4_TEXT", Script::Text("is4 reply text".to_string()));
    daemon
        .provider
        .script("IS4_HOLD", Script::HoldOpen("is4 partial ".to_string()));
    let mut conn = Conn::v2(&daemon, "is4");
    conn.initialize();
    let session = conn.new_session(&work);

    let cursor = conn.cursor(&session);
    let prompt_id = conn.send_prompt(&session, "IS4_TEXT hello");
    let answer = conn.wait("session/prompt", &prompt_id).expect("the prompt answers");
    let message_id = answer["messageId"].as_str().expect("a messageId").to_string();
    assert_eq!(conn.await_idle_after(&session, cursor), "end_turn");
    let lines = conn.stdout();
    let answered = line_index(&lines, "the prompt response", |frame| is_response_to(frame, &prompt_id));
    let first_chunk = line_index(&lines, "the first agent chunk", |frame| {
        frame["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
    });
    assert!(
        answered < first_chunk,
        "the messageId answer ({answered}) precedes the first agent chunk ({first_chunk})"
    );
    let updates = updates_of(&conn.take_notes(), &session);
    assert!(
        chunk_text(&updates, "agent_message_chunk").contains("is4 reply text"),
        "{updates:?}"
    );
    let user = of_kind(&updates, "user_message");
    assert_eq!(user.len(), 1, "{updates:?}");
    assert_eq!(user[0]["messageId"], message_id.as_str(), "{updates:?}");

    let hold_id = conn.send_prompt(&session, "IS4_HOLD stream");
    conn.wait("session/prompt", &hold_id)
        .expect("the held prompt is inserted");
    daemon
        .provider
        .wait_for_hold_open_opened(WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", daemon.logs()));
    let busy = conn.err(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "IS4_BUSY_CONTENT second"}]}),
    );
    assert_eq!(
        busy["code"], -32602,
        "a busy prompt is refused before insertion: {busy}"
    );
    let cursor = conn.cursor(&session);
    conn.notify("session/cancel", json!({"sessionId": session}));
    assert_eq!(conn.await_idle_after(&session, cursor), "cancelled");
    conn.barrier();
    assert!(
        conn.stdout().iter().all(|line| !line.contains("IS4_BUSY_CONTENT")),
        "a refused prompt writes no update"
    );
    conn.assert_schema_clean();
}

/// IS-5: the foreground state of every turn in the session: the editor's own
/// turn, a turn a plain client on the same daemon session starts (with its
/// user message), an agent-initiated turn (no user message), and a close
/// while a prompt waits for insertion behind a running turn: that prompt is
/// -32800 and the idle `cancelled` line precedes the close response.
#[test]
fn is5_foreground_state() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    daemon
        .provider
        .script("IS5_OWN", Script::Text("is5 own reply".to_string()));
    daemon
        .provider
        .script("IS5_PEER", Script::Text("is5 peer reply".to_string()));
    daemon
        .provider
        .script("IS5_HOLD", Script::HoldOpen("is5 partial ".to_string()));
    let mut conn = Conn::v2(&daemon, "is5");
    conn.initialize();
    let session = conn.new_session(&work);

    let cursor = conn.cursor(&session);
    let (_, updates, stop) = conn.turn(&session, "IS5_OWN go");
    assert_eq!(stop, "end_turn");
    assert_eq!(
        conn.states_since(&session, cursor),
        ["running", "idle:end_turn"],
        "{updates:?}"
    );

    let mut peer = Peer::connect(&daemon.socket, &work, Some(&session), false);
    assert_eq!(
        peer.session_id, session,
        "the plain client attached to the editor's session"
    );
    conn.take_notes();
    let cursor = conn.cursor(&session);
    let peer_id = peer.send_message("IS5_PEER from the plain client");
    assert_eq!(conn.await_idle_after(&session, cursor), "end_turn");
    peer.await_event("the peer turn's done", |event| {
        event["type"] == "done" && event["id"] == peer_id
    });
    assert_eq!(conn.states_since(&session, cursor), ["running", "idle:end_turn"]);
    let updates = updates_of(&conn.take_notes(), &session);
    let user = of_kind(&updates, "user_message");
    assert_eq!(user.len(), 1, "the peer turn's user message: {updates:?}");
    assert_eq!(
        user[0]["content"],
        json!([{"type": "text", "text": "IS5_PEER from the plain client"}]),
        "{updates:?}"
    );
    assert!(user[0]["messageId"].is_string(), "{updates:?}");
    assert!(
        chunk_text(&updates, "agent_message_chunk").contains("is5 peer reply"),
        "{updates:?}"
    );

    let cursor = conn.cursor(&session);
    peer.request(json!({"type": "notify_session", "session_id": session, "message": "IS5_WAKE scheduled wake"}));
    let stop = conn.await_idle_after(&session, cursor);
    assert_eq!(
        conn.states_since(&session, cursor)[0],
        "running",
        "the wake turn starts running"
    );
    assert!(
        conn.states_since(&session, cursor)
            .last()
            .is_some_and(|state| state.starts_with("idle"))
    );
    let updates = updates_of(&conn.take_notes(), &session);
    assert!(
        of_kind(&updates, "user_message").is_empty(),
        "an agent-initiated turn has no user message (stop {stop:?}): {updates:?}"
    );

    let cursor = conn.cursor(&session);
    peer.send_message("IS5_HOLD the peer holds the turn");
    daemon
        .provider
        .wait_for_hold_open_opened(WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", daemon.logs()));
    conn.await_update(&session, "the peer turn running", |update| {
        update["sessionUpdate"] == "state_update" && update["state"] == "running"
    });
    let queued = conn.send_prompt(&session, "IS5_QUEUED waits for the turn");
    peer.await_event("the editor's message reaching the daemon", |event| {
        event["type"] == "peer_user_message"
            && event["content"]
                .as_str()
                .is_some_and(|text| text.contains("IS5_QUEUED"))
    });
    let close = conn.send("session/close", json!({"sessionId": session}));
    let refused = conn
        .wait("session/prompt", &queued)
        .expect_err("the queued prompt is withdrawn");
    assert_eq!(refused["code"], -32800, "{refused}");
    assert_eq!(conn.wait("session/close", &close), Ok(json!({})));
    assert_eq!(
        conn.client.state().idle_since(&session, cursor).as_deref(),
        Some("cancelled")
    );
    let lines = conn.stdout();
    let idle = line_index(&lines, "the idle cancelled update", |frame| {
        frame["params"]["sessionId"] == session.as_str()
            && frame["params"]["update"]
                == json!({"sessionUpdate": "state_update", "state": "idle", "stopReason": "cancelled"})
    });
    let closed = line_index(&lines, "the close response", |frame| is_response_to(frame, &close));
    assert!(
        idle < closed,
        "idle cancelled ({idle}) precedes the close response ({closed})"
    );
    assert!(
        lines.iter().all(|line| !line.contains("IS5_QUEUED")),
        "the withdrawn prompt writes no update"
    );
    conn.assert_schema_clean();
}

/// IS-5 across back-to-back turns of two surfaces (todo 15): a plain client's
/// turn holds the agent while this editor's prompt waits for it. The editor
/// sees that turn end (`idle`) before its own prompt is inserted and its turn
/// starts (`running`), and every update of its own turn between the two.
#[test]
fn is5_back_to_back_turns_of_two_surfaces() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    daemon
        .provider
        .script("T15_OWN", Script::Text("t15 own reply".to_string()));
    let mut conn = Conn::v2(&daemon, "t15");
    conn.initialize();
    let session = conn.new_session(&work);
    let mut peer = Peer::connect(&daemon.socket, &work, Some(&session), false);
    conn.take_notes();
    let cursor = conn.cursor(&session);

    daemon.provider.hold_turn();
    let peer_id = peer.send_message(lifecycle::HOLD_TURN_PROMPT);
    daemon
        .provider
        .wait_for_request(lifecycle::HOLD_TURN_PROMPT, WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", daemon.logs()));
    conn.await_update(&session, "the peer turn running", |update| {
        update["sessionUpdate"] == "state_update" && update["state"] == "running"
    });
    let queued = conn.send_prompt(&session, "T15_OWN after the peer turn");
    peer.await_event("the editor's message reaching the daemon", |event| {
        event["type"] == "peer_user_message" && event["content"].as_str().is_some_and(|text| text.contains("T15_OWN"))
    });
    daemon.provider.release_turn();
    peer.await_event("the peer turn's done", |event| {
        event["type"] == "done" && event["id"] == peer_id
    });
    let answered = conn
        .wait("session/prompt", &queued)
        .unwrap_or_else(|error| panic!("the queued prompt is inserted: {error}"));
    let message_id = answered["messageId"].as_str().expect("a messageId").to_string();
    conn.await_update(&session, "the editor turn's reply", |update| {
        update["sessionUpdate"] == "agent_message_chunk"
            && update["content"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("t15 own reply"))
    });
    let after_reply = conn.cursor(&session);
    assert_eq!(conn.await_idle_after(&session, after_reply), "end_turn");

    assert_eq!(
        conn.states_since(&session, cursor),
        ["running", "idle:end_turn", "running", "idle:end_turn"],
        "one running and one idle per turn, in turn order"
    );
    let updates = updates_of(&conn.take_notes(), &session);
    let position = |what: &str, matches: &dyn Fn(&Value) -> bool| {
        updates
            .iter()
            .position(matches)
            .unwrap_or_else(|| panic!("no {what} in {updates:?}"))
    };
    let is_state = |update: &Value, state: &str| update["sessionUpdate"] == "state_update" && update["state"] == state;
    let chunk_with = |update: &Value, text: &str| {
        update["sessionUpdate"] == "agent_message_chunk"
            && update["content"]["text"]
                .as_str()
                .is_some_and(|chunk| chunk.contains(text))
    };
    let peer_reply = position("peer reply", &|update| chunk_with(update, lifecycle::PLAIN_TEXT));
    let peer_idle = position("peer idle", &|update| is_state(update, "idle"));
    let own_user = position("own user message", &|update| {
        update["sessionUpdate"] == "user_message" && update["messageId"] == message_id.as_str()
    });
    let own_running = peer_idle
        + 1
        + updates[peer_idle + 1..]
            .iter()
            .position(|update| is_state(update, "running"))
            .unwrap_or_else(|| panic!("no running after the peer idle in {updates:?}"));
    let own_reply = position("own reply", &|update| chunk_with(update, "t15 own reply"));
    assert!(
        peer_reply < peer_idle && peer_idle < own_user && own_user < own_running && own_running < own_reply,
        "peer reply {peer_reply} < peer idle {peer_idle} < own user message {own_user} < own running \
         {own_running} < own reply {own_reply}: {updates:?}"
    );
    conn.assert_schema_clean();
}

/// IS-5 cancel clause across surfaces (todo 16): this editor's turn runs
/// while a second v2 connection's prompt waits for the agent. After
/// `session/cancel` the editor gets `idle` with `stopReason: cancelled` for
/// its own turn before the waiting turn reports `running`.
#[test]
fn is5_cancelled_turn_idles_before_the_queued_peer_turn() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    daemon
        .provider
        .script("T16_HOLD", Script::HoldOpen("t16 partial ".to_string()));
    daemon
        .provider
        .script("T16_PEER", Script::Text("t16 peer reply".to_string()));
    let mut conn = Conn::v2(&daemon, "t16");
    conn.initialize();
    let session = conn.new_session(&work);
    let mut peer = Peer::connect(&daemon.socket, &work, Some(&session), true);
    // A withdraw reply travels the peer's queue behind every earlier request,
    // so once it arrives the daemon has finished with them.
    let peer_barrier = |peer: &mut Peer| {
        let id = peer.next_id;
        peer.write(json!({"type": "withdraw_message", "id": id, "message_request_id": 9_999_999}));
        peer.await_event("the peer barrier", |event| {
            event["type"] == "error" && event["id"] == id
        });
    };
    // The subscribe finishes on the daemon before the own turn takes the agent.
    peer_barrier(&mut peer);
    conn.take_notes();
    let cursor = conn.cursor(&session);

    let own = conn.send_prompt(&session, "T16_HOLD the editor holds the turn");
    conn.wait("session/prompt", &own)
        .unwrap_or_else(|error| panic!("the own prompt is inserted: {error}"));
    daemon
        .provider
        .wait_for_hold_open_opened(WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", daemon.logs()));
    conn.await_update(&session, "the own turn running", |update| {
        update["sessionUpdate"] == "state_update" && update["state"] == "running"
    });
    let peer_id = peer.send_message("T16_PEER waits for the agent");
    // The peer's turn now waits for the agent.
    peer_barrier(&mut peer);

    conn.notify("session/cancel", json!({"sessionId": session}));
    peer.await_event("the peer turn's done", |event| {
        event["type"] == "done" && event["id"] == peer_id
    });
    conn.await_update(&session, "the peer turn's reply", |update| {
        update["sessionUpdate"] == "agent_message_chunk"
            && update["content"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("t16 peer reply"))
    });
    let after_reply = conn.cursor(&session);
    assert_eq!(conn.await_idle_after(&session, after_reply), "end_turn");
    assert_eq!(
        conn.states_since(&session, cursor),
        ["running", "idle:cancelled", "running", "idle:end_turn"],
        "the cancelled turn ends with idle cancelled before the peer turn runs"
    );
    let updates = updates_of(&conn.take_notes(), &session);
    let cancelled = updates
        .iter()
        .position(|update| {
            *update == json!({"sessionUpdate": "state_update", "state": "idle", "stopReason": "cancelled"})
        })
        .unwrap_or_else(|| panic!("no idle cancelled in {updates:?}"));
    let peer_running = updates
        .iter()
        .rposition(|update| update["sessionUpdate"] == "state_update" && update["state"] == "running")
        .unwrap_or_else(|| panic!("no peer running in {updates:?}"));
    assert!(
        cancelled < peer_running,
        "idle cancelled {cancelled} precedes the peer turn's running {peer_running}: {updates:?}"
    );
    conn.assert_schema_clean();
}

/// IS-6: every chunk carries a messageId; the user message has the prompt's
/// id, and the agent ids are `<messageId>:a<n>` with `n` advancing by one
/// after the tool round.
#[test]
fn is6_message_ids() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    let fixture = work.join("is6.txt");
    std::fs::write(&fixture, "is6 fixture\n").expect("fixture");
    let mut frames = vec![lifecycle::chat_chunk(
        json!({"role": "assistant", "content": "is6 first round "}),
        None,
    )];
    frames.extend(function_call_frames(
        "call_is6_read",
        "read",
        &json!({"file_path": cwd_str(&fixture)}),
    ));
    frames.push(completed());
    daemon.provider.script(
        "IS6_IDS",
        Script::Frames {
            frames,
            follow_up: ("call_is6_read".to_string(), "is6 second round".to_string()),
        },
    );
    let mut conn = Conn::v2(&daemon, "is6");
    conn.initialize();
    let session = conn.new_session(&work);
    let (message_id, updates, stop) = conn.turn(&session, "IS6_IDS go");
    assert_eq!(stop, "end_turn", "{updates:?}");
    let user = of_kind(&updates, "user_message");
    assert_eq!(user.len(), 1, "{updates:?}");
    assert_eq!(user[0]["messageId"], message_id.as_str());
    let chunks: Vec<&Value> = updates
        .iter()
        .filter(|update| {
            update["sessionUpdate"]
                .as_str()
                .is_some_and(|kind| kind.ends_with("_message_chunk") || kind == "agent_thought_chunk")
        })
        .collect();
    assert!(!chunks.is_empty(), "{updates:?}");
    assert!(chunks.iter().all(|chunk| chunk["messageId"].is_string()), "{chunks:?}");
    let id_of = |needle: &str| {
        chunks
            .iter()
            .find(|chunk| {
                chunk["content"]["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(needle))
            })
            .unwrap_or_else(|| panic!("a chunk with {needle:?}: {chunks:?}"))["messageId"]
            .as_str()
            .expect("a messageId")
            .to_string()
    };
    let first = id_of("is6 first round");
    let second = id_of("is6 second round");
    assert_eq!(first, format!("{message_id}:a1"), "{chunks:?}");
    assert_eq!(
        second,
        format!("{message_id}:a2"),
        "the id advances after the tool round: {chunks:?}"
    );
    conn.assert_schema_clean();
}

/// IS-7: no `tool_call` variant; the first `tool_call_update` creates the
/// call (pending, no rawInput) and a later patch carries rawInput and an
/// absolute location.
#[test]
fn is7_tool_calls() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    let fixture = work.join("is7.txt");
    std::fs::write(&fixture, "is7 fixture text\n").expect("fixture");
    daemon.provider.script(
        "IS7_READ",
        tool_turn("read", json!({"file_path": cwd_str(&fixture)}), "is7 read done"),
    );
    let mut conn = Conn::v2(&daemon, "is7");
    conn.initialize();
    let session = conn.new_session(&work);
    let (_, updates, stop) = conn.turn(&session, "IS7_READ read it");
    assert_eq!(stop, "end_turn");
    assert!(!kinds(&updates).iter().any(|kind| kind == "tool_call"), "{updates:?}");
    let calls = of_kind(&updates, "tool_call_update");
    let first = calls
        .first()
        .copied()
        .unwrap_or_else(|| panic!("a tool update: {updates:?}"));
    let tool_call_id = first["toolCallId"].clone();
    assert_eq!(first["status"], "pending", "{first}");
    assert_eq!(first["name"], "read", "{first}");
    assert_eq!(first["kind"], "read", "{first}");
    assert!(
        first["title"].as_str().is_some_and(|title| !title.is_empty()),
        "{first}"
    );
    assert!(first.get("rawInput").is_none(), "{first}");
    let same: Vec<&&Value> = calls.iter().filter(|call| call["toolCallId"] == tool_call_id).collect();
    let patch = same
        .iter()
        .find(|call| call.get("rawInput").is_some())
        .unwrap_or_else(|| panic!("a patch with rawInput: {same:?}"));
    assert_eq!(patch["rawInput"]["file_path"], cwd_str(&fixture), "{patch}");
    assert_eq!(patch["locations"], json!([{"path": cwd_str(&fixture)}]), "{patch}");
    assert!(
        same.iter().any(|call| call["status"] == "completed"),
        "the call completes: {same:?}"
    );
    conn.assert_schema_clean();
}

/// IS-8: file edits arrive as v2 diff content: `changes` with absolute paths
/// plus a `git_patch`; edit is modify, a new file is add from `/dev/null`,
/// apply_patch delete is delete, and a move carries oldPath.
#[test]
fn is8_diffs() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    let edited = work.join("is8-edit.txt");
    std::fs::write(&edited, "is8 before\n").expect("fixture");
    let created = work.join("is8-new.txt");
    let deleted = work.join("is8-delete.txt");
    std::fs::write(&deleted, "is8 doomed\n").expect("fixture");
    let moved_from = work.join("is8-from.txt");
    std::fs::write(&moved_from, "is8 old line\n").expect("fixture");
    let moved_to = work.join("is8-to.txt");
    let mut frames = function_call_frames(
        "call_is8_edit",
        "edit",
        &json!({"file_path": cwd_str(&edited), "old_string": "is8 before", "new_string": "is8 after"}),
    );
    frames.extend(function_call_frames(
        "call_is8_write",
        "write",
        &json!({"file_path": cwd_str(&created), "content": "is8 created\n"}),
    ));
    frames.extend(function_call_frames(
        "call_is8_delete",
        "apply_patch",
        &json!({"patch_text": format!("*** Begin Patch\n*** Delete File: {}\n*** End Patch", cwd_str(&deleted))}),
    ));
    frames.extend(function_call_frames(
        "call_is8_move",
        "apply_patch",
        &json!({"patch_text": format!(
            "*** Begin Patch\n*** Update File: {}\n*** Move to: {}\n@@\n-is8 old line\n+is8 new line\n*** End Patch",
            cwd_str(&moved_from),
            cwd_str(&moved_to)
        )}),
    ));
    frames.push(completed());
    daemon.provider.script(
        "IS8_DIFFS",
        Script::Frames {
            frames,
            follow_up: ("call_is8_edit".to_string(), "is8 diffs done".to_string()),
        },
    );
    let mut conn = Conn::v2(&daemon, "is8");
    conn.initialize();
    let session = conn.new_session(&work);
    let (_, updates, stop) = conn.turn(&session, "IS8_DIFFS edit everything");
    assert_eq!(stop, "end_turn", "{}", daemon.logs());
    let diffs: Vec<&Value> = of_kind(&updates, "tool_call_content_chunk")
        .into_iter()
        .map(|chunk| &chunk["content"])
        .filter(|content| content["type"] == "diff")
        .collect();
    let diff_for = |operation: &str| {
        diffs
            .iter()
            .find(|diff| diff["changes"][0]["operation"] == operation)
            .copied()
            .unwrap_or_else(|| panic!("a {operation} diff: {diffs:?}\n{updates:?}"))
    };
    for diff in &diffs {
        assert_eq!(diff["patch"]["format"], "git_patch", "{diff}");
        assert!(diff.get("oldText").is_none() && diff.get("newText").is_none(), "{diff}");
    }
    let modify = diff_for("modify");
    assert_eq!(
        modify["changes"],
        json!([{"operation": "modify", "path": cwd_str(&edited), "fileType": "text"}])
    );
    let text = modify["patch"]["text"].as_str().expect("patch text");
    let header = format!("diff --git {} {}", cwd_str(&edited), cwd_str(&edited));
    assert!(text.starts_with(&header), "absolute paths, no a/ b/ prefixes: {text}");
    assert!(text.contains("-is8 before\n+is8 after\n"), "{text}");
    let add = diff_for("add");
    assert_eq!(add["changes"][0]["path"], cwd_str(&created), "{add}");
    let text = add["patch"]["text"].as_str().expect("patch text");
    assert!(
        text.contains("--- /dev/null\n") && text.contains(&format!("+++ {}\n", cwd_str(&created))),
        "{text}"
    );
    let delete = diff_for("delete");
    assert_eq!(delete["changes"][0]["path"], cwd_str(&deleted), "{delete}");
    assert!(
        delete["patch"]["text"]
            .as_str()
            .is_some_and(|text| text.contains("+++ /dev/null")),
        "{delete}"
    );
    let moved = diff_for("move");
    assert_eq!(moved["changes"][0]["oldPath"], cwd_str(&moved_from), "{moved}");
    assert_eq!(moved["changes"][0]["path"], cwd_str(&moved_to), "{moved}");
    assert!(!deleted.exists() && !moved_from.exists(), "the patch ran");
    assert_eq!(std::fs::read_to_string(&moved_to).expect("moved"), "is8 new line\n");
    conn.assert_schema_clean();
}

/// IS-9: a foreground bash call is an agent-owned display terminal: terminal
/// content, the command, then the complete output (base64) with its exit
/// status; never a `terminal/*` request to the client.
#[test]
fn is9_terminal() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    daemon.provider.script(
        "IS9_BASH",
        tool_turn(
            "bash",
            json!({"command": "echo hi", "intent": "is9 terminal fixture"}),
            "is9 done",
        ),
    );
    let mut conn = Conn::v2(&daemon, "is9");
    conn.initialize();
    let session = conn.new_session(&work);
    let (_, updates, stop) = conn.turn(&session, "IS9_BASH run it");
    assert_eq!(stop, "end_turn", "{}", daemon.logs());
    let content = of_kind(&updates, "tool_call_content_chunk")
        .into_iter()
        .find(|chunk| chunk["content"]["type"] == "terminal")
        .unwrap_or_else(|| panic!("terminal content: {updates:?}"));
    let terminal_id = content["content"]["terminalId"].clone();
    assert!(terminal_id.is_string(), "{content}");
    let terminal: Vec<&Value> = of_kind(&updates, "terminal_update")
        .into_iter()
        .filter(|update| update["terminalId"] == terminal_id)
        .collect();
    let started = terminal
        .iter()
        .find(|update| update.get("command").is_some())
        .unwrap_or_else(|| panic!("the command: {terminal:?}"));
    assert_eq!(started["command"], "echo hi", "{started}");
    assert_eq!(started["cwd"], cwd_str(&work), "{started}");
    let finished = terminal
        .iter()
        .find(|update| update.get("exitStatus").is_some())
        .unwrap_or_else(|| panic!("the exit: {terminal:?}"));
    assert_eq!(
        decode_base64(finished["output"]["data"].as_str().expect("output data")),
        "hi\n",
        "{finished}"
    );
    assert_eq!(finished["exitStatus"]["exitCode"], 0, "{finished}");
    assert!(
        conn.stdout()
            .iter()
            .all(|line| !line.contains("\"method\":\"terminal/")),
        "no terminal/* request on v2"
    );
    assert!(conn.client.handlers().terminal.calls().is_empty());
    conn.assert_schema_clean();
}

/// IS-10: in ask-commands, bash raises a v2 permission request with a title
/// and a command subject with the absolute cwd; the state goes
/// requires_action, then running again; an unknown outcome never runs it.
#[test]
fn is10_permission() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    let bash = |tag: &str, product: &str| {
        tool_turn(
            "bash",
            json!({"command": format!("echo is10-$(({product}))-{tag}"), "intent": "is10 permission fixture"}),
            &format!("is10 {tag} done"),
        )
    };
    daemon.provider.script("IS10_ALLOW", bash("allow", "6*7"));
    daemon.provider.script("IS10_CUSTOM", bash("custom", "5*5"));
    let mut conn = Conn::v2(&daemon, "is10");
    conn.initialize();
    let ask_commands = |conn: &mut Conn, session: &str| {
        let set = conn.ok(
            "session/set_config_option",
            json!({"sessionId": session, "configId": "mode", "type": "id", "value": "ask-commands"}),
        );
        assert_eq!(set["configOptions"][0]["currentValue"], "ask-commands", "{set}");
    };

    let session = conn.new_session(&work);
    ask_commands(&mut conn, &session);
    conn.client
        .handlers()
        .permission
        .script([PermissionOutcome::Selected("allow-once".to_string())]);
    let cursor = conn.cursor(&session);
    let (_, updates, stop) = conn.turn(&session, "IS10_ALLOW run it");
    assert_eq!(stop, "end_turn");
    let requests = conn.client.handlers().permission.requests();
    assert_eq!(requests.len(), 1, "ask-commands asks before bash");
    let request = &requests[0];
    assert_eq!(request["sessionId"], session.as_str(), "{request}");
    assert!(
        request["title"].as_str().is_some_and(|title| !title.is_empty()),
        "{request}"
    );
    assert_eq!(request["subject"]["type"], "command", "{request}");
    assert_eq!(request["subject"]["command"], "echo is10-$((6*7))-allow", "{request}");
    assert_eq!(request["subject"]["cwd"], cwd_str(&work), "{request}");
    assert!(request["subject"]["toolCallId"].is_string(), "{request}");
    assert!(request.get("toolCall").is_none(), "{request}");
    let states = conn.states_since(&session, cursor);
    let waiting = states
        .iter()
        .position(|state| state == "requires_action")
        .unwrap_or_else(|| panic!("requires_action: {states:?}"));
    assert_eq!(
        states[waiting + 1],
        "running",
        "running again after the answer: {states:?}"
    );
    assert_eq!(states.last().map(String::as_str), Some("idle:end_turn"), "{states:?}");
    assert_eq!(
        terminal_output(&updates),
        "is10-42-allow\n",
        "allow-once runs it: {updates:?}"
    );

    let session = conn.new_session(&work);
    ask_commands(&mut conn, &session);
    conn.client.handlers().permission.script([PermissionOutcome::Hold]);
    conn.take_notes();
    let cursor = conn.cursor(&session);
    conn.send_prompt(&session, "IS10_CUSTOM run it");
    let request_id = conn
        .client
        .await_agent_request("session/request_permission", WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", daemon.logs()));
    let custom = json!({"jsonrpc": "2.0", "id": request_id, "result": {"outcome": {"outcome": "_custom"}}});
    conn.write_invalid_line(&custom.to_string());
    assert_eq!(conn.await_idle_after(&session, cursor), "end_turn");
    let updates = updates_of(&conn.take_notes(), &session);
    assert!(
        !terminal_output(&updates).contains("is10-25-custom") && !mentions(&updates, "is10-25-custom"),
        "an unknown outcome never runs it: {updates:?}"
    );
    assert!(
        updates
            .iter()
            .any(|update| update["sessionUpdate"] == "tool_call_update" && update["status"] == "failed"),
        "the refused call fails: {updates:?}"
    );
    conn.assert_schema_clean();
}

/// IS-11: the to-do list arrives as `plan_update` with `type: items` and
/// `planId: "todo"`.
#[test]
fn is11_plan() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    daemon.provider.script(
        "IS11_TODO",
        tool_turn(
            "todo",
            json!({"todos": [{"content": "is11 step", "status": "in_progress", "priority": "high", "id": "1", "confidence": "plausible"}]}),
            "is11 planned",
        ),
    );
    let mut conn = Conn::v2(&daemon, "is11");
    conn.initialize();
    let session = conn.new_session(&work);
    let (_, updates, stop) = conn.turn(&session, "IS11_TODO plan it");
    assert_eq!(stop, "end_turn");
    assert!(!kinds(&updates).iter().any(|kind| kind == "plan"), "{updates:?}");
    let plan = of_kind(&updates, "plan_update");
    let plan = plan.first().unwrap_or_else(|| panic!("a plan_update: {updates:?}"));
    assert_eq!(plan["plan"]["type"], "items", "{plan}");
    assert_eq!(plan["plan"]["planId"], "todo", "{plan}");
    assert_eq!(
        plan["plan"]["entries"],
        json!([{"content": "is11 step", "priority": "high", "status": "in_progress"}]),
        "{plan}"
    );
    conn.assert_schema_clean();
}

/// IS-12: config options use `configId` with the mode option first, a
/// set_config_option without `type` is -32602, and setup responses carry
/// `availableCommands` tagged `type: "command"` and no `modes`.
#[test]
fn is12_config_commands() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    let mut conn = Conn::v2(&daemon, "is12");
    conn.initialize();
    let created = conn.ok("session/new", json!({"cwd": cwd_str(&work)}));
    let session = created["sessionId"].as_str().expect("a session id").to_string();
    assert!(created.get("modes").is_none(), "{created}");
    let options = created["configOptions"].as_array().expect("configOptions");
    assert_eq!(options[0]["configId"], "mode", "{created}");
    assert_eq!(options[0]["category"], "mode", "{created}");
    assert!(options.iter().all(|option| option.get("id").is_none()), "{created}");
    let commands = created["availableCommands"].as_array().expect("availableCommands");
    assert!(!commands.is_empty(), "{created}");
    assert!(commands.iter().all(|command| command["type"] == "command"), "{created}");
    let untyped = conn.err_invalid(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "mode", "value": "auto"}),
    );
    assert_eq!(untyped["code"], -32602, "{untyped}");
    let set = conn.ok(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "mode", "type": "id", "value": "auto"}),
    );
    let list = set["configOptions"].as_array().expect("the full list");
    assert_eq!(list.len(), options.len(), "{set}");
    assert_eq!(list[0]["currentValue"], "auto", "{set}");
    let update = conn.await_update(&session, "config_option_update", |update| {
        update["sessionUpdate"] == "config_option_update"
    });
    assert_eq!(update["configOptions"][0]["currentValue"], "auto", "{update}");
    conn.barrier();
    let notes = conn.take_notes();
    assert!(
        !kinds(&updates_of(&notes, &session))
            .iter()
            .any(|kind| kind == "current_mode_update"),
        "{notes:?}"
    );
    conn.assert_schema_clean();
}

/// IS-13: session/resume with `replayFrom: {type: "start"}` replays the user
/// message under the prompt's messageId, then the idle snapshot, then the
/// response; a bad list cursor is -32602, an unknown session -32002 with
/// `data.sessionId`, and an `sse` MCP server -32602.
#[test]
fn is13_sessions() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    daemon
        .provider
        .script("IS13_FIRST", Script::Text("is13 first reply".to_string()));
    let mut first = Conn::v2(&daemon, "is13a");
    first.initialize();
    let session = first.new_session(&work);
    let (message_id, _, stop) = first.turn(&session, "IS13_FIRST hello");
    assert_eq!(stop, "end_turn");
    let bad_cursor = first.err("session/list", json!({"cursor": "!!!!"}));
    assert_eq!(bad_cursor["code"], -32602, "{bad_cursor}");
    let listed = first.ok("session/list", json!({}));
    assert!(
        listed["sessions"]
            .as_array()
            .expect("sessions")
            .iter()
            .any(|row| row["sessionId"] == session.as_str() && row["cwd"] == cwd_str(&work)),
        "{listed}"
    );
    let sse = first.err_invalid(
        "session/new",
        json!({"cwd": cwd_str(&work), "mcpServers": [{"type": "sse", "name": "is13", "url": "http://127.0.0.1:9/sse", "headers": []}]}),
    );
    assert_eq!(sse["code"], -32602, "{sse}");
    first.assert_schema_clean();

    let mut second = Conn::v2(&daemon, "is13b");
    second.initialize();
    let resume = second.send(
        "session/resume",
        json!({"sessionId": session, "cwd": cwd_str(&work), "replayFrom": {"type": "start"}}),
    );
    let resumed = second.wait("session/resume", &resume).expect("the resume answers");
    assert!(resumed.get("modes").is_none(), "{resumed}");
    let lines = second.stdout();
    let user = line_index(&lines, "the replayed user message", |frame| {
        frame["params"]["update"]["sessionUpdate"] == "user_message"
    });
    let replayed: Value = serde_json::from_str(&lines[user]).expect("JSON");
    assert_eq!(
        replayed["params"]["update"]["messageId"],
        message_id.as_str(),
        "the replayed user message keeps the prompt's id: {replayed}"
    );
    let agent = line_index(&lines, "the replayed agent message", |frame| {
        frame["params"]["update"]["sessionUpdate"] == "agent_message"
    });
    let snapshot = line_index(&lines, "the state snapshot", |frame| {
        frame["params"]["update"]["sessionUpdate"] == "state_update"
    });
    let answered = line_index(&lines, "the resume response", |frame| is_response_to(frame, &resume));
    assert!(
        user < snapshot && agent < snapshot && snapshot < answered,
        "replay, snapshot, response: {lines:#?}"
    );
    let snapshot: Value = serde_json::from_str(&lines[snapshot]).expect("JSON");
    assert_eq!(snapshot["params"]["update"]["state"], "idle", "{snapshot}");
    let unknown = "session_nope_1700000000000_0123456789abcdef";
    let missing = second.err("session/resume", json!({"sessionId": unknown, "cwd": cwd_str(&work)}));
    assert_eq!(missing["code"], -32002, "{missing}");
    assert_eq!(missing["data"]["sessionId"], unknown, "{missing}");
    second.assert_schema_clean();
}

/// IS-14: an `ask` question raises `elicitation/create` carrying the
/// sessionId; the state is requires_action while it is pending, and a decline
/// falls back to the recommended option.
#[test]
fn is14_elicitation() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    daemon.provider.script(
        "IS14_ASK",
        tool_turn(
            "ask",
            json!({"questions": [{
                "id": "color",
                "question": "Which color?",
                "options": [{"label": "is14-red"}, {"label": "is14-green"}, {"label": "is14-blue"}],
                "recommended": 2,
            }]}),
            "is14 ask done",
        ),
    );
    let capabilities = ClientCapabilities {
        elicitation_form: true,
        ..ClientCapabilities::default()
    };
    let mut conn = Conn::v2_with(&daemon, capabilities, false, "is14");
    conn.initialize();
    let session = conn.new_session(&work);
    conn.client.handlers().hold("elicitation/create");
    let cursor = conn.cursor(&session);
    conn.send_prompt(&session, "IS14_ASK ask me");
    let request_id = conn
        .client
        .await_agent_request("elicitation/create", WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", daemon.logs()));
    let lines = conn.stdout();
    let request: Value = serde_json::from_str(
        &lines[line_index(&lines, "the elicitation request", |frame| {
            frame["method"] == "elicitation/create"
        })],
    )
    .expect("JSON");
    assert_eq!(request["params"]["sessionId"], session.as_str(), "{request}");
    assert_eq!(request["params"]["mode"], "form", "{request}");
    conn.await_update(&session, "requires_action", |update| {
        update["sessionUpdate"] == "state_update" && update["state"] == "requires_action"
    });
    assert_eq!(
        conn.states_since(&session, cursor).last().map(String::as_str),
        Some("requires_action"),
        "pending while unanswered"
    );
    conn.client
        .write_value(&json!({"jsonrpc": "2.0", "id": request_id, "result": {"action": "decline"}}))
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(conn.await_idle_after(&session, cursor), "end_turn");
    let output = function_call_output(&daemon, "ask", "IS14_ASK");
    assert!(
        output.contains("-> is14-blue") && output.contains("Resolved 1 question(s) automatically"),
        "a decline falls back to the recommended option: {output}"
    );
    assert!(
        !output.contains("-> is14-red") && !output.contains("-> is14-green"),
        "no other option is chosen: {output}"
    );
    conn.assert_schema_clean();
}

/// IS-15: auth/logout ends this connection's authenticated state only:
/// session/new is -32000 until auth/login, and no credential file in the
/// isolated home changes.
#[test]
fn is15_auth() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    let mut conn = Conn::v2(&daemon, "is15");
    conn.initialize();
    conn.new_session(&work);
    let before = daemon_credentials(&daemon);
    assert!(
        before[&daemon.home.join("config.toml")].is_some(),
        "the provider configuration exists: {:?}",
        before.keys().collect::<Vec<_>>()
    );
    assert_eq!(conn.ok("auth/logout", json!({})), json!({}));
    let refused = conn.err("session/new", json!({"cwd": cwd_str(&work)}));
    assert_eq!(refused["code"], -32000, "{refused}");
    assert_eq!(
        conn.ok("auth/login", json!({"methodId": "horon-agent-credentials"})),
        json!({})
    );
    let session = conn.new_session(&work);
    assert!(!session.is_empty());
    let after = daemon_credentials(&daemon);
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>(),
        "no credential file appeared under .openclaw/agents or the provider env dir"
    );
    for (path, bytes) in &before {
        assert!(
            after[path] == *bytes,
            "{} was created, removed or changed by logout/login",
            path.display()
        );
    }
    eprintln!(
        "IS15-CREDENTIALS unchanged locations={} present={}",
        before.len(),
        before.values().filter(|bytes| bytes.is_some()).count()
    );
    conn.assert_schema_clean();
}

/// Dual-stack 1: one daemon, a v1 and a v2 client; each gets its own
/// surface for initialize, session setup and a prompt.
#[test]
fn dual1_each_client_reaches_its_surface() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    daemon
        .provider
        .script("DUAL1_V1", Script::Text("dual1 v1 reply".to_string()));
    daemon
        .provider
        .script("DUAL1_V2", Script::Text("dual1 v2 reply".to_string()));
    let mut v1 = Conn::v1(&daemon, "dual1-v1");
    let mut v2 = Conn::v2(&daemon, "dual1-v2");
    let first = v1.initialize();
    assert!(
        first["agentCapabilities"].is_object() && first.get("info").is_none(),
        "{first}"
    );
    let second = v2.initialize();
    assert!(
        second["capabilities"].is_object() && second.get("agentCapabilities").is_none(),
        "{second}"
    );

    let created = v1.ok("session/new", json!({"cwd": cwd_str(&work), "mcpServers": []}));
    assert!(created["modes"].is_object(), "the v1 setup answer: {created}");
    let v1_session = created["sessionId"].as_str().expect("a session id").to_string();
    let answer = v1.ok(
        "session/prompt",
        json!({"sessionId": v1_session, "prompt": [{"type": "text", "text": "DUAL1_V1 hi"}]}),
    );
    assert_eq!(answer["stopReason"], "end_turn", "{answer}");

    let v2_session = v2.new_session(&work);
    let (_, updates, stop) = v2.turn(&v2_session, "DUAL1_V2 hi");
    assert_eq!(stop, "end_turn");
    assert!(
        chunk_text(&updates, "agent_message_chunk").contains("dual1 v2 reply"),
        "{updates:?}"
    );
    v1.assert_schema_clean();
    v2.assert_schema_clean();
}

/// Dual-stack 2: a v2 client against an agent with v2 disabled is answered
/// version 1 with the v1 capabilities shape, and can continue in v1.
#[test]
fn dual2_v2_client_against_v2_disabled() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    let mut env = daemon.env_pairs();
    env.push((V2_OFF.0.to_string(), V2_OFF.1.to_string()));
    let mut conn = Conn::open(env, AcpClientOptions::new(Vec::new()), 1, "dual2");
    let v2_params = AcpClientOptions::new(Vec::new())
        .protocol_version(2)
        .initialize_params();
    let request = json!({"jsonrpc": "2.0", "id": 900, "method": "initialize", "params": v2_params});
    assert_eq!(
        FrameValidator::for_version(2, Direction::ClientToAgent).validate(&request),
        Vec::<Violation>::new(),
        "the probe is a valid v2 initialize"
    );
    // Validated as v2 above; this reader speaks v1, so the probe is counted
    // instead of being validated against the v1 set.
    let id = conn.send_invalid("initialize", v2_params);
    let result = conn.wait("initialize", &id).expect("the agent answers version 1");
    assert_eq!(result["protocolVersion"], 1, "{result}");
    assert!(result["agentCapabilities"].is_object(), "{result}");
    assert!(
        result.get("capabilities").is_none() && result.get("info").is_none(),
        "{result}"
    );
    let created = conn.ok("session/new", json!({"cwd": cwd_str(&work), "mcpServers": []}));
    assert!(created["sessionId"].is_string(), "{created}");
    conn.assert_schema_clean();
}

/// Dual-stack 3: a newer protocolVersion with an unknown params field still
/// selects v2.
#[test]
fn dual3_newer_version_and_unknown_params_select_v2() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    let mut conn = Conn::v2(&daemon, "dual3");
    let mut params = conn.client.initialize_params();
    params["protocolVersion"] = json!(99);
    params["horonFutureField"] = json!({"anything": true});
    let id = conn.send_invalid("initialize", params);
    let result = conn.wait("initialize", &id).expect("a newer version selects v2");
    assert_eq!(result["protocolVersion"], 2, "{result}");
    assert_eq!(result["info"]["name"], "horon-agent", "{result}");
    conn.new_session(&work);
    conn.assert_schema_clean();
}

/// Dual-stack 4: prompt and cancel through both surfaces on one daemon: v1
/// answers the prompt with stopReason cancelled, v2 reports idle cancelled.
#[test]
fn dual4_prompt_and_cancel_through_both_surfaces() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    daemon
        .provider
        .script("DUAL4_V1", Script::HoldOpen("dual4 v1 partial ".to_string()));
    daemon
        .provider
        .script("DUAL4_V2", Script::HoldOpen("dual4 v2 partial ".to_string()));

    let mut v1 = Conn::v1(&daemon, "dual4-v1");
    v1.initialize();
    let created = v1.ok("session/new", json!({"cwd": cwd_str(&work), "mcpServers": []}));
    let v1_session = created["sessionId"].as_str().expect("a session id").to_string();
    let prompt = v1.send(
        "session/prompt",
        json!({"sessionId": v1_session, "prompt": [{"type": "text", "text": "DUAL4_V1 hold"}]}),
    );
    daemon
        .provider
        .wait_for_hold_open_opened(WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", daemon.logs()));
    v1.notify("session/cancel", json!({"sessionId": v1_session}));
    let answer = v1.wait("session/prompt", &prompt).expect("the v1 prompt answers");
    assert_eq!(answer["stopReason"], "cancelled", "{answer}");
    assert!(
        v1.stdout().iter().all(|line| !line.contains("state_update")),
        "no v2 state on the v1 surface"
    );
    v1.assert_schema_clean();

    let mut v2 = Conn::v2(&daemon, "dual4-v2");
    v2.initialize();
    let session = v2.new_session(&work);
    let cursor = v2.cursor(&session);
    let prompt = v2.send_prompt(&session, "DUAL4_V2 hold");
    let inserted = v2.wait("session/prompt", &prompt).expect("the v2 prompt is inserted");
    assert!(inserted["messageId"].is_string(), "{inserted}");
    v2.await_update(&session, "the v2 turn running", |update| {
        update["sessionUpdate"] == "state_update" && update["state"] == "running"
    });
    v2.notify("session/cancel", json!({"sessionId": session}));
    assert_eq!(v2.await_idle_after(&session, cursor), "cancelled");
    assert_eq!(v2.states_since(&session, cursor), ["running", "idle:cancelled"]);
    v2.assert_schema_clean();
}

/// Dual-stack 5: with v2 disabled a v1 client works end to end, and the
/// requests a v2 build would answer 2 or -32022 are answered as v1 does.
#[test]
fn dual5_disabling_v2_keeps_v1_working() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    daemon
        .provider
        .script("DUAL5_TEXT", Script::Text("dual5 reply".to_string()));
    let mut env = daemon.env_pairs();
    env.push((V2_OFF.0.to_string(), V2_OFF.1.to_string()));
    let mut conn = Conn::open(env.clone(), AcpClientOptions::new(Vec::new()), 1, "dual5");
    conn.initialize();
    let created = conn.ok("session/new", json!({"cwd": cwd_str(&work), "mcpServers": []}));
    let session = created["sessionId"].as_str().expect("a session id").to_string();
    let answer = conn.ok(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "DUAL5_TEXT hi"}]}),
    );
    assert_eq!(answer["stopReason"], "end_turn", "{answer}");
    let updates = updates_of(&conn.take_notes(), &session);
    assert!(
        chunk_text(&updates, "agent_message_chunk").contains("dual5 reply"),
        "{updates:?}"
    );
    conn.assert_schema_clean();

    for version in [0, 2, 65535] {
        let mut probe = Conn::open(
            env.clone(),
            AcpClientOptions::new(Vec::new()),
            1,
            &format!("dual5-{version}"),
        );
        let answer = probe.ok(
            "initialize",
            json!({"protocolVersion": version, "clientCapabilities": {}}),
        );
        assert_eq!(
            answer["protocolVersion"], 1,
            "v2 off answers {version} with 1: {answer}"
        );
        probe.assert_schema_clean();
    }
}

/// Dual-stack 6: protocolVersion 0 is -32022 and creates nothing: the
/// isolated sessions directory stays empty.
#[test]
fn dual6_unsupported_version_error() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let sessions = daemon.home.join("sessions");
    let listing = |dir: &Path| -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path.clone());
                }
                found.push(path);
            }
        }
        found.sort();
        found
    };
    let before = listing(&sessions);
    let mut conn = Conn::v2(&daemon, "dual6");
    let mut params = conn.client.initialize_params();
    params["protocolVersion"] = json!(0);
    let error = conn.err("initialize", params);
    assert_eq!(error["code"], -32022, "{error}");
    assert_eq!(error["data"]["requested"], 0, "{error}");
    assert_eq!(error["data"]["supported"], json!([1, 2]), "{error}");
    let early = conn.err("session/list", json!({}));
    assert_eq!(early["code"], -32600, "still uninitialized: {early}");
    let after = listing(&sessions);
    assert_eq!(before, after, "an unsupported version creates nothing");
    assert!(after.is_empty(), "the isolated sessions dir stays empty: {after:?}");
    conn.client.close_stdin();
    let status = conn.client.wait_exit(WAIT).expect("the adapter exits on stdin EOF");
    assert!(status.success(), "{status}");
    assert!(
        listing(&sessions).is_empty(),
        "nothing after the connection ends either"
    );
    conn.assert_schema_clean();
}

/// Dual-stack 7: a session prompted over v1 and resumed over v2 with
/// `replayFrom: {type: "start"}` replays its user message under the id the
/// daemon stored for it.
#[test]
fn dual7_v1_session_resumed_by_v2_replays_stored_ids() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    daemon
        .provider
        .script("DUAL7_V1", Script::Text("dual7 v1 reply".to_string()));
    let mut v1 = Conn::v1(&daemon, "dual7-v1");
    v1.initialize();
    let created = v1.ok("session/new", json!({"cwd": cwd_str(&work), "mcpServers": []}));
    let session = created["sessionId"].as_str().expect("a session id").to_string();
    let answer = v1.ok(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "DUAL7_V1 hello"}]}),
    );
    assert_eq!(answer["stopReason"], "end_turn", "{answer}");
    v1.assert_schema_clean();
    v1.client.close_stdin();
    v1.client.wait_exit(WAIT).expect("the v1 adapter exits on stdin EOF");

    let peer = Peer::connect(&daemon.socket, &work, Some(&session), true);
    let history = peer
        .events
        .iter()
        .find(|event| event["type"] == "history")
        .unwrap_or_else(|| panic!("the subscribe answers History: {:?}", peer.events));
    let stored = history["messages"]
        .as_array()
        .expect("history messages")
        .iter()
        .find(|row| {
            row["role"] == "user"
                && row["content"]
                    .as_str()
                    .is_some_and(|text| text.contains("DUAL7_V1 hello"))
        })
        .unwrap_or_else(|| panic!("the stored user row: {history}"))["message_id"]
        .as_str()
        .unwrap_or_else(|| panic!("a stored message id: {history}"))
        .to_string();
    drop(peer);

    let mut v2 = Conn::v2(&daemon, "dual7-v2");
    v2.initialize();
    let resumed = v2.ok(
        "session/resume",
        json!({"sessionId": session, "cwd": cwd_str(&work), "replayFrom": {"type": "start"}}),
    );
    assert!(resumed.get("modes").is_none(), "{resumed}");
    let replay = updates_of(&v2.take_notes(), &session);
    let user = of_kind(&replay, "user_message");
    assert_eq!(user.len(), 1, "{replay:?}");
    assert_eq!(user[0]["messageId"], stored.as_str(), "the stored id: {replay:?}");
    assert!(user[0]["content"].to_string().contains("DUAL7_V1 hello"), "{replay:?}");
    let agent = of_kind(&replay, "agent_message");
    assert!(
        agent
            .iter()
            .any(|message| message["messageId"] == format!("{stored}:a1").as_str()
                && message.to_string().contains("dual7 v1 reply")),
        "the v1 reply replays under the stored turn id: {replay:?}"
    );
    v2.assert_schema_clean();
}

/// Removes a scratch directory when the test ends, also when it panics.
struct ScratchDir(PathBuf);

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The IS-15 credential snapshot is a closed set of locations: runtime state
/// whose name happens to contain a credential word (session names are random
/// animals, `session_turkey_*` and `session_monkey_*` included) never enters
/// it, while every kind of real credential location does.
#[test]
fn credential_snapshot_ignores_runtime_state() {
    let scratch = ScratchDir(std::env::temp_dir().join(format!("acpv2-cred-{}", std::process::id())));
    let root = scratch.0.clone();
    let home = root.join("home");
    let xdg = root.join("config");
    let snapshot = || credential_files(&home, &root, &xdg);
    std::fs::create_dir_all(home.join("active_pids")).expect("active_pids");
    std::fs::write(home.join("openai-auth.json"), "{}").expect("credential");
    let before = snapshot();
    for name in [
        "session_turkey_1793200277865_93744219ff0eb0d6",
        "session_monkey_1793200277865_0123456789abcdef",
    ] {
        std::fs::write(home.join("active_pids").join(name), "4242").expect("pid file");
    }
    std::fs::create_dir_all(home.join("sessions/session_turkey_1")).expect("session dir");
    std::fs::write(home.join("sessions/session_turkey_1/key.json"), "{}").expect("session file");
    assert_eq!(snapshot(), before, "runtime state is not a credential");
    std::fs::write(home.join("openai-auth.json"), "{\"rotated\":true}").expect("rotate");
    assert_ne!(snapshot(), before, "a credential change is seen");
    let app_config = if cfg!(target_os = "macos") {
        root.join("Library/Application Support/horon-agent")
    } else {
        xdg.join("horon-agent")
    };
    let appearing = [
        root.join(".local/share/opencode/auth.json"),
        root.join(".config/github-copilot/hosts.json"),
        root.join(".openclaw/agents/is15/agent/auth-profiles.json"),
        app_config.join("openai.env"),
        home.join("external/.local/share/opencode/auth.json"),
        home.join("external/.config/github-copilot/hosts.json"),
        home.join("external/.openclaw/agents/main/agent/auth-profiles.json"),
        home.join("config/horon-agent/openai.env"),
        home.join("config/horon-agent/horon-agent-subscription.env"),
    ];
    for path in &appearing {
        let before = snapshot();
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("store dir");
        std::fs::write(path, "{}").expect("new store");
        let after = snapshot();
        assert_ne!(
            after,
            before,
            "a credential store that appears is seen: {}",
            path.display()
        );
        assert_eq!(after.get(path), Some(&Some(b"{}".to_vec())), "{}", path.display());
    }
}

/// The copied credential lists equal the private lists the shipped code
/// reads, so the IS-15 snapshot cannot silently miss a store the product adds.
#[test]
fn credential_lists_match_the_shipped_code() {
    fn literals(source: &str, start: &str) -> Vec<String> {
        let begin = source.find(start).unwrap_or_else(|| panic!("{start} is in launch.rs")) + start.len();
        let end = begin + source[begin..].find("];").expect("the list ends");
        source[begin..end]
            .split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect()
    }
    let launch = include_str!("../crates/agent-sdk/src/launch.rs");
    assert_eq!(
        literals(launch, "const CREDENTIAL_FILES: &[&str] = &["),
        SHIPPED_HOME_CREDENTIAL_FILES
    );
    assert_eq!(
        literals(launch, "const EXTERNAL_CREDENTIAL_FILES: &[&str] = &["),
        SHIPPED_EXTERNAL_CREDENTIAL_FILES
    );
    assert!(
        launch.contains(r#"for name in ["auth-profiles.json", "auth.json"] {"#)
            && launch.contains(r#".join("agents")"#),
        "the per-agent OpenClaw store names"
    );
    assert!(
        launch.contains(r#"home_dir().join("Library/Application Support/horon-agent")"#)
            && launch.contains(r#".join("horon-agent")"#)
            && launch.contains(r#"ends_with(".env")"#),
        "the provider env files of user_app_config_dir()"
    );
    assert!(
        launch.contains(r#"PathBuf::from("external").join(relative)"#)
            && launch.contains(r#"PathBuf::from("external").join(&relative)"#)
            && launch.contains(r#"PathBuf::from("config").join("horon-agent").join(&name)"#),
        "launch.rs links the stores to <instance>/external/<rel> and <instance>/config/horon-agent/<name>.env"
    );
    let storage = include_str!("../crates/storage/src/lib.rs");
    assert!(
        storage.contains(r#"return Ok(PathBuf::from(path).join("external").join(relative));"#)
            && storage.contains(r#"return Ok(PathBuf::from(path).join("config").join("horon-agent"));"#),
        "under HORON_AGENT_HOME the daemon resolves user_home_path to $HORON_AGENT_HOME/external/<rel> and app_config_dir to $HORON_AGENT_HOME/config/horon-agent"
    );
    let oauth = include_str!("../crates/base/src/mcp/oauth.rs");
    assert!(
        oauth.contains(r#"const STORE_FILE: &str = "mcp-oauth.json";"#),
        "the MCP OAuth store name"
    );
}
