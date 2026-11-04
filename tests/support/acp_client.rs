//! A mock ACP stdio client for the conformance tests.
//!
//! [`AcpClient`] spawns the shipped binary as `horon-agent acp`, writes JSON-RPC
//! frames, reads the agent's newline-framed stdout with a bounded deadline,
//! validates every frame against the vendored schema of the protocol version it
//! speaks ([`crate::acp_schema`]) and answers the agent's client-directed
//! requests (`fs/*`, `terminal/*`, `session/request_permission`,
//! `elicitation/create`) from pluggable handlers.
//!
//! The version defaults to 1, which is what every v1 test target expects. With
//! [`AcpClientOptions::protocol_version`] set to 2 the client writes the v2
//! `initialize` params (`protocolVersion`, `info`, `capabilities`), validates
//! the stream against the v2 set, records every `state_update` transition per
//! session ([`StateTracker`]) and offers the v2 helpers: [`AcpClient::await_idle`],
//! [`AcpClient::prompt_v2`] and [`AcpClient::send_batch`].
//!
//! Include it from a test target together with the modules it names:
//!
//! ```ignore
//! #[path = "support/acp_schema.rs"] mod acp_schema;
//! #[path = "support/acp_client.rs"] mod acp_client;
//! ```
#![allow(dead_code)]

use crate::acp_schema::{Direction, FrameValidator, Violation};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Client capabilities advertised in `initialize`, and enforced when the agent
/// asks for a capability-gated method.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClientCapabilities {
    pub fs_read_text_file: bool,
    pub fs_write_text_file: bool,
    pub terminal: bool,
    pub elicitation_form: bool,
}

impl ClientCapabilities {
    pub fn all() -> Self {
        Self {
            fs_read_text_file: true,
            fs_write_text_file: true,
            terminal: true,
            elicitation_form: true,
        }
    }

    pub fn to_json(self) -> Value {
        let mut capabilities = json!({
            "fs": {
                "readTextFile": self.fs_read_text_file,
                "writeTextFile": self.fs_write_text_file,
            },
            "terminal": self.terminal,
        });
        if self.elicitation_form {
            capabilities["elicitation"] = json!({"form": {}});
        }
        capabilities
    }

    /// The v2 `capabilities` object: v2 dropped the client `fs/*` and
    /// `terminal/*` methods, its capability markers are `{}` objects, and the
    /// terminal authentication marker is separate from the terminal methods.
    pub fn to_json_v2(self, auth_terminal: bool) -> Value {
        let mut capabilities = json!({});
        if auth_terminal {
            capabilities["auth"] = json!({"terminal": {}});
        }
        if self.elicitation_form {
            capabilities["elicitation"] = json!({"form": {}});
        }
        capabilities
    }
}

#[derive(Clone, Debug)]
pub struct AcpClientOptions {
    /// The isolated environment the client child runs with.
    pub env: Vec<(String, String)>,
    pub capabilities: ClientCapabilities,
    /// Upper bound for a single frame wait.
    pub deadline: Duration,
    pub client_name: String,
    /// Root keys the adapter adds today and this run accepts anyway.
    pub extension_root_keys: Vec<String>,
    /// The protocol version this client speaks: 1 (the default) or 2.
    pub protocol_version: u64,
    /// Advertise `auth.terminal`: this client can reproduce the configured agent
    /// invocation in a terminal. A boolean in v1, a `{}` marker in v2.
    pub auth_terminal: bool,
}

impl AcpClientOptions {
    pub fn new(env: Vec<(String, String)>) -> Self {
        Self {
            env,
            capabilities: ClientCapabilities::default(),
            deadline: Duration::from_secs(120),
            client_name: "mock-acp-client".to_string(),
            extension_root_keys: Vec::new(),
            protocol_version: 1,
            auth_terminal: false,
        }
    }

    pub fn protocol_version(mut self, version: u64) -> Self {
        self.protocol_version = version;
        self
    }

    pub fn auth_terminal(mut self, auth_terminal: bool) -> Self {
        self.auth_terminal = auth_terminal;
        self
    }

