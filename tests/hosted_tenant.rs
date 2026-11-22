#![cfg(unix)]
//! The hosting boundary against two real daemons.
//!
//! Each tenant gets its own `HORON_HOME`, runtime directory and daemon, started
//! as a per-user container: the daemons pass the startup boundary check, write
//! their own state under their own volume, and never answer for the other
//! tenant. The tests also drive the suspend and delete paths from
//! `horon_agent_sdk::launch_hosted` against those live daemons.
//!
//! A tiny scripted OpenAI-compatible provider is the only stand-in: it returns
//! one deterministic assistant message, so a session restore can be asserted on
//! the transcript rather than on a process staying alive.

use horon_agent::protocol::ServerEvent;
use horon_agent::server::Client;
use horon_agent_sdk::{HostedTenantPaths, delete_hosted_instance, stop_hosted_instance, tenant_paths};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const BINARY: &str = env!("CARGO_BIN_EXE_horon-agent");
const MODEL: &str = "scripted-model";
const REPLY: &str = "hosted boundary reply";
const STARTUP: Duration = Duration::from_secs(120);
const EVENT_WAIT: Duration = Duration::from_secs(30);

fn scratch_root(label: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    // A Unix socket path must fit the OS limit, so the root is short and
    // explicit rather than the deep per-user temporary directory macOS uses.
    let preferred = Path::new("/tmp");
    let base = if preferred.is_dir() {
        preferred.to_path_buf()
    } else {
        std::env::temp_dir()
    }
    .canonicalize()
    .expect("temporary directory");
    let root = base.join(format!("hh{}{label}{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("scratch root");
    root
}

fn private_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path).expect("directory");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("mode");
}

