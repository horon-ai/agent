#![cfg(unix)]
//! ACP v1 stdio conformance suite: one test per IS row of the ACP v1 plan.
//!
//! Every test boots its own isolated `horon-agent serve` (the `acp` tool
//! profile and a provider profile pointed at the scripted upstream) and drives the
//! shipped `horon-agent acp` binary over stdio through the mock client. Each
//! test then validates every frame in both directions against the vendored
//! `schema-v1.26.0.json` and asserts zero violations. Frames a test writes on
//! purpose to probe an error path are named as such and left out of the
//! client-direction count; the agent's answers to them are still validated.

#[path = "support/acp_client.rs"]
mod acp_client;
#[path = "support/acp_daemon.rs"]
mod acp_daemon;
#[path = "support/acp_schema.rs"]
mod acp_schema;
#[path = "support/lifecycle.rs"]
mod lifecycle;

use acp_client::{
    AcpClient, AcpClientError, AcpClientOptions, ClientCapabilities, ElicitationAction, PermissionOutcome,
};
use acp_daemon::AcpDaemon;
use acp_schema::{Direction, FrameValidator};
use lifecycle::{
    BINARY, ChangeWatch, Script, TOOL_PROMPT, scratch_root, set_mode, wait_for_pid_exit, wait_for_socket, write_config,
};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// Bound for one response or one awaited event. Model turns run through the
/// scripted provider, so a turn is seconds, not minutes; the bound only keeps a
/// broken run from hanging.
const WAIT: Duration = Duration::from_secs(120);

const ACP_ONLY_EVENTS: [&str; 4] = [
    "client_request",
    "tool_permission_requested",
    "plan_updated",
    "tool_diff",
];

struct Conn {
    client: AcpClient,
    label: String,
    /// (id, method) of every request this client wrote.
    sent: Vec<(Value, String)>,
    /// Ids of requests written on purpose with params the schema refuses.
    invalid_ids: Vec<Value>,
    /// Methods of notifications written on purpose that the schema does not know.
    invalid_methods: Vec<String>,
    /// Raw lines written on purpose that are not JSON-RPC at all.
    invalid_lines: Vec<String>,
    capabilities: ClientCapabilities,
}

impl Conn {
    fn open(daemon: &AcpDaemon, capabilities: ClientCapabilities, label: &str) -> Self {
        Self::open_env(daemon.env_pairs(), capabilities, label)
    }

    fn open_env(env: Vec<(String, String)>, capabilities: ClientCapabilities, label: &str) -> Self {
        let client = AcpClient::spawn(AcpClientOptions::new(env).capabilities(capabilities).deadline(WAIT));
        Self {
            client,
            label: label.to_string(),
            sent: Vec::new(),
            invalid_ids: Vec::new(),
            invalid_methods: Vec::new(),
            invalid_lines: Vec::new(),
            capabilities,
        }
    }

