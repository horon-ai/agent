//! `horon-agent run --warn-cost` and `--max-cost` through the real binary, against
//! a loopback OpenAI-compatible mock that reports one million prompt tokens per
//! response. A priced model therefore costs its input price per request, and the
//! mock scripts one tool-call round and then the final text.
use serde_json::{Value, json};
use std::path::Path;
use std::process::{Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const RUN_DEADLINE: Duration = Duration::from_secs(90);

/// Serves `/v1/chat/completions` and counts the requests it received.
async fn start_mock() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind the mock");
    let base_url = format!("http://127.0.0.1:{}/v1", listener.local_addr().unwrap().port());
    let requests = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&requests);
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let counter = Arc::clone(&counter);
            tokio::spawn(async move {
                let _ = answer(socket, &counter).await;
            });
        }
    });
    (base_url, requests)
}

async fn answer(mut socket: tokio::net::TcpStream, requests: &AtomicUsize) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let (header_end, length) = loop {
        let read = socket.read(&mut chunk).await?;
        assert!(read > 0, "the peer closed before sending a request");
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buffer[..end]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(0usize);
            break (end + 4, length);
        }
    };
    while buffer.len() < header_end + length {
        let read = socket.read(&mut chunk).await?;
        assert!(read > 0, "the peer closed mid-body");
        buffer.extend_from_slice(&chunk[..read]);
    }
    let round = requests.fetch_add(1, Ordering::SeqCst) + 1;
    let frame = |delta: Value, finish: Value| {
        json!({"id": "c", "object": "chat.completion.chunk", "model": "qa-model",
               "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    };
    let frames = if round == 1 {
        let call = json!({"role": "assistant", "tool_calls": [{"index": 0, "id": "call_1", "type": "function",
                          "function": {"name": "ls", "arguments": r#"{"path":"."}"#}}]});
        vec![frame(call, Value::Null), frame(json!({}), json!("tool_calls"))]
    } else {
        vec![
            frame(json!({"role": "assistant", "content": "pong"}), Value::Null),
            frame(json!({}), json!("stop")),
        ]
    };
    let usage = json!({"id": "c", "object": "chat.completion.chunk", "model": "qa-model", "choices": [],
                       "usage": {"prompt_tokens": 1_000_000, "completion_tokens": 0, "total_tokens": 1_000_000}});
    let mut sse = String::new();
    for frame in frames.iter().chain(std::iter::once(&usage)) {
        sse.push_str(&format!("data: {frame}\n\n"));
    }
    sse.push_str("data: [DONE]\n\n");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}",
        sse.len()
    );
    socket.write_all(response.as_bytes()).await?;
    socket.flush().await
}

/// An isolated home with the mock registered as `profile`.
///
/// A loopback endpoint under a profile name the pricing catalog does not map is
/// treated as free, so a price applies only under a mapped name such as `deepseek`.
/// With `priced`, a fresh catalog lists `qa-model` at $0.60 per million input tokens.
fn home(profile: &str, base_url: &str, priced: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temporary home");
    let config = format!(
        "[compaction]\nbackground = false\nmax_context_tokens = 20000\nrecent_tokens = 2048\n\n\
[provider]\ndefault_provider = \"{profile}\"\ndefault_model = \"qa-model\"\n\n\
[providers.{profile}]\ntype = \"openai-compatible\"\nbase_url = \"{base_url}\"\nauth = \"none\"\n\
default_model = \"qa-model\"\nmodels = [{{ id = \"qa-model\" }}]\n"
    );
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    if priced {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let catalog = json!({"cached_at_unix_secs": now, "providers": {"deepseek": {"qa-model":
            {"input_usd_per_mtok": 0.6, "output_usd_per_mtok": 1.0}}}});
        std::fs::create_dir_all(dir.path().join("cache")).unwrap();
        std::fs::write(dir.path().join("cache/models_dev_pricing.json"), catalog.to_string()).unwrap();
    }
    dir
}

async fn run(home: &Path, args: &[&str]) -> Output {
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_horon-agent"))
        .current_dir(home)
        .env_clear()
        .env("HOME", home)
        .env("HORON_HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("HORON_AGENT_NON_INTERACTIVE", "1")
        .env("HORON_AGENT_NO_AUTO_UPDATE", "1")
        .env("HORON_AGENT_MEMORY_ENABLED", "0")
        .env("HORON_AGENT_MEMORY_SIDECAR_ENABLED", "0")
        .env("HORON_AGENT_RUN_MCP", "0")
        .env("NO_COLOR", "1")
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .args(["run", "hi", "--no-update"])
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("the CLI spawns");
    tokio::time::timeout(RUN_DEADLINE, child.wait_with_output())
        .await
        .expect("the CLI finishes in time")
        .expect("the CLI output")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn warn_cost_warns_once_and_the_run_finishes() {
    let (base_url, requests) = start_mock().await;
    let home = home("deepseek", &base_url, true);
    let output = run(home.path(), &["--warn-cost", "0.5", "--max-cost", "5"]).await;
    let stderr = stderr(&output);
    assert!(output.status.success(), "stderr={stderr}");
    assert_eq!(stderr.matches("spend warning").count(), 1, "{stderr}");
    assert_eq!(requests.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn max_cost_admits_no_request_after_the_cap_is_reached() {
    let (base_url, requests) = start_mock().await;
    let home = home("deepseek", &base_url, true);
    let output = run(home.path(), &["--max-cost", "0.5"]).await;
    let stderr = stderr(&output);
    assert!(!output.status.success());
    assert!(stderr.contains("spend cap of $0.5000 reached"), "{stderr}");
    assert_eq!(requests.load(Ordering::SeqCst), 1, "the second request is not sent");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unpriced_model_is_reported_not_counted_as_free() {
    let (base_url, _) = start_mock().await;
    let home = home("qa", &base_url, false);
    let output = run(home.path(), &["--max-cost", "0.01"]).await;
    let stderr = stderr(&output);
    assert!(output.status.success(), "stderr={stderr}");
    assert!(stderr.contains("no price is known for qa-model"), "{stderr}");
    assert!(stderr.contains("2 request(s) had no known price"), "{stderr}");
}
