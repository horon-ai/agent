//! The writer task: bounded queue, control capacity, write deadline.
//!
//! These tests measure the property the module exists for, not that a function
//! was called: a peer that stops reading must not stop the connection from
//! translating its own cancel and approval, other connections must keep
//! working, frames must never interleave, and no writer may outlive the
//! connection it served.

use super::*;
use crate::writer::{CONTROL_QUEUE_FRAMES, CloseOutcome, OutboundQueue, write_loop};
use serde_json::json;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "horon-writer-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            % 1_000_000_000
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A write half that records its own drop, so a writer task left running after
/// the connection ends is observable rather than merely unlikely.
struct DropSignalWriter {
    inner: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    dropped: Arc<AtomicBool>,
}

impl AsyncWrite for DropSignalWriter {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

impl Drop for DropSignalWriter {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

/// A fake daemon that answers control requests and, on `state`, pushes deltas
/// until the bridge stops reading them. It also records when the bridge closes
/// each of its connections, which is how a test observes the write deadline
/// without reading from the stalled client.
struct FakeDaemon {
    dir: std::path::PathBuf,
    socket: std::path::PathBuf,
    seen: Arc<tokio::sync::Mutex<Vec<(String, Instant)>>>,
    closed: Arc<tokio::sync::Mutex<Vec<(bool, Instant)>>>,
}

impl FakeDaemon {
    async fn start(tag: &str, flood: bool) -> Self {
        let dir = temp_dir(tag);
        let socket = dir.join("legacy.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let seen: Arc<tokio::sync::Mutex<Vec<(String, Instant)>>> = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let closed: Arc<tokio::sync::Mutex<Vec<(bool, Instant)>>> = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let recorder = seen.clone();
        let closer = closed.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let recorder = recorder.clone();
                let closer = closer.clone();
                tokio::spawn(async move {
                    let (read, write) = stream.into_split();
                    // One writer per daemon connection, fed by the read loop:
                    // the flood and the replies cannot interleave.
                    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
                    tokio::spawn(async move {
                        let mut write = write;
                        while let Some(value) = rx.recv().await {
                            if value["type"] == "flood" {
                                if !flood {
                                    continue;
                                }
                                let delta = json!({"type":"reasoning_delta","text":"y".repeat(4096)}).to_string();
                                loop {
                                    if write.write_all(delta.as_bytes()).await.is_err()
                                        || write.write_all(b"\n").await.is_err()
                                    {
                                        return;
                                    }
                                }
                            }
                            if write_json_line(&mut write, &value).await.is_err() {
                                return;
                            }
                        }
                    });
                    let mut lines = BufReader::new(read).lines();
                    let mut saw_state = false;
                    while let Ok(Some(line)) = lines.next_line().await {
                        let Ok(request) = serde_json::from_str::<Value>(&line) else {
                            continue;
                        };
                        let kind = request["type"].as_str().unwrap_or_default().to_string();
                        let id = request["id"].clone();
                        match kind.as_str() {
                            "ping" => {
                                let _ = tx.send(json!({"type":"pong","id":id}));
                            }
                            "state" => {
                                saw_state = true;
                                let _ = tx.send(json!({"type":"state","id":id,"session_id":"stall"}));
                                let _ = tx.send(json!({"type":"flood"}));
                            }
                            "cancel" | "permission_decision" => {
                                recorder.lock().await.push((kind.clone(), Instant::now()));
                                let _ = tx.send(json!({"type":"ok","id":id}));
                            }
                            _ => {}
                        }
                    }
                    closer.lock().await.push((saw_state, Instant::now()));
                });
            }
        });
        Self {
            dir,
            socket,
            seen,
            closed,
        }
    }

    fn api_socket(&self) -> std::path::PathBuf {
        self.dir.join("api.sock")
    }

