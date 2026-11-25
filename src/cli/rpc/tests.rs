//! Tests for the stdio JSONL RPC adapter.
//!
//! The daemon end is scripted over an in-process socket pair, so these tests
//! exercise the real protocol path: stdin lines in, daemon requests out, daemon
//! events in, stdout frames out.

use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::cli::provider_init::ProviderChoice;
use crate::transport::{ReadHalf, Stream, WriteHalf};

use super::adapter::{self, Input};
use super::link;

const WAIT: Duration = Duration::from_secs(5);
const SESSION_ID: &str = "session_rpc_test";

async fn within<T>(what: &str, future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, future)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// The host side of the connection: stdin writer, stdout lines, and the running
/// adapter.
struct Host {
    input: Option<DuplexStream>,
    output: Lines<BufReader<DuplexStream>>,
    serve: JoinHandle<anyhow::Result<()>>,
}

struct FakeDaemon {
    lines: Lines<BufReader<ReadHalf>>,
    writer: WriteHalf,
}

fn scripted_daemon() -> (link::Connector, mpsc::UnboundedReceiver<FakeDaemon>) {
    let (sender, daemons) = mpsc::unbounded_channel();
    let connector: link::Connector = std::sync::Arc::new(move || {
        let sender = sender.clone();
        Box::pin(async move {
            let (adapter_side, daemon_side) = Stream::pair()?;
            let (reader, writer) = adapter_side.into_split();
            let (daemon_reader, daemon_writer) = daemon_side.into_split();
            let _ = sender.send(FakeDaemon {
                lines: BufReader::new(daemon_reader).lines(),
                writer: daemon_writer,
            });
            Ok((reader, writer))
        })
    });
    (connector, daemons)
}

fn start_host(connector: link::Connector) -> Host {
    start_host_with_plan(connector, false)
}

fn start_host_with_plan(connector: link::Connector, plan_mode: bool) -> Host {
    let (output_writer, output_reader) = tokio::io::duplex(1 << 20);
    let (input_writer, input_reader) = tokio::io::duplex(1 << 20);
    let provider = ProviderChoice::Auto;
    let input: Input = Box::new(BufReader::new(input_reader));
    let serve = tokio::spawn(async move {
        adapter::run(adapter::Start {
            provider: &provider,
            model: None,
            provider_profile: None,
            cwd: None,
            resume: None,
            plan_mode,
            connector: Some(connector),
            out: Box::new(output_writer),
            input,
        })
        .await
    });
    Host {
        input: Some(input_writer),
        output: BufReader::new(output_reader).lines(),
        serve,
    }
}

impl FakeDaemon {
    async fn recv(&mut self) -> Value {
        let line = within("a daemon request", self.lines.next_line())
            .await
            .unwrap()
            .expect("the adapter closed the daemon socket");
        serde_json::from_str(&line).unwrap()
    }

    async fn recv_type(&mut self, kind: &str) -> Value {
        loop {
            let request = self.recv().await;
            if request["type"] == kind {
                return request;
            }
        }
    }

    async fn send(&mut self, frame: Value) {
        let mut line = frame.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await.unwrap();
        self.writer.flush().await.unwrap();
    }

    /// Answer the handshake every adapter start performs: subscribe, then
    /// history.
    async fn handshake(&mut self) {
        let subscribe = self.recv_type("subscribe").await;
        self.send(json!({"type": "ack", "id": subscribe["id"]})).await;
        let history = self.recv_type("get_history").await;
        self.send(history_frame(history["id"].as_u64().unwrap())).await;
    }
}

fn history_frame(id: u64) -> Value {
    json!({
        "type": "history",
        "id": id,
        "session_id": SESSION_ID,
        "messages": [],
        "provider_name": "test-provider",
        "provider_model": "test-model",
        "available_models": ["test-model", "other-model"],
    })
}

impl Host {
    async fn send(&mut self, value: Value) {
        self.send_raw(&format!("{value}\n")).await;
    }

    async fn send_raw(&mut self, text: &str) {
        let input = self.input.as_mut().expect("stdin open");
        input.write_all(text.as_bytes()).await.unwrap();
        input.flush().await.unwrap();
    }

    async fn recv(&mut self) -> Value {
        let line = within("a stdout frame", self.output.next_line())
            .await
            .unwrap()
            .expect("stdout closed");
        serde_json::from_str(&line).unwrap_or_else(|error| panic!("stdout line is not JSON ({error}): {line}"))
    }

