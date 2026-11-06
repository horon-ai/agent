#![cfg(unix)]
//! ACP v1 session lifecycle and authentication against a real daemon.
//!
//! Each test owns an isolated `horon-agent serve` started with `--model`, so the
//! daemon reports a model without a provider profile and no model turn runs.
//! Every line the adapter writes is parsed and schema-validated by the mock
//! client's frame parser.

#[path = "support/acp_client.rs"]
mod acp_client;
#[path = "support/acp_daemon.rs"]
mod acp_daemon;
#[path = "support/acp_schema.rs"]
mod acp_schema;
#[path = "support/lifecycle.rs"]
mod lifecycle;

mod acp {
    use super::acp_client::{Frame, FrameParser};
    use super::acp_daemon::AcpDaemon;
    use super::lifecycle::{
        BINARY, HOLD_TURN_PROMPT, scratch_root, set_mode, wait_for_socket, write_config, write_provider_config,
    };
    use horon_agent::session::event_store_api as store;
    use serde_json::{Value, json};
    use std::io::{BufRead, BufReader, Write};
    use std::path::{Path, PathBuf};
    use std::process::{Child, ChildStdin, Command, Stdio};
    use std::sync::mpsc::{Receiver, RecvTimeoutError};
    use std::time::{Duration, Instant};

    const STARTUP: Duration = Duration::from_secs(120);
    const RESPONSE: Duration = Duration::from_secs(60);
    /// Bound for reloading a session that is already active on the connection:
    /// the daemon answers its configuration requests without a model turn.
    const ACTIVE_RELOAD: Duration = Duration::from_secs(5);
    /// Bound for a `session/load` that re-attaches a session this connection
    /// just deleted. The daemon takes ~0.6 s to attach a restored session, so
    /// anything near this bound means the adapter waited on a daemon that was
    /// never going to answer (the F3 D-1 stall waited 30.5 s).
    const LOAD_AFTER_DELETE: Duration = Duration::from_secs(3);
    /// Bound for the daemon's own log lines to reach `home/logs` after it has
    /// answered. The lines are written just after the response is flushed.
    const LOG_SETTLE: Duration = Duration::from_secs(10);
    /// Poll interval for the log settle wait: short enough to return as soon as
    /// the line lands, and only used inside the `LOG_SETTLE` deadline.
    const LOG_POLL: Duration = Duration::from_millis(20);
    const MODEL: &str = "gpt-5";

    struct Daemon {
        root: PathBuf,
        child: Child,
    }

    impl Daemon {
        fn start() -> Self {
            let root = scratch_root("acplife");
            for dir in ["home", "rt", "work"] {
                std::fs::create_dir_all(root.join(dir)).expect("scratch dirs");
            }
            for dir in [root.clone(), root.join("home"), root.join("rt")] {
                set_mode(&dir, 0o700);
            }
            write_config(&root.join("home"));
            write_provider_config(&root.join("home"), "http://127.0.0.1:9/v1", MODEL);
            let log = std::fs::File::create(root.join("rt/daemon.log")).expect("daemon log");
            let errors = log.try_clone().expect("daemon log clone");
            let mut command = Command::new(BINARY);
            command.args(["--model", MODEL, "serve", "--no-update"]);
            isolate(&mut command, &root);
            let child = command
                .stdin(Stdio::null())
                .stdout(log)
                .stderr(Stdio::from(errors))
                .spawn()
                .expect("spawn daemon");
            let daemon = Self { root, child };
            wait_for_socket(&daemon.socket(), STARTUP);
            daemon
        }

        fn socket(&self) -> PathBuf {
            self.root.join("rt/horon-agent.sock")
        }

        fn work(&self) -> String {
            self.root.join("work").display().to_string()
        }

        /// The daemon's own log, for a failure message.
        fn logs(&self) -> String {
            let text = std::fs::read_to_string(self.root.join("rt/daemon.log")).unwrap_or_default();
            let tail: Vec<&str> = text.lines().rev().take(40).collect();
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        }
    }