    /// `initialize` with this client's capabilities, with `extra` merged into
    /// `clientCapabilities` (for example `auth.terminal`, `session.notices`).
    fn initialize(&mut self, extra: Value) -> Value {
        let mut capabilities = self.capabilities.to_json();
        if let (Some(target), Some(extra)) = (capabilities.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                target.insert(key.clone(), value.clone());
            }
        }
        let result = self.ok(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": capabilities,
                "clientInfo": {"name": "acp-stdio-suite", "version": "1.0.0"},
            }),
        );
        assert_eq!(result["protocolVersion"], 1, "{result}");
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

    /// A request whose params break the schema on purpose.
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

    fn notify_invalid(&mut self, method: &str, params: Value) {
        self.invalid_methods.push(method.to_string());
        self.notify(method, params);
    }

    fn write_invalid_line(&mut self, line: &str) {
        self.invalid_lines.push(line.to_string());
        self.client
            .write_line(line)
            .unwrap_or_else(|error| panic!("{}: write: {error}", self.label));
    }

    fn new_session(&mut self, cwd: &Path) -> String {
        self.new_session_with(json!({"cwd": cwd_str(cwd), "mcpServers": []}))
    }

    fn new_session_with(&mut self, params: Value) -> String {
        let created = self.ok("session/new", params);
        created["sessionId"].as_str().expect("a session id").to_string()
    }

    /// The prompt result and the session/update payloads written before it.
    fn prompt(&mut self, session_id: &str, text: &str) -> (Result<Value, Value>, Vec<Value>) {
        self.client.drain_notifications();
        let result = self.call(
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": text}]}),
        );
        (result, session_updates(&self.client.drain_notifications()))
    }

    fn prompt_ok(&mut self, session_id: &str, text: &str) -> (String, Vec<Value>) {
        let (result, updates) = self.prompt(session_id, text);
        let result = result.unwrap_or_else(|error| panic!("{}: prompt {text:?}: {error}", self.label));
        (
            result["stopReason"].as_str().expect("a stopReason").to_string(),
            updates,
        )
    }

    fn set_mode(&mut self, session_id: &str, mode_id: &str) {
        assert_eq!(
            self.ok("session/set_mode", json!({"sessionId": session_id, "modeId": mode_id})),
            json!({})
        );
    }

    /// `_horon/barrier` is unknown, so it is answered -32601 in order after
    /// everything written before it: once it is answered, no earlier frame is
    /// still on its way.
    fn barrier(&mut self) {
        let error = self.err("_horon/barrier", json!({}));
        assert_eq!(error["code"], -32601, "{error}");
    }

    /// The next session/update of `kind` (earlier ones of other kinds are
    /// skipped), whether it was already buffered or is still to arrive.
    fn await_update(&mut self, kind: &str) -> Value {
        loop {
            let note = self
                .client
                .next_notification(WAIT)
                .unwrap_or_else(|error| panic!("{}: waiting for {kind}: {error}", self.label));
            if note["method"] == "session/update" && note["params"]["update"]["sessionUpdate"] == kind {
                return note["params"]["update"].clone();
            }
        }
    }

    fn stdout(&self) -> Vec<String> {
        self.client.stdout_lines()
    }

    /// Validates every frame of this connection in both directions and prints
    /// the tally; returns the number of violations.
    fn schema_violations(&self) -> usize {
        let mut agent = FrameValidator::new(Direction::AgentToClient);
        for (id, method) in &self.sent {
            agent.note_request(id, method);
        }
        let mut client = FrameValidator::new(Direction::ClientToAgent);
        let mut violations = 0;
        let stdout = self.client.stdout_lines();
        for line in &stdout {
            let value: Value = serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("{}: stdout line is not JSON ({error}): {line}", self.label));
            if let (Some(method), Some(id)) = (value.get("method").and_then(Value::as_str), value.get("id")) {
                client.note_request(id, method);
            }
            for violation in agent.validate(&value) {
                violations += 1;
                eprintln!("{}-VIOLATION agent->client {violation} in {line}", self.label);
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
            let method = value.get("method").and_then(Value::as_str);
            let probe = match (method, value.get("id")) {
                (Some(_), Some(id)) => self.invalid_ids.contains(id),
                (Some(method), None) => self.invalid_methods.iter().any(|name| name == method),
                (None, _) => false,
            };
            if probe {
                deliberate += 1;
                continue;
            }
            for violation in client.validate(&value) {
                violations += 1;
                eprintln!("{}-VIOLATION client->agent {violation} in {line}", self.label);
            }
        }
        eprintln!(
            "IS-SCHEMA {} agent_frames={} client_frames={} deliberate_error_probes={deliberate} violations={violations}",
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

fn session_updates(notifications: &[Value]) -> Vec<Value> {
    notifications
        .iter()
        .filter(|note| note["method"] == "session/update")
        .map(|note| note["params"]["update"].clone())
        .collect()
}

fn kinds(updates: &[Value]) -> Vec<String> {
    updates
        .iter()
        .filter_map(|update| update["sessionUpdate"].as_str().map(str::to_string))
        .collect()
}

fn text_of(updates: &[Value], kind: &str) -> String {
    updates
        .iter()
        .filter(|update| update["sessionUpdate"] == kind)
        .filter_map(|update| update["content"]["text"].as_str())
        .collect()
}

fn mentions(values: &[Value], needle: &str) -> bool {
    values.iter().any(|value| value.to_string().contains(needle))
}

fn assert_not_found(error: &Value, session_id: &str) {
    assert_eq!(error["code"], -32002, "{error}");
    assert_eq!(error["data"]["sessionId"], session_id, "{error}");
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

/// A response the provider cut at its output limit. The daemon asks for a
/// continuation (a user reminder naming the stop reason) up to three times
/// before it stops the turn, so the same script also answers that reminder.
fn register_output_limit(daemon: &AcpDaemon, needle: &str, text: &str) {
    let script = Script::Frames {
        frames: vec![
            lifecycle::chat_chunk(json!({"role": "assistant", "content": text}), None),
            lifecycle::chat_chunk(json!({}), Some("length")),
        ],
        follow_up: ("call_unused_output_limit".to_string(), String::new()),
    };
    daemon.provider.script(needle, script.clone());
    daemon.provider.script("stop_reason: length", script);
}

fn completed() -> Value {
    lifecycle::chat_chunk(json!({}), Some("tool_calls"))
}

/// The provider request body that contains every needle; the turn that sent
/// it has already been answered, so the body is present.
fn provider_body(daemon: &AcpDaemon, needles: &[&str]) -> String {
    daemon
        .provider
        .requests()
        .into_iter()
        .find(|body| needles.iter().all(|needle| body.contains(needle)))
        .unwrap_or_else(|| panic!("no provider request contains all of {needles:?}\n{}", daemon.logs()))
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

/// Waits until a daemon log line contains `needle`. The wait is driven by the
/// file-change events of the log directory and its files: the watches are
/// registered before each scan, so a line written between the scan and the
/// wait still wakes it.
fn wait_for_log(home: &Path, needle: &str, deadline: Duration) {
    let dir = home.join("logs");
    let end = Instant::now() + deadline;
    loop {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
            .unwrap_or_default();
        files.retain(|path| path.extension().is_some_and(|ext| ext == "log"));
        let mut watched: Vec<&Path> = vec![home, &dir];
        watched.extend(files.iter().map(PathBuf::as_path));
        let watch = ChangeWatch::arm(&watched);
        let found = files
            .iter()
            .any(|path| std::fs::read_to_string(path).is_ok_and(|text| text.contains(needle)));
        if found {
            return;
        }
        let remaining = end.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            panic!("no daemon log line contains {needle:?} within {deadline:?}");
        }
        watch.wait(remaining);
    }
}

/// Waits for `path` to exist and returns its text, on the directory-change
/// events of its parent.
fn wait_for_file(path: &Path, deadline: Duration) -> String {
    let end = Instant::now() + deadline;
    let parent = path.parent().expect("a parent dir");
    // The directory must exist before the watch is armed; a missing one is a
    // broken fixture, not a slow file.
    std::fs::File::open(parent).expect("the parent dir");
    loop {
        let watch = ChangeWatch::arm(&[parent]);
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.trim().is_empty()
        {
            return text;
        }
        let remaining = end.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            panic!("{} did not appear within {deadline:?}", path.display());
        }
        watch.wait(remaining);
    }
}

/// argv[1] is a file the server writes its pid to on `initialize`, argv[2] the
/// name of its one tool. `tools/call` answers with the token it was given and
/// the value of `MCP_FIXTURE_ENV`, so a test sees both arguments and env pass.
const MCP_SERVER: &str = r#"
import json, os, sys
pid_file, tool = sys.argv[1], sys.argv[2]
for line in sys.stdin:
    req = json.loads(line)
    method = req.get('method')
    if 'id' not in req:
        continue
    reply = {'jsonrpc': '2.0', 'id': req['id']}
    if method == 'initialize':
        with open(pid_file + '.tmp', 'w') as out:
            out.write(str(os.getpid()))
        os.replace(pid_file + '.tmp', pid_file)
        reply['result'] = {'protocolVersion': '2024-11-05', 'capabilities': {'tools': {}},
                           'serverInfo': {'name': 'acp-stdio-fixture', 'version': '1'}}
    elif method == 'tools/list':
        reply['result'] = {'tools': [{'name': tool, 'description': 'acp stdio fixture probe',
                           'inputSchema': {'type': 'object', 'properties': {'token': {'type': 'string'}}}}]}
    elif method == 'tools/call':
        token = req['params'].get('arguments', {}).get('token', '')
        text = 'mcp-probe-ok:' + token + ':' + os.environ.get('MCP_FIXTURE_ENV', '')
        reply['result'] = {'content': [{'type': 'text', 'text': text}], 'isError': False}
    else:
        reply['error'] = {'code': -32601, 'message': 'unknown method'}
    print(json.dumps(reply), flush=True)
"#;

fn mcp_server(name: &str, tool: &str, pid_file: &Path) -> Value {
    json!({
        "name": name,
        "command": "python3",
        "args": ["-c", MCP_SERVER, cwd_str(pid_file), tool],
        "env": [{"name": "MCP_FIXTURE_ENV", "value": format!("{name}-env-ok")}],
    })
}

fn assert_python3() {
    let status = Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    assert!(
        status.is_ok_and(|status| status.success()),
        "python3 runs the stdio MCP fixture server"
    );
}

fn read_pid(path: &Path) -> i32 {
    wait_for_file(path, WAIT).trim().parse().expect("a pid")
}

struct PlainClient {
    writer: UnixStream,
    lines: Receiver<Value>,
    session_id: String,
    /// Every event this client ever received.
    events: Vec<Value>,
    next_id: u64,
}

impl PlainClient {
    fn connect(socket: &Path, working_dir: &Path, target_session_id: Option<&str>) -> Self {
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
        let mut client = Self {
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
        let events = client.request(subscribe);
        client.session_id = events
            .iter()
            .rev()
            .find_map(|event| match event["type"].as_str() {
                Some("session") | Some("history") => event["session_id"].as_str().map(str::to_string),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the subscribe names its session: {events:?}"));
        client
    }

    /// Writes one request and returns every event up to its `done`.
    fn request(&mut self, request: Value) -> Vec<Value> {
        let id = request["id"].as_u64().expect("a request id");
        self.next_id = self.next_id.max(id + 1);
        writeln!(self.writer, "{request}").expect("write to the daemon");
        self.writer.flush().expect("flush to the daemon");
        let end = Instant::now() + WAIT;
        let mut seen = Vec::new();
        loop {
            let remaining = end.saturating_duration_since(Instant::now());
            let event = match self.lines.recv_timeout(remaining) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => panic!("no done for request {id} within {WAIT:?}: {seen:?}"),
                Err(RecvTimeoutError::Disconnected) => panic!("the daemon closed the plain connection: {seen:?}"),
            };
            self.events.push(event.clone());
            seen.push(event.clone());
            if event["id"] == id {
                match event["type"].as_str() {
                    Some("done") => return seen,
                    Some("error") => panic!("the daemon refused request {id}: {event}"),
                    _ => {}
                }
            }
        }
    }

    fn message(&mut self, content: &str) -> Vec<Value> {
        let id = self.next_id;
        self.request(json!({"type": "message", "id": id, "content": content}))
    }

    fn acp_only_events(&self) -> Vec<&Value> {
        self.events
            .iter()
            .filter(|event| {
                event["type"]
                    .as_str()
                    .is_some_and(|kind| ACP_ONLY_EVENTS.contains(&kind))
            })
            .collect()
    }
}

struct NoModelDaemon {
    root: PathBuf,
    child: Child,
}

impl NoModelDaemon {
    fn start() -> Self {
        let root = scratch_root("acpnomodel");
        for dir in ["home", "rt", "work"] {
            std::fs::create_dir_all(root.join(dir)).expect("scratch dirs");
        }
        for dir in [root.clone(), root.join("home"), root.join("rt")] {
            set_mode(&dir, 0o700);
        }
        write_config(&root.join("home"));
        let log = std::fs::File::create(root.join("rt/daemon.log")).expect("daemon log");
        let errors = log.try_clone().expect("daemon log clone");
        let mut command = Command::new(BINARY);
        command.args(["serve", "--no-update"]).env_clear();
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        for (key, value) in Self::env_for(&root) {
            command.env(key, value);
        }
        let child = command
            .env_remove("HORON_AGENT_API_SOCKET")
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(Stdio::from(errors))
            .spawn()
            .expect("spawn the no-model daemon");
        let daemon = Self { root, child };
        wait_for_socket(&daemon.root.join("rt/horon-agent.sock"), WAIT);
        daemon
    }

    fn env_for(root: &Path) -> Vec<(String, String)> {
        let path = |rel: &str| root.join(rel).display().to_string();
        vec![
            ("HORON_AGENT_HOME".to_string(), path("home")),
            ("HORON_AGENT_RUNTIME_DIR".to_string(), path("rt")),
            ("HOME".to_string(), root.display().to_string()),
            ("XDG_CONFIG_HOME".to_string(), path("config")),
            ("XDG_CACHE_HOME".to_string(), path("cache")),
            ("HORON_AGENT_SOCKET".to_string(), path("rt/horon-agent.sock")),
            ("HORON_AGENT_NO_AUTO_UPDATE".to_string(), "1".to_string()),
            ("HORON_AGENT_MEMORY_ENABLED".to_string(), "0".to_string()),
            ("HORON_AGENT_MEMORY_SIDECAR_ENABLED".to_string(), "0".to_string()),
            ("HORON_AGENT_NON_INTERACTIVE".to_string(), "1".to_string()),
            ("HORON_AGENT_TOOL_PROFILE".to_string(), "acp".to_string()),
        ]
    }

    fn env(&self) -> Vec<(String, String)> {
        Self::env_for(&self.root)
    }
}

impl Drop for NoModelDaemon {
    fn drop(&mut self) {
        if std::thread::panicking() {
            dump_logs(&self.root, &self.root.join("home"));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// IS-1: a strict handshake. Any request before `initialize` is -32600, a
/// missing or non-integer protocolVersion is -32602, the answer is always
/// version 1 (also for 65535), and a repeated initialize answers the same
/// result while refreshing the stored client capabilities.
#[test]
fn is1_handshake() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    // 65535 is answered with the v1 result: the v2 surface must be off for it.
    let mut env = daemon.env_pairs();
    env.push(("HORON_AGENT_ACP_PROTOCOL_V2".to_string(), "0".to_string()));
    let mut conn = Conn::open_env(env, ClientCapabilities::default(), "is1");

    let early = conn.err("session/new", json!({"cwd": cwd_str(&work), "mcpServers": []}));
    assert_eq!(early["code"], -32600, "request before initialize: {early}");
    let early = conn.err("session/list", json!({}));
    assert_eq!(early["code"], -32600, "{early}");

    let missing = conn.err_invalid("initialize", json!({"clientCapabilities": {}}));
    assert_eq!(missing["code"], -32602, "missing protocolVersion: {missing}");
    let string = conn.err_invalid("initialize", json!({"protocolVersion": "1"}));
    assert_eq!(string["code"], -32602, "string protocolVersion: {string}");
    let fraction = conn.err_invalid("initialize", json!({"protocolVersion": 1.5}));
    assert_eq!(fraction["code"], -32602, "non-integer protocolVersion: {fraction}");
    let still_early = conn.err("session/new", json!({"cwd": cwd_str(&work), "mcpServers": []}));
    assert_eq!(
        still_early["code"], -32600,
        "a refused initialize does not initialize: {still_early}"
    );

    let params = json!({
        "protocolVersion": 65535,
        "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
        "clientInfo": {"name": "is1-client", "version": "1.0.0"},
    });
    let first = conn.ok("initialize", params.clone());
    assert_eq!(first["protocolVersion"], 1, "{first}");
    assert_eq!(first["agentCapabilities"]["loadSession"], true, "{first}");
    for capability in ["close", "resume", "list", "delete", "additionalDirectories"] {
        assert!(
            first["agentCapabilities"]["sessionCapabilities"][capability].is_object(),
            "sessionCapabilities.{capability}: {first}"
        );
    }
    assert_eq!(first["agentInfo"]["name"], "horon-agent", "{first}");
    let auth_ids: Vec<&str> = first["authMethods"]
        .as_array()
        .expect("authMethods")
        .iter()
        .filter_map(|method| method["id"].as_str())
        .collect();
    assert_eq!(auth_ids, ["horon-agent-credentials"], "{first}");

    let again = conn.ok("initialize", params);
    assert_eq!(again, first, "a repeated initialize answers the same result");
    let refreshed = conn.ok(
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {"auth": {"terminal": true}}}),
    );
    let auth_ids: Vec<&str> = refreshed["authMethods"]
        .as_array()
        .expect("authMethods")
        .iter()
        .filter_map(|method| method["id"].as_str())
        .collect();
    assert_eq!(
        auth_ids,
        ["horon-agent-credentials", "horon-agent-terminal-login"],
        "the repeated initialize refreshed the stored capabilities: {refreshed}"
    );

    let session_id = conn.new_session(&work);
    assert!(!session_id.is_empty());
    conn.assert_schema_clean();
}

/// IS-2: nothing capability-gated is ever requested from a client that did not
/// advertise it (the tools fall back to the local disk, the local shell and the
/// ask policy), unknown `_` requests and unknown methods are -32601, unknown
/// notifications are ignored and no notification is ever answered, and a
/// second prompt on a busy session is -32602 rather than -32000.
#[test]
fn is2_gating() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    let fixture = work.join("is2-disk.txt");
    std::fs::write(&fixture, "is2 disk text\n").expect("fixture");
    daemon.provider.script(
        "IS2_READ",
        tool_turn("read", json!({"file_path": cwd_str(&fixture)}), "is2 read done"),
    );
    daemon.provider.script(
        "IS2_BASH",
        tool_turn(
            "bash",
            json!({"command": "echo is2-$((3*3))-ran", "intent": "is2 gating fixture"}),
            "is2 bash done",
        ),
    );
    daemon.provider.script(
        "IS2_ASK",
        tool_turn(
            "ask",
            json!({"questions": [{"id": "pick", "question": "Which one?", "options": [{"label": "is2-alpha"}, {"label": "is2-beta"}], "recommended": 1}]}),
            "is2 ask done",
        ),
    );
    daemon
        .provider
        .script("IS2_HOLD", Script::HoldOpen("is2 partial ".to_string()));

    let mut conn = Conn::open(&daemon, ClientCapabilities::default(), "is2");
    conn.ok("initialize", json!({"protocolVersion": 1, "clientCapabilities": {}}));

    let session = conn.new_session(&work);
    let (stop, _) = conn.prompt_ok(&session, "IS2_READ read the fixture");
    assert_eq!(stop, "end_turn");
    let body = provider_body(&daemon, &["tool_call_id", "IS2_READ"]);
    assert!(body.contains("is2 disk text"), "read used the local disk: {body}");

    let session = conn.new_session(&work);
    let (stop, updates) = conn.prompt_ok(&session, "IS2_BASH run it");
    assert_eq!(stop, "end_turn");
    assert!(mentions(&updates, "is2-9-ran"), "bash ran locally: {updates:?}");

    let session = conn.new_session(&work);
    let (stop, _) = conn.prompt_ok(&session, "IS2_ASK ask me");
    assert_eq!(stop, "end_turn");
    let body = provider_body(&daemon, &["tool_call_id", "IS2_ASK"]);
    assert!(
        body.contains("is2-beta"),
        "ask fell back to the recommended option: {body}"
    );

    let stdout = conn.stdout();
    for prefix in [
        "\"method\":\"fs/",
        "\"method\":\"terminal/",
        "\"method\":\"elicitation/",
    ] {
        assert!(
            stdout.iter().all(|line| !line.contains(prefix)),
            "no {prefix} request without the capability"
        );
    }
    assert!(conn.client.handlers().fs.read_paths().is_empty());
    assert!(conn.client.handlers().terminal.calls().is_empty());
    assert!(conn.client.handlers().elicitation.requests().is_empty());

    let underscore = conn.err("_horon/unknown_extension", json!({}));
    assert_eq!(underscore["code"], -32601, "{underscore}");
    let unknown = conn.err_invalid("horon/not_a_method", json!({}));
    assert_eq!(unknown["code"], -32601, "{unknown}");

    let before = conn.stdout().len();
    conn.notify_invalid("horon/unknown_notification", json!({"x": 1}));
    conn.notify("session/cancel", json!({"sessionId": session}));
    conn.barrier();
    assert_eq!(
        conn.stdout().len(),
        before + 1,
        "notifications are never answered: {:?}",
        &conn.stdout()[before..]
    );

    let busy_session = conn.new_session(&work);
    let prompt_id = conn.send(
        "session/prompt",
        json!({"sessionId": busy_session, "prompt": [{"type": "text", "text": "IS2_HOLD stream"}]}),
    );
    daemon
        .provider
        .wait_for_hold_open_opened(WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", daemon.logs()));
    let busy = conn.err(
        "session/prompt",
        json!({"sessionId": busy_session, "prompt": [{"type": "text", "text": "IS2_HOLD again"}]}),
    );
    assert_eq!(
        busy["code"], -32602,
        "a busy prompt is invalid params, never -32000: {busy}"
    );
    conn.notify("session/cancel", json!({"sessionId": busy_session}));
    let cancelled = conn
        .wait("session/prompt", &prompt_id)
        .expect("the held prompt answers");
    assert_eq!(cancelled["stopReason"], "cancelled", "{cancelled}");
    conn.assert_schema_clean();
}

/// IS-3: `$/cancel_request` both ways. The client cancels an in-flight prompt
/// and gets exactly one response (cancelled); an unknown id is harmless; the
/// agent issues its own string ids for client requests and cancels its open
/// permission request when the prompt is cancelled.
#[test]
fn is3_cancel_request() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    daemon
        .provider
        .script("IS3_HOLD", Script::HoldOpen("is3 partial ".to_string()));
    daemon.provider.script(
        "IS3_BASH",
        tool_turn(
            "bash",
            json!({"command": "echo is3-$((7*3))-ran", "intent": "is3 cancel fixture"}),
            "is3 bash done",
        ),
    );
    let mut conn = Conn::open(&daemon, ClientCapabilities::default(), "is3");
    conn.initialize(json!({}));

    let session = conn.new_session(&work);
    let prompt_id = conn.send(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "IS3_HOLD stream"}]}),
    );
    daemon
        .provider
        .wait_for_hold_open_opened(WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", daemon.logs()));
    conn.notify("$/cancel_request", json!({"requestId": prompt_id}));
    let response = conn.wait("session/prompt", &prompt_id).expect("the prompt answers");
    assert_eq!(response["stopReason"], "cancelled", "{response}");
    daemon
        .provider
        .wait_for_hold_open_closed(WAIT)
        .expect("the daemon drops the provider stream");
    conn.notify("$/cancel_request", json!({"requestId": 987_654_321}));
    conn.notify("$/cancel_request", json!({"requestId": "horon-no-such-request"}));
    conn.barrier();
    let answers = conn
        .stdout()
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|frame| frame.get("method").is_none() && frame["id"] == prompt_id)
        .count();
    assert_eq!(answers, 1, "exactly one response for the cancelled prompt");

    let session = conn.new_session(&work);
    conn.set_mode(&session, "ask-commands");
    conn.client.handlers().permission.script([PermissionOutcome::Hold]);
    let prompt_id = conn.send(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "IS3_BASH run it"}]}),
    );
    let permission_id = conn
        .client
        .await_agent_request("session/request_permission", WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", daemon.logs()));
    assert!(
        permission_id.as_str().is_some_and(|id| id.starts_with("horon-")),
        "the agent uses its own string ids for client requests: {permission_id}"
    );
    conn.notify("$/cancel_request", json!({"requestId": prompt_id}));
    let response = conn.wait("session/prompt", &prompt_id).expect("the prompt answers");
    assert_eq!(response["stopReason"], "cancelled", "{response}");
    let notifications = conn.client.drain_notifications();
    assert!(
        notifications
            .iter()
            .any(|note| note["method"] == "$/cancel_request" && note["params"]["requestId"] == permission_id),
        "the agent cancels its open permission request: {notifications:?}"
    );
    conn.barrier();
    assert!(
        conn.stdout().iter().all(|line| !line.contains("is3-21-ran")),
        "the held command never ran"
    );
    conn.assert_schema_clean();
}