    /// The three frames every start emits: `ready`, the widget acknowledgement,
    /// and the command list.
    async fn handshake(&mut self) {
        let ready = self.recv().await;
        assert_eq!(ready["type"], "ready", "{ready}");
        assert_eq!(ready["protocolVersion"], 1);
        assert_eq!(ready["supportedProtocolVersions"], json!([1]));
        assert_eq!(ready["maxFrameBytes"], 1024 * 1024);
        assert_eq!(ready["maxReassembledFrameBytes"], 64 * 1024 * 1024);
        assert_eq!(self.recv().await["type"], "extension_ui_request");
        assert_eq!(self.recv().await["type"], "available_commands_update");
    }

    /// Read until the response for `id`, returning everything before it.
    async fn response(&mut self, id: Value) -> (Vec<Value>, Value) {
        let mut before = Vec::new();
        loop {
            let frame = self.recv().await;
            if frame["type"] == "response" && frame["id"] == id {
                return (before, frame);
            }
            before.push(frame);
        }
    }

    async fn close(self) {
        let Host { input, output, serve } = self;
        drop(input);
        let _ = within("the adapter to stop on EOF", serve).await;
        // The adapter closes stdout when it returns.
        let _ = output;
    }
}

#[tokio::test]
async fn handshake_announces_protocol_and_commands() {
    let (connector, mut daemons) = scripted_daemon();
    let mut host = start_host(connector);
    let mut daemon = daemons.recv().await.expect("a daemon connection");
    daemon.handshake().await;
    host.handshake().await;
    host.close().await;
}

#[tokio::test]
async fn negotiate_protocol_answers_version_one() {
    let (connector, mut daemons) = scripted_daemon();
    let mut host = start_host(connector);
    let mut daemon = daemons.recv().await.expect("a daemon connection");
    daemon.handshake().await;
    host.handshake().await;

    host.send(json!({"id": "n1", "type": "negotiate_protocol", "protocolVersion": 2}))
        .await;
    let (_, response) = host.response(json!("n1")).await;
    assert_eq!(response["success"], true, "{response}");
    assert_eq!(response["command"], "negotiate_protocol");
    // The host may ask for 2; this build answers 1 and the host downgrades.
    assert_eq!(response["data"]["protocolVersion"], 1);
    host.close().await;
}

#[tokio::test]
async fn unknown_command_fails_and_keeps_the_connection() {
    let (connector, mut daemons) = scripted_daemon();
    let mut host = start_host(connector);
    let mut daemon = daemons.recv().await.expect("a daemon connection");
    daemon.handshake().await;
    host.handshake().await;

    host.send(json!({"id": "u1", "type": "nope"})).await;
    let (_, response) = host.response(json!("u1")).await;
    assert_eq!(response["success"], false, "{response}");
    assert_eq!(response["code"], "unsupported_command");

    // The connection still answers a known command afterwards.
    host.send(json!({"id": "g1", "type": "get_state"})).await;
    let (_, response) = host.response(json!("g1")).await;
    assert_eq!(response["success"], true, "{response}");
    host.close().await;
}

#[tokio::test]
async fn non_json_lines_are_ignored() {
    let (connector, mut daemons) = scripted_daemon();
    let mut host = start_host(connector);
    let mut daemon = daemons.recv().await.expect("a daemon connection");
    daemon.handshake().await;
    host.handshake().await;

    host.send_raw("not json\n\n[]\n{\"type\":123}\n").await;
    host.send(json!({"id": "g1", "type": "get_state"})).await;
    let (_, response) = host.response(json!("g1")).await;
    assert_eq!(response["success"], true, "{response}");
    host.close().await;
}

#[tokio::test]
async fn get_state_reports_the_session() {
    let (connector, mut daemons) = scripted_daemon();
    let mut host = start_host(connector);
    let mut daemon = daemons.recv().await.expect("a daemon connection");
    daemon.handshake().await;
    host.handshake().await;

    host.send(json!({"id": "s1", "type": "get_state"})).await;
    let (_, response) = host.response(json!("s1")).await;
    let state = &response["data"];
    assert_eq!(state["sessionId"], SESSION_ID, "{state}");
    assert_eq!(state["model"]["id"], "test-model", "{state}");
    assert_eq!(state["isStreaming"], false);
    assert_eq!(state["todoPhases"], json!([]));
    assert_eq!(state["contextUsage"]["contextWindow"], 200000);
    host.close().await;
}

