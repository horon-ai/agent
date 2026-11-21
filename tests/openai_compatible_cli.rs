//! `horon-agent run` against a direct OpenAI-compatible `/chat/completions`
//! endpoint, driven through the real binary in an owned HORON_HOME.
//!
//! The loopback mock scripts one tool-call round and then the final text, so a
//! run that prints `pong` proves the binary reached the mock twice: once for the
//! call and once with the tool result. Every wait is bounded, and the child is
//! killed and reaped on timeout.
//!
//! The three other `[providers.<name>]` types are covered the same way: each
//! profile is registered through the real `provider add`, run once against a
//! recorder that keeps the path, headers and body it was sent, and answered in
//! that wire's own streaming shape.

use serde_json::{Value, json};
use std::path::Path;
use std::process::{Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const RUN_DEADLINE: Duration = Duration::from_secs(90);
const FINAL_TEXT: &str = "pong";
/// Prompt sent by the wire round trips; the recorded body must carry it, which
/// is what tells the recorded request apart from any other the binary might make.
const PROMPT: &str = "hello";
/// Environment variable named by `provider add --api-key-env`, and the fake key
/// the child process reads from it. Never a real credential.
const KEY_ENV: &str = "QA_WIRE_API_KEY";
const FAKE_KEY: &str = "qa-fake-key-not-a-secret";
/// Compaction work stays off so one run makes one request and its evidence is
/// not disturbed by a background turn.
const COMPACTION_HEAD: &str = "[compaction]\nbackground = false\nmax_context_tokens = 20000\nrecent_tokens = 2048\n\n";
/// Model id the wire round trips ask for. Gemini puts it in the path, so the
/// recorded path also proves which model the run selected.
const WIRE_MODEL: &str = "wire-qa-model";
/// Text each wire's mock streams, and the text the run must print.
const WIRE_TEXT: &str = "wire-ok";

struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("a temporary home"),
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn write_config(&self, base_url: &str) {
        let config = format!(
            "{COMPACTION_HEAD}[provider]\ndefault_provider = \"qa\"\ndefault_model = \"qa-model\"\n\n\
[providers.qa]\ntype = \"openai-compatible\"\nbase_url = \"{base_url}\"\nauth = \"none\"\n\
default_model = \"qa-model\"\nmodels = [{{ id = \"qa-model\" }}]\n"
        );
        std::fs::write(self.path().join("config.toml"), config).expect("write config.toml");
    }

    /// Register one `[providers.<name>]` profile through the real
    /// `provider add`, which is the entry point a user has.
    async fn add_profile(&self, profile: &str, wire: Wire, base_url: &str) {
        let output = self
            .run(&[
                "provider",
                "add",
                profile,
                "--type",
                wire.type_arg(),
                "--base-url",
                base_url,
                "--model",
                WIRE_MODEL,
                "--api-key-env",
                KEY_ENV,
            ])
            .await;
        assert!(
            output.status.success(),
            "provider add {profile} failed: status={:?}\nstdout={}\nstderr={}",
            output.status,
            text(&output.stdout),
            text(&output.stderr)
        );
    }

    async fn run(&self, args: &[&str]) -> Output {
        self.run_with_env(args, &[]).await
    }

    async fn run_with_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_horon-agent"));
        command
            .current_dir(self.path())
            .env_clear()
            .env("HOME", self.path())
            .env("HORON_HOME", self.path())
            .env("XDG_CONFIG_HOME", self.path().join("config"))
            .env("XDG_CACHE_HOME", self.path().join("cache"))
            .env("HORON_AGENT_NON_INTERACTIVE", "1")
            .env("HORON_AGENT_NO_AUTO_UPDATE", "1")
            .env("HORON_AGENT_MEMORY_ENABLED", "0")
            .env("HORON_AGENT_MEMORY_SIDECAR_ENABLED", "0")
            .env("HORON_AGENT_RUN_MCP", "0")
            .env("NO_COLOR", "1")
            .env("PATH", std::env::var_os("PATH").unwrap_or_default());
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("the CLI spawns");
        let stdout = child.stdout.take().expect("a stdout pipe");
        let stderr = child.stderr.take().expect("a stderr pipe");
        let deadline = tokio::time::Instant::now() + RUN_DEADLINE;
        let outcome = tokio::time::timeout_at(deadline, async {
            tokio::try_join!(child.wait(), drain(stdout), drain(stderr))
        })
        .await;
        match outcome {
            Ok(Ok((status, stdout, stderr))) => Output { status, stdout, stderr },
            Ok(Err(error)) => panic!("the CLI IO failed: {error}"),
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                panic!("the CLI did not finish within {RUN_DEADLINE:?}");
            }
        }
    }
}