/// IS-4: session/new (cwd required and absolute, additionalDirectories reach
/// the model), session/list (cwd filter, bad cursor, empty), session/close
/// (in-flight prompt ends cancelled, then -32002), unknown sessions -32002 with
/// data.sessionId, session/resume without replay, session/delete idempotent,
/// and session/load replaying the history faithfully before its response.
#[test]
fn is4_lifecycle() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    let extra = daemon.root.join("extra-dir");
    std::fs::create_dir_all(&extra).expect("extra dir");
    let fixture = work.join("is4-fixture.txt");
    std::fs::write(&fixture, "is4 tool fixture\n").expect("fixture");
    daemon
        .provider
        .script("IS4_FIRST", Script::Text("is4 first reply".to_string()));
    daemon
        .provider
        .script("IS4_HOLD", Script::HoldOpen("is4 partial ".to_string()));
    daemon.provider.script(
        "IS4_TOOL",
        tool_turn("read", json!({"file_path": cwd_str(&fixture)}), "is4 tool done"),
    );
    let mut conn = Conn::open(&daemon, ClientCapabilities::default(), "is4a");
    conn.initialize(json!({}));

    let missing = conn.err_invalid("session/new", json!({"mcpServers": []}));
    assert_eq!(missing["code"], -32602, "{missing}");
    let relative = conn.err("session/new", json!({"cwd": "work", "mcpServers": []}));
    assert_eq!(relative["code"], -32602, "{relative}");
    let relative_dir = conn.err(
        "session/new",
        json!({"cwd": cwd_str(&work), "mcpServers": [], "additionalDirectories": ["extra-dir"]}),
    );
    assert_eq!(relative_dir["code"], -32602, "{relative_dir}");

    let first = conn.new_session_with(json!({
        "cwd": cwd_str(&work),
        "mcpServers": [],
        "additionalDirectories": [cwd_str(&extra)],
    }));
    let (stop, updates) = conn.prompt_ok(&first, "IS4_FIRST hello");
    assert_eq!(stop, "end_turn");
    assert!(text_of(&updates, "agent_message_chunk").contains("is4 first reply"));
    let body = provider_body(&daemon, &["IS4_FIRST"]);
    assert!(
        body.contains("Additional directories:") && body.contains(&cwd_str(&extra)),
        "additionalDirectories reach the model's request"
    );

    let tool_session = conn.new_session(&work);
    let (stop, _) = conn.prompt_ok(&tool_session, "IS4_TOOL read the fixture");
    assert_eq!(stop, "end_turn");

    let listed = conn.ok("session/list", json!({}));
    let sessions = listed["sessions"].as_array().expect("sessions");
    let row = sessions
        .iter()
        .find(|info| info["sessionId"] == first.as_str())
        .unwrap_or_else(|| panic!("the prompted session is listed: {listed}"));
    assert_eq!(row["cwd"], cwd_str(&work), "{row}");
    for info in sessions {
        assert!(Path::new(info["cwd"].as_str().expect("cwd")).is_absolute(), "{info}");
    }
    let filtered = conn.ok("session/list", json!({"cwd": cwd_str(&work)}));
    assert!(
        filtered["sessions"]
            .as_array()
            .expect("sessions")
            .iter()
            .any(|info| info["sessionId"] == first.as_str()),
        "{filtered}"
    );
    let empty = conn.ok("session/list", json!({"cwd": "/nowhere/is4"}));
    assert_eq!(empty, json!({"sessions": []}));
    let bad_cursor = conn.err("session/list", json!({"cursor": "!!!!"}));
    assert_eq!(bad_cursor["code"], -32602, "{bad_cursor}");

    let held = conn.new_session(&work);
    let prompt_id = conn.send(
        "session/prompt",
        json!({"sessionId": held, "prompt": [{"type": "text", "text": "IS4_HOLD stream"}]}),
    );
    daemon
        .provider
        .wait_for_hold_open_opened(WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", daemon.logs()));
    let close_id = conn.send("session/close", json!({"sessionId": held}));
    let response = conn.wait("session/prompt", &prompt_id).expect("the prompt answers");
    assert_eq!(
        response["stopReason"], "cancelled",
        "close cancels the prompt: {response}"
    );
    assert_eq!(conn.wait("session/close", &close_id), Ok(json!({})));

    assert_eq!(conn.ok("session/close", json!({"sessionId": first})), json!({}));
    let again = conn.err("session/close", json!({"sessionId": first}));
    assert_not_found(&again, &first);
    let closed_prompt = conn.err(
        "session/prompt",
        json!({"sessionId": first, "prompt": [{"type": "text", "text": "after close"}]}),
    );
    assert_not_found(&closed_prompt, &first);
    let unknown = "session_nope_1700000000000_0123456789abcdef";
    for method in ["session/load", "session/resume"] {
        let error = conn.err(
            method,
            json!({"sessionId": unknown, "cwd": cwd_str(&work), "mcpServers": []}),
        );
        assert_not_found(&error, unknown);
    }
    conn.assert_schema_clean();

    let mut second = Conn::open(&daemon, ClientCapabilities::default(), "is4b");
    second.initialize(json!({}));
    second.client.drain_notifications();
    let loaded = second.ok(
        "session/load",
        json!({"sessionId": first, "cwd": cwd_str(&work), "mcpServers": []}),
    );
    assert!(loaded.is_object(), "{loaded}");
    let replay = session_updates(&second.client.drain_notifications());
    assert!(
        text_of(&replay, "user_message_chunk").contains("IS4_FIRST hello"),
        "load replays the user turn before answering: {replay:?}"
    );
    assert!(
        text_of(&replay, "agent_message_chunk").contains("is4 first reply"),
        "load replays the agent turn before answering: {replay:?}"
    );
    let resumed = second.ok(
        "session/resume",
        json!({"sessionId": tool_session, "cwd": cwd_str(&work), "mcpServers": []}),
    );
    assert!(resumed.is_object(), "{resumed}");
    let resume_updates = session_updates(&second.client.drain_notifications());
    assert!(
        resume_updates.iter().all(|update| !matches!(
            update["sessionUpdate"].as_str(),
            Some(
                "user_message_chunk" | "agent_message_chunk" | "agent_thought_chunk" | "tool_call" | "tool_call_update"
            )
        )),
        "resume never replays: {resume_updates:?}"
    );

    for _ in 0..2 {
        assert_eq!(second.ok("session/delete", json!({"sessionId": first})), json!({}));
    }
    assert_eq!(
        second.ok(
            "session/delete",
            json!({"sessionId": "session_never_1_0123456789abcdef"})
        ),
        json!({})
    );
    let after = second.ok("session/list", json!({"cwd": cwd_str(&work)}));
    assert!(
        after["sessions"]
            .as_array()
            .expect("sessions")
            .iter()
            .all(|info| info["sessionId"] != first.as_str()),
        "a deleted session leaves the list: {after}"
    );
    second.assert_schema_clean();

    // Faithful replay (todo 13): the tool row comes back as a completed
    // tool_call with its input and absolute location, then its result.
    let mut third = Conn::open(&daemon, ClientCapabilities::default(), "is4c");
    third.initialize(json!({}));
    third.client.drain_notifications();
    third.ok(
        "session/load",
        json!({"sessionId": tool_session, "cwd": cwd_str(&work), "mcpServers": []}),
    );
    let replay = session_updates(&third.client.drain_notifications());
    let fixture_path = cwd_str(&fixture);
    let call = replay
        .iter()
        .find(|update| update["sessionUpdate"] == "tool_call")
        .unwrap_or_else(|| panic!("load replays the tool row as a tool_call: {:?}", kinds(&replay)));
    assert_eq!(call["status"], "completed", "{call}");
    assert_eq!(call["kind"], "read", "{call}");
    assert_eq!(call["rawInput"]["file_path"], fixture_path, "{call}");
    assert_eq!(call["locations"], json!([{"path": fixture_path}]), "{call}");
    assert!(
        replay.iter().any(|update| update["sessionUpdate"] == "tool_call_update"
            && update["toolCallId"] == call["toolCallId"]
            && update["status"] == "completed"
            && update.to_string().contains("is4 tool fixture")),
        "the replayed tool result: {replay:?}"
    );
    assert!(
        text_of(&replay, "user_message_chunk").contains("IS4_TOOL"),
        "{replay:?}"
    );
    third.assert_schema_clean();
}

