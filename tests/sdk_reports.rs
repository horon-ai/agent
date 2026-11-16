//! Real-daemon QA for the Rust SDK's report and acknowledgement methods:
//! `search_sessions`, `list_artifacts`, `usage_report`, `ack_approval` and
//! `ack_failover`.
//!
//! Everything here crosses two processes and two Unix sockets: a real
//! `horon-agent serve` daemon and a real `api-bridge`, both running under a
//! scratch `HORON_HOME` so nothing touches the developer's own state. A method
//! that only "works" against a fake harness — a reply shape nobody builds, a
//! request field the bridge renames — fails here.
//!
//! The two tests share a file-local lock because each one starts two child
//! processes, and three cohorts starting at once starve the daemons' startup
//! window.

#![cfg(unix)]

use horon_agent_sdk::{ConnectOptions, HoronAgentClient};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BINARY: &str = env!("CARGO_BIN_EXE_horon-agent");

/// One daemon plus the bridge in front of it, both owned by a scratch HOME.
struct Instance {
    root: PathBuf,
    home: PathBuf,
    api_socket: PathBuf,
    daemon: Child,
    bridge: Child,
}

impl Instance {
    fn start(root: &Path, label: &str) -> Self {
        let base = root.join(label);
        let home = base.join("home");
        let runtime = base.join("rt");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&runtime).expect("runtime");
        let api_socket = base.join("api.sock");
        let daemon = spawn(&home, &runtime, &["serve", "--no-update"]);
        wait_for_socket(&runtime.join("horon-agent.sock"), Duration::from_secs(30));
        let bridge = spawn(
            &home,
            &runtime,
            &[
                "api-bridge",
                "--no-update",
                "--api-socket",
                api_socket.to_str().expect("utf-8 socket path"),
            ],
        );
        wait_for_socket(&api_socket, Duration::from_secs(30));
        Self {
            root: base,
            home,
            api_socket,
            daemon,
            bridge,
        }
    }

    /// A fresh client connection to the same bridge socket.
    fn client(&self, name: &str) -> HoronAgentClient {
        HoronAgentClient::connect(ConnectOptions {
            socket_path: Some(self.api_socket.clone()),
            client_name: name.to_string(),
            request_timeout: Some(Duration::from_secs(30)),
            ensure_runtime: false,
            supports_user_response: false,
        })
        .unwrap_or_else(|error| panic!("connect to the bridge: {error}\n{}", self.logs()))
    }

    fn logs(&self) -> String {
        let read = |name: &str| std::fs::read_to_string(self.root.join("rt").join(name)).unwrap_or_default();
        format!(
            "daemon.log:\n{}\nbridge.log:\n{}",
            read("daemon.log"),
            read("bridge.log")
        )
    }

    fn stop(&mut self) {
        let _ = self.bridge.kill();
        let _ = self.daemon.kill();
        let _ = self.bridge.wait();
        let _ = self.daemon.wait();
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.stop();
    }
}

fn spawn(home: &Path, runtime: &Path, args: &[&str]) -> Child {
    let log_name = if args.first() == Some(&"serve") {
        "daemon.log"
    } else {
        "bridge.log"
    };
    let log = std::fs::File::create(runtime.join(log_name)).expect("log file");
    let errors = log.try_clone().expect("log clone");
    Command::new(BINARY)
        .args(args)
        .env("HORON_HOME", home)
        .env("HORON_AGENT_RUNTIME_DIR", runtime)
        .env("HORON_AGENT_NO_AUTO_UPDATE", "1")
        .env("HORON_AGENT_MEMORY_ENABLED", "0")
        .env("HORON_AGENT_MEMORY_SIDECAR_ENABLED", "0")
        .env("HORON_AGENT_NON_INTERACTIVE", "1")
        .env_remove("HORON_AGENT_SOCKET")
        .env_remove("HORON_AGENT_API_SOCKET")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(errors))
        .spawn()
        .expect("spawn horon-agent")
}

