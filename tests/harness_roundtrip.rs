//! Deterministic, credential-free smoke test for the headless agent surface.
//!
//! Two paths are proven with a stub provider (no network, no [OI] credentials):
//!
//!  (a) in-process `horon-agent run` shape: an `Agent` built directly with the stub
//!      provider settles one message with the stub's canned assistant text.
//!  (b) socket shape: a real `serve` daemon plus the real `api-bridge` over temp
//!      Unix sockets. An NDJSON client does `hello` -> `create_session` ->
//!      `send_message` and observes `hello_ok`, `attached`, `text_delta`, and a
//!      `turn_done` carrying the stub's canned text. Adversarial frames (a
//!      malformed line, and a `send_message` naming an unattached session) must
//!      return an `error` frame within a bounded deadline instead of hanging.
//!
//! The stub provider is injected by constructing the `Agent` / `Server` with it
//! directly, so nothing here touches the external provider registry and no real
//! provider runtime is exercised. Every wait subscribes before sending and is
//! wrapped in a finite timeout; there are no sleep-based polls of turn state.
#![cfg(unix)]
// The env guard is held across awaits on purpose, matching the e2e harness.
#![allow(clippy::await_holding_lock)]

use anyhow::{Context, Result};
use async_stream::stream;
use horon_agent::agent::Agent;
use horon_agent::message::{Message, StreamEvent, ToolDefinition};
use horon_agent::provider::{EventStream, Provider};
use horon_agent::server;
use horon_agent::session::Session;
use horon_agent::session::event_store_api as store;
use horon_agent::tool::Registry;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::timeout;

/// The canned assistant text every stub completion returns. The tests assert
/// this exact string, so a turn that "succeeds" with empty or wrong output
/// fails rather than passing on the absence of an error.
const STUB_TEXT: &str = "stub provider deterministic reply";

/// A stub provider that completes deterministically with no network and no
/// credentials. Modeled on the deleted `src/bin/harness.rs` `NoopProvider`
/// (todo 8) and the e2e `MockProvider`, but it always returns the same
/// scripted stream so a turn is reproducible without queueing.
#[derive(Clone)]
struct StubProvider {
    text: String,
}

impl StubProvider {
    fn new() -> Self {
        Self {
            text: STUB_TEXT.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl Provider for StubProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let text = self.text.clone();
        let stream = stream! {
            yield Ok(StreamEvent::TextDelta(text));
            yield Ok(StreamEvent::TextDone);
            yield Ok(StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".to_string()),
            });
        };
        Ok(Box::pin(stream))
    }

    fn name(&self) -> &str {
        "stub"
    }

