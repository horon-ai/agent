#![cfg_attr(test, allow(clippy::await_holding_lock))]

use super::*;
use crate::mcp::RemoteTransport;
use crate::message::{Message, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::tool::mcp::{McpCallTool, McpSearchTool};
use crate::tool::{Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::Path;
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// A JSON-RPC stdio MCP server labelled argv[1]. It writes its PID to argv[2]
/// before reading anything; mode `hang` never answers, so its handshake can
/// only end by cancellation; mode `block` answers the handshake but never a
/// `tools/call`, and writes `called` to argv[2] when one arrives. Both keep
/// argv[2] open until they die (see [`watch_fifo`]). `echo` replies with the
/// label, PID, cwd and arguments.
pub(crate) const STDIO_SERVER: &str = r#"
import json, os, sys
name, pid_file, mode = sys.argv[1], sys.argv[2], sys.argv[3]
handle = open(pid_file, 'w')
handle.write(str(os.getpid()) + '\n')
handle.flush()
if mode not in ('hang', 'block'):
    handle.close()
for line in sys.stdin:
    req = json.loads(line)
    method = req.get('method')
    if method == 'shutdown':
        break
    if mode == 'hang' or 'id' not in req:
        continue
    if mode == 'block' and method == 'tools/call':
        handle.write('called\n')
        handle.flush()
        continue
    reply = {'jsonrpc': '2.0', 'id': req['id']}
    if method == 'initialize':
        reply['result'] = {'protocolVersion': '2024-11-05', 'capabilities': {'tools': {}},
                           'serverInfo': {'name': name, 'version': '1'}}
    elif method == 'tools/list':
        reply['result'] = {'tools': [{'name': 'echo', 'description': name + ' echo',
                                      'inputSchema': {'type': 'object', 'properties': {
                                          'token': {'type': 'string'}}}}]}
    elif method == 'tools/call' and req['params']['name'] == 'echo':
        value = {'server': name, 'pid': os.getpid(), 'cwd': os.getcwd(),
                 'arguments': req['params'].get('arguments')}
        reply['result'] = {'content': [{'type': 'text', 'text': json.dumps(value)}], 'isError': False}
    else:
        reply['error'] = {'code': -32601, 'message': 'unknown method'}
    print(json.dumps(reply), flush=True)
"#;

pub(crate) fn python3_unavailable(test: &str) -> bool {
    let available = std::process::Command::new("python3")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !available {
        eprintln!("SKIP {test}: python3 is unavailable");
    }
    !available
}

pub(crate) fn stdio_server(name: &str, pid_file: &Path, mode: &str) -> SessionMcpServer {
    labelled_stdio_server(name, name, pid_file, mode)
}

/// [`stdio_server`] whose replies carry `label` instead of the server name, so
/// two servers registered under one name can be told apart.
fn labelled_stdio_server(name: &str, label: &str, pid_file: &Path, mode: &str) -> SessionMcpServer {
    SessionMcpServer {
        name: name.to_string(),
        transport: "stdio".to_string(),
        command: Some("python3".to_string()),
        args: vec![
            "-I".to_string(),
            "-S".to_string(),
            "-u".to_string(),
            "-c".to_string(),
            STDIO_SERVER.to_string(),
            label.to_string(),
            pid_file.display().to_string(),
            mode.to_string(),
        ],
        env: Vec::new(),
        url: None,
        headers: Vec::new(),
    }
}

pub(crate) fn read_pid(pid_file: &Path) -> u32 {
    std::fs::read_to_string(pid_file)
        .expect("the stub wrote its PID before the handshake")
        .trim()
        .parse()
        .expect("a numeric PID")
}

/// `kill -0`: whether a process with this PID still exists. Shutdown awaits
/// the child's exit, so a stopped server is reaped, not a zombie.
pub(crate) fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Make `path` a FIFO and read a `hang` or `block` stub's PID from it, then
/// every further line it writes. The stub holds the write end until it dies,
/// so the last receiver fires exactly when the process has exited: an event,
/// not a poll.
#[cfg(unix)]
pub(crate) fn watch_fifo(
    path: &Path,
) -> (
    tokio::sync::oneshot::Receiver<u32>,
    mpsc::UnboundedReceiver<String>,
    tokio::sync::oneshot::Receiver<()>,
) {
    let status = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "mkfifo {}", path.display());
    let path = path.to_path_buf();
    let (pid_tx, pid_rx) = tokio::sync::oneshot::channel();
    let (line_tx, line_rx) = mpsc::unbounded_channel();
    let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        // Opening a FIFO for reading blocks until the stub opens it to write.
        let mut lines = std::io::BufReader::new(std::fs::File::open(&path).expect("open the PID FIFO")).lines();
        let pid = lines
            .next()
            .expect("the stub wrote its PID")
            .expect("read the stub PID");
        let _ = pid_tx.send(pid.trim().parse().expect("a numeric PID"));
        for line in lines.map_while(Result::ok) {
            let _ = line_tx.send(line);
        }
        let _ = exit_tx.send(());
    });
    (pid_rx, line_rx, exit_rx)
}