    /// The `initialize` params this client writes: the v1 `clientCapabilities`
    /// object, or the v2 `{protocolVersion, info, capabilities}` request that
    /// `info` makes mandatory.
    pub fn initialize_params(&self) -> Value {
        match self.protocol_version {
            1 => {
                let mut client_capabilities = self.capabilities.to_json();
                if self.auth_terminal {
                    client_capabilities["auth"] = json!({"terminal": true});
                }
                json!({"protocolVersion": 1, "clientCapabilities": client_capabilities})
            }
            version => json!({
                "protocolVersion": version,
                "info": {
                    "name": self.client_name,
                    "title": "Mock ACP client",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": self.capabilities.to_json_v2(self.auth_terminal),
            }),
        }
    }

    pub fn extension_root_keys(mut self, keys: impl IntoIterator<Item = String>) -> Self {
        self.extension_root_keys = keys.into_iter().collect();
        self
    }

    pub fn capabilities(mut self, capabilities: ClientCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }
}

#[derive(Debug)]
pub enum AcpClientError {
    NonJson {
        line: String,
        error: String,
    },
    Schema {
        line: String,
        violations: Vec<Violation>,
    },
    Timeout {
        what: String,
        deadline: Duration,
    },
    Closed,
    Rpc {
        method: String,
        error: Value,
    },
    /// The agent answered without a field the protocol version requires.
    MissingField {
        what: String,
        field: &'static str,
    },
    Io(String),
}

impl fmt::Display for AcpClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonJson { line, error } => write!(formatter, "stdout line is not JSON ({error}): {line}"),
            Self::Schema { line, violations } => {
                write!(formatter, "schema violation in {line}:")?;
                for violation in violations {
                    write!(formatter, "\n  {violation}")?;
                }
                Ok(())
            }
            Self::Timeout { what, deadline } => write!(formatter, "{what} did not arrive within {deadline:?}"),
            Self::Closed => formatter.write_str("the agent closed stdout"),
            Self::Rpc { method, error } => write!(formatter, "{method} answered with a JSON-RPC error: {error}"),
            Self::MissingField { what, field } => write!(formatter, "{what} carries no {field}"),
            Self::Io(error) => formatter.write_str(error),
        }
    }
}

impl std::error::Error for AcpClientError {}

/// One parsed frame, classified by the shape a client sees.
#[derive(Clone, Debug)]
pub enum Frame {
    Request { id: Value, method: String, params: Value },
    Notification { method: String, params: Value },
    Response { id: Value, result: Result<Value, Value> },
}

impl Frame {
    pub fn method(&self) -> Option<&str> {
        match self {
            Self::Request { method, .. } | Self::Notification { method, .. } => Some(method),
            Self::Response { .. } => None,
        }
    }
}

/// Turns one stdout line into a [`Frame`], rejecting anything that is not a
/// single JSON object or that violates the vendored schema.
#[derive(Debug, Default)]
pub struct FrameParser {
    validator: FrameValidator,
}

impl FrameParser {
    pub fn new() -> Self {
        Self {
            validator: FrameValidator::new(Direction::AgentToClient),
        }
    }

    /// A parser for one negotiated protocol version, validating what the agent
    /// writes against that version's set.
    pub fn for_version(version: u64) -> Self {
        Self {
            validator: FrameValidator::for_version(version, Direction::AgentToClient),
        }
    }

    /// Remember a request the client itself wrote, so the response that
    /// answers it is validated against that method's response schema.
    pub fn note_request(&mut self, id: &Value, method: &str) {
        self.validator.note_request(id, method);
    }

    /// Accept the named root keys in spec objects for this run.
    pub fn allow_extension_root_keys(&mut self, keys: &[&str]) {
        self.validator.allow_extension_root_keys(keys);
    }

    pub fn parse(&mut self, line: &str) -> Result<Frame, AcpClientError> {
        let value: Value = serde_json::from_str(line).map_err(|error| AcpClientError::NonJson {
            line: line.to_string(),
            error: error.to_string(),
        })?;
        if !value.is_object() {
            return Err(AcpClientError::NonJson {
                line: line.to_string(),
                error: "a frame must be a JSON object".to_string(),
            });
        }
        let violations = self.validator.validate(&value);
        if !violations.is_empty() {
            return Err(AcpClientError::Schema {
                line: line.to_string(),
                violations,
            });
        }
        Ok(classify(&value))
    }

    /// Parses and validates one JSON-RPC batch line, returning its entries.
    ///
    /// A batch is a JSON array of request, notification or response objects, so
    /// each entry is validated on its own and a violation names its index.
    pub fn parse_batch(&mut self, line: &str) -> Result<Vec<Value>, AcpClientError> {
        let value: Value = serde_json::from_str(line).map_err(|error| AcpClientError::NonJson {
            line: line.to_string(),
            error: error.to_string(),
        })?;
        let Some(entries) = value.as_array() else {
            return Err(AcpClientError::NonJson {
                line: line.to_string(),
                error: "a batch frame must be a JSON array".to_string(),
            });
        };
        let mut violations = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            if !entry.is_object() {
                violations.push(Violation {
                    path: format!("/{index}"),
                    message: "a batch entry must be a JSON object".to_string(),
                    schema_path: "envelope:object".to_string(),
                });
                continue;
            }
            for violation in self.validator.validate(entry) {
                violations.push(Violation {
                    path: format!("/{index}{}", violation.path),
                    ..violation
                });
            }
        }
        if !violations.is_empty() {
            return Err(AcpClientError::Schema {
                line: line.to_string(),
                violations,
            });
        }
        Ok(entries.clone())
    }
}