    fn model(&self) -> String {
        "stub-model".to_string()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

/// Serializes the two tests: they mutate process-global env (HORON_AGENT_HOME and
/// friends) and global config caches, so they must not run concurrently.
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Points horon-agent at an isolated temp home and disables the memory sidecar (whose
/// background provider call would make the stub turn nondeterministic), then
/// restores the previous environment on drop.
struct TestEnv {
    _lock: MutexGuard<'static, ()>,
    previous: Vec<(&'static str, Option<OsString>)>,
    _home: tempfile::TempDir,
}

impl TestEnv {
    fn new() -> Result<Self> {
        let lock = env_lock();
        let home = tempfile::Builder::new()
            .prefix("horon-agent-roundtrip-home-")
            .tempdir()?;
        let runtime_dir = home.path().join("runtime");
        std::fs::create_dir_all(&runtime_dir)?;

        let mut previous = Vec::new();
        let mut remember = |key: &'static str| {
            previous.push((key, std::env::var_os(key)));
        };
        for key in [
            "HORON_AGENT_HOME",
            "HORON_AGENT_RUNTIME_DIR",
            "HORON_AGENT_TEST_SESSION",
            "HORON_AGENT_DEBUG_CONTROL",
            "HORON_AGENT_RUNTIME_PROVIDER",
            "HORON_AGENT_ACTIVE_PROVIDER",
            "HORON_AGENT_OPENROUTER_CACHE_NAMESPACE",
            "HORON_AGENT_MEMORY_ENABLED",
            "HORON_AGENT_MEMORY_SIDECAR_ENABLED",
        ] {
            remember(key);
        }

        horon_agent::env::set_var("HORON_AGENT_HOME", home.path());
        horon_agent::env::set_var("HORON_AGENT_RUNTIME_DIR", &runtime_dir);
        horon_agent::env::set_var("HORON_AGENT_TEST_SESSION", "1");
        horon_agent::env::set_var("HORON_AGENT_DEBUG_CONTROL", "1");
        horon_agent::env::remove_var("HORON_AGENT_RUNTIME_PROVIDER");
        horon_agent::env::remove_var("HORON_AGENT_ACTIVE_PROVIDER");
        horon_agent::env::remove_var("HORON_AGENT_OPENROUTER_CACHE_NAMESPACE");
        horon_agent::env::set_var("HORON_AGENT_MEMORY_ENABLED", "0");
        horon_agent::env::set_var("HORON_AGENT_MEMORY_SIDECAR_ENABLED", "0");

        Ok(Self {
            _lock: lock,
            previous,
            _home: home,
        })
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..) {
            match value {
                Some(value) => horon_agent::env::set_var(key, value),
                None => horon_agent::env::remove_var(key),
            }
        }
    }
}

/// (a) In-process `horon-agent run` shape: an `Agent` built with the stub provider
/// completes one message with the stub's canned text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_process_agent_roundtrip_settles_with_stub_text() -> Result<()> {
    let _env = TestEnv::new()?;
    let provider: Arc<dyn Provider> = Arc::new(StubProvider::new());
    let registry = Registry::new(Arc::clone(&provider)).await;
    let mut agent = Agent::new_with_session(Arc::clone(&provider), registry, Session::create(None, None), None);
    agent.set_memory_enabled(false);

    let output = agent
        .run_once_capture("ping")
        .await
        .context("in-process agent turn failed")?;

    assert_eq!(
        output.trim(),
        STUB_TEXT,
        "in-process turn must settle with the stub's canned assistant text"
    );
    Ok(())
}

async fn send(writer: &mut tokio::net::unix::OwnedWriteHalf, frame: Value) -> Result<()> {
    writer.write_all(format!("{frame}\n").as_bytes()).await?;
    Ok(())
}

/// Read exactly one NDJSON frame, bounded so a missing reply fails the test
/// instead of hanging.
async fn recv(reader: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>) -> Result<Value> {
    let line = timeout(Duration::from_secs(15), reader.next_line())
        .await
        .context("frame deadline exceeded (no reply)")??
        .context("harness disconnected before a frame arrived")?;
    let frame: Value = serde_json::from_str(&line)?;
    Ok(frame)
}

/// Read frames until one satisfies `predicate`, bounded by an overall deadline.
/// Records every observed frame through `record` for the evidence log.
async fn recv_until<F>(
    reader: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    record: &mut Vec<Value>,
    predicate: F,
) -> Result<Value>
where
    F: Fn(&Value) -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let frame = recv(reader).await?;
        let matched = predicate(&frame);
        record.push(frame.clone());
        if matched {
            return Ok(frame);
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("expected frame never arrived; observed: {record:?}");
        }
    }
}

fn log_frames(label: &str, frames: &[Value]) {
    for frame in frames {
        eprintln!("OBSERVED_FRAME[{label}] {frame}");
    }
}