struct StubProvider;

#[async_trait]
impl Provider for StubProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        anyhow::bail!("stub provider must never be called")
    }

    fn name(&self) -> &str {
        "stub"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(StubProvider)
    }
}

struct TestEnv {
    _lock: std::sync::MutexGuard<'static, ()>,
    previous_home: Option<OsString>,
    dir: tempfile::TempDir,
}

impl TestEnv {
    fn new() -> Self {
        let lock = crate::storage::lock_test_env();
        let dir = tempfile::tempdir().expect("isolated HORON_AGENT_HOME");
        let previous_home = std::env::var_os("HORON_AGENT_HOME");
        crate::env::set_var("HORON_AGENT_HOME", dir.path());
        Self {
            _lock: lock,
            previous_home,
            dir,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        match self.previous_home.take() {
            Some(previous) => crate::env::set_var("HORON_AGENT_HOME", previous),
            None => crate::env::remove_var("HORON_AGENT_HOME"),
        }
    }
}

/// The ACP connection the tests install as a session's owner.
pub(crate) const ACP_CONNECTION: &str = "conn-acp";

/// [`ACP_CONNECTION`] installed as the ACP owner of a session until dropped.
struct AcpOwner {
    session_id: String,
    token: u64,
}

impl Drop for AcpOwner {
    fn drop(&mut self) {
        acp_session::clear(&self.session_id, self.token);
    }
}

fn acp_owner(session_id: &str) -> AcpOwner {
    let (tx, _rx) = mpsc::unbounded_channel();
    AcpOwner {
        session_id: session_id.to_string(),
        token: acp_session::install(session_id, ACP_CONNECTION, tx),
    }
}

/// A turn started by [`ACP_CONNECTION`], the only turn that sees its servers.
fn acp_turn(session_id: &str) -> acp_session::TurnOwnership {
    let turn = acp_session::claim_turn(session_id, Some(ACP_CONNECTION));
    assert!(turn.is_owning(), "the ACP connection owns this turn");
    turn
}

async fn session_registry() -> Registry {
    session_registry_with(Vec::new()).await.0
}

/// A session registry whose configured manager connects `servers` and
/// registers their proxies the way a session's configured MCP startup does.
async fn session_registry_with(servers: Vec<(String, McpServerConfig)>) -> (Registry, Arc<RwLock<McpManager>>) {
    let registry = Registry::new(Arc::new(StubProvider)).await;
    let mut config = McpConfig::default();
    config.servers.extend(servers.iter().cloned());
    let configured = Arc::new(RwLock::new(McpManager::with_config(config)));
    registry
        .register(
            "mcp_search".to_string(),
            Arc::new(McpSearchTool::new(Arc::clone(&configured)).with_registry(registry.clone())) as Arc<dyn Tool>,
        )
        .await;
    registry
        .register(
            "mcp_call".to_string(),
            Arc::new(McpCallTool::new(Arc::clone(&configured)).with_registry(registry.clone())) as Arc<dyn Tool>,
        )
        .await;
    if !servers.is_empty() {
        for (name, config) in &servers {
            configured
                .read()
                .await
                .connect(name, config)
                .await
                .expect("configured server");
        }
        let tools = crate::mcp::create_mcp_tools(Arc::clone(&configured)).await;
        let connected = configured.read().await.connected_servers().await;
        registry.refresh_mcp_tools(tools, &connected).await;
    }
    (registry, configured)
}

fn ready(registry: &Registry) -> BoxFuture<'static, Registry> {
    let registry = registry.clone();
    Box::pin(async move { registry })
}