    /// Wait until the bridge has bound its socket, so a test connects to a
    /// listening bridge rather than racing its startup.
    async fn wait_until_listening(&self) {
        let path = self.api_socket();
        let start = Instant::now();
        while !path.exists() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "the bridge never bound {}",
                path.display()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Wait until the bridge closes the daemon connection that carried an
    /// attach, which is the stalled client's connection. The capability probe
    /// and other clients also close, so the close has to be attributable.
    async fn wait_for_stalled_close(&self, timeout: Duration) -> Option<Duration> {
        let start = Instant::now();
        loop {
            let closed = {
                let closes = self.closed.lock().await;
                closes.iter().find(|(attached, _)| *attached).map(|(_, at)| *at)
            };
            if let Some(at) = closed {
                return Some(at.saturating_duration_since(start));
            }
            if start.elapsed() > timeout {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Wait until the daemon has recorded `kind`, reporting how long it took.
    async fn wait_for(&self, kind: &str, timeout: Duration) -> Option<Duration> {
        let start = Instant::now();
        loop {
            let recorded = {
                let seen = self.seen.lock().await;
                seen.iter().find(|(seen_kind, _)| seen_kind == kind).map(|(_, at)| *at)
            };
            if let Some(at) = recorded {
                return Some(at.saturating_duration_since(start));
            }
            if start.elapsed() > timeout {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

impl Drop for FakeDaemon {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Open one API connection and complete the handshake.
///
/// The reader is returned as its own owner so a caller can drop it and stop
/// reading while keeping the connection open, which is what this file tests.
async fn connect_api(
    path: &std::path::Path,
) -> (
    tokio::net::unix::OwnedWriteHalf,
    BufReader<tokio::net::unix::OwnedReadHalf>,
) {
    let stream = UnixStream::connect(path).await.unwrap();
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    write_json_line(
        &mut write,
        &json!({"v":1,"id":1,"req":"hello","min_version":1,"max_version":1,"client":"writer-test"}),
    )
    .await
    .unwrap();
    let mut line = String::new();
    read_frame(&mut reader, &mut line).await.unwrap();
    let hello: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(hello["ev"], "hello_ok");
    (write, reader)
}

/// The headline guarantee: with the peer refusing to read, the connection still
/// translates a cancel and an approval, a second connection is unaffected, and
/// the stalled connection closes at the deadline with a named outcome.
#[tokio::test(flavor = "multi_thread")]
async fn a_peer_that_stops_reading_does_not_stop_its_own_control_requests() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let daemon = FakeDaemon::start("stall", true).await;
        let api_path = daemon.api_socket();
        let legacy = daemon.socket.clone();
        let deadline = Duration::from_millis(600);
        let bridge = tokio::spawn(async move {
            let _ = run_bridge_with_deadline(api_path, legacy, deadline).await;
        });
        daemon.wait_until_listening().await;

        // Baseline: a healthy connection's round trip, measured before the peer
        // stalls so the stalled-path numbers have something to be compared to.
        let (mut b_write, mut b_reader) = connect_api(&daemon.api_socket()).await;
        let baseline_start = Instant::now();
        write_json_line(&mut b_write, &json!({"v":1,"id":2,"req":"ping"}))
            .await
            .unwrap();
        let mut line = String::new();
        read_frame(&mut b_reader, &mut line).await.unwrap();
        let baseline = baseline_start.elapsed();
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["ev"], "pong");
        assert!(
            baseline < Duration::from_secs(2),
            "baseline round trip took {baseline:?}"
        );

        // The stalled connection attaches, so the daemon starts streaming a
        // large output into it, and then stops reading for good.
        let (mut a_write, mut a_reader) = connect_api(&daemon.api_socket()).await;
        write_json_line(
            &mut a_write,
            &json!({"v":1,"id":2,"req":"attach_session","session_id":"stall"}),
        )
        .await
        .unwrap();
        let mut line = String::new();
        read_frame(&mut a_reader, &mut line).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["ev"], "attached");
        // The peer now stops reading. Nothing polls `a_reader` until the bridge
        // has closed the connection, so the socket buffer really does fill.
        tokio::time::sleep(Duration::from_millis(400)).await;

        write_json_line(&mut a_write, &json!({"v":1,"id":3,"req":"cancel","session_id":"stall"}))
            .await
            .unwrap();
        let cancel_latency = daemon
            .wait_for("cancel", Duration::from_secs(5))
            .await
            .expect("the daemon must receive the cancel even though the client is not reading");
        write_json_line(
            &mut a_write,
            &json!({"v":1,"id":4,"req":"permission_response","request_id":"r1","decision":"allow"}),
        )
        .await
        .unwrap();
        let approval_latency = daemon
            .wait_for("permission_decision", Duration::from_secs(5))
            .await
            .expect("the daemon must receive the approval even though the client is not reading");
        eprintln!(
            "measured: baseline round trip {baseline:?}, cancel {cancel_latency:?}, approval {approval_latency:?}"
        );

        // Other connections keep working throughout the stall.
        write_json_line(&mut b_write, &json!({"v":1,"id":3,"req":"ping"}))
            .await
            .unwrap();
        line.clear();
        let during = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut b_reader, &mut line))
            .await
            .expect("a healthy connection must not wait on the stalled one")
            .unwrap();
        assert!(during > 0);
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["ev"], "pong");

        // The stalled connection closes at the deadline rather than waiting
        // forever. The daemon sees its side drop, which is the close; the
        // client's own read is untouched until then, so the peer stayed stalled.
        let closed = daemon
            .wait_for_stalled_close(Duration::from_secs(10))
            .await
            .expect("the stalled connection must close at the write deadline");
        eprintln!("measured: the stalled connection closed {closed:?} after the deadline was armed ({deadline:?})");
        assert!(
            closed < Duration::from_secs(5),
            "the close took {closed:?}, well past the {deadline:?} deadline"
        );

        // Only now does the client read what reached it, and everything it
        // delivered is whole frames: a frame cut by the deadline is never
        // terminated, so an NDJSON reader discards it.
        let mut delivered = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut buffer = [0u8; 64 * 1024];
            loop {
                match a_reader.read(&mut buffer).await {
                    Ok(0) => return,
                    Ok(n) => delivered.extend_from_slice(&buffer[..n]),
                    Err(_) => return,
                }
            }
        })
        .await
        .expect("the client's read half must reach end of stream once the bridge closes");

        let mut terminated = 0usize;
        let mut unterminated = 0usize;
        let mut lines: Vec<&[u8]> = delivered.split(|byte| *byte == b'\n').collect();
        if !delivered.ends_with(b"\n") {
            // A fragment cut by the deadline is never terminated, so an NDJSON
            // reader discards it instead of misreading half a frame as one.
            if lines.last().is_some_and(|line| !line.is_empty()) {
                unterminated = 1;
            }
            lines.pop();
        }
        for chunk in lines.into_iter().filter(|chunk| !chunk.is_empty()) {
            serde_json::from_slice::<Value>(chunk)
                .unwrap_or_else(|error| panic!("a delivered frame was not whole JSON: {error}"));
            terminated += 1;
        }
        eprintln!("measured: {terminated} whole frames delivered, {unterminated} unterminated fragment(s) discarded");
        assert!(
            terminated > 0,
            "the flood must have reached the client before the deadline"
        );

        bridge.abort();
    })
    .await
    .expect("stalled-peer test exceeded its own budget");
}