/// IS-5: true stop reasons (end_turn, max_tokens, refusal, cancelled), every update of
/// a turn written before its response, no non-spec root field on the
/// response, and a cancelled turn that never leaks into the next prompt.
#[test]
fn is5_stop_reasons() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    daemon
        .provider
        .script("IS5_END", Script::Text("is5 end reply".to_string()));
    daemon
        .provider
        .script("IS5_HOLD", Script::HoldOpen("is5 partial ".to_string()));
    daemon
        .provider
        .script("IS5_AFTER", Script::Text("is5 after reply".to_string()));
    register_output_limit(&daemon, "IS5_LIMIT", "is5 truncated");
    daemon.provider.script(
        "IS5_REFUSE",
        Script::Frames {
            frames: vec![lifecycle::chat_chunk(json!({}), Some("content_filter"))],
            follow_up: ("call_is5_unused".to_string(), String::new()),
        },
    );
    let mut conn = Conn::open(&daemon, ClientCapabilities::default(), "is5");
    conn.initialize(json!({}));

    let session = conn.new_session(&work);
    let (result, updates) = conn.prompt(&session, "IS5_END reply");
    let result = result.expect("end_turn result");
    assert_eq!(result["stopReason"], "end_turn", "{result}");
    assert!(
        result
            .as_object()
            .expect("a result object")
            .keys()
            .all(|key| key == "stopReason" || key == "_meta"),
        "no non-spec root fields: {result}"
    );
    assert!(text_of(&updates, "agent_message_chunk").contains("is5 end reply"));
    let answered = conn.stdout().len();
    conn.barrier();
    assert!(
        conn.stdout()[answered..]
            .iter()
            .all(|line| !line.contains("session/update")),
        "every update precedes the response"
    );

    let session = conn.new_session(&work);
    let (stop, updates) = conn.prompt_ok(&session, "IS5_LIMIT go");
    assert_eq!(stop, "max_tokens", "{updates:?}\n{}", daemon.logs());

    let session = conn.new_session(&work);
    let (stop, updates) = conn.prompt_ok(&session, "IS5_REFUSE go");
    assert_eq!(stop, "refusal", "{updates:?}\n{}", daemon.logs());

    let session = conn.new_session(&work);
    let prompt_id = conn.send(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "IS5_HOLD stream"}]}),
    );
    daemon
        .provider
        .wait_for_hold_open_opened(WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", daemon.logs()));
    conn.notify("session/cancel", json!({"sessionId": session}));
    let response = conn.wait("session/prompt", &prompt_id).expect("the prompt answers");
    assert_eq!(response["stopReason"], "cancelled", "{response}");
    let answered_at = conn
        .stdout()
        .iter()
        .rposition(|line| line.contains("\"stopReason\""))
        .expect("the response line");

    // 10a: the next prompt on the same session starts right away; nothing of
    // the cancelled turn reaches it, and it ends on its own stop reason.
    let (stop, after) = conn.prompt_ok(&session, "IS5_AFTER next");
    assert_eq!(stop, "end_turn", "the next prompt is not the cancelled turn: {after:?}");
    assert!(
        text_of(&after, "agent_message_chunk").contains("is5 after reply"),
        "{after:?}"
    );
    assert!(
        !mentions(&after, "is5 partial"),
        "the cancelled turn's frames never reach the next prompt: {after:?}"
    );
    let lines = conn.stdout();
    let leaked: Vec<&String> = lines[answered_at + 1..]
        .iter()
        .filter(|line| line.contains("is5 partial"))
        .collect();
    assert!(
        leaked.is_empty(),
        "frames of the cancelled turn after its response: {leaked:?}"
    );
    conn.assert_schema_clean();
}