pub(crate) fn context(session_id: &str) -> ToolContext {
    ToolContext {
        session_id: session_id.to_string(),
        message_id: "message".to_string(),
        tool_call_id: "call".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: horon_agent_tool_core::ToolExecutionMode::Direct,
    }
}

async fn next_status(events: &mut mpsc::UnboundedReceiver<ServerEvent>) -> Vec<String> {
    let event = tokio::time::timeout(Duration::from_secs(20), events.recv())
        .await
        .expect("a session MCP server finished connecting")
        .expect("the event channel stays open");
    match event {
        ServerEvent::McpStatus { servers } => servers,
        other => panic!("expected McpStatus, got {other:?}"),
    }
}

async fn call_json(registry: &Registry, name: &str, input: Value, session_id: &str) -> Value {
    let output = registry
        .execute(name, input, context(session_id))
        .await
        .unwrap_or_else(|error| panic!("{name} failed: {error:#}"));
    serde_json::from_str(&output.output).unwrap_or_else(|_| panic!("{name} returned {}", output.output))
}

async fn mcp_tool_names(registry: &Registry) -> Vec<String> {
    let mut names: Vec<String> = registry
        .tool_names()
        .await
        .into_iter()
        .filter(|name| name.starts_with("mcp__"))
        .collect();
    names.sort();
    names
}

fn remote_server(name: &str, transport: &str, url: Option<&str>) -> SessionMcpServer {
    SessionMcpServer {
        name: name.to_string(),
        transport: transport.to_string(),
        command: None,
        args: Vec::new(),
        env: Vec::new(),
        url: url.map(str::to_string),
        headers: vec![("Authorization".to_string(), "Bearer editor-token".to_string())],
    }
}

