//! Server-initiated requests over the remote (Streamable HTTP) transport.
//!
//! The stdio path is covered end to end in `client::server_requests_tests`.
//! This file proves the other transport answers through the same reply table,
//! and that a reply is POSTed as its own frame instead of travelling the
//! request path (a reply has an `id` and no `method`).

use super::*;
use crate::mcp::client::McpClient;
use crate::mcp::protocol::{METHOD_NOT_FOUND_CODE, McpServerConfig};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// One HTTP request the fixture saw, reduced to what the assertions need.
#[derive(Clone, Debug)]
struct Seen {
    /// The JSON-RPC body: a single frame or a batch.
    body: Value,
}

impl Seen {
    fn frames(&self) -> Vec<Value> {
        match &self.body {
            Value::Array(items) => items.clone(),
            other => vec![other.clone()],
        }
    }
}

struct Fixture {
    url: String,
    seen: Arc<std::sync::Mutex<Vec<Seen>>>,
    task: JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fixture {
    /// The `initialize` POST is answered with a batch: the initialize result and
    /// the three server-initiated requests, so every inbound direction is
    /// exercised without a second stream to synchronize against.
    async fn start(expose_server_requests: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}/mcp", listener.local_addr().expect("addr"));
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let own_seen = Arc::clone(&seen);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break };
                        let seen = Arc::clone(&own_seen);
                        connections.spawn(async move {
                            let _ = serve(socket, seen, expose_server_requests).await;
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {},
                }
            }
        });
        Self { url, seen, task }
    }

    fn config(&self) -> McpServerConfig {
        serde_json::from_value(json!({"type": "http", "url": self.url, "timeout_secs": 5})).expect("fixture config")
    }

    /// Every frame the fixture received whose reply is a JSON-RPC response to a
    /// server-initiated request: an `id` and no `method`.
    fn client_replies(&self) -> Vec<Value> {
        self.seen
            .lock()
            .expect("seen")
            .iter()
            .flat_map(Seen::frames)
            .filter(|frame| frame.get("method").is_none() && frame.get("id").is_some())
            .collect()
    }
}

async fn serve(socket: TcpStream, seen: Arc<std::sync::Mutex<Vec<Seen>>>, probe: bool) -> Result<()> {
    let (request, mut socket) = read_request(socket).await?;
    let recorded = Seen { body: request.clone() };
    seen.lock().expect("seen").push(recorded);
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let id = request.get("id").cloned().unwrap_or(Value::Null);

    // A reply to a server-initiated request: acknowledge it and record nothing
    // else, exactly as a real server would.
    if method.is_empty() {
        return respond(&mut socket, 202, "text/plain", "").await;
    }
    if method == "tools/list" {
        let body = json!({"jsonrpc": "2.0", "id": id, "result": {"tools": []}}).to_string();
        return respond(&mut socket, 200, "application/json", &body).await;
    }
    if method != "initialize" {
        return respond(&mut socket, 202, "text/plain", "").await;
    }

    let init = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "http-probe", "version": "1"}},
    });
    let mut batch = vec![init];
    if probe {
        batch.push(json!({"jsonrpc": "2.0", "id": 10, "method": "ping"}));
        batch.push(json!({"jsonrpc": "2.0", "id": 11, "method": "sampling/createMessage", "params": {}}));
        batch.push(json!({"jsonrpc": "2.0", "id": 12, "method": "roots/list"}));
    }
    respond(&mut socket, 200, "application/json", &Value::Array(batch).to_string()).await
}

async fn read_request(socket: TcpStream) -> Result<(Value, TcpStream)> {
    let mut reader = BufReader::new(socket);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let mut headers = std::collections::HashMap::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 || line == "\r\n" {
            break;
        }
        let (key, value) = line.split_once(':').context("header")?;
        headers.insert(key.to_ascii_lowercase(), value.trim().to_string());
    }
    let size = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes).await?;
    let body = if size == 0 {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)?
    };
    Ok((body, reader.into_inner()))
}