async fn wait_for_socket(path: &Path) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if UnixStream::connect(path).await.is_ok() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("daemon socket {} never became connectable", path.display());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// (b) Socket shape: real `serve` daemon + real `api-bridge`, driven over temp
/// Unix sockets with NDJSON. Proves the end-to-end translation, not a duplex.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serve_api_bridge_ndjson_roundtrip_reaches_turn_done() -> Result<()> {
    let _env = TestEnv::new()?;
    let runtime = tempfile::Builder::new()
        .prefix("horon-agent-roundtrip-sock-")
        .tempdir()?;
    let daemon_socket = runtime.path().join("daemon.sock");
    let debug_socket = runtime.path().join("debug.sock");
    let api_socket = runtime.path().join("api.sock");

    let provider: Arc<dyn Provider> = Arc::new(StubProvider::new());
    let server = server::Server::new_with_paths(provider, daemon_socket.clone(), debug_socket.clone());
    let daemon = tokio::spawn(async move { server.run().await });
    let bridge = tokio::spawn(horon_agent_harness_api_server::run_bridge(
        api_socket.clone(),
        daemon_socket.clone(),
    ));

    let result = async {
        wait_for_socket(&daemon_socket).await?;

        let stream = timeout(Duration::from_secs(15), async {
            loop {
                match UnixStream::connect(&api_socket).await {
                    Ok(stream) => break stream,
                    Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
                }
            }
        })
        .await
        .context("api socket connect deadline")?;

        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half).lines();

        send(
            &mut write_half,
            json!({"v": 1, "id": 1, "req": "hello", "min_version": 1, "max_version": 1, "client": "harness-roundtrip"}),
        )
        .await?;
        let hello = recv(&mut reader).await?;
        log_frames("hello", std::slice::from_ref(&hello));
        assert_eq!(hello["ev"], "hello_ok", "hello reply was {hello}");

        send(
            &mut write_half,
            json!({"v": 1, "id": 2, "req": "create_session", "working_dir": runtime.path()}),
        )
        .await?;
        let mut create_frames = Vec::new();
        let attached = recv_until(&mut reader, &mut create_frames, |frame| frame["ev"] == "attached").await?;
        log_frames("create_session", &create_frames);
        let session_id = attached["session"]["session_id"]
            .as_str()
            .context("attached frame had no session_id")?
            .to_string();

        send(
            &mut write_half,
            json!({"v": 1, "id": 3, "req": "send_message", "session_id": session_id, "content": "ping"}),
        )
        .await?;
        let mut turn_frames = Vec::new();
        let turn_done = recv_until(&mut reader, &mut turn_frames, |frame| frame["ev"] == "turn_done").await?;
        log_frames("turn", &turn_frames);
        assert_eq!(turn_done["ev"], "turn_done");

        let streamed_text: String = turn_frames
            .iter()
            .filter(|frame| frame["ev"] == "text_delta")
            .filter_map(|frame| frame["text"].as_str())
            .collect();
        assert!(
            turn_frames.iter().any(|frame| frame["ev"] == "text_delta"),
            "turn must stream at least one text_delta; observed {turn_frames:?}"
        );
        assert_eq!(
            streamed_text, STUB_TEXT,
            "streamed assistant text must equal the stub's canned reply"
        );

        // Adversarial 1: a malformed frame is rejected with an error, and the
        // connection stays usable (a following ping still answers) rather than
        // hanging or wedging.
        write_half.write_all(b"{ this is not valid json }\n").await?;
        let mut malformed_frames = Vec::new();
        let malformed = recv_until(&mut reader, &mut malformed_frames, |frame| frame["ev"] == "error").await?;
        log_frames("malformed", &malformed_frames);
        assert_eq!(malformed["ev"], "error");
        assert_eq!(malformed["code"], "invalid_request");

        send(&mut write_half, json!({"v": 1, "id": 4, "req": "ping"})).await?;
        let mut ping_frames = Vec::new();
        let pong = recv_until(&mut reader, &mut ping_frames, |frame| frame["ev"] == "pong")
            .await
            .context("connection wedged after a malformed frame")?;
        log_frames("ping_after_malformed", &ping_frames);
        assert_eq!(pong["reply_to"], 4);

        // Adversarial 2: send_message naming an unattached session is refused
        // with an error within the deadline, not applied silently.
        send(
            &mut write_half,
            json!({"v": 1, "id": 5, "req": "send_message", "session_id": "does-not-exist", "content": "ping"}),
        )
        .await?;
        let mut bad_session_frames = Vec::new();
        let refused = recv_until(&mut reader, &mut bad_session_frames, |frame| frame["ev"] == "error").await?;
        log_frames("unattached_send", &bad_session_frames);
        assert_eq!(refused["ev"], "error");

        // The turn is durable in the session event log read straight from the
        // store, and the session store holds no SQLite database: history is the
        // append-only originals plus the index, not a second copy.
        let home = std::env::var_os("HORON_AGENT_HOME").context("HORON_AGENT_HOME")?;
        let store_dir = Path::new(&home).join("sessions");
        let log = store_dir.join(&session_id).join("events.jsonl");
        assert!(
            log.is_file(),
            "the session history lives in the event log: {}",
            log.display()
        );
        let sqlite: Vec<String> = std::fs::read_dir(&store_dir)
            .context("read the session store")?
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains("sqlite"))
            .collect();
        assert!(
            sqlite.is_empty(),
            "the session store holds no SQLite database: {sqlite:?}"
        );
        let paths = store::SessionPaths::new(store_dir.clone());
        let session = store::SessionId::parse(&session_id).context("session id")?;
        let graph = store::EventGraph::open(&paths, &session).context("open the event graph")?;
        let projection = graph.replay(None).context("replay the originals")?;
        let rows: Vec<String> = projection
            .messages()
            .iter()
            .map(|message| serde_json::to_string(&message.message).expect("message json"))
            .collect();
        assert!(
            rows.iter().any(|row| row.contains(STUB_TEXT)),
            "the streamed reply is an original record: {rows:?}"
        );
        assert!(
            projection.messages().len() >= 2,
            "the user turn and the assistant reply are both original records: {}",
            projection.messages().len()
        );

        Ok(())
    }
    .await;

    bridge.abort();
    daemon.abort();
    let _ = bridge.await;
    let _ = daemon.await;
    result
}