#[tokio::test]
async fn configure_session_mcp_stdio_connects_and_registers_tools() {
    if python3_unavailable("configure_session_mcp_stdio_connects_and_registers_tools") {
        return;
    }
    let env = TestEnv::new();
    let registry = session_registry().await;
    let pid_file = env.path("editor.pid");
    let configs = server_configs(vec![stdio_server("editor", &pid_file, "serve")]).expect("a valid stdio server");
    assert!(!configs[0].1.shared, "editor servers are never shared");
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let _owner = acp_owner("session-a");
    let session = SessionMcp::start(
        "session-a",
        ACP_CONNECTION,
        configs,
        Some(env.dir.path().to_path_buf()),
        ready(&registry),
        events_tx,
    );

    assert_eq!(next_status(&mut events).await, ["editor:1"]);
    let _turn = acp_turn("session-a");
    let alias = crate::mcp::dispatch_name("editor", "echo");
    assert_eq!(mcp_tool_names(&registry).await, std::slice::from_ref(&alias));
    let reply = call_json(&registry, &alias, json!({"token": "direct"}), "session-a").await;
    let pid = read_pid(&pid_file);
    assert_eq!(reply["server"], "editor");
    assert_eq!(reply["pid"], pid);
    assert_eq!(reply["arguments"], json!({"token": "direct"}));
    assert_eq!(
        std::fs::canonicalize(reply["cwd"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(env.dir.path()).unwrap(),
        "the server runs in the session working directory"
    );

    let found = registry
        .execute("mcp_search", json!({"server": "editor"}), context("session-a"))
        .await
        .expect("mcp_search runs");
    assert!(found.output.contains(&alias), "mcp_search lists it: {}", found.output);
    let via_call = call_json(
        &registry,
        "mcp_call",
        json!({"server": "editor", "tool": "echo", "arguments": {"token": "via-call"}}),
        "session-a",
    )
    .await;
    assert_eq!(via_call["arguments"], json!({"token": "via-call"}));
    assert_eq!(via_call["pid"], pid);

    assert!(pid_alive(pid));
    session.shutdown().await;
    assert!(
        mcp_tool_names(&registry).await.is_empty(),
        "shutdown withdraws the tools"
    );
    assert!(!pid_alive(pid), "shutdown stops the server process");
}

#[test]
fn configure_session_mcp_rejects_missing_command() {
    for command in [None, Some(String::new()), Some("   ".to_string())] {
        let server = SessionMcpServer {
            command,
            ..stdio_server("editor", Path::new("/unused"), "serve")
        };
        let error = server_configs(vec![server]).expect_err("a stdio server without a command is refused");
        assert!(error.contains("needs a command"), "{error}");
    }
}

#[test]
fn configure_session_mcp_rejects_unknown_transport() {
    let server = remote_server("socket", "ws", Some("ws://127.0.0.1:1/mcp"));
    let error = server_configs(vec![server]).expect_err("an unknown transport is refused");
    assert!(error.contains("unknown transport \"ws\""), "{error}");

    let unnamed = stdio_server(" ", Path::new("/unused"), "serve");
    assert!(server_configs(vec![unnamed]).is_err(), "an empty name is refused");
    let twice = vec![
        stdio_server("editor", Path::new("/unused"), "serve"),
        stdio_server("editor", Path::new("/unused"), "serve"),
    ];
    let error = server_configs(twice).expect_err("a repeated name is refused");
    assert!(error.contains("more than once"), "{error}");
}

#[tokio::test]
async fn session_mcp_not_visible_to_other_session() {
    if python3_unavailable("session_mcp_not_visible_to_other_session") {
        return;
    }
    let env = TestEnv::new();
    let owner = session_registry().await;
    let other = session_registry().await;
    let pid_file = env.path("owner.pid");
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let _owner = acp_owner("session-owner");
    let session = SessionMcp::start(
        "session-owner",
        ACP_CONNECTION,
        server_configs(vec![stdio_server("editor", &pid_file, "serve")]).unwrap(),
        None,
        ready(&owner),
        events_tx,
    );
    assert_eq!(next_status(&mut events).await, ["editor:1"]);
    let _turn = acp_turn("session-owner");

    assert_eq!(
        mcp_tool_names(&owner).await,
        [crate::mcp::dispatch_name("editor", "echo")]
    );
    assert!(mcp_tool_names(&other).await.is_empty(), "another session has no proxy");
    let search = other
        .execute("mcp_search", json!({"server": "editor"}), context("session-other"))
        .await
        .expect("mcp_search runs");
    assert!(
        !search.output.contains("echo"),
        "another session cannot find it: {}",
        search.output
    );
    let call = other
        .execute(
            "mcp_call",
            json!({"server": "editor", "tool": "echo", "arguments": {}}),
            context("session-other"),
        )
        .await;
    assert!(call.is_err(), "another session cannot call it");

    session.shutdown().await;
}

#[test]
fn session_mcp_http_maps_transport() {
    let configs = server_configs(vec![
        remote_server("web", "http", Some("http://127.0.0.1:9/mcp")),
        remote_server("events", "sse", Some("https://example.com/sse")),
    ])
    .expect("valid remote servers");
    let (name, http) = &configs[0];
    assert_eq!(name, "web");
    assert_eq!(http.transport.as_deref(), Some("http"));
    assert_eq!(http.remote_transport(), Some(RemoteTransport::StreamableHttp));
    assert_eq!(http.url.as_deref(), Some("http://127.0.0.1:9/mcp"));
    assert_eq!(
        http.headers.get("Authorization").map(String::as_str),
        Some("Bearer editor-token")
    );
    assert!(http.command.is_empty() && !http.shared);
    let (_, sse) = &configs[1];
    assert_eq!(sse.remote_transport(), Some(RemoteTransport::LegacySse));
    assert!(!sse.shared);

    for url in [
        None,
        Some("not a url"),
        Some("ftp://example.com/mcp"),
        Some("http://user:secret@example.com/mcp"),
    ] {
        let server = remote_server("web", "http", url);
        assert!(server_configs(vec![server]).is_err(), "accepted {url:?}");
    }
}

/// A minimal MCP server over HTTP on 127.0.0.1. Streamable HTTP answers each
/// POST with JSON; legacy SSE announces `/messages` on the GET stream and
/// answers POSTs over that stream.
struct HttpStub {
    url: String,
    authorization: Arc<StdMutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for HttpStub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct HttpStubState {
    legacy: bool,
    authorization: Arc<StdMutex<Vec<String>>>,
    sse: StdMutex<Option<mpsc::UnboundedSender<String>>>,
}

impl HttpStub {
    async fn start(legacy: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind the stub");
        let address = listener.local_addr().expect("stub address");
        let url = format!("http://{address}/{}", if legacy { "sse" } else { "mcp" });
        let authorization = Arc::new(StdMutex::new(Vec::new()));
        let state = Arc::new(HttpStubState {
            legacy,
            authorization: Arc::clone(&authorization),
            sse: StdMutex::new(None),
        });
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { return };
                        connections.spawn(serve_http(socket, Arc::clone(&state)));
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Self {
            url,
            authorization,
            task,
        }
    }
}

async fn serve_http(socket: TcpStream, state: Arc<HttpStubState>) {
    let mut reader = BufReader::new(socket);
    let mut line = String::new();
    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }
    let method = line.split_whitespace().next().unwrap_or_default().to_string();
    let mut length = 0usize;
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_ascii_lowercase();
            if key == "content-length" {
                length = value.trim().parse().unwrap_or(0);
            } else if key == "authorization" {
                state.authorization.lock().unwrap().push(value.trim().to_string());
            }
        }
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).await.is_err() {
        return;
    }
    let mut socket = reader.into_inner();

    if method == "GET" {
        if !state.legacy {
            let _ = write_response(&mut socket, 405, "text/plain", "", "").await;
            return;
        }
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        *state.sse.lock().unwrap() = Some(tx);
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";
        if socket.write_all(head.as_bytes()).await.is_err()
            || socket.write_all(b"event: endpoint\ndata: /messages\n\n").await.is_err()
        {
            return;
        }
        while let Some(message) = rx.recv().await {
            let event = format!("event: message\ndata: {message}\n\n");
            if socket.write_all(event.as_bytes()).await.is_err() {
                return;
            }
        }
        return;
    }
    if method != "POST" {
        let _ = write_response(&mut socket, 405, "text/plain", "", "").await;
        return;
    }
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let Some(id) = request.get("id").cloned() else {
        let _ = write_response(&mut socket, 202, "text/plain", "", "").await;
        return;
    };
    let result = match request.get("method").and_then(Value::as_str).unwrap_or_default() {
        "initialize" => json!({
            "protocolVersion": request.pointer("/params/protocolVersion").cloned().unwrap_or(json!("2025-06-18")),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "http-stub", "version": "1"},
        }),
        "tools/list" => json!({"tools": [{
            "name": "echo",
            "description": "http stub echo",
            "inputSchema": {"type": "object", "properties": {"token": {"type": "string"}}},
        }]}),
        "tools/call" => json!({"content": [{
            "type": "text",
            "text": json!({
                "server": if state.legacy { "sse-stub" } else { "http-stub" },
                "arguments": request.pointer("/params/arguments").cloned().unwrap_or(Value::Null),
            }).to_string(),
        }]}),
        _ => json!({}),
    };
    let reply = json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
    if state.legacy {
        let sender = state.sse.lock().unwrap().clone();
        if let Some(sender) = sender {
            let _ = sender.send(reply);
        }
        let _ = write_response(&mut socket, 202, "text/plain", "", "").await;
    } else {
        let _ = write_response(
            &mut socket,
            200,
            "application/json",
            &reply,
            "Mcp-Session-Id: stub-session\r\n",
        )
        .await;
    }
}