async fn drain(mut reader: impl tokio::io::AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).await?;
    Ok(buf)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A loopback `/v1/chat/completions` mock: the first request gets a `read`
/// tool call, and the request carrying the tool result gets the final text.
struct Mock {
    base_url: String,
    requests: Arc<AtomicUsize>,
    bodies: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Mock {
    async fn start() -> Self {
        Self::start_with_tool("ls", r#"{"path":"."}"#).await
    }

    async fn start_with_tool(tool: &'static str, arguments: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind the mock");
        let port = listener.local_addr().expect("the mock address").port();
        let requests = Arc::new(AtomicUsize::new(0));
        let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
        let requests_thread = Arc::clone(&requests);
        let bodies_thread = Arc::clone(&bodies);
        tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let requests = Arc::clone(&requests_thread);
                let bodies = Arc::clone(&bodies_thread);
                tokio::spawn(async move {
                    let _ = serve(socket, &requests, &bodies, tool, arguments).await;
                });
            }
        });
        Self {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            requests,
            bodies,
        }
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    fn bodies(&self) -> Vec<String> {
        self.bodies
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

async fn serve(
    mut socket: tokio::net::TcpStream,
    requests: &AtomicUsize,
    bodies: &std::sync::Mutex<Vec<String>>,
    tool: &str,
    arguments: &str,
) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let read = socket.read(&mut chunk).await?;
        assert!(read > 0, "the peer closed before sending a request");
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = find_subsequence(&buffer, b"\r\n\r\n") {
            break position + 4;
        }
    };
    let headers = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut length = 0usize;
    for header in headers.lines() {
        if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    while buffer.len() < header_end + length {
        let read = socket.read(&mut chunk).await?;
        assert!(read > 0, "the peer closed mid-body");
        buffer.extend_from_slice(&chunk[..read]);
    }
    let body = String::from_utf8_lossy(&buffer[header_end..header_end + length]).into_owned();
    bodies
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(body.clone());
    let round = requests.fetch_add(1, Ordering::SeqCst) + 1;

    let (delta, finish) = if round == 1 {
        (
            json!({"role": "assistant", "tool_calls": [{
                "index": 0,
                "id": "call_qa_1",
                "type": "function",
                "function": {"name": tool, "arguments": arguments}
            }]}),
            Value::Null,
        )
    } else {
        (json!({"role": "assistant", "content": FINAL_TEXT}), Value::Null)
    };
    let frame_of = |delta: Value, finish: Value| {
        json!({
            "id": "chatcmpl-qa",
            "object": "chat.completion.chunk",
            "model": "qa-model",
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
        })
    };
    let frames = if round == 1 {
        vec![frame_of(delta, Value::Null), frame_of(json!({}), json!("tool_calls"))]
    } else {
        vec![frame_of(delta, finish), frame_of(json!({}), json!("stop"))]
    };
    let mut sse = String::new();
    for frame in frames {
        sse.push_str(&format!("data: {frame}\n\n"));
    }
    sse.push_str("data: [DONE]\n\n");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        sse.len(),
        sse
    );
    socket.write_all(response.as_bytes()).await?;
    socket.flush().await
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn tool_round_seen(bodies: &[String]) -> bool {
    bodies.iter().any(|body| {
        serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|value| value["messages"].as_array().cloned())
            .is_some_and(|messages| messages.iter().any(|message| message["role"] == "tool"))
    })
}

