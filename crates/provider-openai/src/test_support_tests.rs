//! Loopback HTTP fake and stream helpers shared by this crate's tests.
//!
//! Every wire's tests drive a real socket instead of a mocked client, so the
//! request path, the headers the credential rides on and the chunked SSE body
//! are proven end to end. The server lives here rather than in one test module
//! because Chat Completions, Responses, Anthropic Messages and Gemini
//! `generateContent` differ in the bytes, not in the transport.

use futures::StreamExt;
use horon_agent_message_types::StreamEvent;
use horon_agent_provider_core::EventStream;
use serde_json::Value;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The credential every fake endpoint is configured with, so a leak into a
/// message is visible as this literal.
pub(crate) const TEST_KEY: &str = "sk-test-secret";

/// Bound on one awaited stream event, so a stalled stream fails the test
/// instead of hanging the suite.
pub(crate) const DEADLINE: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Loopback HTTP mock

#[derive(Clone, Debug, Default)]
pub(crate) struct RecordedRequest {
    pub(crate) path: String,
    headers: Vec<(String, String)>,
    pub(crate) body: String,
}

impl RecordedRequest {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, value)| value.as_str())
    }
}

pub(crate) struct Reply {
    status: u16,
    content_type: &'static str,
    /// Written as one HTTP chunk each when `chunked`, otherwise concatenated.
    parts: Vec<String>,
    chunked: bool,
}

impl Reply {
    pub(crate) fn sse(parts: &[&str]) -> Self {
        Self {
            status: 200,
            content_type: "text/event-stream",
            parts: parts.iter().map(|part| (*part).to_string()).collect(),
            chunked: true,
        }
    }

    pub(crate) fn status(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: "application/json",
            parts: vec![body.to_string()],
            chunked: false,
        }
    }
}

pub(crate) struct MockServer {
    pub(crate) url: String,
    seen: Arc<Mutex<Vec<RecordedRequest>>>,
    stop: Arc<AtomicBool>,
    accept: Option<std::thread::JoinHandle<()>>,
}

impl MockServer {
    pub(crate) fn start(reply: Reply) -> Self {
        Self::start_sequence(vec![reply])
    }

    /// Serve one reply per connection in order, repeating the last one.
    ///
    /// The repetition keeps a broken retry loop from hanging the test on a
    /// connection the script never answered, and lets a two-reply script stand
    /// for "rejection, then success".
    pub(crate) fn start_sequence(replies: Vec<Reply>) -> Self {
        assert!(!replies.is_empty(), "a mock server needs at least one reply");
        let replies: VecDeque<Arc<Reply>> = replies.into_iter().map(Arc::new).collect();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        listener.set_nonblocking(true).expect("non-blocking listener");
        let address = listener.local_addr().expect("loopback address");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let seen_thread = Arc::clone(&seen);
        let stop_thread = Arc::clone(&stop);
        let replies = Arc::new(Mutex::new(replies));
        let accept = std::thread::spawn(move || {
            while !stop_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let seen = Arc::clone(&seen_thread);
                        let reply = {
                            let mut replies = replies.lock().expect("reply script");
                            if replies.len() > 1 {
                                replies.pop_front().expect("more than one reply queued")
                            } else {
                                Arc::clone(replies.front().expect("the last reply repeats"))
                            }
                        };
                        std::thread::spawn(move || {
                            let _ = serve(stream, &seen, &reply);
                        });
                    }
                    // Nothing is waiting: poll again after a short pause.
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    // A failed `accept` (a client that aborted between its SYN and
                    // the accept, an interrupted call) concerns that one connection,
                    // not the listener. Exiting here would leave the mock refusing
                    // every later request, so keep serving until dropped.
                    Err(_) if stop_thread.load(Ordering::Relaxed) => break,
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        Self {
            url: format!("http://{address}"),
            seen,
            stop,
            accept: Some(accept),
        }
    }

    pub(crate) fn requests(&self) -> Vec<RecordedRequest> {
        self.seen.lock().expect("request log").clone()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

fn serve(mut stream: TcpStream, seen: &Arc<Mutex<Vec<RecordedRequest>>>, reply: &Reply) -> std::io::Result<()> {
    // `accept` hands back a socket that inherits the listener's `O_NONBLOCK`
    // (POSIX leaves the flags of the new socket unspecified, and Darwin copies
    // them). A non-blocking socket ignores `SO_RCVTIMEO`, so the first request
    // read would return `WouldBlock` whenever the client's bytes had not landed
    // yet, and the mock would drop the connection before answering - the client
    // then reported "error sending request". Serving is synchronous here, so go
    // back to blocking mode and let the read timeout bound it.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let request = read_request(&mut stream)?;
    seen.lock().expect("request log").push(request);

    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nConnection: close\r\n",
        reply.status,
        reason(reply.status),
        reply.content_type
    );
    if reply.chunked {
        head.push_str("Transfer-Encoding: chunked\r\n\r\n");
        stream.write_all(head.as_bytes())?;
        for part in &reply.parts {
            if part.is_empty() {
                continue;
            }
            stream.write_all(format!("{:X}\r\n", part.len()).as_bytes())?;
            stream.write_all(part.as_bytes())?;
            stream.write_all(b"\r\n")?;
            stream.flush()?;
        }
        stream.write_all(b"0\r\n\r\n")?;
        stream.flush()?;
    } else {
        let body = reply.parts.concat();
        head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
        stream.write_all(head.as_bytes())?;
        stream.write_all(body.as_bytes())?;
        stream.flush()?;
    }
    Ok(())
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<RecordedRequest> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 2048];
    let header_end = loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Ok(RecordedRequest::default());
        }
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(position) = find_header_end(&bytes) {
            break position;
        }
    };

    let head = String::from_utf8_lossy(&bytes[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let path = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_string();
    let mut headers = Vec::new();
    let mut content_length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_string();
        if name == "content-length" {
            content_length = value.parse().unwrap_or(0);
        }
        headers.push((name, value));
    }

    while bytes.len() < header_end + content_length {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    let body_end = (header_end + content_length).min(bytes.len());
    let body = String::from_utf8_lossy(&bytes[header_end..body_end]).to_string();
    Ok(RecordedRequest { path, headers, body })
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Response",
    }
}

// ---------------------------------------------------------------------------
// Stream helpers

/// One SSE frame carrying `value`, as a provider would write it.
pub(crate) fn sse_frame(value: Value) -> String {
    format!("data: {value}\n\n")
}

/// Drain `stream` under [`DEADLINE`], so a stalled body fails instead of hanging.
pub(crate) async fn collect(stream: &mut EventStream) -> Vec<StreamEvent> {
    let mut events = Vec::new();
    while let Some(event) = tokio::time::timeout(DEADLINE, stream.next())
        .await
        .expect("stream event within the deadline")
    {
        events.push(event.expect("stream event is Ok"));
    }
    events
}
