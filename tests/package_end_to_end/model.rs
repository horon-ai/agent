//! A loopback `/v1/chat/completions` mock that scripts one package call per turn.
//!
//! The agent under test is the real binary and the package under test is the real
//! example, so the model is the only thing faked here. A request is answered from
//! its own body rather than from a counter: a request that carries the turn's
//! marker and no tool result asks for the scripted tool call, one that also
//! carries a tool result gets the closing text, and any other request (a title, a
//! summary) gets the closing text too. A counter would misroute those background
//! requests and make a failure depend on how many of them a run happens to make.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

/// Text the closing round carries, so a test can tell the whole turn finished.
pub const FINAL_TEXT: &str = "PKG-E2E turn complete";

/// How long one connection may stay silent before the mock gives up on it.
const READ_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone)]
struct Script {
    marker: String,
    tool: String,
    arguments: String,
}

struct State {
    script: Mutex<Option<Script>>,
    bodies: Mutex<Vec<String>>,
    calls: AtomicU64,
    stop: AtomicBool,
}

/// The fake model endpoint and the requests it answered.
pub struct Model {
    base_url: String,
    address: SocketAddr,
    state: Arc<State>,
}

impl Model {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the mock model");
        let address = listener.local_addr().expect("the mock model address");
        let state = Arc::new(State {
            script: Mutex::new(None),
            bodies: Mutex::new(Vec::new()),
            calls: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        });
        let accepting = Arc::clone(&state);
        thread::spawn(move || {
            for incoming in listener.incoming() {
                if accepting.stop.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(socket) = incoming else {
                    return;
                };
                let serving = Arc::clone(&accepting);
                thread::spawn(move || {
                    let _ = serve(socket, &serving);
                });
            }
        });
        Self {
            base_url: format!("http://127.0.0.1:{}/v1", address.port()),
            address,
            state,
        }
    }

    /// The `base_url` a `[providers.*]` table points at.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Answer the next request that carries `marker` with `tool`.
    pub fn script(&self, marker: &str, tool: &str, arguments: &str) {
        let mut script = lock(&self.state.script);
        *script = Some(Script {
            marker: marker.to_owned(),
            tool: tool.to_owned(),
            arguments: arguments.to_owned(),
        });
    }

    /// Every request body the mock has received, in arrival order.
    pub fn bodies(&self) -> Vec<String> {
        lock(&self.state.bodies).clone()
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::SeqCst);
        // Unblock the accept loop; the flag makes it return without serving.
        let _ = TcpStream::connect(self.address);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Read one HTTP request, record it, and answer in the streaming shape the
/// provider parses.
fn serve(mut socket: TcpStream, state: &State) -> std::io::Result<()> {
    socket.set_read_timeout(Some(READ_TIMEOUT))?;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        let read = socket.read(&mut chunk)?;
        if read == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = find(&buffer, b"\r\n\r\n") {
            break position + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut length = 0usize;
    for line in head.lines() {
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    while buffer.len() < header_end + length {
        let read = socket.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let end = (header_end + length).min(buffer.len());
    let body = String::from_utf8_lossy(&buffer[header_end..end]).into_owned();
    lock(&state.bodies).push(body.clone());

    let script = lock(&state.script).clone();
    let call = script.filter(|script| body.contains(&script.marker) && !carries_tool_result(&body));
    let (delta, finish) = match call {
        Some(script) => {
            let id = format!("call_pkg_{}", state.calls.fetch_add(1, Ordering::SeqCst) + 1);
            (
                json!({"role": "assistant", "tool_calls": [{
                    "index": 0,
                    "id": id,
                    "type": "function",
                    "function": {"name": script.tool, "arguments": script.arguments}
                }]}),
                json!("tool_calls"),
            )
        }
        None => (json!({"role": "assistant", "content": FINAL_TEXT}), json!("stop")),
    };

    let frame = |delta: Value, finish: Value| {
        json!({
            "id": "chatcmpl-pkg-e2e",
            "object": "chat.completion.chunk",
            "model": "qa-model",
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
        })
    };
    let mut stream = String::new();
    stream.push_str(&format!("data: {}\n\n", frame(delta, Value::Null)));
    stream.push_str(&format!("data: {}\n\n", frame(json!({}), finish)));
    stream.push_str("data: [DONE]\n\n");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        stream.len(),
        stream
    );
    socket.write_all(response.as_bytes())?;
    socket.flush()
}

/// Whether a request already carries a tool result, i.e. the call has run.
fn carries_tool_result(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    value["messages"]
        .as_array()
        .is_some_and(|messages| messages.iter().any(|message| message["role"] == "tool"))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}