async fn respond(socket: &mut TcpStream, status: u16, content_type: &str, body: &str) -> Result<()> {
    let headers = format!(
        "HTTP/1.1 {status} Fixture\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nMcp-Session-Id: probe-session\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(headers.as_bytes()).await?;
    socket.write_all(body.as_bytes()).await?;
    Ok(())
}

/// An isolated agent home, so `[mcp] expose_roots` can be flipped per test.
struct Home {
    _guard: std::sync::MutexGuard<'static, ()>,
    dir: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
}

impl Home {
    fn new(expose_roots: bool) -> Self {
        let guard = crate::storage::lock_test_env();
        let dir = tempfile::tempdir().expect("home");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", dir.path());
        if expose_roots {
            std::fs::write(dir.path().join("config.toml"), "[mcp]\nexpose_roots = true\n").expect("config");
        }
        crate::config::Config::invalidate_cache();
        Self {
            _guard: guard,
            dir,
            previous,
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

async fn wait_for_replies(fixture: &Fixture, count: usize) -> Vec<Value> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let replies = fixture.client_replies();
        if replies.len() >= count {
            return replies;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the client answered {} of {count} server requests over HTTP",
            replies.len()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn http_server_requests_are_answered_on_a_dedicated_post() {
    let home = Home::new(false);
    let fixture = Fixture::start(true).await;
    let client = McpClient::connect_in_dir("http-probe".to_string(), &fixture.config(), Some(home.dir.path()))
        .await
        .expect("the fixture server connects");

    let replies = wait_for_replies(&fixture, 3).await;
    // Sorted, because three independent POSTs race and the order carries no
    // meaning. Each reply is matched by its own id.
    let mut by_id: Vec<(u64, Value)> = replies
        .into_iter()
        .map(|reply| (reply["id"].as_u64().expect("reply id"), reply))
        .collect();
    by_id.sort_by_key(|(id, _)| *id);
    by_id.truncate(3);
    assert_eq!(by_id[0].1, json!({"jsonrpc": "2.0", "id": 10, "result": {}}));
    assert_eq!(
        by_id[1].1,
        json!({
            "jsonrpc": "2.0",
            "id": 11,
            "error": {"code": METHOD_NOT_FOUND_CODE, "message": "Method not found: sampling/createMessage"},
        })
    );
    assert_eq!(
        by_id[2].1,
        json!({
            "jsonrpc": "2.0",
            "id": 12,
            "error": {"code": METHOD_NOT_FOUND_CODE, "message": "Method not found: roots/list"},
        }),
        "without the opt-in the remote connection refuses roots/list like any other method"
    );

    // The reply POST left the connection healthy and did not disturb the
    // request registry: a normal call still works afterwards.
    assert!(!client.handle().is_closed());
    client.handle().refresh_tools().await.expect("tools/list after replies");
}

#[tokio::test]
async fn http_roots_are_served_when_the_opt_in_is_on() {
    let home = Home::new(true);
    let fixture = Fixture::start(true).await;
    let root = home.dir.path().join("session-root");
    std::fs::create_dir_all(&root).expect("root dir");
    let _client = McpClient::connect_in_dir("http-probe".to_string(), &fixture.config(), Some(&root))
        .await
        .expect("the fixture server connects");

    let replies = wait_for_replies(&fixture, 3).await;
    let roots_reply = replies
        .into_iter()
        .find(|reply| reply["id"] == json!(12))
        .expect("roots/list must be answered");
    let uri = roots_reply["result"]["roots"][0]["uri"]
        .as_str()
        .unwrap_or_else(|| panic!("roots/list must carry one URI: {roots_reply}"));
    assert!(
        uri.starts_with("file://") && uri.ends_with("session-root/"),
        "the root is the session working directory as a file:// URI: {uri}"
    );
}