    /// Everything the daemon has written under `home/logs`, read fresh so a
    /// caller can call it again after more lines land.
    fn home_logs(daemon: &Daemon) -> String {
        std::fs::read_dir(daemon.root.join("home/logs"))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| std::fs::read_to_string(entry.path()).unwrap_or_default())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The daemon flushes its refusal to the client before it writes the
    /// matching warning (app-core's client_lifecycle flushes the client events,
    /// then calls logging::warn), so one read right after the response can miss
    /// the last line. Wait for `expected` occurrences of `needle` in
    /// `home/logs`, bounded by `deadline`, and return the count last read.
    fn wait_for_home_log_count(daemon: &Daemon, needle: &str, expected: usize, deadline: Duration) -> usize {
        let end = Instant::now() + deadline;
        loop {
            let count = home_logs(daemon).matches(needle).count();
            if count >= expected || Instant::now() >= end {
                return count;
            }
            std::thread::sleep(LOG_POLL);
        }
    }

    impl Drop for Daemon {
        fn drop(&mut self) {
            if std::thread::panicking() {
                let logs_dir = self.root.join("home/logs");
                let mut logs: Vec<PathBuf> = std::fs::read_dir(&logs_dir)
                    .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
                    .unwrap_or_default();
                logs.sort();
                for log in logs {
                    let text = std::fs::read_to_string(&log).unwrap_or_default();
                    let lines: Vec<&str> = text.lines().filter(|line| !line.contains("TIMING")).collect();
                    let start = lines.len().saturating_sub(60);
                    eprintln!("--- {} ---\n{}", log.display(), lines[start..].join("\n"));
                }
                for log in ["rt/daemon.log", "acp-stderr.log"] {
                    let text = std::fs::read_to_string(self.root.join(log)).unwrap_or_default();
                    let tail: Vec<&str> = text.lines().rev().take(40).collect();
                    eprintln!("--- {log} (last 40 lines, newest first) ---\n{}", tail.join("\n"));
                }
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn isolate(command: &mut Command, root: &Path) {
        command.env_clear();
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        command
            .env("HORON_AGENT_HOME", root.join("home"))
            .env("HORON_AGENT_RUNTIME_DIR", root.join("rt"))
            .env("HOME", root)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("HORON_AGENT_SOCKET", root.join("rt/horon-agent.sock"))
            .env("HORON_AGENT_NO_AUTO_UPDATE", "1")
            .env("HORON_AGENT_MEMORY_ENABLED", "0")
            .env("HORON_AGENT_MEMORY_SIDECAR_ENABLED", "0")
            .env("HORON_AGENT_NON_INTERACTIVE", "1")
            .env("HORON_AGENT_TOOL_PROFILE", "acp")
            .env_remove("HORON_AGENT_API_SOCKET");
    }

    struct Adapter {
        child: Child,
        stdin: Option<ChildStdin>,
        lines: Receiver<String>,
        parser: FrameParser,
        next_id: i64,
        early: Vec<(Value, Vec<Value>, Result<Value, Value>)>,
    }

    impl Adapter {
        fn spawn(daemon: &Daemon) -> Self {
            Self::spawn_in(&daemon.root)
        }

        /// An adapter for the daemon whose scratch root is `root` (home, rt and
        /// the socket under it), such as a scripted-provider [`AcpDaemon`].
        fn spawn_in(root: &Path) -> Self {
            let mut command = Command::new(BINARY);
            command.arg("acp");
            isolate(&mut command, root);
            let mut child = command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(root.join("acp-stderr.log"))
                        .map(Stdio::from)
                        .unwrap_or_else(|_| Stdio::null()),
                )
                .spawn()
                .expect("spawn horon-agent acp");
            let stdin = child.stdin.take();
            let stdout = child.stdout.take().expect("adapter stdout");
            let (sender, lines) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            });
            let mut adapter = Self {
                child,
                stdin,
                lines,
                parser: FrameParser::new(),
                next_id: 0,
                early: Vec::new(),
            };
            let init = adapter.request("initialize", json!({"protocolVersion": 1}));
            assert_eq!(init["protocolVersion"], 1, "{init}");
            adapter
        }