async fn write_response(
    socket: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &str,
    extra: &str,
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status} Stub\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
        body.len()
    );
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(body.as_bytes()).await?;
    socket.flush().await
}

async fn assert_remote_stub_connects(transport: &str, legacy: bool) {
    let _env = TestEnv::new();
    let stub = HttpStub::start(legacy).await;
    let registry = session_registry().await;
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let _owner = acp_owner("session-remote");
    let session = SessionMcp::start(
        "session-remote",
        ACP_CONNECTION,
        server_configs(vec![remote_server("web", transport, Some(&stub.url))]).unwrap(),
        None,
        ready(&registry),
        events_tx,
    );
    assert_eq!(next_status(&mut events).await, ["web:1"]);
    let _turn = acp_turn("session-remote");
    let alias = crate::mcp::dispatch_name("web", "echo");
    assert_eq!(mcp_tool_names(&registry).await, std::slice::from_ref(&alias));
    let reply = call_json(&registry, &alias, json!({"token": transport}), "session-remote").await;
    assert_eq!(reply["arguments"], json!({"token": transport}));
    assert_eq!(reply["server"], if legacy { "sse-stub" } else { "http-stub" });
    let seen = stub.authorization.lock().unwrap().clone();
    assert!(
        !seen.is_empty() && seen.iter().all(|value| value == "Bearer editor-token"),
        "the editor's headers reach the server: {seen:?}"
    );
    session.shutdown().await;
    assert!(mcp_tool_names(&registry).await.is_empty());
}