/// IS-6: the turn streams a thought chunk, a plan from the to-do list, a tool
/// call with kind, title, absolute locations and rawInput, a completed edit
/// carrying its diff, message chunks with messageIds, mode/config updates, and
/// notices only for a client that advertised `session.notices`.
#[test]
fn is6_streaming() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    let fixture = work.join("is6-fixture.txt");
    std::fs::write(&fixture, "is6 before line\n").expect("fixture");
    let fixture_path = cwd_str(&fixture);
    let mut frames = vec![lifecycle::chat_chunk(
        json!({"role": "assistant", "reasoning_content": "is6 thinking about the edit"}),
        None,
    )];
    frames.extend(function_call_frames(
        "call_is6_todo",
        "todo",
        &json!({"todos": [
            {"content": "is6 edit the fixture", "status": "in_progress", "priority": "high", "id": "1", "confidence": "plausible"},
            {"content": "is6 dropped step", "status": "cancelled", "priority": "low", "id": "2", "confidence": "plausible"}
        ]}),
    ));
    frames.extend(function_call_frames(
        "call_is6_edit",
        "edit",
        &json!({"file_path": fixture_path, "old_string": "is6 before line", "new_string": "is6 after line"}),
    ));
    frames.push(completed());
    daemon.provider.script(
        "IS6_STREAM",
        Script::Frames {
            frames,
            follow_up: ("call_is6_todo".to_string(), "is6 stream complete".to_string()),
        },
    );
    register_output_limit(&daemon, "IS6_LIMIT", "is6 truncated");

    let mut conn = Conn::open(&daemon, ClientCapabilities::default(), "is6a");
    conn.initialize(json!({}));
    conn.client.drain_notifications();
    let session = conn.new_session(&work);
    let commands = conn.await_update("available_commands_update");
    assert!(
        commands["availableCommands"]
            .as_array()
            .is_some_and(|commands| !commands.is_empty()),
        "available commands follow session/new: {commands}"
    );
    let (stop, updates) = conn.prompt_ok(&session, "IS6_STREAM reason, plan, then edit");
    assert_eq!(stop, "end_turn", "{}", daemon.logs());
    let thought = updates
        .iter()
        .find(|update| update["sessionUpdate"] == "agent_thought_chunk")
        .unwrap_or_else(|| panic!("a thought chunk: {:?}", kinds(&updates)));
    assert!(thought["messageId"].is_string(), "{thought}");
    assert!(
        text_of(&updates, "agent_thought_chunk").contains("is6 thinking"),
        "{updates:?}"
    );
    let plan = updates
        .iter()
        .find(|update| update["sessionUpdate"] == "plan")
        .unwrap_or_else(|| panic!("a plan update: {:?}", kinds(&updates)));
    assert_eq!(
        plan["entries"],
        json!([{"content": "is6 edit the fixture", "priority": "high", "status": "in_progress"}]),
        "cancelled entries are left out of the plan: {plan}"
    );
    let call = updates
        .iter()
        .find(|update| update["sessionUpdate"] == "tool_call" && update["kind"] == "edit")
        .unwrap_or_else(|| panic!("an edit tool_call: {updates:?}"));
    assert!(call["title"].as_str().is_some_and(|title| !title.is_empty()), "{call}");
    // The call is announced before its arguments stream in; its input and
    // location then arrive on updates of the same toolCallId.
    let same_call: Vec<&Value> = updates
        .iter()
        .filter(|update| update["toolCallId"] == call["toolCallId"])
        .collect();
    assert!(
        same_call
            .iter()
            .any(|update| update["locations"] == json!([{"path": fixture_path}])),
        "an absolute location for the edited file: {same_call:?}"
    );
    assert!(
        same_call
            .iter()
            .any(|update| update["rawInput"]["file_path"] == fixture_path.as_str()),
        "the call's rawInput: {same_call:?}"
    );
    let diff =
        json!({"type": "diff", "path": fixture_path, "oldText": "is6 before line\n", "newText": "is6 after line\n"});
    assert!(
        updates
            .iter()
            .any(|update| update["sessionUpdate"] == "tool_call_update"
                && update["toolCallId"] == call["toolCallId"]
                && update["status"] == "completed"
                && update["content"]
                    .as_array()
                    .is_some_and(|content| content.contains(&diff))),
        "the completed edit carries its diff: {updates:?}"
    );
    let chunks: Vec<&Value> = updates
        .iter()
        .filter(|update| update["sessionUpdate"] == "agent_message_chunk")
        .collect();
    assert!(!chunks.is_empty(), "{updates:?}");
    assert!(chunks.iter().all(|chunk| chunk["messageId"].is_string()), "{chunks:?}");
    assert!(text_of(&updates, "agent_message_chunk").contains("is6 stream complete"));
    assert_eq!(std::fs::read_to_string(&fixture).expect("fixture"), "is6 after line\n");

    conn.client.drain_notifications();
    conn.set_mode(&session, "auto");
    let mode = conn.await_update("current_mode_update");
    assert_eq!(mode["currentModeId"], "auto", "{mode}");
    let options = conn.await_update("config_option_update");
    assert_eq!(options["configOptions"][0]["currentValue"], "auto", "{options}");

    let limited = conn.new_session(&work);
    let (stop, quiet) = conn.prompt_ok(&limited, "IS6_LIMIT go");
    assert_eq!(stop, "max_tokens");
    let all = session_updates(&conn.client.drain_notifications());
    for kind in ["notice", "compaction_update"] {
        assert!(
            !kinds(&quiet).iter().chain(kinds(&all).iter()).any(|seen| seen == kind),
            "no {kind} without the client capability: {quiet:?}"
        );
    }
    conn.assert_schema_clean();

    let mut noticed = Conn::open(&daemon, ClientCapabilities::default(), "is6b");
    noticed.initialize(json!({"session": {"notices": {}}}));
    let session = noticed.new_session(&work);
    let (stop, updates) = noticed.prompt_ok(&session, "IS6_LIMIT go");
    assert_eq!(stop, "max_tokens");
    assert!(
        updates.iter().any(|update| update["sessionUpdate"] == "notice"),
        "a client that advertised session.notices sees the turn's notice: {:?}",
        kinds(&updates)
    );
    noticed.assert_schema_clean();
}