        fn send(&mut self, method: &str, params: Value) -> Value {
            self.next_id += 1;
            let id = json!(self.next_id);
            self.parser.note_request(&id, method);
            let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
            let stdin = self.stdin.as_mut().expect("adapter stdin open");
            writeln!(stdin, "{line}").expect("write to the adapter");
            stdin.flush().expect("flush to the adapter");
            id
        }

        /// The response to request `id` (result or error), with every
        /// notification written before it.
        fn wait(&mut self, id: &Value) -> (Vec<Value>, Result<Value, Value>) {
            if let Some(position) = self.early.iter().position(|(early_id, _, _)| early_id == id) {
                let (_, before, result) = self.early.remove(position);
                return (before, result);
            }
            let end = Instant::now() + RESPONSE;
            let mut before = Vec::new();
            loop {
                let remaining = end.saturating_duration_since(Instant::now());
                let line = match self.lines.recv_timeout(remaining) {
                    Ok(line) => line,
                    Err(RecvTimeoutError::Timeout) => panic!("request {id} got no response within {RESPONSE:?}"),
                    Err(RecvTimeoutError::Disconnected) => panic!("the adapter closed stdout before answering {id}"),
                };
                match self.parser.parse(&line).unwrap_or_else(|error| panic!("{error}")) {
                    Frame::Response {
                        id: response_id,
                        result,
                    } if response_id == *id => return (before, result),
                    Frame::Response {
                        id: response_id,
                        result,
                    } => self.early.push((response_id, std::mem::take(&mut before), result)),
                    Frame::Notification { method, params } => before.push(json!({"method": method, "params": params})),
                    other => panic!("unexpected frame while waiting for {id}: {other:?}"),
                }
            }
        }

        fn call(&mut self, method: &str, params: Value) -> (Vec<Value>, Result<Value, Value>) {
            let id = self.send(method, params);
            self.wait(&id)
        }

        fn request(&mut self, method: &str, params: Value) -> Value {
            match self.call(method, params) {
                (_, Ok(result)) => result,
                (_, Err(error)) => panic!("{method} failed: {error}"),
            }
        }

        fn error(&mut self, method: &str, params: Value) -> Value {
            match self.call(method, params) {
                (_, Err(error)) => error,
                (_, Ok(result)) => panic!("{method} succeeded unexpectedly: {result}"),
            }
        }

        fn new_session(&mut self, cwd: &str) -> String {
            let result = self.request("session/new", json!({"cwd": cwd, "mcpServers": []}));
            result["sessionId"].as_str().expect("sessionId").to_string()
        }

        fn finish(mut self) {
            self.stdin.take();
            let end = Instant::now() + RESPONSE;
            loop {
                if let Some(status) = self.child.try_wait().expect("wait for the adapter") {
                    assert!(status.success(), "the adapter exits cleanly on stdin EOF: {status}");
                    return;
                }
                assert!(Instant::now() < end, "the adapter did not exit on stdin EOF");
                let remaining = end.saturating_duration_since(Instant::now());
                let _ = self.lines.recv_timeout(remaining.min(Duration::from_millis(50)));
            }
        }
    }

    impl Drop for Adapter {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn history_updates(notifications: &[Value]) -> Vec<&Value> {
        notifications
            .iter()
            .filter(|note| {
                matches!(
                    note["params"]["update"]["sessionUpdate"].as_str(),
                    Some(
                        "user_message_chunk"
                            | "agent_message_chunk"
                            | "agent_thought_chunk"
                            | "tool_call"
                            | "tool_call_update"
                    )
                )
            })
            .collect()
    }

