#![cfg(unix)]
//! Graceful daemon shutdown on `SIGINT` and `SIGTERM` (#775).
//!
//! Spawns a real `horon-agent serve` daemon in an isolated `HORON_HOME` and
//! runtime directory, completes a scripted turn, sends `SIGINT`, `SIGTERM`, or
//! both back-to-back, and verifies exit code 0, once-only shutdown logging,
//! registry and socket/lock/metadata cleanup, and full session restoration on
//! restart.

use horon_agent::protocol::ServerEvent;
use horon_agent::server::Client;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const BINARY: &str = env!("CARGO_BIN_EXE_horon-agent");
const MODEL: &str = "scripted-model";
const REPLY: &str = "signal shutdown reply";
const STARTUP: Duration = Duration::from_secs(120);
const EVENT_WAIT: Duration = Duration::from_secs(30);
const EXIT_WAIT: Duration = Duration::from_secs(15);

fn scratch_root(label: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let preferred = Path::new("/tmp");
    let base = if preferred.is_dir() {
        preferred.to_path_buf()
    } else {
        std::env::temp_dir()
    }
    .canonicalize()
    .expect("temporary directory");
    let root = base.join(format!("hsig{}{label}{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("scratch root");
    root
}

fn private_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path).expect("directory");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("mode");
}

struct ScriptedProvider {
    base_url: String,
}

impl ScriptedProvider {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind scripted provider");
        let port = listener.local_addr().expect("address").port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                std::thread::spawn(move || {
                    let _ = respond(stream);
                });
            }
        });
        Self {
            base_url: format!("http://127.0.0.1:{port}/v1"),
        }
    }
}