/// IS-7: in ask-commands, bash raises session/request_permission with the four
/// option kinds and waits for the answer: allow-once runs it, reject-once and a
/// cancelled outcome never do, allow-always is remembered for the session, and
/// a plain file write runs without a prompt.
#[test]
fn is7_permission() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    let bash = |tag: &str, product: &str| {
        tool_turn(
            "bash",
            json!({"command": format!("echo is7-$(({product}))-{tag}"), "intent": "is7 permission fixture"}),
            &format!("is7 {tag} done"),
        )
    };
    daemon.provider.script("IS7_REJECT", bash("reject", "5*6"));
    daemon.provider.script("IS7_CANCELLED", bash("cancelled", "4*8"));
    daemon.provider.script("IS7_ALLOW", bash("allow", "5*7"));
    daemon.provider.script("IS7_AGAIN", bash("again", "6*6"));
    daemon.provider.script(
        "IS7_WRITE",
        tool_turn(
            "write",
            json!({"file_path": "is7-write.txt", "content": "is7 written\n"}),
            "is7 write done",
        ),
    );
    let mut conn = Conn::open(&daemon, ClientCapabilities::default(), "is7");
    conn.initialize(json!({}));
    let permissions = |conn: &Conn| conn.client.handlers().permission.requests();

    let session = conn.new_session(&work);
    conn.set_mode(&session, "ask-commands");
    conn.client
        .handlers()
        .permission
        .script([PermissionOutcome::Selected("reject-once".to_string())]);
    let (stop, updates) = conn.prompt_ok(&session, "IS7_REJECT run it");
    assert_eq!(stop, "end_turn");
    let requests = permissions(&conn);
    assert_eq!(requests.len(), 1, "ask-commands asks before bash");
    assert_eq!(requests[0]["sessionId"], session.as_str());
    assert_eq!(requests[0]["toolCall"]["kind"], "execute", "{}", requests[0]);
    assert_eq!(requests[0]["toolCall"]["status"], "pending", "{}", requests[0]);
    let option_kinds: Vec<&str> = requests[0]["options"]
        .as_array()
        .expect("options")
        .iter()
        .filter_map(|option| option["kind"].as_str())
        .collect();
    assert_eq!(
        option_kinds,
        ["allow_once", "allow_always", "reject_once", "reject_always"],
        "{}",
        requests[0]
    );
    assert!(!mentions(&updates, "is7-30-reject"), "a rejected command never runs");
    assert!(
        updates
            .iter()
            .any(|update| update["sessionUpdate"] == "tool_call_update"
                && update["status"] == "failed"
                && update.to_string().contains("denied")),
        "the refused call fails with the denial text: {updates:?}"
    );

    let session = conn.new_session(&work);
    conn.set_mode(&session, "ask-commands");
    conn.client.handlers().permission.script([PermissionOutcome::Cancelled]);
    let (_, updates) = conn.prompt_ok(&session, "IS7_CANCELLED run it");
    assert_eq!(permissions(&conn).len(), 2);
    assert!(!mentions(&updates, "is7-32-cancelled"), "a cancelled outcome rejects");

    let session = conn.new_session(&work);
    conn.set_mode(&session, "ask-commands");
    conn.client
        .handlers()
        .permission
        .script([PermissionOutcome::Selected("allow-once".to_string())]);
    let (_, updates) = conn.prompt_ok(&session, "IS7_ALLOW run it");
    assert_eq!(permissions(&conn).len(), 3);
    assert!(mentions(&updates, "is7-35-allow"), "allow-once runs it: {updates:?}");

    // Two bash calls in one session: the built-in tool turn first, then a
    // scripted one, so the second request is not answered by the first
    // call's scripted follow-up.
    let session = conn.new_session(&work);
    conn.set_mode(&session, "ask-commands");
    conn.client
        .handlers()
        .permission
        .script([PermissionOutcome::Selected("allow-always".to_string())]);
    let (_, updates) = conn.prompt_ok(&session, TOOL_PROMPT);
    assert_eq!(permissions(&conn).len(), 4);
    assert!(mentions(&updates, "t17-tool-ok"), "allow-always runs it: {updates:?}");
    let (_, updates) = conn.prompt_ok(&session, "IS7_AGAIN run it");
    assert_eq!(
        permissions(&conn).len(),
        4,
        "allow-always is remembered for the session"
    );
    assert!(mentions(&updates, "is7-36-again"), "{updates:?}");

    let session = conn.new_session(&work);
    conn.set_mode(&session, "ask-commands");
    let (stop, _) = conn.prompt_ok(&session, "IS7_WRITE write it");
    assert_eq!(stop, "end_turn");
    assert_eq!(permissions(&conn).len(), 4, "a plain file write runs without a prompt");
    assert_eq!(
        std::fs::read_to_string(work.join("is7-write.txt")).expect("the write ran"),
        "is7 written\n"
    );
    conn.assert_schema_clean();
}

/// IS-8: modes and config options. session/new answers three modes and a mode
/// option first; set_mode and set_config_option(mode) work while a prompt runs
/// and emit current_mode_update + config_option_update; set_config_option
/// answers the full list; booleans, unknown ids and unknown modes are -32602;
/// no option has an empty currentValue.
#[test]
fn is8_modes_config() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    daemon
        .provider
        .script("IS8_HOLD", Script::HoldOpen("is8 partial ".to_string()));
    let mut conn = Conn::open(&daemon, ClientCapabilities::default(), "is8");
    conn.initialize(json!({}));
    let created = conn.ok("session/new", json!({"cwd": cwd_str(&work), "mcpServers": []}));
    let session = created["sessionId"].as_str().expect("a session id").to_string();
    assert_eq!(created["modes"]["currentModeId"], "default", "{created}");
    let mode_ids: Vec<&str> = created["modes"]["availableModes"]
        .as_array()
        .expect("availableModes")
        .iter()
        .filter_map(|mode| mode["id"].as_str())
        .collect();
    assert_eq!(mode_ids, ["default", "ask-commands", "auto"], "{created}");
    let options = created["configOptions"].as_array().expect("configOptions").clone();
    assert_eq!(options[0]["id"], "mode", "{created}");
    assert_eq!(options[0]["category"], "mode", "{created}");
    assert_eq!(options[0]["type"], "select", "{created}");
    assert_eq!(options[0]["currentValue"], "default", "{created}");
    for option in &options {
        assert!(
            option["currentValue"].as_str().is_some_and(|value| !value.is_empty()),
            "no empty currentValue: {option}"
        );
        let choices = option["options"].as_array().expect("select choices");
        assert!(!choices.is_empty(), "{option}");
        for choice in choices {
            assert!(
                choice["value"].as_str().is_some_and(|value| !value.is_empty()),
                "{choice}"
            );
            assert!(choice["name"].as_str().is_some_and(|name| !name.is_empty()), "{choice}");
        }
    }

    let prompt_id = conn.send(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "IS8_HOLD stream"}]}),
    );
    daemon
        .provider
        .wait_for_hold_open_opened(WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", daemon.logs()));
    conn.client.drain_notifications();
    conn.set_mode(&session, "ask-commands");
    let mode = conn.await_update("current_mode_update");
    assert_eq!(
        mode["currentModeId"], "ask-commands",
        "set_mode mid-prompt emits current_mode_update: {mode}"
    );
    let options_update = conn.await_update("config_option_update");
    assert_eq!(
        options_update["configOptions"][0]["currentValue"], "ask-commands",
        "set_mode mid-prompt emits config_option_update: {options_update}"
    );
    let set = conn.ok(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "mode", "value": "auto"}),
    );
    let list = set["configOptions"].as_array().expect("the full list");
    assert_eq!(list.len(), options.len(), "{set}");
    assert_eq!(list[0]["currentValue"], "auto", "{set}");
    let mode = conn.await_update("current_mode_update");
    assert_eq!(
        mode["currentModeId"], "auto",
        "set_config_option(mode) emits current_mode_update: {mode}"
    );
    if let Some(model) = options.iter().find(|option| option["category"] == "model") {
        let busy = conn.err(
            "session/set_config_option",
            json!({"sessionId": session, "configId": model["id"], "value": model["currentValue"]}),
        );
        assert_eq!(busy["code"], -32602, "model changes wait for the prompt: {busy}");
    }
    let boolean = conn.err(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "mode", "type": "boolean", "value": true}),
    );
    assert_eq!(boolean["code"], -32602, "{boolean}");
    let unknown_id = conn.err(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "is8-nope", "value": "x"}),
    );
    assert_eq!(unknown_id["code"], -32602, "{unknown_id}");
    let unknown_mode = conn.err("session/set_mode", json!({"sessionId": session, "modeId": "plan"}));
    assert_eq!(unknown_mode["code"], -32602, "{unknown_mode}");

    conn.notify("session/cancel", json!({"sessionId": session}));
    let response = conn.wait("session/prompt", &prompt_id).expect("the prompt answers");
    assert_eq!(response["stopReason"], "cancelled", "{response}");
    conn.set_mode(&session, "default");
    conn.assert_schema_clean();
}

/// IS-9: a client that advertises fs gets every text read and write: the model
/// sees the client's unsaved overlay, and a write lands in the client, not on
/// disk.
#[test]
fn is9_fs() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    let read_path = work.join("is9-read.txt");
    std::fs::write(&read_path, "is9 disk text\n").expect("fixture");
    let write_path = work.join("is9-write.txt");
    let mut frames = function_call_frames("call_is9_read", "read", &json!({"file_path": cwd_str(&read_path)}));
    frames.extend(function_call_frames(
        "call_is9_write",
        "write",
        &json!({"file_path": cwd_str(&write_path), "content": "is9 written via client\n"}),
    ));
    frames.push(completed());
    daemon.provider.script(
        "IS9_FS",
        Script::Frames {
            frames,
            follow_up: ("call_is9_read".to_string(), "is9 fs done".to_string()),
        },
    );
    let capabilities = ClientCapabilities {
        fs_read_text_file: true,
        fs_write_text_file: true,
        ..ClientCapabilities::default()
    };
    let mut conn = Conn::open(&daemon, capabilities, "is9");
    conn.client.handlers().fs.insert(&read_path, "is9 overlay text\n");
    conn.client.handlers().fs.insert(&write_path, "is9 old overlay\n");
    conn.initialize(json!({}));
    let session = conn.new_session(&work);
    let (stop, _) = conn.prompt_ok(&session, "IS9_FS read and write");
    assert_eq!(stop, "end_turn", "{}", daemon.logs());

    let body = provider_body(&daemon, &["tool_call_id", "IS9_FS"]);
    assert!(body.contains("is9 overlay text"), "the model sees the overlay");
    assert!(!body.contains("is9 disk text"), "the model never sees the disk text");
    let read_requests: Vec<Value> = conn
        .stdout()
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|frame| frame["method"] == "fs/read_text_file")
        .collect();
    assert!(
        read_requests
            .iter()
            .any(|frame| frame["params"]["sessionId"] == session.as_str()
                && frame["params"]["path"] == cwd_str(&read_path)),
        "fs/read_text_file carries the session id and the absolute path: {read_requests:?}"
    );
    assert!(
        conn.client
            .handlers()
            .fs
            .writes()
            .contains(&(write_path.clone(), "is9 written via client\n".to_string())),
        "the write landed in the client: {:?}",
        conn.client.handlers().fs.writes()
    );
    assert!(!write_path.exists(), "the write never touched the disk");
    assert_eq!(std::fs::read_to_string(&read_path).expect("fixture"), "is9 disk text\n");
    conn.assert_schema_clean();
}