    fn assert_not_found(error: &Value, session_id: &str) {
        assert_eq!(error["code"], -32002, "{error}");
        assert_eq!(error["data"]["sessionId"], session_id, "{error}");
    }

    #[test]
    fn authenticate_ten_times_is_stable() {
        let daemon = Daemon::start();
        let mut adapter = Adapter::spawn(&daemon);
        for _ in 0..10 {
            let result = adapter.request("authenticate", json!({"methodId": "horon-agent-credentials"}));
            assert_eq!(result, json!({}));
        }
        let session_id = adapter.new_session(&daemon.work());
        assert!(!session_id.is_empty());
        adapter.finish();
    }

    #[test]
    fn is4_resume_unprompted_session_succeeds() {
        let daemon = Daemon::start();
        let mut adapter = Adapter::spawn(&daemon);
        let session_id = adapter.new_session(&daemon.work());
        let params = json!({"sessionId": session_id, "cwd": daemon.work(), "mcpServers": []});
        let (before, result) = adapter.call("session/resume", params.clone());
        assert!(result.as_ref().is_ok_and(Value::is_object), "{result:?}");
        assert!(history_updates(&before).is_empty(), "{before:?}");

        let mut other = Adapter::spawn(&daemon);
        let (before, result) = other.call("session/resume", params);
        assert!(
            result.as_ref().is_ok_and(Value::is_object),
            "a second connection resumes it: {result:?}"
        );
        assert!(history_updates(&before).is_empty(), "{before:?}");
        other.finish();
        adapter.finish();
    }

    #[test]
    fn is4_reload_active_session_settles_on_the_daemon_ack() {
        let daemon = Daemon::start();
        let mut adapter = Adapter::spawn(&daemon);
        let session_id = adapter.new_session(&daemon.work());
        for method in ["session/load", "session/resume"] {
            let params = json!({"sessionId": session_id, "cwd": daemon.work(), "mcpServers": []});
            let started = Instant::now();
            let (_, result) = adapter.call(method, params);
            let elapsed = started.elapsed();
            assert!(result.as_ref().is_ok_and(Value::is_object), "{method}: {result:?}");
            assert!(
                elapsed < ACTIVE_RELOAD,
                "{method} of an active session took {elapsed:?}"
            );
        }
        adapter.finish();
    }

    #[test]
    fn is4_load_unknown_id_is_32002() {
        let daemon = Daemon::start();
        let mut adapter = Adapter::spawn(&daemon);
        let unknown = "session_nope_1700000000000_0123456789abcdef";
        for method in ["session/load", "session/resume"] {
            let error = adapter.error(
                method,
                json!({"sessionId": unknown, "cwd": daemon.work(), "mcpServers": []}),
            );
            assert_not_found(&error, unknown);
        }
        let error = adapter.error(
            "session/prompt",
            json!({"sessionId": "nope", "prompt": [{"type": "text", "text": "hi"}]}),
        );
        assert_not_found(&error, "nope");
        adapter.finish();
    }