/// One request a recorder kept: the header block exactly as sent, and the body.
#[derive(Clone)]
struct Recorded {
    head: String,
    body: String,
}

impl Recorded {
    /// The request-line target, e.g. `/v1/responses`.
    fn path(&self) -> &str {
        self.head.split_whitespace().nth(1).unwrap_or_default()
    }

    /// The value of `name`, with the name matched case-insensitively and the
    /// value returned as sent.
    fn header(&self, name: &str) -> Option<&str> {
        self.head.lines().find_map(|line| {
            let (found, value) = line.split_once(':')?;
            found.trim().eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|error| panic!("the recorded body must be JSON ({error}): {}", self.body))
    }
}

/// Reads one HTTP request off `socket`, headers and body.
async fn read_request(socket: &mut tokio::net::TcpStream) -> std::io::Result<Recorded> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let read = socket.read(&mut chunk).await?;
        assert!(read > 0, "the peer closed before sending a request");
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = find_subsequence(&buffer, b"\r\n\r\n") {
            break position + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut length = 0usize;
    for header in head.lines() {
        if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    while buffer.len() < header_end + length {
        let read = socket.read(&mut chunk).await?;
        assert!(read > 0, "the peer closed mid-body");
        buffer.extend_from_slice(&chunk[..read]);
    }
    Ok(Recorded {
        head,
        body: String::from_utf8_lossy(&buffer[header_end..header_end + length]).into_owned(),
    })
}

/// The `[providers.<name>].type` values that speak a wire other than Chat
/// Completions, with the endpoint each one posts to.
#[derive(Clone, Copy, Debug)]
enum Wire {
    Responses,
    Anthropic,
    Gemini,
}

impl Wire {
    fn type_arg(self) -> &'static str {
        match self {
            Self::Responses => "openai-responses",
            Self::Anthropic => "anthropic-compatible",
            Self::Gemini => "gemini-compatible",
        }
    }

    /// The target the run must reach. Gemini names the model in its path, so
    /// this also proves the run asked for the profile's model.
    fn path(self) -> String {
        match self {
            Self::Responses => "/v1/responses".to_string(),
            Self::Anthropic => "/v1/messages".to_string(),
            Self::Gemini => format!("/v1/models/{WIRE_MODEL}:streamGenerateContent?alt=sse"),
        }
    }

    /// One minimal answer that closes the turn, in this wire's own shape.
    fn sse(self) -> String {
        let frames: Vec<String> = match self {
            Self::Responses => vec![
                json!({ "type": "response.output_text.delta", "delta": WIRE_TEXT }).to_string(),
                json!({
                    "type": "response.completed",
                    "response": { "status": "completed", "usage": { "input_tokens": 3, "output_tokens": 2 } }
                })
                .to_string(),
            ],
            Self::Anthropic => vec![
                json!({
                    "type": "message_start",
                    "message": { "id": "msg_qa", "usage": { "input_tokens": 3, "output_tokens": 0 } }
                })
                .to_string(),
                json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } })
                    .to_string(),
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": { "type": "text_delta", "text": WIRE_TEXT }
                })
                .to_string(),
                json!({ "type": "content_block_stop", "index": 0 }).to_string(),
                json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 2 } })
                    .to_string(),
                json!({ "type": "message_stop" }).to_string(),
            ],
            Self::Gemini => {
                // Two chunks prove the parts accumulate; the last one closes the turn.
                let (head, tail) = WIRE_TEXT.split_at(WIRE_TEXT.len() / 2);
                vec![
                    json!({ "candidates": [{ "content": { "parts": [{ "text": head }] } }] }).to_string(),
                    json!({
                        "candidates": [{ "content": { "parts": [{ "text": tail }] }, "finishReason": "STOP" }]
                    })
                    .to_string(),
                ]
            }
        };
        frames.iter().map(|frame| format!("data: {frame}\n\n")).collect()
    }
}