/// IS-10: a client that advertises terminal runs foreground bash: create,
/// wait_for_exit, output and release in that order, and the tool call embeds
/// the terminal.
#[test]
fn is10_terminal() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    daemon.provider.script(
        "IS10_TERM",
        tool_turn(
            "bash",
            json!({"command": "echo is10-$((4*4))-ran", "intent": "is10 terminal fixture"}),
            "is10 terminal done",
        ),
    );
    let capabilities = ClientCapabilities {
        terminal: true,
        ..ClientCapabilities::default()
    };
    let mut conn = Conn::open(&daemon, capabilities, "is10");
    conn.initialize(json!({}));
    let session = conn.new_session(&work);
    let (stop, updates) = conn.prompt_ok(&session, "IS10_TERM run it");
    assert_eq!(stop, "end_turn", "{}", daemon.logs());

    let calls = conn.client.handlers().terminal.calls();
    assert_eq!(
        calls,
        ["create", "wait_for_exit", "output", "release"],
        "the terminal lifecycle in order"
    );
    let record = conn
        .client
        .handlers()
        .terminal
        .record("term-1")
        .expect("the client terminal");
    assert!(record.released, "the terminal is released");
    assert!(record.output.contains("is10-16-ran"), "{record:?}");
    let create = conn
        .stdout()
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|frame| frame["method"] == "terminal/create")
        .expect("a terminal/create request");
    assert_eq!(create["params"]["sessionId"], session.as_str(), "{create}");
    assert!(
        create["params"]["cwd"]
            .as_str()
            .is_some_and(|cwd| Path::new(cwd).is_absolute()),
        "{create}"
    );
    let call = updates
        .iter()
        .find(|update| update["sessionUpdate"] == "tool_call" && update["kind"] == "execute")
        .unwrap_or_else(|| panic!("a bash tool_call: {updates:?}"));
    assert!(
        updates
            .iter()
            .any(|update| update["sessionUpdate"] == "tool_call_update"
                && update["toolCallId"] == call["toolCallId"]
                && update["content"]
                    .as_array()
                    .is_some_and(|content| content.contains(&json!({"type": "terminal", "terminalId": "term-1"})))),
        "the tool call embeds the terminal: {updates:?}"
    );
    assert!(mentions(&updates, "is10-16-ran"), "{updates:?}");
    conn.assert_schema_clean();
}

/// IS-11: a client that advertises elicitation.form answers ask questions as a
/// form; an accepted answer reaches the model, a decline falls back to the
/// recommended option.
#[test]
fn is11_elicitation() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    let ask = json!({"questions": [{
        "id": "color",
        "header": "Color",
        "question": "Which color?",
        "options": [{"label": "is11-red"}, {"label": "is11-green"}, {"label": "is11-blue"}],
        "recommended": 2,
    }]});
    daemon
        .provider
        .script("IS11_ACCEPT", tool_turn("ask", ask.clone(), "is11 accept done"));
    daemon
        .provider
        .script("IS11_DECLINE", tool_turn("ask", ask, "is11 decline done"));
    let capabilities = ClientCapabilities {
        elicitation_form: true,
        ..ClientCapabilities::default()
    };
    let mut conn = Conn::open(&daemon, capabilities, "is11");
    conn.initialize(json!({}));

    let session = conn.new_session(&work);
    conn.client
        .handlers()
        .elicitation
        .script([ElicitationAction::Accept(json!({"color": "is11-green"}))]);
    let (stop, _) = conn.prompt_ok(&session, "IS11_ACCEPT ask me");
    assert_eq!(stop, "end_turn", "{}", daemon.logs());
    let requests = conn.client.handlers().elicitation.requests();
    assert_eq!(requests.len(), 1, "one form elicitation");
    let request = &requests[0];
    assert_eq!(request["mode"], "form", "{request}");
    assert_eq!(request["sessionId"], session.as_str(), "{request}");
    let property = &request["requestedSchema"]["properties"]["color"];
    assert_eq!(
        property["enum"],
        json!(["is11-red", "is11-green", "is11-blue"]),
        "{request}"
    );
    assert_eq!(property["default"], "is11-blue", "{request}");
    assert_eq!(request["requestedSchema"]["required"], json!(["color"]), "{request}");
    let body = provider_body(&daemon, &["tool_call_id", "IS11_ACCEPT"]);
    assert!(body.contains("is11-green"), "the accepted answer reaches the model");

    let session = conn.new_session(&work);
    conn.client.handlers().elicitation.script([ElicitationAction::Decline]);
    let (stop, _) = conn.prompt_ok(&session, "IS11_DECLINE ask me");
    assert_eq!(stop, "end_turn");
    assert_eq!(conn.client.handlers().elicitation.requests().len(), 2);
    let body = provider_body(&daemon, &["tool_call_id", "IS11_DECLINE"]);
    assert!(
        body.contains("is11-blue"),
        "a decline falls back to the recommended option"
    );
    conn.assert_schema_clean();
}

/// IS-12: editor-provided stdio MCP servers connect for that session only,
/// their tools are usable, their children exit when the session closes or the
/// connection drops, and a session loaded with mcpServers connects them too.
#[test]
fn is12_mcp() {
    assert_python3();
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    let pids = daemon.root.join("pids");
    std::fs::create_dir_all(&pids).expect("pid dir");
    daemon.provider.script(
        "IS12_MCP",
        tool_turn("mcp__is12__is12_probe", json!({"token": "t12"}), "is12 mcp done"),
    );
    daemon
        .provider
        .script("IS12_OTHER", Script::Text("is12 other reply".to_string()));
    daemon
        .provider
        .script("IS12_LOADED", Script::Text("is12 loaded reply".to_string()));
    let mut conn = Conn::open(&daemon, ClientCapabilities::default(), "is12a");
    conn.initialize(json!({}));

    let pid_file = pids.join("is12.pid");
    let session = conn.new_session_with(json!({
        "cwd": cwd_str(&work),
        "mcpServers": [mcp_server("is12", "is12_probe", &pid_file)],
    }));
    wait_for_log(
        &daemon.home,
        &format!("session {session} server 'is12' connected with 1 tool(s)"),
        WAIT,
    );
    let (stop, updates) = conn.prompt_ok(&session, "IS12_MCP call the probe");
    assert_eq!(stop, "end_turn", "{}", daemon.logs());
    let first = provider_body(&daemon, &["IS12_MCP"]);
    assert!(
        first.contains("mcp__is12__is12_probe"),
        "the session's MCP tool is offered to the model"
    );
    let follow_up = provider_body(&daemon, &["tool_call_id", "IS12_MCP"]);
    assert!(
        follow_up.contains("mcp-probe-ok:t12:is12-env-ok"),
        "the MCP tool ran with its arguments and env: {updates:?}"
    );

    let other = conn.new_session(&work);
    let (stop, _) = conn.prompt_ok(&other, "IS12_OTHER plain");
    assert_eq!(stop, "end_turn");
    let other_body = provider_body(&daemon, &["IS12_OTHER"]);
    assert!(
        !other_body.contains("mcp__is12__"),
        "another session never sees this session's MCP tool"
    );

    let pid = read_pid(&pid_file);
    assert_eq!(conn.ok("session/close", json!({"sessionId": session})), json!({}));
    wait_for_pid_exit(pid, WAIT).expect("the MCP child exits when its session closes");

    let dropped_file = pids.join("is12-drop.pid");
    let dropped = conn.new_session_with(json!({
        "cwd": cwd_str(&work),
        "mcpServers": [mcp_server("is12drop", "is12_probe", &dropped_file)],
    }));
    wait_for_log(
        &daemon.home,
        &format!("session {dropped} server 'is12drop' connected with 1 tool(s)"),
        WAIT,
    );
    let dropped_pid = read_pid(&dropped_file);
    conn.assert_schema_clean();
    conn.client.close_stdin();
    let status = conn.client.wait_exit(WAIT).expect("the adapter exits on stdin EOF");
    assert!(status.success(), "{status}");
    wait_for_pid_exit(dropped_pid, WAIT).expect("the MCP child exits when the connection drops");

    let mut loader = Conn::open(&daemon, ClientCapabilities::default(), "is12b");
    loader.initialize(json!({}));
    let loaded_file = pids.join("is12-load.pid");
    loader.ok(
        "session/load",
        json!({
            "sessionId": other,
            "cwd": cwd_str(&work),
            "mcpServers": [mcp_server("is12load", "is12_probe", &loaded_file)],
        }),
    );
    wait_for_log(
        &daemon.home,
        &format!("session {other} server 'is12load' connected with 1 tool(s)"),
        WAIT,
    );
    let (stop, _) = loader.prompt_ok(&other, "IS12_LOADED again");
    assert_eq!(stop, "end_turn");
    let loaded_body = provider_body(&daemon, &["IS12_LOADED"]);
    assert!(
        loaded_body.contains("mcp__is12load__is12_probe"),
        "a loaded session connects its mcpServers"
    );
    let loaded_pid = read_pid(&loaded_file);
    assert_eq!(loader.ok("session/close", json!({"sessionId": other})), json!({}));
    wait_for_pid_exit(loaded_pid, WAIT).expect("the loaded session's MCP child exits on close");
    loader.assert_schema_clean();
}