    #[test]
    fn is4_session_list_close_delete_round_trip() {
        let daemon = Daemon::start();
        let mut adapter = Adapter::spawn(&daemon);
        let missing_cwd = adapter.error("session/new", json!({"mcpServers": []}));
        assert_eq!(missing_cwd["code"], -32602, "{missing_cwd}");
        let relative = adapter.error("session/new", json!({"cwd": "work", "mcpServers": []}));
        assert_eq!(relative["code"], -32602, "{relative}");

        let session_id = adapter.new_session(&daemon.work());
        let other_id = adapter.new_session(&daemon.work());
        assert_ne!(session_id, other_id);
        let listed = adapter.request("session/list", json!({}));
        for info in listed["sessions"].as_array().expect("sessions array") {
            assert!(Path::new(info["cwd"].as_str().expect("cwd")).is_absolute(), "{info}");
        }
        let empty = adapter.request("session/list", json!({"cwd": "/nowhere/at/all"}));
        assert_eq!(empty, json!({"sessions": []}));
        let bad_cursor = adapter.error("session/list", json!({"cursor": "!!!!"}));
        assert_eq!(bad_cursor["code"], -32602, "{bad_cursor}");

        assert_eq!(
            adapter.request("session/close", json!({"sessionId": session_id})),
            json!({})
        );
        let again = adapter.error("session/close", json!({"sessionId": session_id}));
        assert_not_found(&again, &session_id);
        for _ in 0..2 {
            assert_eq!(
                adapter.request("session/delete", json!({"sessionId": session_id})),
                json!({})
            );
        }
        assert_eq!(
            adapter.request(
                "session/delete",
                json!({"sessionId": "session_never_1_0123456789abcdef"})
            ),
            json!({})
        );
        let after = adapter.request("session/list", json!({"cwd": daemon.work()}));
        assert!(
            after["sessions"]
                .as_array()
                .expect("sessions array")
                .iter()
                .all(|info| info["sessionId"] != session_id.as_str()),
            "{after}"
        );
        adapter.finish();
    }

    /// The removed `--login` argument is a clap usage error and nothing starts.
    #[test]
    fn acp_login_flag_is_gone() {
        let root = scratch_root("acplogin");
        for dir in ["home", "rt"] {
            std::fs::create_dir_all(root.join(dir)).expect("scratch dirs");
        }
        let mut command = Command::new(BINARY);
        command.args(["acp", "--login"]);
        isolate(&mut command, &root);
        let output = command
            .stdin(Stdio::null())
            .output()
            .expect("run horon-agent acp --login");
        let daemon_started = root.join("rt/horon-agent.sock").exists();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            output.status.code(),
            Some(2),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("unexpected argument"), "stderr: {stderr}");
        assert!(
            output.stdout.is_empty(),
            "stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(!daemon_started, "--login never starts the daemon");
    }

    /// A session with one message on disk, written through the store the
    /// way a prompted session is, since no model turn can run here.
    ///
    /// The returned writer holds the session's store file lock, which is what
    /// the daemon's own writer collides with; dropping it releases the lock.
    fn persisted_session(daemon: &Daemon, session_id: &str) -> store::BlockingWriter {
        let paths = store::SessionPaths::new(daemon.root.join("home/sessions"));
        let id = store::SessionId::parse(session_id).expect("session id");
        let mut writer = store::BlockingWriter::open(paths, id.clone()).expect("session writer");
        let metadata = store::SessionMetadata {
            working_dir: Some(daemon.work()),
            ..store::SessionMetadata::default()
        };
        let root = store::EventDraft::new(
            id.clone(),
            store::EventId::new_v4(),
            None,
            store::OriginId::new_v4(),
            metadata.created_at,
            store::EventKind::new(store::KnownEventKind::SessionCreated),
            json!(store::SessionCreatedPayload {
                branch_root: false,
                reset_provenance: None,
                metadata,
            }),
        )
        .seal()
        .expect("root event");
        writer.append(&root).expect("append root");
        super::lifecycle::append_message(&mut writer, &id, &root.envelope().event_id, "m1", "user", "persisted");
        writer
    }