/// A scripted Chat Completions endpoint: one assistant delta, then a stop.
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
            "id": "chatcmpl-hosted",
            "object": "chat.completion.chunk",
            "model": MODEL,
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": REPLY}, "finish_reason": null}]
        }),
        json!({
            "id": "chatcmpl-hosted",
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

/// One tenant's on-disk layout and process, allocated from the hosted paths.
struct Tenant {
    name: &'static str,
    paths: HostedTenantPaths,
    home_dir: PathBuf,
    debug_socket: PathBuf,
    legacy_socket: PathBuf,
    log: PathBuf,
    child: Option<Child>,
}

impl Tenant {
    fn prepare(root: &Path, runtime_base: &Path, name: &'static str, provider: &ScriptedProvider) -> Self {
        let paths = tenant_paths(root, name, runtime_base).expect("tenant paths");
        let home_dir = root.join(format!("user-{name}"));
        private_dir(&home_dir);
        for directory in [&paths.instance, &paths.home, &paths.runtime_dir] {
            private_dir(directory);
        }
        std::fs::write(paths.home.join("config.toml"), provider_config(&provider.base_url)).expect("config");
        std::fs::write(
            paths.home.join("mcp.json"),
            format!("{{\"mcpServers\":{{\"{}-only\":{{\"command\":\"/bin/true\"}}}}}}", name),
        )
        .expect("mcp config");
        std::fs::write(
            paths.home.join("approvals.json"),
            format!("{{\"version\":1,\"grants\":[],\"tenant\":\"{name}\"}}"),
        )
        .expect("approvals");
        private_dir(&paths.home.join("memory"));
        std::fs::write(
            paths.home.join("memory/global.json"),
            format!("{{\"tenant\":\"{name}\"}}"),
        )
        .expect("memory");
        private_dir(&paths.home.join("user-files"));
        std::fs::write(paths.home.join("user-files/note.txt"), name).expect("user file");
        private_dir(&root.join("logs"));
        Self {
            name,
            legacy_socket: paths.runtime_dir.join("horon-agent.sock"),
            debug_socket: paths.runtime_dir.join("horon-agent-debug.sock"),
            log: root.join("logs").join(format!("{name}.log")),
            home_dir,
            paths,
            child: None,
        }
    }

    fn start(&mut self) {
        let log = std::fs::File::create(&self.log).expect("daemon log");
        let errors = log.try_clone().expect("daemon log clone");
        let mut command = Command::new(BINARY);
        command
            .arg("--no-update")
            .arg("--socket")
            .arg(&self.legacy_socket)
            .arg("serve")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home_dir)
            .env("HORON_HOME", &self.paths.home)
            .env("HORON_AGENT_RUNTIME_DIR", &self.paths.runtime_dir)
            .env("HORON_AGENT_SOCKET", &self.legacy_socket)
            .env("HORON_AGENT_HOSTING_MODE", "per-user-container")
            .env("HORON_AGENT_TENANT_ID", self.name)
            .env("HORON_AGENT_DEBUG_SOCKET", "1")
            .env("HORON_AGENT_WORKSPACE_TRUST", "trust")
            .env("HORON_AGENT_NO_AUTO_UPDATE", "1")
            .env("HORON_AGENT_NON_INTERACTIVE", "1")
            .env("HORON_AGENT_MEMORY_ENABLED", "0")
            .env("HORON_AGENT_MEMORY_SIDECAR_ENABLED", "0")
            .env("XDG_CONFIG_HOME", self.home_dir.join("config"))
            .env("XDG_CACHE_HOME", self.home_dir.join("cache"))
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(Stdio::from(errors));
        self.child = Some(command.spawn().expect("spawn daemon"));
    }

    async fn wait_ready(&mut self) {
        let deadline = Instant::now() + STARTUP;
        while Instant::now() < deadline {
            if let Ok(mut probe) = Client::connect_debug_with_path(self.debug_socket.clone()).await
                && probe.ping().await.unwrap_or(false)
            {
                return;
            }
            if let Some(child) = self.child.as_mut()
                && let Ok(Some(status)) = child.try_wait()
            {
                panic!(
                    "{} exited during startup ({status}); log:\n{}",
                    self.name,
                    std::fs::read_to_string(&self.log).unwrap_or_default()
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!(
            "{} never answered on its debug socket; log:\n{}",
            self.name,
            std::fs::read_to_string(&self.log).unwrap_or_default()
        );
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

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Tenant {
    fn drop(&mut self) {
        self.stop();
    }
}

fn provider_config(base_url: &str) -> String {
    format!(
        "[compaction]\nbackground = false\nmax_context_tokens = 20000\nrecent_tokens = 512\n\n\
[provider]\ndefault_provider = \"scripted\"\ndefault_model = \"{MODEL}\"\n\n\
[providers.scripted]\ntype = \"openai-compatible\"\nbase_url = \"{base_url}\"\nauth = \"none\"\n\
default_model = \"{MODEL}\"\nmodels = [{{ id = \"{MODEL}\" }}]\n"
    )
}

/// Read events until the daemon answers `subscribe`, so a later resume is
/// delivered on the same connection.
async fn subscribe(client: &mut Client) -> Result<(), String> {
    let id = client.subscribe().await.map_err(|error| error.to_string())?;
    loop {
        let event = tokio::time::timeout(EVENT_WAIT, client.read_event())
            .await
            .map_err(|_| "subscribe timeout".to_string())?
            .map_err(|error| error.to_string())?;
        if matches!(event, ServerEvent::Done { id: done } if done == id) {
            return Ok(());
        }
    }
}

/// Resume a stored session on a restarted daemon and return its history.
async fn resume_history(socket: &Path, session_id: &str) -> Result<Vec<(String, String)>, String> {
    let mut client = Client::connect_with_path(socket.to_path_buf())
        .await
        .map_err(|error| error.to_string())?;
    subscribe(&mut client).await?;
    let id = client
        .resume_session(session_id)
        .await
        .map_err(|error| error.to_string())?;
    loop {
        let event = tokio::time::timeout(EVENT_WAIT, client.read_event())
            .await
            .map_err(|_| "resume timeout".to_string())?
            .map_err(|error| error.to_string())?;
        if let ServerEvent::History {
            id: history_id,
            messages,
            ..
        } = event
            && history_id == id
        {
            return Ok(messages
                .into_iter()
                .map(|message| (message.role, message.content))
                .collect());
        }
    }
}

/// Wait until the session's event log on this tenant's volume holds `needle`.
///
/// `Done` on the live connection precedes the asynchronous store write, so a
/// restart test that stops immediately would race the write. The drain at stop
/// covers a write already in flight; this wait makes the write observable
/// first, so the assertion is about restoration rather than scheduling.
async fn wait_for_persisted_reply(home: &Path, session: &str, needle: &str) -> Result<(), String> {
    let log = home.join("sessions").join(session).join("events.jsonl");
    let deadline = Instant::now() + EVENT_WAIT;
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(&log)
            && text.contains(needle)
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(format!(
        "{} never recorded {needle:?}: {}",
        log.display(),
        std::fs::read_to_string(&log).unwrap_or_default()
    ))
}

fn configured_servers(output: &str) -> Value {
    let value: Value = serde_json::from_str(output).expect("mcp:servers json");
    value.get("configured_servers").cloned().expect("configured_servers")
}

/// Two tenants share a host and still see only their own state.
#[tokio::test(flavor = "multi_thread")]
async fn two_tenants_see_only_their_own_sessions_mcp_and_files() {
    let root = scratch_root("two");
    let runtime_base = root.join("run");
    let provider = ScriptedProvider::start();
    let mut alice = Tenant::prepare(&root, &runtime_base, "alice", &provider);
    let mut bob = Tenant::prepare(&root, &runtime_base, "bob", &provider);
    alice.start();
    alice.wait_ready().await;
    bob.start();
    bob.wait_ready().await;

    let alice_session = alice.create_session().await;
    let bob_session = bob.create_session().await;
    assert_ne!(alice_session, bob_session);
    alice
        .debug("message:alice turn", Some(&alice_session))
        .await
        .expect("alice turn");
    bob.debug("message:bob turn", Some(&bob_session))
        .await
        .expect("bob turn");
    wait_for_persisted_reply(&alice.paths.home, &alice_session, "alice turn")
        .await
        .expect("alice session persisted on her volume");
    wait_for_persisted_reply(&bob.paths.home, &bob_session, "bob turn")
        .await
        .expect("bob session persisted on his volume");

    // The MCP view is per daemon and reads that daemon's own home, so each
    // tenant sees only the server its control plane provisioned.
    let alice_mcp = alice
        .debug("mcp:servers", Some(&alice_session))
        .await
        .expect("alice mcp");
    let bob_mcp = bob.debug("mcp:servers", Some(&bob_session)).await.expect("bob mcp");
    let alice_config: Value = serde_json::from_str(&alice_mcp).expect("alice json");
    assert_eq!(
        alice_config.get("config_path").and_then(Value::as_str),
        Some(alice.paths.home.join("mcp.json").to_str().expect("utf-8 path")),
        "alice's daemon reads alice's mcp.json"
    );
    assert_eq!(configured_servers(&alice_mcp), json!(["alice-only"]));
    assert_eq!(configured_servers(&bob_mcp), json!(["bob-only"]));
    assert!(
        alice.paths.home.join("sessions").join(&alice_session).exists(),
        "alice's session lives on alice's volume"
    );
    assert!(
        !bob.paths.home.join("sessions").join(&alice_session).exists(),
        "bob's volume must not hold alice's session"
    );
    assert!(
        !alice.paths.home.join("sessions").join(&bob_session).exists(),
        "alice's volume must not hold bob's session"
    );

    let cross = bob.debug("history", Some(&alice_session)).await;
    let error = cross.expect_err("bob must not answer for alice's session");
    assert!(error.contains(&alice_session), "{error}");

    // A daemon's live-session view must never name the other tenant's session,
    // even when its own list is empty (no client is attached in this test).
    let alice_sessions = alice.debug("sessions", None).await.expect("alice sessions");
    assert!(!alice_sessions.contains(&bob_session), "{alice_sessions}");
    let bob_sessions = bob.debug("sessions", None).await.expect("bob sessions");
    assert!(!bob_sessions.contains(&alice_session), "{bob_sessions}");

    assert_eq!(
        std::fs::read_to_string(bob.paths.home.join("memory/global.json")).expect("bob memory"),
        "{\"tenant\":\"bob\"}"
    );
    assert_eq!(
        std::fs::read_to_string(alice.paths.home.join("user-files/note.txt")).expect("alice file"),
        "alice"
    );
    assert_eq!(
        std::fs::read_to_string(bob.paths.home.join("user-files/note.txt")).expect("bob file"),
        "bob"
    );
    assert!(!bob.paths.home.join("user-files/alice-note.txt").exists());

    alice.stop();
    bob.stop();
    let _ = std::fs::remove_dir_all(&root);
}

/// A stop drains, and a restart restores the session and the home files.
#[tokio::test(flavor = "multi_thread")]
async fn restart_restores_the_session_and_memory_files() {
    let root = scratch_root("restart");
    let runtime_base = root.join("run");
    let provider = ScriptedProvider::start();
    let mut tenant = Tenant::prepare(&root, &runtime_base, "carol", &provider);
    tenant.start();
    tenant.wait_ready().await;

    let session = tenant.create_session().await;
    let output = tenant
        .debug("message:remember the hosted boundary", Some(&session))
        .await
        .expect("turn");
    assert!(!output.is_empty(), "the turn must answer");
    wait_for_persisted_reply(&tenant.paths.home, &session, REPLY)
        .await
        .expect("the assistant reply is on the volume before the stop");

    stop_hosted_instance(&root, "carol", &runtime_base).expect("stop drains the daemon");
    assert!(
        Client::connect_debug_with_path(tenant.debug_socket.clone())
            .await
            .is_err(),
        "the stopped daemon refuses connections"
    );
    if let Some(mut child) = tenant.child.take() {
        let _ = child.wait();
    }

    tenant.start();
    tenant.wait_ready().await;
    let history = resume_history(&tenant.legacy_socket, &session)
        .await
        .expect("resume after restart");
    assert!(
        history
            .iter()
            .any(|(role, content)| role == "assistant" && content.contains(REPLY)),
        "the assistant reply survives the restart: {history:?}"
    );
    assert!(
        history
            .iter()
            .any(|(role, content)| role == "user" && content.contains("remember the hosted boundary")),
        "the user turn survives the restart: {history:?}"
    );
    assert_eq!(
        std::fs::read_to_string(tenant.paths.home.join("memory/global.json")).expect("memory file"),
        "{\"tenant\":\"carol\"}",
        "the home volume kept the memory file across the restart"
    );

    tenant.stop();
    let _ = std::fs::remove_dir_all(&root);
}

/// Deleting one tenant stops and removes exactly that tenant.
#[tokio::test(flavor = "multi_thread")]
async fn deleting_one_tenant_leaves_the_other_running() {
    let root = scratch_root("delete");
    let runtime_base = root.join("run");
    let provider = ScriptedProvider::start();
    let mut alice = Tenant::prepare(&root, &runtime_base, "alice", &provider);
    let mut bob = Tenant::prepare(&root, &runtime_base, "bob", &provider);
    alice.start();
    alice.wait_ready().await;
    bob.start();
    bob.wait_ready().await;
    let bob_session = bob.create_session().await;
    let alice_session = alice.create_session().await;

    delete_hosted_instance(&root, "alice", &runtime_base).expect("delete alice");
    alice.stop();
    assert!(!alice.paths.instance.exists(), "alice's instance directory is gone");
    assert!(!alice.paths.runtime_dir.exists(), "alice's runtime directory is gone");

    let bob_mcp = bob
        .debug("mcp:servers", Some(&bob_session))
        .await
        .expect("bob still answers");
    assert_eq!(configured_servers(&bob_mcp), json!(["bob-only"]));
    assert!(
        bob.paths.home.join("sessions").join(&bob_session).exists(),
        "bob's session survives alice's deletion"
    );
    assert!(
        bob.paths.home.join("user-files/note.txt").exists(),
        "bob's files survive alice's deletion"
    );
    assert_eq!(
        std::fs::read_to_string(bob.paths.home.join("memory/global.json")).expect("bob memory"),
        "{\"tenant\":\"bob\"}"
    );
    assert!(!alice.paths.home.join("sessions").join(&alice_session).exists());

    bob.stop();
    let _ = std::fs::remove_dir_all(&root);
}