/// IS-13: authMethods carry the agent method always and the terminal method
/// only for a client that advertised auth.terminal; authenticate and
/// session/new answer -32000 while no model is usable and succeed once one is;
/// logout is never offered.
#[test]
fn is13_auth() {
    let no_model = NoModelDaemon::start();
    let work = no_model.root.join("work");
    let mut conn = Conn::open_env(no_model.env(), ClientCapabilities::default(), "is13a");
    let init = conn.initialize(json!({}));
    let ids: Vec<&str> = init["authMethods"]
        .as_array()
        .expect("authMethods")
        .iter()
        .filter_map(|method| method["id"].as_str())
        .collect();
    assert_eq!(ids, ["horon-agent-credentials"], "{init}");
    assert!(
        init["agentCapabilities"]["auth"]["logout"].is_null(),
        "logout is not advertised: {init}"
    );
    let auth = conn.err("authenticate", json!({"methodId": "horon-agent-credentials"}));
    assert_eq!(auth["code"], -32000, "no usable model: {auth}");
    let created = conn.err("session/new", json!({"cwd": cwd_str(&work), "mcpServers": []}));
    assert_eq!(created["code"], -32000, "session/new without a usable model: {created}");
    let unknown = conn.err("authenticate", json!({"methodId": "is13-nope"}));
    assert_eq!(unknown["code"], -32602, "{unknown}");
    let terminal_via_rpc = conn.err("authenticate", json!({"methodId": "horon-agent-terminal-login"}));
    assert_eq!(terminal_via_rpc["code"], -32602, "{terminal_via_rpc}");
    let logout = conn.err("logout", json!({}));
    assert_eq!(logout["code"], -32601, "no logout method: {logout}");
    let refreshed = conn.initialize(json!({"auth": {"terminal": true}}));
    let methods = refreshed["authMethods"].as_array().expect("authMethods");
    let ids: Vec<&str> = methods.iter().filter_map(|method| method["id"].as_str()).collect();
    assert_eq!(
        ids,
        ["horon-agent-credentials", "horon-agent-terminal-login"],
        "{refreshed}"
    );
    let terminal = &methods[1];
    assert_eq!(terminal["type"], "terminal", "{terminal}");
    assert_eq!(terminal["args"], json!(["--login"]), "{terminal}");
    conn.assert_schema_clean();
    drop(conn);
    drop(no_model);

    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    let mut conn = Conn::open(&daemon, ClientCapabilities::default(), "is13b");
    conn.initialize(json!({}));
    assert_eq!(
        conn.ok("authenticate", json!({"methodId": "horon-agent-credentials"})),
        json!({}),
        "a usable model authenticates"
    );
    let session = conn.new_session(&work);
    assert!(!session.is_empty());
    conn.assert_schema_clean();
}

/// IS-14: stdout carries only newline-delimited UTF-8 JSON-RPC messages (also
/// for a malformed input line and non-ASCII text), and stdin EOF exits 0.
#[test]
fn is14_transport() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    let reply = "is14 ünïcødé ✓ 한국어 reply";
    daemon.provider.script("IS14_UTF8", Script::Text(reply.to_string()));
    let mut conn = Conn::open(&daemon, ClientCapabilities::default(), "is14");
    conn.initialize(json!({}));
    conn.write_invalid_line("{this is not json");
    let parse = conn
        .wait("parse error", &Value::Null)
        .expect_err("a malformed line is answered with an error");
    assert_eq!(parse["code"], -32700, "{parse}");
    let session = conn.new_session(&work);
    let (stop, updates) = conn.prompt_ok(&session, "IS14_UTF8 유니코드 ✓");
    assert_eq!(stop, "end_turn");
    assert!(text_of(&updates, "agent_message_chunk").contains(reply), "{updates:?}");
    conn.assert_schema_clean();
    conn.client.close_stdin();
    let status = conn.client.wait_exit(WAIT).expect("the adapter exits on stdin EOF");
    assert!(status.success(), "stdin EOF exits cleanly: {status}");
    let lines = conn.stdout();
    assert!(!lines.is_empty());
    for line in &lines {
        assert!(!line.is_empty(), "no empty stdout line");
        let value: Value =
            serde_json::from_str(line).unwrap_or_else(|error| panic!("stdout line is not JSON ({error}): {line:?}"));
        assert!(value.is_object(), "{line}");
        assert_eq!(value["jsonrpc"], "2.0", "{line}");
    }
}

/// IS-15: a plain daemon client never receives ACP-only events, never sees an
/// ACP session's MCP tools, never has its turn's file, terminal or permission
/// traffic routed to the editor on a shared session, and never inherits the
/// ACP mode after the ACP connection drops.
#[test]
fn is15_isolation() {
    assert_python3();
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    let own_file = work.join("is15-own.txt");
    let mut frames = function_call_frames(
        "call_is15_todo",
        "todo",
        &json!({"todos": [{"content": "is15 step", "status": "in_progress", "priority": "high", "id": "1", "confidence": "plausible"}]}),
    );
    frames.extend(function_call_frames(
        "call_is15_write",
        "write",
        &json!({"file_path": cwd_str(&own_file), "content": "is15 own write\n"}),
    ));
    frames.extend(function_call_frames(
        "call_is15_bash",
        "bash",
        &json!({"command": "echo is15-$((2*8))-own", "intent": "is15 own session"}),
    ));
    frames.push(completed());
    daemon.provider.script(
        "IS15_OWN",
        Script::Frames {
            frames,
            follow_up: ("call_is15_todo".to_string(), "is15 own done".to_string()),
        },
    );
    daemon.provider.script(
        "IS15_AFTER",
        tool_turn(
            "bash",
            json!({"command": "echo is15-$((3*7))-after", "intent": "is15 after the drop"}),
            "is15 after done",
        ),
    );

    let capabilities = ClientCapabilities {
        fs_read_text_file: true,
        fs_write_text_file: true,
        terminal: true,
        elicitation_form: false,
    };
    let mut acp = Conn::open(&daemon, capabilities, "is15");
    acp.initialize(json!({}));
    let pid_file = work.join("is15-mcp.pid");
    let acp_session = acp.new_session_with(json!({
        "cwd": cwd_str(&work),
        "mcpServers": [mcp_server("is15", "is15_probe", &pid_file)],
    }));
    acp.set_mode(&acp_session, "ask-commands");
    wait_for_log(
        &daemon.home,
        &format!("session {acp_session} server 'is15' connected with 1 tool(s)"),
        WAIT,
    );

    let mut own = PlainClient::connect(&daemon.socket, &work, None);
    assert_ne!(own.session_id, acp_session);
    let events = own.message("IS15_OWN plan, write and run");
    let own_body = provider_body(&daemon, &["IS15_OWN"]);
    assert!(
        !own_body.contains("mcp__is15__"),
        "the ACP session's MCP tools are absent from a plain client's session"
    );
    assert!(own.acp_only_events().is_empty(), "{:?}", own.acp_only_events());
    assert!(
        mentions(&events, "is15-16-own"),
        "the plain client's bash ran: {events:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&own_file).expect("the plain write went to disk"),
        "is15 own write\n"
    );

    let mut shared = PlainClient::connect(&daemon.socket, &work, Some(&acp_session));
    assert_eq!(
        shared.session_id, acp_session,
        "the plain client attached to the ACP session"
    );
    let events = shared.message(TOOL_PROMPT);
    assert!(
        mentions(&events, "t17-tool-ok"),
        "a plain client's turn on the shared session is not gated by the ACP mode: {events:?}"
    );
    assert!(shared.acp_only_events().is_empty(), "{:?}", shared.acp_only_events());
    assert!(
        acp.client.handlers().permission.requests().is_empty(),
        "the plain client's turn raised no editor permission request"
    );
    assert!(
        acp.client.handlers().terminal.calls().is_empty(),
        "the plain client's bash never ran in the editor's terminal"
    );
    assert!(
        acp.client.handlers().fs.read_paths().is_empty() && acp.client.handlers().fs.writes().is_empty(),
        "the plain client's turn never used the editor's files"
    );
    acp.barrier();
    let editor_requests: Vec<String> = acp
        .stdout()
        .into_iter()
        .filter(|line| {
            line.contains("\"method\":\"fs/")
                || line.contains("\"method\":\"terminal/")
                || line.contains("\"method\":\"session/request_permission\"")
        })
        .collect();
    assert!(editor_requests.is_empty(), "{editor_requests:?}");
    acp.assert_schema_clean();

    acp.client.close_stdin();
    let status = acp.client.wait_exit(WAIT).expect("the adapter exits on stdin EOF");
    assert!(status.success(), "{status}");
    let events = shared.message("IS15_AFTER run it");
    assert!(
        mentions(&events, "is15-21-after"),
        "after the ACP connection dropped the plain client's bash runs: {events:?}"
    );
    assert!(
        events.iter().all(|event| event["type"] != "approval_pending"),
        "no ACP mode is inherited after the drop: {events:?}"
    );
    assert!(shared.acp_only_events().is_empty(), "{:?}", shared.acp_only_events());
    assert!(own.acp_only_events().is_empty());
}