#[tokio::test]
async fn session_mcp_http_stub_connects() {
    assert_remote_stub_connects("http", false).await;
}

#[tokio::test]
async fn session_mcp_sse_stub_connects() {
    assert_remote_stub_connects("sse", true).await;
}

#[cfg(unix)]
#[tokio::test]
async fn session_mcp_hung_server_does_not_hold_back_others_and_is_stopped() {
    if python3_unavailable("session_mcp_hung_server_does_not_hold_back_others_and_is_stopped") {
        return;
    }
    let env = TestEnv::new();
    let registry = session_registry().await;
    let hung_pid_file = env.path("hung.pid");
    let (hung_pid, _hung_lines, hung_exit) = watch_fifo(&hung_pid_file);
    let good_pid_file = env.path("good.pid");
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let _owner = acp_owner("session-hung");
    let session = SessionMcp::start(
        "session-hung",
        ACP_CONNECTION,
        server_configs(vec![
            stdio_server("hung", &hung_pid_file, "hang"),
            stdio_server("good", &good_pid_file, "serve"),
        ])
        .unwrap(),
        None,
        ready(&registry),
        events_tx,
    );
    let hung_pid = tokio::time::timeout(Duration::from_secs(20), hung_pid)
        .await
        .expect("the hung stub started")
        .expect("the FIFO reader sent its PID");
    assert_eq!(next_status(&mut events).await, ["good:1"]);
    let _turn = acp_turn("session-hung");
    assert_eq!(
        mcp_tool_names(&registry).await,
        [crate::mcp::dispatch_name("good", "echo")],
        "the server that never answers registers nothing"
    );
    let good_pid = read_pid(&good_pid_file);
    assert!(pid_alive(hung_pid), "the hung handshake is still pending");

    session.shutdown().await;
    tokio::time::timeout(Duration::from_secs(10), hung_exit)
        .await
        .expect("shutdown cancels the pending handshake and its process exits")
        .expect("the FIFO reader saw the stub exit");
    assert!(!pid_alive(good_pid));
    assert!(mcp_tool_names(&registry).await.is_empty());
}

