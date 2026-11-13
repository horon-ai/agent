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

    let (frames, response) = host.response(json!("p1")).await;
    assert_eq!(response["success"], true, "{response}");
    assert_eq!(response["command"], "prompt");
    assert!(response.get("data").is_none(), "prompt acks without data: {response}");

    let mut kinds: Vec<String> = frames
        .iter()
        .map(|frame| frame["type"].as_str().unwrap_or_default().to_string())
        .collect();
    // The response itself is already consumed, so append the trailing frames.
    while kinds.last().map(String::as_str) != Some("session_settled") {
        let frame = host.recv().await;
        kinds.push(frame["type"].as_str().unwrap_or_default().to_string());
    }

    assert_eq!(kinds.first().map(String::as_str), Some("agent_start"), "{kinds:?}");
    assert_eq!(
        kinds.iter().filter(|kind| *kind == "prompt_result").count(),
        1,
        "{kinds:?}"
    );
    assert_eq!(
        kinds.iter().filter(|kind| *kind == "session_settled").count(),
        1,
        "{kinds:?}"
    );
    assert!(kinds.iter().any(|kind| kind == "text_delta"), "{kinds:?}");
    assert_eq!(kinds.last().map(String::as_str), Some("session_settled"));
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