/// Serializes the two tests: each starts two child processes, and running both
/// at once starves the daemons' startup window.
fn instance_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::LazyLock<std::sync::Mutex<()>> = std::sync::LazyLock::new(|| std::sync::Mutex::new(()));
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_for_socket(path: &Path, deadline: Duration) {
    let start = Instant::now();
    loop {
        if UnixStream::connect(path).is_ok() {
            return;
        }
        if start.elapsed() >= deadline {
            let log = path
                .parent()
                .and_then(|dir| std::fs::read_to_string(dir.join("daemon.log")).ok())
                .unwrap_or_default();
            panic!(
                "socket {} never became connectable within {deadline:?}; daemon log:\n{log}",
                path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn scratch_root(label: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let short: String = label.chars().filter(|ch| ch.is_ascii_alphanumeric()).take(4).collect();
    // The OS spelling, not `/private/tmp`: the canonical form matters on macOS,
    // where `/var` is a symlink and the store rejects a path that walks through
    // one.
    let base = std::env::temp_dir()
        .canonicalize()
        .expect("the OS temporary directory must resolve");
    let root = base.join(format!("hr{}-{short}{n}", std::process::id()));
    std::fs::create_dir_all(&root).expect("scratch root");
    root
}

/// Removes the scratch tree even when an assertion fails mid-test.
struct ScratchRoot(PathBuf);

impl Drop for ScratchRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn reports_round_trip() {
    let _serial = instance_lock();
    let root = scratch_root("reports");
    let _root_guard = ScratchRoot(root.clone());
    let instance = Instance::start(&root, "one");
    let client = instance.client("reports-qa/1.0");

    let working_dir = instance.home.display().to_string();
    let session = client
        .create_session(Some(working_dir.clone()))
        .unwrap_or_else(|error| panic!("create_session: {error}\n{}", instance.logs()));
    let id = session.session_id.clone();
    client
        .send_context_message(&id, "reports fixture message")
        .unwrap_or_else(|error| panic!("send_context_message: {error}\n{}", instance.logs()));

    let hits = client
        .search_sessions("fixture", None)
        .unwrap_or_else(|error| panic!("search_sessions: {error}\n{}", instance.logs()));
    println!("search_sessions(\"fixture\", None): {} hit(s)", hits.len());
    for hit in hits.iter().take(3) {
        println!("  hit {} role={} preview={:?}", hit.session_id, hit.role, hit.preview);
    }

    let report = client
        .usage_report(None)
        .unwrap_or_else(|error| panic!("usage_report: {error}\n{}", instance.logs()));
    println!(
        "usage_report(None): session_id={:?} sessions={} totals.requests={} totals.input={:?} pricing={} estimated_cost={:?}",
        report.session_id,
        report.sessions.len(),
        report.totals.requests,
        report.totals.input,
        report.pricing,
        report.estimated_cost
    );
    assert_eq!(report.session_id, None, "a full report names no single session");

    let artifacts = client
        .list_artifacts(&id)
        .unwrap_or_else(|error| panic!("list_artifacts: {error}\n{}", instance.logs()));
    println!("list_artifacts({id}): {} entr(ies)", artifacts.len());
    for artifact in artifacts.iter().take(3) {
        println!("  {} {:?} {} bytes", artifact.path, artifact.kind, artifact.bytes);
    }

    client
        .ack_approval(&id, "no-such-approval")
        .unwrap_or_else(|error| panic!("ack_approval: {error}\n{}", instance.logs()));
    println!("ack_approval({id}, \"no-such-approval\"): ok");

    client
        .ack_failover(&id, "chain", "a", "b", 1)
        .unwrap_or_else(|error| panic!("ack_failover: {error}\n{}", instance.logs()));
    println!("ack_failover({id}, \"chain\", \"a\", \"b\", 1): ok");

    assert_eq!(session.session_id, id);
    assert_eq!(working_dir, instance.home.display().to_string());
}

#[test]
fn reports_reject_bad_input() {
    let _serial = instance_lock();
    let root = scratch_root("rejects");
    let _root_guard = ScratchRoot(root.clone());
    let instance = Instance::start(&root, "one");
    let client = instance.client("reports-qa-errors/1.0");

    let session = client
        .create_session(Some(instance.home.display().to_string()))
        .unwrap_or_else(|error| panic!("create_session: {error}\n{}", instance.logs()));
    let id = session.session_id.clone();

    let error = client
        .search_sessions("", None)
        .expect_err("a blank query matches nothing and must not be answered");
    println!("search_sessions(\"\", None): {}", error.code());
    assert_eq!(error.code(), "invalid_request");

    let error = client
        .usage_report(Some("no-such-session"))
        .expect_err("a named session with no stored record is not an empty report");
    println!("usage_report(Some(\"no-such-session\")): {}", error.code());
    assert_eq!(error.code(), "unknown_session");

    // Both acks are attach-gated: a request for a session this connection is
    // not attached to answers `unknown_session` before the empty-value check.
    let error = client
        .ack_approval(&id, "")
        .expect_err("an ack with no identifier would clear nothing");
    println!("ack_approval({id}, \"\"): {}", error.code());
    assert_eq!(error.code(), "invalid_request");

    let error = client
        .ack_failover(&id, "", "a", "b", 0)
        .expect_err("a partial failover key would clear nothing");
    println!("ack_failover({id}, \"\", \"a\", \"b\", 0): {}", error.code());
    assert_eq!(error.code(), "invalid_request");

    // A second `create_session` on the same connection answers with the session
    // it is already attached to, so the session without a transcript is created
    // on a second connection.
    let second = instance.client("reports-qa-second/1.0");
    let fresh = second
        .create_session(Some(instance.home.display().to_string()))
        .unwrap_or_else(|error| panic!("create_session on a second connection: {error}\n{}", instance.logs()));
    let error = second
        .list_artifacts(&fresh.session_id)
        .expect_err("a session with no stored transcript has no artifacts to list");
    println!("list_artifacts({}): {}", fresh.session_id, error.code());
    assert_eq!(error.code(), "unknown_session");
}