/// What `mcp__dup__echo`, `mcp_call dup/echo` and `mcp_search dup` reach for
/// the turn running now, as the label of the answering server.
async fn dup_route(registry: &Registry, session_id: &str) -> (String, String, String) {
    let alias = crate::mcp::dispatch_name("dup", "echo");
    let direct = call_json(registry, &alias, json!({}), session_id).await;
    let via_call = call_json(
        registry,
        "mcp_call",
        json!({"server": "dup", "tool": "echo", "arguments": {}}),
        session_id,
    )
    .await;
    let search = registry
        .execute("mcp_search", json!({"server": "dup"}), context(session_id))
        .await
        .expect("mcp_search runs")
        .output;
    let listed = ["editor", "configured"]
        .into_iter()
        .filter(|label| search.contains(&format!("{label} echo")))
        .collect::<Vec<_>>()
        .join(",");
    (
        direct["server"].as_str().unwrap().to_string(),
        via_call["server"].as_str().unwrap().to_string(),
        listed,
    )
}

/// An editor server named like a configured one shadows it on every path for
/// the ACP connection's own turns, leaves it alone for every other turn, and
/// the configured server is whole again once the editor's list is withdrawn
/// (`session/load` with `mcpServers: []` shuts the list down).
#[tokio::test]
async fn session_mcp_shadows_configured_server_and_restores_it() {
    if python3_unavailable("session_mcp_shadows_configured_server_and_restores_it") {
        return;
    }
    let env = TestEnv::new();
    let session_id = "session-shadow";
    let configured_pid_file = env.path("configured.pid");
    let (registry, configured) = session_registry_with(
        server_configs(vec![labelled_stdio_server(
            "dup",
            "configured",
            &configured_pid_file,
            "serve",
        )])
        .unwrap(),
    )
    .await;
    let alias = crate::mcp::dispatch_name("dup", "echo");
    let configured_route = (
        "configured".to_string(),
        "configured".to_string(),
        "configured".to_string(),
    );
    assert_eq!(dup_route(&registry, session_id).await, configured_route);

    let _owner = acp_owner(session_id);
    let editor_pid_file = env.path("editor.pid");
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let session = SessionMcp::start(
        session_id,
        ACP_CONNECTION,
        server_configs(vec![labelled_stdio_server("dup", "editor", &editor_pid_file, "serve")]).unwrap(),
        None,
        ready(&registry),
        events_tx,
    );
    assert_eq!(next_status(&mut events).await, ["dup:1"]);
    let editor_pid = read_pid(&editor_pid_file);

    {
        let _turn = acp_turn(session_id);
        let editor_route = ("editor".to_string(), "editor".to_string(), "editor".to_string());
        assert_eq!(dup_route(&registry, session_id).await, editor_route);
        assert_eq!(
            mcp_tool_names(&registry).await,
            std::slice::from_ref(&alias),
            "one name, no collision suffix"
        );
        let definitions: Vec<_> = registry
            .definitions(None)
            .await
            .into_iter()
            .filter(|definition| definition.name == alias)
            .collect();
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].description, "editor echo");
    }
    {
        let _turn = acp_session::claim_turn(session_id, Some("conn-desktop"));
        assert_eq!(
            dup_route(&registry, session_id).await,
            configured_route,
            "a plain client's turn keeps the configured server"
        );
    }
    assert_eq!(dup_route(&registry, session_id).await, configured_route);

    session.shutdown().await;
    assert!(!pid_alive(editor_pid), "the editor server stopped");
    {
        let _turn = acp_turn(session_id);
        assert_eq!(
            dup_route(&registry, session_id).await,
            configured_route,
            "the configured server serves the ACP turn again"
        );
        assert_eq!(mcp_tool_names(&registry).await, [alias]);
    }
    let configured_pid = read_pid(&configured_pid_file);
    assert!(pid_alive(configured_pid), "the configured server was never touched");
    configured.read().await.disconnect_all().await;
}