#[tokio::test]
async fn prompt_streams_one_turn_and_reports_once() {
    let (connector, mut daemons) = scripted_daemon();
    let mut host = start_host(connector);
    let mut daemon = daemons.recv().await.expect("a daemon connection");
    daemon.handshake().await;
    host.handshake().await;

    host.send(json!({"id": "p1", "type": "prompt", "message": "hello"}))
        .await;
    let message = daemon.recv_type("message").await;
    assert_eq!(message["content"], "hello");
    let message_id = message["id"].as_u64().unwrap();
    daemon.send(json!({"type": "ack", "id": message_id})).await;
    daemon.send(json!({"type": "text_delta", "text": "hi "})).await;
    daemon.send(json!({"type": "text_delta", "text": "there"})).await;
    daemon.send(json!({"type": "text_done"})).await;
    daemon.send(json!({"type": "message_end"})).await;
    daemon.send(json!({"type": "done", "id": message_id})).await;

    let (mut frames, response) = host.response(json!("p1")).await;
    assert_eq!(response["success"], true, "{response}");
    assert_eq!(response["command"], "prompt");
    assert!(response.get("data").is_none(), "prompt acks without data: {response}");

    // The response itself is already consumed, so append the trailing frames.
    while frames.last().map(|frame| frame["type"].as_str()) != Some(Some("session_settled")) {
        frames.push(host.recv().await);
    }
    let kinds: Vec<&str> = frames
        .iter()
        .map(|frame| frame["type"].as_str().unwrap_or_default())
        .collect();

    assert_eq!(kinds.first().copied(), Some("agent_start"), "{kinds:?}");
    assert_eq!(
        kinds.iter().filter(|kind| **kind == "prompt_result").count(),
        1,
        "{kinds:?}"
    );
    assert_eq!(
        kinds.iter().filter(|kind| **kind == "session_settled").count(),
        1,
        "{kinds:?}"
    );
    // A text delta is an `assistantMessageEvent` inside a `message_update`
    // frame, never a frame of its own, so the events are what gets checked.
    let events: Vec<&str> = frames
        .iter()
        .filter(|frame| frame["type"] == "message_update")
        .map(|frame| frame["assistantMessageEvent"]["type"].as_str().unwrap_or_default())
        .collect();
    assert!(events.contains(&"text_start"), "{events:?}");
    let deltas: Vec<&str> = frames
        .iter()
        .filter(|frame| frame["type"] == "message_update")
        .map(|frame| frame["assistantMessageEvent"]["delta"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(deltas.concat(), "hi there", "{deltas:?}");
    assert_eq!(kinds.last().copied(), Some("session_settled"));
    host.close().await;
}

#[tokio::test]
async fn abort_while_idle_is_accepted() {
    let (connector, mut daemons) = scripted_daemon();
    let mut host = start_host(connector);
    let mut daemon = daemons.recv().await.expect("a daemon connection");
    daemon.handshake().await;
    host.handshake().await;

    host.send(json!({"id": "a1", "type": "abort"})).await;
    let (_, response) = host.response(json!("a1")).await;
    assert_eq!(response["success"], true, "{response}");
    host.close().await;
}

#[tokio::test]
async fn oversized_frame_is_rejected_without_ending_the_session() {
    let (connector, mut daemons) = scripted_daemon();
    let mut host = start_host(connector);
    let mut daemon = daemons.recv().await.expect("a daemon connection");
    daemon.handshake().await;
    host.handshake().await;

    let oversized = format!(
        "{{\"id\":\"big\",\"type\":\"prompt\",\"message\":\"{}\"}}\n",
        "x".repeat(super::frame::MAX_FRAME_BYTES)
    );
    host.send_raw(&oversized).await;
    let frame = host.recv().await;
    assert_eq!(frame["type"], "response", "{frame}");
    assert_eq!(frame["success"], false);
    assert_eq!(frame["code"], "frame_too_large");
    assert_eq!(frame["id"], "big");

    host.send(json!({"id": "g1", "type": "get_state"})).await;
    let (_, response) = host.response(json!("g1")).await;
    assert_eq!(response["success"], true, "{response}");
    host.close().await;
}

#[tokio::test]
async fn set_todos_persists_phases_and_answers_them_back() {
    let _env = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let previous = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", home.path());

    let (connector, mut daemons) = scripted_daemon();
    let mut host = start_host(connector);
    let mut daemon = daemons.recv().await.expect("a daemon connection");
    daemon.handshake().await;
    host.handshake().await;

    host.send(json!({
        "id": "t1",
        "type": "set_todos",
        "phases": [
            {"name": "Build", "tasks": [
                {"content": "write the adapter", "status": "completed"},
                {"content": "wire the flag", "status": "in_progress"},
            ]},
            {"name": "Verify", "tasks": [
                {"content": "run the gate", "status": "blocked", "blocker": "needs CI"},
            ]},
        ],
    }))
    .await;
    let (_, response) = host.response(json!("t1")).await;
    assert_eq!(response["success"], true, "{response}");
    let phases = &response["data"]["todoPhases"];
    assert_eq!(phases[0]["name"], "Build", "{phases}");
    assert_eq!(phases[0]["tasks"][1]["status"], "in_progress");
    // `blocked` is a real status here, so it survives with its reason.
    assert_eq!(phases[1]["tasks"][0]["status"], "blocked", "{phases}");

    let stored = crate::todo::load_todos(SESSION_ID).expect("todos written");
    assert_eq!(stored.len(), 3, "{stored:?}");
    assert_eq!(stored[2].blocker.as_deref(), Some("needs CI"));

    host.close().await;
    match previous {
        Some(previous) => crate::env::set_var("HORON_HOME", previous),
        None => crate::env::remove_var("HORON_HOME"),
    }
}

#[tokio::test]
async fn available_models_projects_the_catalog() {
    let (connector, mut daemons) = scripted_daemon();
    let mut host = start_host(connector);
    let mut daemon = daemons.recv().await.expect("a daemon connection");
    daemon.handshake().await;
    host.handshake().await;

    host.send(json!({"id": "m1", "type": "get_available_models"})).await;
    let catalog = daemon.recv_type("get_model_catalog").await;
    let mut history = history_frame(catalog["id"].as_u64().unwrap());
    history["available_model_routes"] = json!([
        {"model": "test-model", "provider": "test-provider", "api_method": "openai-completions",
         "available": true, "detail": "ready"},
        {"model": "other-model", "provider": "test-provider", "api_method": "openai-completions",
         "available": false, "detail": "no key"},
    ]);
    daemon.send(history).await;

    let (_, response) = host.response(json!("m1")).await;
    let models = response["data"]["models"].as_array().expect("models array");
    // Only the available route is offered, so a host cannot select a route
    // that would fail.
    assert_eq!(models.len(), 1, "{models:?}");
    assert_eq!(models[0]["id"], "test-model");
    assert_eq!(models[0]["provider"], "test-provider");
    assert_eq!(models[0]["api"], "openai-completions");
    assert_eq!(models[0]["contextWindow"], 200000);
    host.close().await;
}

#[tokio::test]
async fn startup_and_state_requests_skip_daemon_ack_before_settlement() {
    let (connector, mut daemons) = scripted_daemon();
    let mut host = start_host(connector);
    let mut daemon = daemons.recv().await.expect("a daemon connection");

    // Simulate real daemon ordering: Ack precedes History on get_history.
    let subscribe = daemon.recv_type("subscribe").await;
    daemon.send(json!({"type": "ack", "id": subscribe["id"]})).await;
    let history = daemon.recv_type("get_history").await;
    let history_id = history["id"].as_u64().unwrap();
    daemon.send(json!({"type": "ack", "id": history_id})).await;
    daemon.send(history_frame(history_id)).await;
    host.handshake().await;

    // get_messages -> ack then history
    host.send(json!({"id": "msg1", "type": "get_messages"})).await;
    let req = daemon.recv_type("get_history").await;
    let id = req["id"].as_u64().unwrap();
    daemon.send(json!({"type": "ack", "id": id})).await;
    daemon.send(history_frame(id)).await;
    let (_, resp) = host.response(json!("msg1")).await;
    assert_eq!(resp["success"], true, "{resp}");
    assert_eq!(resp["data"]["totalMessages"], 0);

    // get_available_models -> ack then history
    host.send(json!({"id": "mod1", "type": "get_available_models"})).await;
    let req = daemon.recv_type("get_model_catalog").await;
    let id = req["id"].as_u64().unwrap();
    daemon.send(json!({"type": "ack", "id": id})).await;
    daemon.send(history_frame(id)).await;
    let (_, resp) = host.response(json!("mod1")).await;
    assert_eq!(resp["success"], true, "{resp}");

    // set_model -> ack then model_changed
    host.send(json!({"id": "sm1", "type": "set_model", "modelId": "other-model"}))
        .await;
    let req = daemon.recv_type("set_model").await;
    let id = req["id"].as_u64().unwrap();
    daemon.send(json!({"type": "ack", "id": id})).await;
    daemon
        .send(json!({"type": "model_changed", "id": id, "model": "other-model"}))
        .await;
    let (_, resp) = host.response(json!("sm1")).await;
    assert_eq!(resp["success"], true, "{resp}");
    assert_eq!(resp["data"]["id"], "other-model");

    // set_thinking_level -> ack then reasoning_effort_changed
    host.send(json!({"id": "st1", "type": "set_thinking_level", "level": "high"}))
        .await;
    let req = daemon.recv_type("set_reasoning_effort").await;
    let id = req["id"].as_u64().unwrap();
    daemon.send(json!({"type": "ack", "id": id})).await;
    daemon
        .send(json!({"type": "reasoning_effort_changed", "id": id, "effort": "high"}))
        .await;
    let (_, resp) = host.response(json!("st1")).await;
    assert_eq!(resp["success"], true, "{resp}");

    // set_session_name -> ack then done
    host.send(json!({"id": "sn1", "type": "set_session_name", "name": "renamed"}))
        .await;
    let req = daemon.recv_type("rename_session").await;
    let id = req["id"].as_u64().unwrap();
    daemon.send(json!({"type": "ack", "id": id})).await;
    daemon.send(json!({"type": "done", "id": id})).await;
    let (_, resp) = host.response(json!("sn1")).await;
    assert_eq!(resp["success"], true, "{resp}");

    // compact -> ack then compact_result
    host.send(json!({"id": "c1", "type": "compact"})).await;
    let req = daemon.recv_type("compact").await;
    let id = req["id"].as_u64().unwrap();
    daemon.send(json!({"type": "ack", "id": id})).await;
    daemon
        .send(json!({"type": "compact_result", "id": id, "success": true, "message": "ok"}))
        .await;
    let (_, resp) = host.response(json!("c1")).await;
    assert_eq!(resp["success"], true, "{resp}");
    assert_eq!(resp["data"]["summary"], "ok");

    host.close().await;
}

struct EnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl EnvRestore {
    fn capture(keys: &[&'static str]) -> Self {
        Self(keys.iter().map(|k| (*k, std::env::var_os(k))).collect())
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (k, v) in &self.0 {
            match v {
                Some(val) => crate::env::set_var(k, val),
                None => crate::env::remove_var(k),
            }
        }
        crate::config::invalidate_config_cache();
    }
}

#[tokio::test]
async fn bash_command_enforces_policy_gates() {
    let _env_lock = crate::storage::lock_test_env();
    let _restore = EnvRestore::capture(&[
        "HORON_HOME",
        "HORON_AGENT_TOOL_PROFILE",
        "HORON_AGENT_DISABLED_TOOLS",
        "HORON_AGENT_DISABLE_BASE_TOOLS",
    ]);
    let home = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    crate::env::set_var("HORON_HOME", home.path());
    crate::env::remove_var("HORON_AGENT_TOOL_PROFILE");
    crate::env::remove_var("HORON_AGENT_DISABLED_TOOLS");
    crate::env::remove_var("HORON_AGENT_DISABLE_BASE_TOOLS");

    let run_rpc_bash = |plan: bool, cmd: String| async move {
        let (connector, mut daemons) = scripted_daemon();
        let mut host = start_host_with_plan(connector, plan);
        let mut daemon = daemons.recv().await.expect("a daemon connection");
        daemon.handshake().await;
        host.handshake().await;
        host.send(json!({"id": "b1", "type": "bash", "command": cmd})).await;
        let (_, resp) = host.response(json!("b1")).await;
        host.close().await;
        resp
    };

    // 1. tool_profile=none blocks bash without touching disk
    let blocked_file = scratch.path().join("blocked_profile.txt");
    crate::env::set_var("HORON_AGENT_TOOL_PROFILE", "none");
    crate::config::invalidate_config_cache();
    let resp = run_rpc_bash(false, format!("touch '{}'", blocked_file.display())).await;
    assert_eq!(resp["success"], false, "{resp}");
    assert_eq!(resp["code"], "bash_failed");
    assert!(!blocked_file.exists());
    crate::env::remove_var("HORON_AGENT_TOOL_PROFILE");

    // 2. disabled_tools=bash blocks bash
    crate::env::set_var("HORON_AGENT_DISABLED_TOOLS", "bash");
    crate::config::invalidate_config_cache();
    let resp = run_rpc_bash(false, format!("touch '{}'", blocked_file.display())).await;
    assert_eq!(resp["success"], false, "{resp}");
    assert!(!blocked_file.exists());
    crate::env::remove_var("HORON_AGENT_DISABLED_TOOLS");

    // 3. plan_mode blocks bash
    crate::config::invalidate_config_cache();
    let resp = run_rpc_bash(true, format!("touch '{}'", blocked_file.display())).await;
    assert_eq!(resp["success"], false, "{resp}");
    assert!(!blocked_file.exists());

    // 4. approval_mode=write blocks non-interactive RPC bash
    std::fs::write(home.path().join("config.toml"), "[tools]\napproval_mode = \"write\"\n").unwrap();
    crate::config::invalidate_config_cache();
    let resp = run_rpc_bash(false, "printf blocked".to_string()).await;
    assert_eq!(resp["success"], false, "{resp}");
    assert!(
        resp["error"].as_str().unwrap_or("").contains("needs approval"),
        "{resp}"
    );

    // 5. yolo allows safe bash, blocks destructive commands
    std::fs::write(home.path().join("config.toml"), "[tools]\napproval_mode = \"yolo\"\n").unwrap();
    crate::config::invalidate_config_cache();
    let resp = run_rpc_bash(false, "rm -rf /etc".to_string()).await;
    assert_eq!(resp["success"], false, "{resp}");

    let resp = run_rpc_bash(false, "printf rpc-ok".to_string()).await;
    assert_eq!(resp["success"], true, "{resp}");
    assert_eq!(resp["data"]["output"], "rpc-ok");
}

#[tokio::test]
async fn abort_and_prompt_cancels_first_settles_aborted_then_starts_fresh_turn() {
    let (connector, mut daemons) = scripted_daemon();
    let mut host = start_host(connector);
    let mut daemon = daemons.recv().await.expect("a daemon connection");
    daemon.handshake().await;
    host.handshake().await;

    host.send(json!({"id": "p1", "type": "prompt", "message": "first"}))
        .await;
    let first = daemon.recv_type("message").await;
    assert_eq!(first["content"], "first");
    let first_id = first["id"].as_u64().unwrap();
    daemon.send(json!({"type": "ack", "id": first_id})).await;
    daemon.send(json!({"type": "text_delta", "text": "partial"})).await;

    host.send(json!({"id": "ap1", "type": "abort_and_prompt", "message": "second"}))
        .await;
    let (_, ap_resp) = host.response(json!("ap1")).await;
    assert_eq!(ap_resp["success"], true, "{ap_resp}");

    // Must send ONLY cancel (never concurrent soft_interrupt) while first turn runs.
    let cancel = daemon.recv().await;
    assert_eq!(cancel["type"], "cancel", "unexpected daemon request: {cancel}");
    daemon.send(json!({"type": "ack", "id": cancel["id"]})).await;
    daemon.send(json!({"type": "interrupted"})).await;
    daemon.send(json!({"type": "done", "id": first_id})).await;

    // First turn must settle aborted before second prompt is dispatched.
    let mut turn1_frames: Vec<Value> = Vec::new();
    while turn1_frames.last().map(|f| f["type"].as_str()) != Some(Some("session_settled")) {
        turn1_frames.push(host.recv().await);
    }
    let prompt_res_1 = turn1_frames
        .iter()
        .find(|f| f["type"] == "prompt_result")
        .expect("turn 1 prompt_result");
    assert_eq!(prompt_res_1["id"], "p1");
    assert_eq!(prompt_res_1["status"], "aborted");

    let second = daemon.recv_type("message").await;
    assert_eq!(second["content"], "second");
    let second_id = second["id"].as_u64().unwrap();
    daemon.send(json!({"type": "ack", "id": second_id})).await;
    daemon.send(json!({"type": "text_delta", "text": "replacement"})).await;
    daemon.send(json!({"type": "text_done"})).await;
    daemon.send(json!({"type": "message_end"})).await;
    daemon.send(json!({"type": "done", "id": second_id})).await;

    let mut turn2_frames: Vec<Value> = Vec::new();
    while turn2_frames.last().map(|f| f["type"].as_str()) != Some(Some("session_settled")) {
        turn2_frames.push(host.recv().await);
    }
    let prompt_res_2 = turn2_frames
        .iter()
        .find(|f| f["type"] == "prompt_result")
        .expect("turn 2 prompt_result");
    assert_eq!(prompt_res_2["id"], "ap1");
    assert_eq!(prompt_res_2["status"], "completed");

    host.close().await;
}

#[tokio::test]
async fn abort_after_abort_and_prompt_discards_replacement_and_completed_race_ignores_stale_interrupt() {
    // 1. Explicit abort clears queued abort_and_prompt replacement
    let (connector, mut daemons) = scripted_daemon();
    let mut host = start_host(connector);
    let mut daemon = daemons.recv().await.expect("a daemon connection");
    daemon.handshake().await;
    host.handshake().await;

    host.send(json!({"id": "p1", "type": "prompt", "message": "first"}))
        .await;
    let first = daemon.recv_type("message").await;
    let first_id = first["id"].as_u64().unwrap();
    daemon.send(json!({"type": "ack", "id": first_id})).await;

    host.send(json!({"id": "ap1", "type": "abort_and_prompt", "message": "second"}))
        .await;
    let (_, _) = host.response(json!("ap1")).await;
    let _ = daemon.recv_type("cancel").await;

    host.send(json!({"id": "a1", "type": "abort"})).await;
    let (_, a_resp) = host.response(json!("a1")).await;
    assert_eq!(a_resp["success"], true);
    let _ = daemon.recv_type("cancel").await;

    daemon.send(json!({"type": "interrupted"})).await;
    let mut frames: Vec<Value> = Vec::new();
    while frames.last().map(|f| f["type"].as_str()) != Some(Some("session_settled")) {
        frames.push(host.recv().await);
    }
    host.send(json!({"id": "g1", "type": "get_state"})).await;
    let (_, g_resp) = host.response(json!("g1")).await;
    assert_eq!(g_resp["data"]["isStreaming"], false);
    host.close().await;

    // 2. First turn completes just before cancel hits idle daemon -> stale interrupted ignored in turn 2
    let (connector, mut daemons) = scripted_daemon();
    let mut host = start_host(connector);
    let mut daemon = daemons.recv().await.expect("a daemon connection");
    daemon.handshake().await;
    host.handshake().await;

    host.send(json!({"id": "p1", "type": "prompt", "message": "first"}))
        .await;
    let first = daemon.recv_type("message").await;
    let first_id = first["id"].as_u64().unwrap();
    daemon.send(json!({"type": "ack", "id": first_id})).await;

    host.send(json!({"id": "ap1", "type": "abort_and_prompt", "message": "second"}))
        .await;
    let (_, _) = host.response(json!("ap1")).await;
    let cancel = daemon.recv_type("cancel").await;
    // Turn 1 finished cleanly before cancel took effect
    daemon.send(json!({"type": "done", "id": first_id})).await;

    let mut turn1_frames: Vec<Value> = Vec::new();
    while turn1_frames.last().map(|f| f["type"].as_str()) != Some(Some("session_settled")) {
        turn1_frames.push(host.recv().await);
    }
    assert_eq!(
        turn1_frames.iter().find(|f| f["type"] == "prompt_result").unwrap()["status"],
        "completed"
    );

    let second = daemon.recv_type("message").await;
    let second_id = second["id"].as_u64().unwrap();
    // Stale cancel reply + idle Interrupted emitted before second turn's Ack
    daemon.send(json!({"type": "ack", "id": cancel["id"]})).await;
    daemon.send(json!({"type": "interrupted"})).await;
    daemon.send(json!({"type": "ack", "id": second_id})).await;
    daemon.send(json!({"type": "text_delta", "text": "still-runs"})).await;
    daemon.send(json!({"type": "text_done"})).await;
    daemon.send(json!({"type": "message_end"})).await;
    daemon.send(json!({"type": "done", "id": second_id})).await;

    let mut turn2_frames: Vec<Value> = Vec::new();
    while turn2_frames.last().map(|f| f["type"].as_str()) != Some(Some("session_settled")) {
        turn2_frames.push(host.recv().await);
    }
    assert_eq!(
        turn2_frames.iter().find(|f| f["type"] == "prompt_result").unwrap()["status"],
        "completed"
    );
    host.close().await;
}