fn classify(value: &Value) -> Frame {
    let params = value.get("params").cloned().unwrap_or(Value::Null);
    match (value.get("method").and_then(Value::as_str), value.get("id")) {
        (Some(method), Some(id)) => Frame::Request {
            id: id.clone(),
            method: method.to_string(),
            params,
        },
        (Some(method), None) => Frame::Notification {
            method: method.to_string(),
            params,
        },
        (None, _) => Frame::Response {
            id: value.get("id").cloned().unwrap_or(Value::Null),
            result: match value.get("error") {
                Some(error) => Err(error.clone()),
                None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
            },
        },
    }
}

/// In-memory path -> text overlay that falls back to the real filesystem.
#[derive(Debug, Default)]
pub struct FsHandler {
    overlay: Mutex<HashMap<PathBuf, String>>,
    reads: Mutex<Vec<PathBuf>>,
    writes: Mutex<Vec<(PathBuf, String)>>,
}

impl FsHandler {
    pub fn insert(&self, path: impl Into<PathBuf>, text: impl Into<String>) {
        self.overlay
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(path.into(), text.into());
    }

    pub fn read(&self, path: &str) -> Result<String, String> {
        let path = PathBuf::from(path);
        self.reads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(path.clone());
        if let Some(text) = self
            .overlay
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&path)
        {
            return Ok(text.clone());
        }
        std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))
    }

    pub fn write(&self, path: &str, content: &str) -> Result<(), String> {
        let path = PathBuf::from(path);
        self.overlay
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(path.clone(), content.to_string());
        self.writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((path, content.to_string()));
        Ok(())
    }

    pub fn writes(&self) -> Vec<(PathBuf, String)> {
        self.writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn read_paths(&self) -> Vec<PathBuf> {
        self.reads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalRecord {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub output: String,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub released: bool,
    pub killed: bool,
}

/// Runs `terminal/create` commands with `std::process`, keeps their captured
/// output, and enforces the protocol's requirement that a terminal is released
/// before it disappears: every call on a released terminal is an error.
#[derive(Debug, Default)]
pub struct TerminalHandler {
    terminals: Mutex<HashMap<String, TerminalRecord>>,
    calls: Mutex<Vec<String>>,
    next_id: AtomicU32,
}

impl TerminalHandler {
    pub fn create(&self, params: &Value) -> Result<Value, String> {
        self.record_call("create");
        let command = params
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| "terminal/create requires a command".to_string())?;
        let args: Vec<String> = params
            .get("args")
            .and_then(Value::as_array)
            .map(|args| args.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        let cwd = params.get("cwd").and_then(Value::as_str).map(str::to_string);
        let mut process = Command::new(command);
        process.args(&args);
        if let Some(cwd) = &cwd {
            process.current_dir(cwd);
        }
        if let Some(env) = params.get("env").and_then(Value::as_array) {
            for variable in env {
                if let (Some(name), Some(value)) = (
                    variable.get("name").and_then(Value::as_str),
                    variable.get("value").and_then(Value::as_str),
                ) {
                    process.env(name, value);
                }
            }
        }
        let id = format!("term-{}", self.next_id.fetch_add(1, Ordering::Relaxed) + 1);
        let record = match process.output() {
            Ok(output) => {
                let mut text = String::from_utf8_lossy(&output.stdout).to_string();
                text.push_str(&String::from_utf8_lossy(&output.stderr));
                TerminalRecord {
                    command: command.to_string(),
                    args,
                    cwd,
                    output: text,
                    exit_code: output.status.code(),
                    signal: None,
                    released: false,
                    killed: false,
                }
            }
            Err(error) => return Err(format!("spawn {command}: {error}")),
        };
        self.terminals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.clone(), record);
        Ok(json!({ "terminalId": id }))
    }

    pub fn output(&self, params: &Value) -> Result<Value, String> {
        self.record_call("output");
        self.with_terminal::<Value>("output", params, |record| {
            Ok(json!({
                "output": record.output,
                "truncated": false,
                "exitStatus": exit_status(record),
            }))
        })
    }

    pub fn wait_for_exit(&self, params: &Value) -> Result<Value, String> {
        self.record_call("wait_for_exit");
        self.with_terminal("wait_for_exit", params, |record| Ok(exit_status(record)))
    }

    pub fn kill(&self, params: &Value) -> Result<Value, String> {
        self.record_call("kill");
        let terminal_id = terminal_id(params)?;
        let mut terminals = self.terminals.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = terminals
            .get_mut(&terminal_id)
            .ok_or_else(|| format!("terminal/kill for an unknown terminal {terminal_id}"))?;
        if record.released {
            return Err(format!("terminal/kill after terminal {terminal_id} was released"));
        }
        record.killed = true;
        Ok(json!({}))
    }

    pub fn release(&self, params: &Value) -> Result<Value, String> {
        self.record_call("release");
        let terminal_id = terminal_id(params)?;
        let mut terminals = self.terminals.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = terminals
            .get_mut(&terminal_id)
            .ok_or_else(|| format!("terminal/release for an unknown terminal {terminal_id}"))?;
        record.released = true;
        Ok(json!({}))
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn record(&self, id: &str) -> Option<TerminalRecord> {
        self.terminals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(id)
            .cloned()
    }

    fn with_terminal<T>(
        &self,
        call: &str,
        params: &Value,
        read: impl FnOnce(&TerminalRecord) -> Result<T, String>,
    ) -> Result<T, String> {
        let terminal_id = terminal_id(params)?;
        let terminals = self.terminals.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = terminals
            .get(&terminal_id)
            .ok_or_else(|| format!("terminal/{call} for an unknown terminal {terminal_id}"))?;
        if record.released {
            return Err(format!("terminal/{call} after terminal {terminal_id} was released"));
        }
        read(record)
    }

    fn record_call(&self, call: &str) {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(call.to_string());
    }
}

fn terminal_id(params: &Value) -> Result<String, String> {
    params
        .get("terminalId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "terminal request requires a terminalId".to_string())
}

fn exit_status(record: &TerminalRecord) -> Value {
    match (record.exit_code, &record.signal) {
        (Some(code), _) => json!({ "exitCode": code }),
        (None, Some(signal)) => json!({ "signal": signal }),
        (None, None) => json!({}),
    }
}

/// The permission outcome the client returns, in script order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionOutcome {
    Selected(String),
    Cancelled,
    /// Record the request and never answer it.
    Hold,
}

#[derive(Debug, Default)]
pub struct PermissionHandler {
    outcomes: Mutex<VecDeque<PermissionOutcome>>,
    requests: Mutex<Vec<Value>>,
}

impl PermissionHandler {
    pub fn script(&self, outcomes: impl IntoIterator<Item = PermissionOutcome>) {
        self.outcomes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend(outcomes);
    }

    pub fn requests(&self) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn answer(&self, params: &Value) -> Option<Value> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(params.clone());
        let outcome = self
            .outcomes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
            .unwrap_or_else(|| {
                let first = params
                    .get("options")
                    .and_then(Value::as_array)
                    .and_then(|options| options.first())
                    .and_then(|option| option.get("optionId"))
                    .and_then(Value::as_str)
                    .unwrap_or("allow")
                    .to_string();
                PermissionOutcome::Selected(first)
            });
        match outcome {
            PermissionOutcome::Selected(option_id) => {
                Some(json!({ "outcome": { "outcome": "selected", "optionId": option_id } }))
            }
            PermissionOutcome::Cancelled => Some(json!({ "outcome": { "outcome": "cancelled" } })),
            PermissionOutcome::Hold => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ElicitationAction {
    Accept(Value),
    Decline,
    Cancel,
}

#[derive(Debug, Default)]
pub struct ElicitationHandler {
    actions: Mutex<VecDeque<ElicitationAction>>,
    requests: Mutex<Vec<Value>>,
}

impl ElicitationHandler {
    pub fn script(&self, actions: impl IntoIterator<Item = ElicitationAction>) {
        self.actions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend(actions);
    }

    pub fn requests(&self) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn answer(&self, params: &Value) -> Value {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(params.clone());
        match self
            .actions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
            .unwrap_or(ElicitationAction::Decline)
        {
            ElicitationAction::Accept(content) => json!({ "action": "accept", "content": content }),
            ElicitationAction::Decline => json!({ "action": "decline" }),
            ElicitationAction::Cancel => json!({ "action": "cancel" }),
        }
    }
}

#[derive(Debug, Default)]
pub struct Handlers {
    pub fs: FsHandler,
    pub terminal: TerminalHandler,
    pub permission: PermissionHandler,
    pub elicitation: ElicitationHandler,
    pub capabilities: ClientCapabilities,
    held: Mutex<Vec<String>>,
}

impl Handlers {
    /// Leave every later `method` request unanswered (opt-in; nothing is held
    /// by default).
    pub fn hold(&self, method: &str) {
        self.held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(method.to_string());
    }

    /// `Ok(None)`: the request is deliberately left unanswered.
    fn answer(&self, method: &str, params: &Value) -> Result<Option<Value>, String> {
        if self
            .held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .any(|held| held == method)
        {
            return Ok(None);
        }
        if method == "session/request_permission" {
            return Ok(self.permission.answer(params));
        }
        self.answer_capability(method, params).map(Some)
    }

    fn answer_capability(&self, method: &str, params: &Value) -> Result<Value, String> {
        match method {
            "fs/read_text_file" if self.capabilities.fs_read_text_file => self
                .fs
                .read(required_str(params, "path")?)
                .map(|content| json!({ "content": content })),
            "fs/write_text_file" if self.capabilities.fs_write_text_file => self
                .fs
                .write(required_str(params, "path")?, required_str(params, "content")?)
                .map(|()| json!({})),
            "terminal/create" if self.capabilities.terminal => self.terminal.create(params),
            "terminal/output" if self.capabilities.terminal => self.terminal.output(params),
            "terminal/wait_for_exit" if self.capabilities.terminal => self.terminal.wait_for_exit(params),
            "terminal/kill" if self.capabilities.terminal => self.terminal.kill(params),
            "terminal/release" if self.capabilities.terminal => self.terminal.release(params),
            "elicitation/create" if self.capabilities.elicitation_form => Ok(self.elicitation.answer(params)),
            method => Err(format!(
                "{method} is not advertised by this client's capabilities ({:?})",
                self.capabilities
            )),
        }
    }
}

fn required_str<'a>(params: &'a Value, key: &str) -> Result<&'a str, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("request params are missing the string field {key:?}"))
}

/// One `state_update` transition of a session, as a v2 client sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateUpdate {
    /// `running`, `requires_action`, `idle`, or a client-specific `_` value.
    pub state: String,
    /// The stop reason an `idle` update carries, when it carries one.
    pub stop_reason: Option<String>,
    /// The failure an `idle` update reports, when it reports one.
    pub error: Option<Value>,
}

impl StateUpdate {
    pub fn is_idle(&self) -> bool {
        self.state == "idle"
    }
}

/// The `state_update` transitions this client has seen, oldest first per session.
#[derive(Debug, Default)]
pub struct StateTracker {
    sessions: HashMap<String, Vec<StateUpdate>>,
}

impl StateTracker {
    /// Records one notification; answers whether it carried a `state_update`.
    pub fn record(&mut self, method: &str, params: &Value) -> bool {
        if method != "session/update" {
            return false;
        }
        let Some(session_id) = params.get("sessionId").and_then(Value::as_str) else {
            return false;
        };
        let Some(update) = params.get("update") else {
            return false;
        };
        if update.get("sessionUpdate").and_then(Value::as_str) != Some("state_update") {
            return false;
        }
        let Some(state) = update.get("state").and_then(Value::as_str) else {
            return false;
        };
        self.sessions
            .entry(session_id.to_string())
            .or_default()
            .push(StateUpdate {
                state: state.to_string(),
                stop_reason: update.get("stopReason").and_then(Value::as_str).map(str::to_string),
                error: update.get("error").filter(|error| !error.is_null()).cloned(),
            });
        true
    }

    /// Every transition recorded for `session_id`, oldest first.
    pub fn transitions(&self, session_id: &str) -> &[StateUpdate] {
        self.sessions.get(session_id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// How many transitions `session_id` has so far. Take this before triggering
    /// work and pass it to [`Self::idle_since`] to wait for that work's end.
    pub fn cursor(&self, session_id: &str) -> usize {
        self.transitions(session_id).len()
    }

    /// The stop reason of the first `idle` transition recorded at or after
    /// `cursor`, or `None` while no such transition has arrived. An `idle`
    /// without a stop reason answers `Some(String::new())`.
    pub fn idle_since(&self, session_id: &str, cursor: usize) -> Option<String> {
        self.transitions(session_id)
            .iter()
            .skip(cursor)
            .find(|update| update.is_idle())
            .map(|update| update.stop_reason.clone().unwrap_or_default())
    }
}

pub struct AcpClient {
    child: Child,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    lines: Receiver<String>,
    stdout: Arc<Mutex<Vec<String>>>,
    /// Every line this client wrote to the agent, in order, so a test can
    /// validate the client-to-agent direction too.
    stdin_record: Arc<Mutex<Vec<String>>>,
    parser: FrameParser,
    handlers: Arc<Handlers>,
    notifications: VecDeque<Value>,
    early_responses: Vec<(Value, Result<Value, Value>)>,
    deadline: Duration,
    next_id: i64,
    /// The protocol version this client was configured with.
    version: u64,
    /// The `initialize` params this client writes.
    initialize: Value,
    /// The `state_update` transitions seen so far, per session.
    state: StateTracker,
}

impl AcpClient {
    pub fn spawn(options: AcpClientOptions) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_horon-agent"));
        command.arg("acp");
        Self::connect(command, options)
    }

    /// Spawns a scripted peer in place of the shipped binary.
    ///
    /// Reading, answering and batching do not care who is on the other end, so a
    /// self-check can drive them with a fixed peer and no daemon.
    pub fn spawn_peer(program: &str, args: &[&str], options: AcpClientOptions) -> Self {
        let mut command = Command::new(program);
        command.args(args);
        Self::connect(command, options)
    }

    fn connect(mut command: Command, options: AcpClientOptions) -> Self {
        command.env_clear();
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        for (key, value) in &options.env {
            command.env(key, value);
        }
        command
            .env_remove("HORON_AGENT_API_SOCKET")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().expect("spawn the ACP peer");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let (sender, lines) = std::sync::mpsc::channel();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&recorded);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                recorder
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(line.clone());
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let handlers = Arc::new(Handlers {
            capabilities: options.capabilities,
            ..Handlers::default()
        });
        let initialize = options.initialize_params();
        let version = options.protocol_version;
        let mut parser = FrameParser::for_version(version);
        if !options.extension_root_keys.is_empty() {
            let keys: Vec<&str> = options.extension_root_keys.iter().map(String::as_str).collect();
            parser.allow_extension_root_keys(&keys);
        }
        Self {
            child,
            stdin: Arc::new(Mutex::new(Some(stdin))),
            lines,
            stdout: recorded,
            stdin_record: Arc::new(Mutex::new(Vec::new())),
            parser,
            handlers,
            notifications: VecDeque::new(),
            early_responses: Vec::new(),
            deadline: options.deadline,
            next_id: 0,
            version,
            initialize,
            state: StateTracker::default(),
        }
    }

    pub fn handlers(&self) -> &Handlers {
        &self.handlers
    }

    pub fn stdout_lines(&self) -> Vec<String> {
        self.stdout
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Every line written to the agent's stdin, including the answers the
    /// handlers gave to the agent's own requests.
    pub fn stdin_lines(&self) -> Vec<String> {
        self.stdin_record
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn next_id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    /// The protocol version this client was configured with.
    pub fn protocol_version(&self) -> u64 {
        self.version
    }

    /// The `initialize` params this client writes for its protocol version.
    pub fn initialize_params(&self) -> Value {
        self.initialize.clone()
    }

    /// Performs the handshake this client's version requires.
    pub fn initialize(&mut self) -> Result<Value, AcpClientError> {
        let params = self.initialize_params();
        self.request("initialize", params)
    }

    /// The `state_update` transitions seen so far.
    pub fn state(&self) -> &StateTracker {
        &self.state
    }

    pub fn write_line(&mut self, line: &str) -> Result<(), AcpClientError> {
        let mut stdin = self.stdin.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(stdin) = stdin.as_mut() else {
            return Err(AcpClientError::Closed);
        };
        self.stdin_record
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(line.to_string());
        writeln!(stdin, "{line}").map_err(|error| AcpClientError::Io(format!("write to the agent: {error}")))?;
        stdin
            .flush()
            .map_err(|error| AcpClientError::Io(format!("flush to the agent: {error}")))
    }

    pub fn write_value(&mut self, value: &Value) -> Result<(), AcpClientError> {
        self.write_line(&value.to_string())
    }

    /// Writes a request and returns its result, or the JSON-RPC error object.
    pub fn request(&mut self, method: &str, params: Value) -> Result<Value, AcpClientError> {
        let id = self.next_id();
        self.request_with_id(json!(id), method, params)
    }

    pub fn request_with_id(&mut self, id: Value, method: &str, params: Value) -> Result<Value, AcpClientError> {
        self.send_request(id.clone(), method, params)?;
        self.await_response(method, &id, self.deadline)
    }

    /// Writes a request without waiting; collect its answer with
    /// [`Self::await_response`].
    pub fn send_request(&mut self, id: Value, method: &str, params: Value) -> Result<(), AcpClientError> {
        self.parser.note_request(&id, method);
        self.write_value(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
    }

    pub fn notify(&mut self, method: &str, params: Value) -> Result<(), AcpClientError> {
        self.write_value(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    /// Reads frames until the response with `id` arrives, answering the
    /// agent's client-directed requests and queueing its notifications.
    pub fn await_response(&mut self, method: &str, id: &Value, deadline: Duration) -> Result<Value, AcpClientError> {
        if let Some(position) = self
            .early_responses
            .iter()
            .position(|(response_id, _)| same_id(response_id, id))
        {
            let (_, result) = self.early_responses.remove(position);
            return result.map_err(|error| AcpClientError::Rpc {
                method: method.to_string(),
                error,
            });
        }
        let end = Instant::now() + deadline;
        loop {
            let frame = self.next_frame(end)?;
            match frame {
                Frame::Response {
                    id: response_id,
                    result,
                } if same_id(&response_id, id) => {
                    return result.map_err(|error| AcpClientError::Rpc {
                        method: method.to_string(),
                        error,
                    });
                }
                Frame::Response { id, result } => self.early_responses.push((id, result)),
                Frame::Notification { method, params } => {
                    self.notifications
                        .push_back(json!({ "method": method, "params": params }));
                }
                Frame::Request { id, method, params } => self.answer_request(id, &method, &params)?,
            }
        }
    }

    pub fn answer_request(&mut self, id: Value, method: &str, params: &Value) -> Result<(), AcpClientError> {
        let response = match self.handlers.answer(method, params) {
            Ok(None) => return Ok(()),
            Ok(Some(result)) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(message) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": message },
            }),
        };
        self.write_value(&response)
    }

    /// Waits for the next frame that is not a response to an earlier request,
    /// answering anything the agent asks for on the way.
    pub fn next_frame(&mut self, end: Instant) -> Result<Frame, AcpClientError> {
        loop {
            let remaining = end.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(AcpClientError::Timeout {
                    what: "the next agent frame".to_string(),
                    deadline: self.deadline,
                });
            }
            let line = match self.lines.recv_timeout(remaining) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(AcpClientError::Timeout {
                        what: "the next agent frame".to_string(),
                        deadline: self.deadline,
                    });
                }
                Err(RecvTimeoutError::Disconnected) => return Err(AcpClientError::Closed),
            };
            let frame = self.parse_line(&line)?;
            if let Frame::Request { id, method, params } = &frame {
                self.answer_request(id.clone(), method, params)?;
                continue;
            }
            return Ok(frame);
        }
    }

    /// Reads frames (answering, queueing or buffering them as usual) until the
    /// agent sends a request for `method`; returns that request's id.
    pub fn await_agent_request(&mut self, method: &str, deadline: Duration) -> Result<Value, AcpClientError> {
        let end = Instant::now() + deadline;
        loop {
            let remaining = end.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(remaining) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(AcpClientError::Timeout {
                        what: format!("an agent {method} request"),
                        deadline,
                    });
                }
                Err(RecvTimeoutError::Disconnected) => return Err(AcpClientError::Closed),
            };
            match self.parse_line(&line)? {
                Frame::Request {
                    id,
                    method: requested,
                    params,
                } => {
                    self.answer_request(id.clone(), &requested, &params)?;
                    if requested == method {
                        return Ok(id);
                    }
                }
                Frame::Notification { method, params } => {
                    self.notifications
                        .push_back(json!({ "method": method, "params": params }));
                }
                Frame::Response { id, result } => self.early_responses.push((id, result)),
            }
        }
    }

    /// The next buffered notification, or the next one the agent writes.
    pub fn next_notification(&mut self, deadline: Duration) -> Result<Value, AcpClientError> {
        if let Some(notification) = self.notifications.pop_front() {
            return Ok(notification);
        }
        let end = Instant::now() + deadline;
        match self.next_frame(end)? {
            Frame::Notification { method, params } => Ok(json!({ "method": method, "params": params })),
            other => Err(AcpClientError::Io(format!("expected a notification, saw {other:?}"))),
        }
    }

    pub fn drain_notifications(&mut self) -> Vec<Value> {
        self.notifications.drain(..).collect()
    }

    /// Waits for the next `idle` this session reports.
    ///
    /// `None` means the deadline passed (or the agent closed stdout) before an
    /// `idle` arrived; an `idle` that carries no stop reason answers
    /// `Some(String::new())`. This starts waiting for transitions recorded after
    /// the call; when the work may already have finished, take a cursor with
    /// [`StateTracker::cursor`] first and call [`Self::await_idle_after`].
    pub fn await_idle(&mut self, session_id: &str, timeout: Duration) -> Option<String> {
        let cursor = self.state.cursor(session_id);
        self.await_idle_after(session_id, cursor, timeout)
    }

    /// Waits for the first `idle` recorded at or after `cursor`.
    ///
    /// `None` means the deadline passed or the agent closed stdout. A schema
    /// violation or a non-JSON line panics instead: it would otherwise look
    /// exactly like "no idle before the timeout" and hide the real failure.
    pub fn await_idle_after(&mut self, session_id: &str, cursor: usize, timeout: Duration) -> Option<String> {
        let end = Instant::now() + timeout;
        loop {
            if let Some(stop_reason) = self.state.idle_since(session_id, cursor) {
                return Some(stop_reason);
            }
            if end.saturating_duration_since(Instant::now()).is_zero() {
                return None;
            }
            match self.absorb_next_frame(end) {
                Ok(()) => {}
                Err(AcpClientError::Timeout { .. } | AcpClientError::Closed) => return None,
                Err(error) => panic!("the agent's stream broke while waiting for idle: {error}"),
            }
        }
    }

    /// `session/prompt` on a v2 connection: answers the id of the message the
    /// agent inserted, which arrives before the turn runs.
    pub fn prompt_v2(&mut self, session_id: &str, blocks: Value) -> Result<String, AcpClientError> {
        let result = self.request("session/prompt", json!({"sessionId": session_id, "prompt": blocks}))?;
        result
            .get("messageId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| AcpClientError::MissingField {
                what: format!("the session/prompt response {result}"),
                field: "messageId",
            })
    }

    /// Writes one JSON-RPC batch line and returns the response array.
    ///
    /// `entries` go out as a single array line. The answer is one array line,
    /// validated entry by entry against the method each entry answers. Every
    /// frame that arrives first is handled the way the normal read loop handles
    /// it: a notification is queued, a response is buffered, and the agent's own
    /// request such as session/request_permission or elicitation/create is
    /// answered, so an unanswered request cannot leave the agent blocked before
    /// the array arrives.
    pub fn send_batch(&mut self, entries: &[Value]) -> Result<Vec<Value>, AcpClientError> {
        for entry in entries {
            if let (Some(id), Some(method)) = (entry.get("id"), entry.get("method").and_then(Value::as_str)) {
                self.parser.note_request(id, method);
            }
        }
        self.write_value(&Value::Array(entries.to_vec()))?;
        let end = Instant::now() + self.deadline;
        loop {
            let remaining = end.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(remaining) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(AcpClientError::Timeout {
                        what: "the batch response array".to_string(),
                        deadline: self.deadline,
                    });
                }
                Err(RecvTimeoutError::Disconnected) => return Err(AcpClientError::Closed),
            };
            if line.trim_start().starts_with('[') {
                return self.parser.parse_batch(&line);
            }
            self.absorb_line(&line)?;
        }
    }

    /// Parses one stdout line and records the `state_update` it may carry.
    fn parse_line(&mut self, line: &str) -> Result<Frame, AcpClientError> {
        let frame = self.parser.parse(line)?;
        if let Frame::Notification { method, params } = &frame {
            self.state.record(method, params);
        }
        Ok(frame)
    }

    /// Handles one frame read while a caller waits for something else.
    ///
    /// An agent request is answered exactly as the normal read loop answers it;
    /// a notification is queued and a response is buffered.
    fn absorb_frame(&mut self, frame: Frame) -> Result<(), AcpClientError> {
        match frame {
            Frame::Notification { method, params } => {
                self.notifications
                    .push_back(json!({ "method": method, "params": params }));
                Ok(())
            }
            Frame::Response { id, result } => {
                self.early_responses.push((id, result));
                Ok(())
            }
            Frame::Request { id, method, params } => self.answer_request(id, &method, &params),
        }
    }

    fn absorb_line(&mut self, line: &str) -> Result<(), AcpClientError> {
        let frame = self.parse_line(line)?;
        self.absorb_frame(frame)
    }

    fn absorb_next_frame(&mut self, end: Instant) -> Result<(), AcpClientError> {
        let frame = self.next_frame(end)?;
        self.absorb_frame(frame)
    }

    /// Drops the client's stdin, which is how a client closes the connection.
    pub fn close_stdin(&mut self) {
        let mut stdin = self.stdin.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(mut stdin) = stdin.take() {
            let _ = stdin.flush();
        }
    }

    pub fn wait_exit(&mut self, deadline: Duration) -> Result<std::process::ExitStatus, AcpClientError> {
        let end = Instant::now() + deadline;
        loop {
            match self
                .child
                .try_wait()
                .map_err(|error| AcpClientError::Io(format!("wait for the agent: {error}")))?
            {
                Some(status) => return Ok(status),
                None if Instant::now() >= end => {
                    return Err(AcpClientError::Timeout {
                        what: "the agent process exit".to_string(),
                        deadline,
                    });
                }
                None => {
                    let remaining = end.saturating_duration_since(Instant::now());
                    let _ = self.lines.recv_timeout(remaining.min(Duration::from_millis(20)));
                }
            }
        }
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for AcpClient {
    fn drop(&mut self) {
        self.close_stdin();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn same_id(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => {
            left.as_i64() == right.as_i64() && left.as_u64() == right.as_u64()
        }
        (Value::String(left), Value::String(right)) => left == right,
        (Value::Null, Value::Null) => true,
        _ => false,
    }
}