    /// F3 D-1's daemon shape, made deterministic: another owner holds the
    /// session's store writer, so the daemon refuses the attach with the
    /// store's retryable busy error. The daemon now sends that refusal instead
    /// of falling silent, so the adapter retries once and answers a clear
    /// -32603 within the bound; F3 waited 30.5 s here for "daemon
    /// disconnected". Once the writer is released the same load succeeds.
    #[test]
    fn is4_load_refused_by_a_busy_session_writer_answers_fast() {
        let daemon = Daemon::start();
        let session_id = "session_acpbusy_1700000000000_0123456789abcdef";
        let held = persisted_session(&daemon, session_id);
        let mut adapter = Adapter::spawn(&daemon);
        let load = json!({"sessionId": session_id, "cwd": daemon.work(), "mcpServers": []});

        let started = Instant::now();
        let (_, result) = adapter.call("session/load", load.clone());
        let elapsed = started.elapsed();
        eprintln!("busy-writer load answered in {elapsed:?}: {result:?}");

        let error = result.expect_err("a session whose writer is owned elsewhere is refused");
        assert_eq!(error["code"], -32603, "{error}");
        let message = error["message"].as_str().expect("message");
        assert!(
            message.contains(session_id) && message.contains("session writer is already owned"),
            "the client gets the daemon's refusal instead of 'daemon disconnected': {message}\n{}",
            daemon.logs()
        );
        assert!(
            elapsed < LOAD_AFTER_DELETE,
            "the refusal answered in {elapsed:?}: one attempt plus one retry, and no wait\n{}",
            daemon.logs()
        );
        let refusals = wait_for_home_log_count(&daemon, "Target-aware subscribe failed to bind", 2, LOG_SETTLE);
        assert_eq!(
            refusals,
            2,
            "the daemon refused the first attempt and the one retry\n{}\n{}",
            home_logs(&daemon),
            daemon.logs()
        );

        drop(held);
        let (before, loaded) = adapter.call("session/load", load);
        assert!(
            loaded.as_ref().is_ok_and(Value::is_object),
            "the released session loads: {loaded:?}\n{}",
            daemon.logs()
        );
        assert_eq!(history_updates(&before).len(), 1, "replayed once: {before:?}");
        adapter.finish();
    }

    #[test]
    fn is4_delete_persisted_session_three_times_is_idempotent() {
        let daemon = Daemon::start();
        let session_id = "session_acpdelete_1700000000000_0123456789abcdef";
        drop(persisted_session(&daemon, session_id));
        let mut adapter = Adapter::spawn(&daemon);
        let listed = adapter.request("session/list", json!({"cwd": daemon.work()}));
        assert_eq!(listed["sessions"][0]["sessionId"], session_id, "{listed}");
        let params = json!({"sessionId": session_id, "cwd": daemon.work(), "mcpServers": []});
        let (before, result) = adapter.call("session/load", params);
        assert!(result.as_ref().is_ok_and(Value::is_object), "{result:?}");
        assert!(
            !history_updates(&before).is_empty(),
            "load replays the stored message: {before:?}"
        );
        assert_eq!(
            adapter.request("session/close", json!({"sessionId": session_id})),
            json!({})
        );
        for attempt in 1..=3 {
            let (_, result) = adapter.call("session/delete", json!({"sessionId": session_id}));
            assert_eq!(result, Ok(json!({})), "delete #{attempt}");
        }
        let after = adapter.request("session/list", json!({"cwd": daemon.work()}));
        assert_eq!(after, json!({"sessions": []}));
        adapter.finish();
    }

    fn prompt(session_id: &str, text: &str) -> Value {
        json!({"sessionId": session_id, "prompt": [{"type": "text", "text": text}]})
    }