/// A loopback recorder: it keeps every request it was sent and answers each one
/// with `wire`'s text reply.
struct Recorder {
    base_url: String,
    seen: Arc<std::sync::Mutex<Vec<Recorded>>>,
}

impl Recorder {
    async fn start(wire: Wire) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind the recorder");
        let port = listener.local_addr().expect("the recorder address").port();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_thread = Arc::clone(&seen);
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let seen = Arc::clone(&seen_thread);
                tokio::spawn(async move {
                    let Ok(request) = read_request(&mut socket).await else {
                        return;
                    };
                    seen.lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push(request);
                    let sse = wire.sse();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        sse.len(),
                        sse
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.flush().await;
                });
            }
        });
        Self {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            seen,
        }
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn requests(&self) -> Vec<Recorded> {
        self.seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn run_sets_validated_model_options_before_requesting_the_model() {
    let recorder = Recorder::start(Wire::Responses).await;
    let home = Home::new();
    home.add_profile("qa-wire", Wire::Responses, recorder.base_url()).await;
    for (flags, success) in [
        (&[][..], true),
        (&["--thinking", "high", "--service-tier", "priority"][..], true),
        (&["--thinking", "unsupported"][..], false),
        (&["--service-tier", "auto"][..], false),
    ] {
        let mut args = vec!["--provider-profile", "qa-wire", "run", PROMPT, "--no-update"];
        args.extend_from_slice(flags);
        let output = home.run_with_env(&args, &[(KEY_ENV, FAKE_KEY)]).await;
        assert_eq!(output.status.success(), success, "{}", text(&output.stderr));
    }
    let requests = recorder.requests();
    assert_eq!(requests.len(), 2, "unsupported values made a model request");
    let default: Value = serde_json::from_str(&requests[0].body).unwrap();
    let changed: Value = serde_json::from_str(&requests[1].body).unwrap();
    assert!(default.get("service_tier").is_none());
    assert!(default.get("reasoning").is_none());
    assert_eq!(changed["service_tier"], "priority");
    assert_eq!(changed["reasoning"]["effort"], "high");
}

/// Registers `wire` through `provider add`, runs one prompt through the real
/// binary, and checks the recorded request and the printed answer together: a
/// passing run that never reached the mock would prove nothing.
async fn assert_wire_round_trip(wire: Wire) {
    let recorder = Recorder::start(wire).await;
    let home = Home::new();
    home.add_profile("qa-wire", wire, recorder.base_url()).await;

    let output = home
        .run_with_env(
            &["--provider-profile", "qa-wire", "run", PROMPT, "--no-update"],
            &[(KEY_ENV, FAKE_KEY)],
        )
        .await;
    assert!(
        output.status.success(),
        "{wire:?} run failed: status={:?}\nstdout={}\nstderr={}",
        output.status,
        text(&output.stdout),
        text(&output.stderr)
    );
    let stdout = text(&output.stdout);
    assert!(stdout.contains(WIRE_TEXT), "{wire:?} prints the mock text: {stdout}");

    let requests = recorder.requests();
    assert!(!requests.is_empty(), "{wire:?}: the mock was never reached");
    for request in &requests {
        assert_eq!(
            request.path(),
            wire.path(),
            "{wire:?} reached its own path: {}",
            request.head
        );
    }
    let request = &requests[0];
    assert!(
        request.body.contains(PROMPT),
        "{wire:?} body carries the prompt: {}",
        request.body
    );

    match wire {
        Wire::Responses => {
            let expected = format!("Bearer {FAKE_KEY}");
            assert_eq!(
                request.header("authorization"),
                Some(expected.as_str()),
                "{wire:?} sends the key as a bearer token: {}",
                request.head
            );
            let body = request.json();
            assert_eq!(body["model"], json!(WIRE_MODEL), "{body}");
            assert!(body["input"].is_array(), "{body}");
        }
        Wire::Anthropic => {
            assert_eq!(
                request.header("x-api-key"),
                Some(FAKE_KEY),
                "{wire:?} sends the key in x-api-key: {}",
                request.head
            );
            assert!(
                request.header("anthropic-version").is_some(),
                "{wire:?} sends the required version header: {}",
                request.head
            );
            assert!(request.header("authorization").is_none(), "{}", request.head);
            let body = request.json();
            assert_eq!(body["model"], json!(WIRE_MODEL), "{body}");
            assert!(body["messages"].is_array(), "{body}");
            assert!(body["max_tokens"].as_u64().is_some(), "{body}");
        }
        Wire::Gemini => {
            assert_eq!(
                request.header("x-goog-api-key"),
                Some(FAKE_KEY),
                "{wire:?} sends the key in x-goog-api-key: {}",
                request.head
            );
            assert!(request.header("authorization").is_none(), "{}", request.head);
            let body = request.json();
            assert!(body["contents"].is_array(), "{body}");
            assert!(
                body.get("model").is_none(),
                "the model lives in the URL, not the body: {body}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn openai_responses_profile_runs_end_to_end() {
    assert_wire_round_trip(Wire::Responses).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn anthropic_compatible_profile_runs_end_to_end() {
    assert_wire_round_trip(Wire::Anthropic).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn gemini_compatible_profile_runs_end_to_end() {
    assert_wire_round_trip(Wire::Gemini).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn run_prints_mock_text_and_sees_the_tool_round() {
    let mock = Mock::start().await;
    let home = Home::new();
    home.write_config(&mock.base_url);

    let output = home.run(&["run", "hi", "--no-update"]).await;
    assert!(
        output.status.success(),
        "run failed: status={:?}\nstdout={}\nstderr={}",
        output.status,
        text(&output.stdout),
        text(&output.stderr)
    );
    assert!(
        text(&output.stdout).contains(FINAL_TEXT),
        "the mock text is printed: {}",
        text(&output.stdout)
    );
    let bodies = mock.bodies();
    assert!(mock.requests() >= 2, "the mock saw two requests: {}", mock.requests());
    assert!(
        tool_round_seen(&bodies),
        "the second request carries the tool result: {bodies:?}"
    );
    let first = serde_json::from_str::<Value>(&bodies[0]).expect("the first request is JSON");
    let prompt_seen = first["messages"].as_array().expect("messages").iter().any(|message| {
        message["role"] == "user" && message["content"].as_str().is_some_and(|text| text.contains("hi"))
    });
    assert!(prompt_seen, "the first request carries the prompt: {}", bodies[0]);
}

#[tokio::test(flavor = "multi_thread")]
async fn max_time_disconnects_stalled_provider_and_reports_the_deadline() {
    let home = Home::new();
    for output_mode in [None, Some("--json"), Some("--ndjson")] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        home.write_config(&format!("http://{}/v1", listener.local_addr().unwrap()));
        let provider = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await.unwrap();
            assert!(request.body.contains("check the deadline"));
            assert_eq!(socket.read(&mut [0]).await.unwrap(), 0, "model connection stayed open");
        });
        let mut args = vec!["run", "check the deadline", "--no-update", "--max-time", "2"];
        args.extend(output_mode);
        let result = home.run(&args).await;
        assert_eq!(result.status.code(), Some(124), "{}", text(&result.stderr));
        tokio::time::timeout(Duration::from_secs(2), provider)
            .await
            .expect("the model sees the cancelled connection")
            .unwrap();

        let stdout = text(&result.stdout);
        if let Some(mode) = output_mode {
            let rows: Vec<Value> = stdout.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
            if mode == "--ndjson" {
                assert_eq!(rows[0]["type"], "start");
                assert!(!rows.iter().any(|row| row["type"] == "done"));
            } else {
                assert_eq!(rows.len(), 1, "{stdout}");
            }
            let error = rows.last().unwrap();
            assert_eq!(error["type"], "error");
            assert_eq!(error["code"], "deadline_exceeded");
            assert_eq!(error["max_time_seconds"], 2.0);
        } else {
            assert!(text(&result.stderr).contains("run exceeded its --max-time limit"));
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn stats_groups_durable_usage_by_date_and_route_without_double_counting() {
    let home = Home::new();
    let seed = |id: &str, events: &[Value]| {
        let session = home.path().join("sessions").join(id);
        std::fs::create_dir_all(&session).unwrap();
        let text = events.iter().map(|event| format!("{event}\n")).collect::<String>();
        std::fs::write(session.join("events.jsonl"), text).unwrap();
    };
    let created = |provider: &str, model: &str| json!({"kind":"session_created","payload":{"metadata":{"provider_key":provider,"model":model}}});
    let appended = |id: &str, time: &str, input: u64| json!({"kind":"message_appended","event_id":id,"timestamp":time,"payload":{"message":{"token_usage":{"input_tokens":input,"output_tokens":2}}}});
    let original = appended("a", "2026-11-20T23:50:00Z", 5);
    let revised = json!({"kind":"message_revised","event_id":"r","timestamp":"2026-11-21T00:01:00Z","payload":{"target_event_id":"a","replacement":{"token_usage":{"input_tokens":12,"output_tokens":3}}}});
    seed(
        "s1",
        &[
            created("alpha", "m1"),
            original.clone(),
            revised.clone(),
            original,
            revised,
            appended("b", "2026-11-21T00:10:00Z", 99),
        ],
    );
    seed(
        "s2",
        &[created("beta", "m2"), appended("c", "2026-11-20T18:00:00-05:00", 7)],
    );
    let result = home
        .run(&["stats", "--from", "2026-11-20", "--through", "2026-11-20", "--json"])
        .await;
    assert!(result.status.success(), "{}", text(&result.stderr));
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["estimated_cost"], Value::Null);
    let rows = report["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0],
        json!({"date":"2026-11-20","provider":"alpha","model":"m1","input":12,"output":3,"cache_read_input":null,"cache_creation_input":null,"requests":1})
    );
    assert_eq!(rows[1]["provider"], "beta");
    assert_eq!(rows[1]["model"], "m2");
    assert_eq!(rows[1]["input"], 7);
    assert_eq!(rows[1]["requests"], 1);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn no_pty_overrides_shell_allocation_for_one_run() {
    use std::io::Write as _;

    let home = Home::new();
    for (flags, expected) in [(&["--no-pty"][..], "pipe-shell"), (&[][..], "tty-shell")] {
        let mock = Mock::start_with_tool(
            "bash",
            r#"{"command":"if test -t 1; then state=tty-shell; else state=pipe-shell; fi; printf '%s' \"$state\" > pty-receipt"}"#,
        )
        .await;
        home.write_config(&mock.base_url);
        std::fs::OpenOptions::new()
            .append(true)
            .open(home.path().join("config.toml"))
            .unwrap()
            .write_all(b"\n[tools]\nbash_pty = \"always\"\n")
            .unwrap();
        let mut args = vec!["run", "check whether the shell has a terminal", "--no-update"];
        args.extend_from_slice(flags);
        let result = home.run(&args).await;
        assert!(result.status.success(), "{}", text(&result.stderr));
        let receipt = home.path().join("pty-receipt");
        assert_eq!(std::fs::read_to_string(&receipt).unwrap(), expected);
        std::fs::remove_file(receipt).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unconfigured_provider_explains_setup() {
    let home = Home::new();
    let output = home.run(&["run", "hi", "--no-update"]).await;
    assert!(
        !output.status.success(),
        "a run without a provider must fail: {}",
        text(&output.stdout)
    );
    let stderr = text(&output.stderr);
    assert!(stderr.contains("openai-compatible"), "{stderr}");
    assert!(stderr.contains("[providers."), "{stderr}");
    assert!(!stderr.contains("router"), "{stderr}");
}