fn respond(stream: TcpStream) -> std::io::Result<()> {
    let mut stream = stream;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed.to_ascii_lowercase().strip_prefix("content-length:") {
            length = rest.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;

    let frames = [
        json!({
            "id": "chatcmpl-signal",
            "object": "chat.completion.chunk",
            "model": MODEL,
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": REPLY}, "finish_reason": null}]
        }),
        json!({
            "id": "chatcmpl-signal",
            "object": "chat.completion.chunk",
            "model": MODEL,
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
        }),
    ];
    let mut payload = String::new();
    for frame in frames {
        payload.push_str(&format!("data: {frame}\n\n"));
    }
    payload.push_str("data: [DONE]\n\n");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

fn provider_config(base_url: &str) -> String {
    format!(
        "[compaction]\nbackground = false\nmax_context_tokens = 20000\nrecent_tokens = 512\n\n\
[provider]\ndefault_provider = \"scripted\"\ndefault_model = \"{MODEL}\"\n\n\
[providers.scripted]\ntype = \"openai-compatible\"\nbase_url = \"{base_url}\"\nauth = \"none\"\n\
default_model = \"{MODEL}\"\nmodels = [{{ id = \"{MODEL}\" }}]\n"
    )
}

struct DaemonFixture {
    server_name: String,
    home: PathBuf,
    user_home: PathBuf,
    runtime_dir: PathBuf,
    socket: PathBuf,
    debug_socket: PathBuf,
    daemon_lock: PathBuf,
    hash_sidecar: PathBuf,
    temp_metadata: PathBuf,
    stdio_log: PathBuf,
    temporary: bool,
    child: Option<Child>,
}

impl DaemonFixture {
    fn new(root: &Path, server_name: &str, provider: &ScriptedProvider, temporary: bool) -> Self {
        let user_home = root.join("user");
        let home = root.join("home");
        let runtime_dir = root.join("run");
        for dir in [&user_home, &home, &runtime_dir] {
            private_dir(dir);
        }
        std::fs::write(home.join("config.toml"), provider_config(&provider.base_url)).expect("config");
        let socket = runtime_dir.join("horon-agent.sock");
        let debug_socket = runtime_dir.join("horon-agent-debug.sock");
        let daemon_lock = runtime_dir.join("horon-agent-daemon.lock");
        let hash_sidecar = PathBuf::from(format!("{}.hash", socket.display()));
        let temp_metadata = runtime_dir.join("horon-agent.sock.server.json");
        let stdio_log = root.join(format!("{server_name}.stdio.log"));
        Self {
            server_name: server_name.to_string(),
            home,
            user_home,
            runtime_dir,
            socket,
            debug_socket,
            daemon_lock,
            hash_sidecar,
            temp_metadata,
            stdio_log,
            temporary,
            child: None,
        }
    }

    fn start(&mut self) {
        let log = std::fs::File::create(&self.stdio_log).expect("stdio log");
        let errors = log.try_clone().expect("stdio log clone");
        let mut command = Command::new(BINARY);
        command
            .arg("--no-update")
            .arg("--socket")
            .arg(&self.socket)
            .arg("serve")
            .arg("--server-name")
            .arg(&self.server_name);
        if self.temporary {
            command
                .arg("--temporary-server")
                .arg("--owner-pid")
                .arg(std::process::id().to_string());
        }
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.user_home)
            .env("HORON_HOME", &self.home)
            .env("HORON_AGENT_RUNTIME_DIR", &self.runtime_dir)
            .env("HORON_AGENT_SOCKET", &self.socket)
            .env("HORON_AGENT_DEBUG_SOCKET", "1")
            .env("HORON_AGENT_WORKSPACE_TRUST", "trust")
            .env("HORON_AGENT_NO_AUTO_UPDATE", "1")
            .env("HORON_AGENT_NON_INTERACTIVE", "1")
            .env("HORON_AGENT_MEMORY_ENABLED", "0")
            .env("HORON_AGENT_MEMORY_SIDECAR_ENABLED", "0")
            .env("XDG_CONFIG_HOME", self.user_home.join("config"))
            .env("XDG_CACHE_HOME", self.user_home.join("cache"))
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(Stdio::from(errors));
        self.child = Some(command.spawn().expect("spawn daemon"));
    }

    fn pid(&self) -> u32 {
        self.child.as_ref().expect("running daemon").id()
    }

    async fn wait_ready_and_registered(&mut self) {
        let deadline = Instant::now() + STARTUP;
        let mut ping_ok = false;
        while Instant::now() < deadline {
            if !ping_ok
                && let Ok(mut probe) = Client::connect_debug_with_path(self.debug_socket.clone()).await
                && probe.ping().await.unwrap_or(false)
            {
                ping_ok = true;
            }
            if ping_ok
                && self.hash_sidecar.exists()
                && self.registry_contains_self()
                && (!self.temporary || self.temp_metadata.exists())
            {
                return;
            }
            if let Some(child) = self.child.as_mut()
                && let Ok(Some(status)) = child.try_wait()
            {
                panic!(
                    "{} exited during startup ({status}); stdio:\n{}\nlog:\n{}",
                    self.server_name,
                    std::fs::read_to_string(&self.stdio_log).unwrap_or_default(),
                    self.read_daemon_logs(),
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!(
            "{} did not become ready and registered; ping_ok={ping_ok}, hash={}, reg={}, stdio:\n{}\nlog:\n{}",
            self.server_name,
            self.hash_sidecar.exists(),
            self.registry_contains_self(),
            std::fs::read_to_string(&self.stdio_log).unwrap_or_default(),
            self.read_daemon_logs(),
        );
    }

    fn registry_contains_self(&self) -> bool {
        let path = self.home.join("servers.json");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return false;
        };
        let Ok(value) = serde_json::from_str::<Value>(&content) else {
            return false;
        };
        value
            .as_object()
            .is_some_and(|servers| servers.contains_key(&self.server_name))
    }

    fn read_daemon_logs(&self) -> String {
        let dir = self.home.join("logs");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return String::new();
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("log"))
            .collect();
        paths.sort();
        let mut combined = String::new();
        for path in paths {
            if let Ok(text) = std::fs::read_to_string(path) {
                combined.push_str(&text);
                combined.push('\n');
            }
        }
        combined
    }

    async fn debug(&self, command: &str, session: Option<&str>) -> Result<String, String> {
        let mut client = Client::connect_debug_with_path(self.debug_socket.clone())
            .await
            .map_err(|error| error.to_string())?;
        let request = client
            .debug_command(command, session)
            .await
            .map_err(|error| error.to_string())?;
        let deadline = Instant::now() + EVENT_WAIT;
        while Instant::now() < deadline {
            let event = tokio::time::timeout(Duration::from_secs(1), client.read_event())
                .await
                .map_err(|_| "debug event timeout".to_string())?
                .map_err(|error| error.to_string())?;
            match event {
                ServerEvent::Ack { .. } => continue,
                ServerEvent::DebugResponse { id, ok, output } if id == request => {
                    return if ok { Ok(output) } else { Err(output) };
                }
                ServerEvent::Error { id, message, .. } if id == request => return Err(message),
                _ => continue,
            }
        }
        Err("no debug response".to_string())
    }

    async fn create_session(&self) -> String {
        let output = self.debug("create_session", None).await.expect("create session");
        let value: Value = serde_json::from_str(&output).expect("create_session json");
        value
            .get("session_id")
            .and_then(Value::as_str)
            .expect("session id")
            .to_string()
    }

    async fn wait_exit(&mut self) -> ExitStatus {
        let mut child = self.child.take().expect("running daemon");
        let deadline = Instant::now() + EXIT_WAIT;
        while Instant::now() < deadline {
            if let Some(status) = child.try_wait().expect("try_wait") {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!(
            "{} did not exit within {:?}; log:\n{}",
            self.server_name,
            EXIT_WAIT,
            self.read_daemon_logs()
        );
    }

    fn kill_if_running(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for DaemonFixture {
    fn drop(&mut self) {
        self.kill_if_running();
    }
}

fn send_signal(pid: u32, name: &str) {
    let status = Command::new("kill")
        .arg("-s")
        .arg(name)
        .arg(pid.to_string())
        .status()
        .expect("invoke POSIX kill");
    assert!(status.success(), "kill -s {name} {pid} failed: {status}");
}

async fn wait_for_persisted_reply(home: &Path, session: &str, needle: &str) {
    let log = home.join("sessions").join(session).join("events.jsonl");
    let deadline = Instant::now() + EVENT_WAIT;
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(&log)
            && text.contains(needle)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "{} never recorded {needle:?}: {}",
        log.display(),
        std::fs::read_to_string(&log).unwrap_or_default()
    );
}

async fn resume_history(socket: &Path, session_id: &str) -> Vec<(String, String)> {
    let mut client = Client::connect_with_path(socket.to_path_buf())
        .await
        .expect("connect to restarted daemon");
    let sub_id = client.subscribe().await.expect("subscribe");
    loop {
        let event = tokio::time::timeout(EVENT_WAIT, client.read_event())
            .await
            .expect("subscribe timeout")
            .expect("subscribe event");
        if matches!(event, ServerEvent::Done { id } if id == sub_id) {
            break;
        }
    }
    let resume_id = client.resume_session(session_id).await.expect("resume session");
    loop {
        let event = tokio::time::timeout(EVENT_WAIT, client.read_event())
            .await
            .expect("resume timeout")
            .expect("resume event");
        if let ServerEvent::History { id, messages, .. } = event
            && id == resume_id
        {
            return messages
                .into_iter()
                .map(|message| (message.role, message.content))
                .collect();
        }
    }
}

fn assert_runtime_cleaned_up(daemon: &DaemonFixture) {
    assert!(
        !daemon.registry_contains_self(),
        "servers.json must drop {} on shutdown",
        daemon.server_name
    );
    assert!(!daemon.socket.exists(), "main socket must be removed on shutdown");
    assert!(
        !daemon.debug_socket.exists(),
        "debug socket must be removed on shutdown"
    );
    assert!(
        !daemon.hash_sidecar.exists(),
        "build-hash sidecar must be removed on shutdown"
    );
    assert!(
        !daemon.daemon_lock.exists(),
        "daemon lock file must be removed on shutdown"
    );
    assert!(
        !daemon.temp_metadata.exists(),
        "temporary server metadata must be removed on shutdown"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sigint_drains_session_unregisters_cleans_sockets_and_exits_zero() {
    let root = scratch_root("int");
    let provider = ScriptedProvider::start();
    let mut daemon = DaemonFixture::new(&root, "sigint-daemon", &provider, false);
    daemon.start();
    daemon.wait_ready_and_registered().await;

    let session = daemon.create_session().await;
    let prompt = "remember sigint graceful shutdown";
    let answer = daemon
        .debug(&format!("message:{prompt}"), Some(&session))
        .await
        .expect("turn before SIGINT");
    assert!(!answer.is_empty());
    wait_for_persisted_reply(&daemon.home, &session, REPLY).await;

    send_signal(daemon.pid(), "INT");
    let status = daemon.wait_exit().await;
    assert_eq!(
        status.code(),
        Some(0),
        "SIGINT must trigger graceful exit 0, got {status:?}; log:\n{}",
        daemon.read_daemon_logs()
    );
    let logs = daemon.read_daemon_logs();
    assert_eq!(
        logs.matches("Server received SIGINT, shutting down gracefully").count(),
        1,
        "expected exactly one SIGINT shutdown log line; log:\n{logs}"
    );
    assert_runtime_cleaned_up(&daemon);

    daemon.start();
    daemon.wait_ready_and_registered().await;
    let history = resume_history(&daemon.socket, &session).await;
    assert!(
        history
            .iter()
            .any(|(role, content)| role == "user" && content.contains(prompt)),
        "user prompt must survive SIGINT shutdown: {history:?}"
    );
    assert!(
        history
            .iter()
            .any(|(role, content)| role == "assistant" && content.contains(REPLY)),
        "assistant reply must survive SIGINT shutdown: {history:?}"
    );

    daemon.kill_if_running();
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread")]
async fn sigterm_drains_session_unregisters_cleans_sockets_and_exits_zero() {
    let root = scratch_root("term");
    let provider = ScriptedProvider::start();
    let mut daemon = DaemonFixture::new(&root, "sigterm-daemon", &provider, false);
    daemon.start();
    daemon.wait_ready_and_registered().await;

    let session = daemon.create_session().await;
    let prompt = "remember sigterm graceful shutdown";
    let answer = daemon
        .debug(&format!("message:{prompt}"), Some(&session))
        .await
        .expect("turn before SIGTERM");
    assert!(!answer.is_empty());
    wait_for_persisted_reply(&daemon.home, &session, REPLY).await;

    send_signal(daemon.pid(), "TERM");
    let status = daemon.wait_exit().await;
    assert_eq!(
        status.code(),
        Some(0),
        "SIGTERM must trigger graceful exit 0, got {status:?}; log:\n{}",
        daemon.read_daemon_logs()
    );
    let logs = daemon.read_daemon_logs();
    assert_eq!(
        logs.matches("Server received SIGTERM, shutting down gracefully")
            .count(),
        1,
        "expected exactly one SIGTERM shutdown log line; log:\n{logs}"
    );
    assert_runtime_cleaned_up(&daemon);

    daemon.start();
    daemon.wait_ready_and_registered().await;
    let history = resume_history(&daemon.socket, &session).await;
    assert!(
        history
            .iter()
            .any(|(role, content)| role == "user" && content.contains(prompt)),
        "user prompt must survive SIGTERM shutdown: {history:?}"
    );
    assert!(
        history
            .iter()
            .any(|(role, content)| role == "assistant" && content.contains(REPLY)),
        "assistant reply must survive SIGTERM shutdown: {history:?}"
    );

    daemon.kill_if_running();
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread")]
async fn consecutive_sigint_and_sigterm_run_shutdown_once_and_exit_zero() {
    let root = scratch_root("both");
    let provider = ScriptedProvider::start();
    let mut daemon = DaemonFixture::new(&root, "double-sig-daemon", &provider, true);
    daemon.start();
    daemon.wait_ready_and_registered().await;

    let session = daemon.create_session().await;
    let prompt = "remember back-to-back termination signals";
    let answer = daemon
        .debug(&format!("message:{prompt}"), Some(&session))
        .await
        .expect("turn before signals");
    assert!(!answer.is_empty());
    wait_for_persisted_reply(&daemon.home, &session, REPLY).await;

    let pid = daemon.pid().to_string();
    let burst = Command::new("sh")
        .arg("-c")
        .arg(r#"kill -s INT "$1"; kill -s TERM "$1" 2>/dev/null || true; kill -s INT "$1" 2>/dev/null || true"#)
        .arg("signal-burst")
        .arg(&pid)
        .status()
        .expect("send consecutive signals");
    assert!(burst.success(), "signal burst shell failed: {burst}");

    let status = daemon.wait_exit().await;
    assert_eq!(
        status.code(),
        Some(0),
        "consecutive SIGINT+SIGTERM must still exit 0 without aborting mid-drain, got {status:?}; log:\n{}",
        daemon.read_daemon_logs()
    );
    let logs = daemon.read_daemon_logs();
    assert_eq!(
        logs.matches("shutting down gracefully").count(),
        1,
        "shutdown path must run exactly once even on consecutive signals; log:\n{logs}"
    );
    assert_runtime_cleaned_up(&daemon);

    daemon.start();
    daemon.wait_ready_and_registered().await;
    let history = resume_history(&daemon.socket, &session).await;
    assert!(
        history
            .iter()
            .any(|(role, content)| role == "user" && content.contains(prompt)),
        "user prompt must survive consecutive signals: {history:?}"
    );
    assert!(
        history
            .iter()
            .any(|(role, content)| role == "assistant" && content.contains(REPLY)),
        "assistant reply must survive consecutive signals: {history:?}"
    );

    daemon.kill_if_running();
    let _ = std::fs::remove_dir_all(&root);
}