    /// F3 D-1: `session/delete` and then `session/load` of the same id on one
    /// connection raced the daemon's session writer once in four attempts and
    /// left the adapter waiting 30.5 s before it answered -32603 "daemon
    /// disconnected". Every round must answer promptly: with the load result
    /// (delete archives, so the session is still loadable) or with a clear
    /// error, never after a stall.
    #[test]
    fn is4_delete_then_load_answers_promptly() {
        let daemon = Daemon::start();
        let mut adapter = Adapter::spawn(&daemon);
        let work = daemon.work();
        let load = |session_id: &str| json!({"sessionId": session_id, "cwd": work, "mcpServers": []});
        for round in 1..=20 {
            let session_id = adapter.new_session(&daemon.work());
            let (_, loaded) = adapter.call("session/load", load(&session_id));
            assert!(
                loaded.as_ref().is_ok_and(Value::is_object),
                "round {round}: the first load failed: {loaded:?}"
            );
            let (_, deleted) = adapter.call("session/delete", json!({"sessionId": session_id}));
            assert_eq!(deleted, Ok(json!({})), "round {round}: delete");

            let started = Instant::now();
            let (before, result) = adapter.call("session/load", load(&session_id));
            let elapsed = started.elapsed();
            eprintln!(
                "round {round}: delete -> load answered in {elapsed:?}: ok={}",
                result.is_ok()
            );
            assert!(
                elapsed < LOAD_AFTER_DELETE,
                "round {round}: delete -> load took {elapsed:?}: {result:?}\n{}",
                daemon.logs()
            );
            match result {
                Ok(value) => assert!(
                    value.is_object(),
                    "round {round}: load answered {value}; replayed {} updates",
                    history_updates(&before).len()
                ),
                Err(error) => panic!("round {round}: delete -> load failed: {error}\n{}", daemon.logs()),
            }
        }
        adapter.finish();
    }

    /// Both tests below run a model turn, so the daemon serves it through the
    /// scripted provider profile.
    fn scripted_daemon() -> (AcpDaemon, String) {
        let daemon = AcpDaemon::start("acp");
        let work = daemon.root.join("work");
        std::fs::create_dir_all(&work).expect("work dir");
        (daemon, work.display().to_string())
    }

    #[test]
    fn is4_close_during_prompt_answers_cancelled() {
        let (daemon, work) = scripted_daemon();
        daemon.provider.hold_turn();
        let mut adapter = Adapter::spawn_in(&daemon.root);
        let session_id = adapter.new_session(&work);
        let prompt_id = adapter.send("session/prompt", prompt(&session_id, HOLD_TURN_PROMPT));
        daemon
            .provider
            .wait_for_request(HOLD_TURN_PROMPT, RESPONSE)
            .unwrap_or_else(|error| panic!("{error}\n{}", daemon.logs()));
        let close_id = adapter.send("session/close", json!({"sessionId": session_id}));
        let (_, prompt_result) = adapter.wait(&prompt_id);
        assert_eq!(prompt_result.expect("prompt result")["stopReason"], "cancelled");
        let (between, close_result) = adapter.wait(&close_id);
        assert_eq!(close_result.expect("close result"), json!({}));
        assert!(
            between
                .iter()
                .all(|note| note["params"]["update"]["sessionUpdate"] != "agent_message_chunk"),
            "no turn output after the cancelled response: {between:?}"
        );
        daemon.provider.release_turn();
        adapter.finish();
    }

    #[test]
    fn is4_load_replays_prompted_history_before_response() {
        let (daemon, work) = scripted_daemon();
        let mut first = Adapter::spawn_in(&daemon.root);
        let session_id = first.new_session(&work);
        let (_, result) = first.call("session/prompt", prompt(&session_id, "hello"));
        assert_eq!(
            result.expect("prompt result")["stopReason"],
            "end_turn",
            "{}",
            daemon.logs()
        );
        first.finish();

        let mut second = Adapter::spawn_in(&daemon.root);
        let params = json!({"sessionId": session_id, "cwd": work, "mcpServers": []});
        let (before, result) = second.call("session/load", params);
        assert!(result.as_ref().is_ok_and(Value::is_object), "{result:?}");
        assert!(
            !history_updates(&before).is_empty(),
            "load replays before answering: {before:?}"
        );
        let listed = second.request("session/list", json!({"cwd": work}));
        assert!(
            listed["sessions"]
                .as_array()
                .expect("sessions array")
                .iter()
                .any(|info| info["sessionId"] == session_id.as_str()),
            "a prompted session is listed: {listed}"
        );
        second.finish();
    }
}