/// Two frames must never share a line, whatever order the queues drain in, and
/// each queue's own order must survive.
#[tokio::test]
async fn queued_frames_never_interleave_and_keep_their_order() {
    let (write, mut read) = tokio::io::duplex(1024 * 1024);
    let (queue, _closed) = OutboundQueue::new();
    let task = tokio::spawn(write_loop(write, queue.clone(), Duration::from_secs(5)));

    let frames = 100;
    for seq in 0..frames {
        queue
            .push_control(format!("{{\"kind\":\"control\",\"seq\":{seq}}}\n").into_bytes())
            .unwrap();
        queue.push_data(format!("{{\"kind\":\"data\",\"seq\":{seq}}}\n").into_bytes());
    }
    queue.close(CloseOutcome::Complete);
    assert_eq!(task.await.unwrap(), CloseOutcome::Complete);

    let mut bytes = Vec::new();
    read.read_to_end(&mut bytes).await.unwrap();
    assert!(bytes.ends_with(b"\n"), "the last frame must be terminated");
    let mut control = Vec::new();
    let mut data = Vec::new();
    for line in bytes.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()) {
        let frame: Value =
            serde_json::from_slice(line).unwrap_or_else(|error| panic!("one line held more than one frame: {error}"));
        let seq = frame["seq"].as_u64().unwrap() as usize;
        match frame["kind"].as_str().unwrap() {
            "control" => control.push(seq),
            "data" => data.push(seq),
            other => panic!("unexpected frame kind {other}"),
        }
    }
    assert_eq!(
        control,
        (0..frames).collect::<Vec<_>>(),
        "control order must be preserved"
    );
    assert_eq!(data, (0..frames).collect::<Vec<_>>(), "data order must be preserved");
}

/// A client that outruns its own replies closes the connection instead of
/// letting the control queue grow without limit.
#[tokio::test]
async fn a_control_queue_that_overflows_closes_the_connection() {
    let (queue, mut closed) = OutboundQueue::new();
    for _ in 0..CONTROL_QUEUE_FRAMES {
        queue.push_control(b"{}\n".to_vec()).unwrap();
    }
    assert_eq!(
        queue.push_control(b"{}\n".to_vec()),
        Err(CloseOutcome::ControlQueueFull)
    );
    queue.close(CloseOutcome::ControlQueueFull);
    closed.changed().await.unwrap();
    assert_eq!(*closed.borrow(), Some(CloseOutcome::ControlQueueFull));
}

/// The writer is joined, not orphaned: when the client disconnects, the write
/// half is dropped before the connection future resolves.
#[tokio::test(flavor = "multi_thread")]
async fn the_writer_is_joined_when_the_client_disconnects() {
    let daemon = FakeDaemon::start("orphan", false).await;
    let dropped = Arc::new(AtomicBool::new(false));
    let (client, bridge) = tokio::io::duplex(8192);
    let (bridge_read, bridge_write) = tokio::io::split(bridge);
    let signal = DropSignalWriter {
        inner: bridge_write,
        dropped: dropped.clone(),
    };
    let socket = daemon.socket.clone();
    let task = tokio::spawn(run_bridge_stream_with_deadline(
        bridge_read,
        signal,
        socket,
        Duration::from_secs(5),
    ));

    let (read, mut write) = tokio::io::split(client);
    let mut reader = BufReader::new(read);
    write_json_line(
        &mut write,
        &json!({"v":1,"id":1,"req":"hello","min_version":1,"max_version":1}),
    )
    .await
    .unwrap();
    let mut line = String::new();
    read_frame(&mut reader, &mut line).await.unwrap();
    assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["ev"], "hello_ok");
    drop(write);
    drop(reader);

    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the connection future must resolve once the client leaves")
        .unwrap()
        .unwrap();
    assert!(
        dropped.load(Ordering::SeqCst),
        "the writer task outlived the connection"
    );
}
