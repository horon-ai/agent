//! `horon-agent run` against a direct OpenAI-compatible `/chat/completions`
//! endpoint, driven through the real binary in an owned HORON_AGENT_HOME.
//!
//! The loopback mock scripts one tool-call round and then the final text, so a
//! run that prints `pong` proves the binary reached the mock twice: once for the
//! call and once with the tool result. Every wait is bounded, and the child is
//! killed and reaped on timeout.

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
            "[compaction]\nbackground = false\nmax_context_tokens = 20000\nrecent_tokens = 2048\n\n\
[provider]\ndefault_provider = \"qa\"\ndefault_model = \"qa-model\"\n\n\
[providers.qa]\ntype = \"openai-compatible\"\nbase_url = \"{base_url}\"\nauth = \"none\"\n\
default_model = \"qa-model\"\nmodels = [{{ id = \"qa-model\" }}]\n"
        );
        std::fs::write(self.path().join("config.toml"), config).expect("write config.toml");
    }

    async fn run(&self, args: &[&str]) -> Output {
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_horon-agent"))
            .current_dir(self.path())
            .env_clear()
            .env("HOME", self.path())
            .env("HORON_AGENT_HOME", self.path())
            .env("XDG_CONFIG_HOME", self.path().join("config"))
            .env("XDG_CACHE_HOME", self.path().join("cache"))
            .env("HORON_AGENT_NON_INTERACTIVE", "1")
            .env("HORON_AGENT_NO_AUTO_UPDATE", "1")
            .env("HORON_AGENT_MEMORY_ENABLED", "0")
            .env("HORON_AGENT_MEMORY_SIDECAR_ENABLED", "0")
            .env("HORON_AGENT_RUN_MCP", "0")
            .env("NO_COLOR", "1")
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
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
                    let _ = serve(socket, &requests, &bodies).await;
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
                "function": {"name": "ls", "arguments": "{\"path\":\".\"}"}
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