/// Every way a turn reaches a tool, from a turn the installing ACP connection
/// did not start: nothing of the editor's servers is visible or callable.
async fn assert_editor_tools_hidden(registry: &Registry, session_id: &str, alias: &str, who: &str) {
    assert!(mcp_tool_names(registry).await.is_empty(), "{who}: tool_names");
    assert!(
        registry
            .definitions(None)
            .await
            .iter()
            .all(|definition| !definition.name.starts_with("mcp__")),
        "{who}: definitions"
    );
    assert!(
        registry
            .definitions_for_session(session_id)
            .await
            .iter()
            .all(|d| d.name != alias),
        "{who}: codemode definitions"
    );
    let search = registry
        .execute("mcp_search", json!({"server": "editor"}), context(session_id))
        .await
        .expect("mcp_search runs");
    assert!(!search.output.contains("echo"), "{who}: mcp_search {}", search.output);
    let call = registry
        .execute(
            "mcp_call",
            json!({"server": "editor", "tool": "echo", "arguments": {}}),
            context(session_id),
        )
        .await;
    assert!(call.is_err(), "{who}: mcp_call reached the editor server");
    let direct = registry
        .execute(alias, json!({}), context(session_id))
        .await
        .expect_err("a direct call is refused");
    assert!(format!("{direct:#}").contains("Unknown tool"), "{who}: {direct:#}");
    assert!(registry.mcp_identity_for_alias(alias).is_none(), "{who}: identity");
}

/// IS-12: a plain client attached to the ACP session, a daemon turn and a call
/// outside any turn cannot list or call the editor's servers; only the
/// installing connection's own turn can.
#[tokio::test]
async fn session_mcp_hidden_from_turns_the_acp_connection_did_not_start() {
    if python3_unavailable("session_mcp_hidden_from_turns_the_acp_connection_did_not_start") {
        return;
    }
    let env = TestEnv::new();
    let session_id = "session-plain-turn";
    let registry = session_registry().await;
    let _owner = acp_owner(session_id);
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let session = SessionMcp::start(
        session_id,
        ACP_CONNECTION,
        server_configs(vec![stdio_server("editor", &env.path("editor.pid"), "serve")]).unwrap(),
        None,
        ready(&registry),
        events_tx,
    );
    assert_eq!(next_status(&mut events).await, ["editor:1"]);
    let alias = crate::mcp::dispatch_name("editor", "echo");

    {
        let _turn = acp_session::claim_turn(session_id, Some("conn-desktop"));
        assert_editor_tools_hidden(&registry, session_id, &alias, "plain client turn").await;
    }
    {
        let _turn = acp_session::claim_turn(session_id, None);
        assert_editor_tools_hidden(&registry, session_id, &alias, "daemon turn").await;
    }
    assert_editor_tools_hidden(&registry, session_id, &alias, "no turn").await;

    {
        let _turn = acp_turn(session_id);
        assert_eq!(mcp_tool_names(&registry).await, std::slice::from_ref(&alias));
        assert_eq!(
            registry.mcp_identity_for_alias(&alias),
            Some(("editor".into(), "echo".into()))
        );
        let reply = call_json(&registry, &alias, json!({}), session_id).await;
        assert_eq!(reply["server"], "editor");
    }
    session.shutdown().await;
}
