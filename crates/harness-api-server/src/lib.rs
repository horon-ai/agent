//! Harness API bridge: exposes the stable versioned harness API on its own
//! Unix socket and translates to the internal (legacy) horon-agent protocol.
//!
//! Architecture (milestone 2 of docs/protocol/HARNESS_API.md; storage format in
//! docs/protocol/SESSION_STORE.md):
//! - Listens on `~/.horon/horon-agent-api.sock` (or `HORON_AGENT_API_SOCKET`).
//! - For each API client, dials the legacy daemon socket (`HORON_AGENT_SOCKET` or
//!   `~/.horon/horon-agent.sock`) and speaks `subscribe`/`message`/... on its
//!   behalf.
//! - Translation is JSON-to-JSON so this crate does not depend on the heavy
//!   internal protocol types and cannot be broken by additive internal
//!   changes.
//!
//! This keeps the daemon untouched while the API surface stabilizes. Once
//! proven, the same translation can move in-process behind a `hello` sniff on
//! the main socket.
// Tests hold the std env/home serialization lock across awaits on purpose.
#![cfg_attr(test, allow(clippy::await_holding_lock))]

pub mod allocator;
pub mod background_progress;
mod degradation_receipts;
mod message_content;
mod message_modes;
pub mod replication;
pub mod session_prefix;
mod tool_args_repaired;
pub mod translate;
mod writer;

#[cfg(test)]
mod capability_contract_tests;
#[cfg(test)]
mod large_reply_tests;
#[cfg(test)]
mod pending_drain_tests;
#[cfg(test)]
mod session_presence_tests;
#[cfg(test)]
mod stdio_tests;
#[cfg(test)]
mod writer_tests;

use anyhow::{Context, Result};
use horon_agent_harness_api::{API_VERSION_MAJOR, ApiEvent, ErrorCode, ServerFrame};
use serde_json::Value;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
// Unix sockets on Unix, named pipes on Windows, one API. Without this the
// bridge simply did not compile for Windows, so the SDK could not run there at
// all.
use horon_agent_transport::{Listener, Stream};

// Socket paths live in `horon-agent-harness-api` so clients and the bridge can never
// resolve different directories (they once did, and the desktop app could not
// connect as a result).
pub use horon_agent_harness_api::{api_socket_path, legacy_socket_path};

/// Frames above this size are transient (history, transcripts, file reads).
/// Their buffers are released instead of pinning that capacity for the rest
/// of the connection, and the allocator is asked to return the pages.
const LARGE_FRAME_BYTES: usize = 256 * 1024;

fn release_large_frame(buffer: &mut Vec<u8>) {
    if buffer.capacity() > LARGE_FRAME_BYTES {
        *buffer = Vec::new();
        allocator::trim();
    }
}

/// Capabilities the bridge itself advertises, independent of what the native
/// daemon supports. The `hello_ok` reply adds the daemon-dependent ones on top.
///
/// Every capability a client is told to check must appear here: a method gated
/// on a capability the bridge never advertises is unreachable no matter what
/// the daemon can do.
pub const BRIDGE_CAPABILITIES: &[&str] = &[
    "sessions",
    "streaming",
    "text_framing",
    "turn_stop_reasons",
    "persisted_session_discovery",
    "runtime_info",
    "api_key_provisioning",
    "auth_changed_notification",
    "usage_invalidation",
    "session_archive",
    "session_retention",
    "session_files",
    "session_fork",
    "todo_state",
    "session_event_store_v1",
    "session_replication_v1",
    "session_history_cursor_v1",
    "structured_compaction_v1",
    "session_search",
    "artifacts",
    "usage_report",
    // A client that sees these may render approval cards and agent questions;
    // both round-trip through the bridge to the daemon's own decision store.
    "permissions",
    "asks",
    // Durable runtime surfaces. Every one of these is a request the bridge
    // serves itself or gates on `ApiRequest::required_capability`, so a client
    // that checks the capability before sending must find it here: a method
    // gated on a capability the bridge never advertises is unreachable no
    // matter what the daemon can do.
    horon_agent_harness_api::CAPABILITY_DURABLE_RUNTIME_V1,
    horon_agent_harness_api::CAPABILITY_MONITOR_V1,
    horon_agent_harness_api::CAPABILITY_SNAPSHOT_MARKER_V1,
    horon_agent_harness_api::CAPABILITY_ASK_RECEIPTS_V1,
];

/// Capabilities the native daemon advertises in its ping reply. Empty when
/// the daemon is too old to advertise any or did not answer in time.
///
/// Probe only an old, universally supported control request on a disposable
/// connection. Older daemons close that connection after ping, and may close
/// connections receiving unknown requests. Never probe on the session stream.
async fn daemon_capabilities(socket: &std::path::Path) -> Vec<String> {
    let probe = async {
        let mut stream = Stream::connect(socket).await.ok()?;
        write_json_line(&mut stream, &serde_json::json!({"type":"ping", "id":0}))
            .await
            .ok()?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        read_frame(&mut reader, &mut line).await.ok()?;
        let reply: Value = serde_json::from_str(&line).ok()?;
        if reply["type"] != "pong" || reply["id"] != 0 {
            return None;
        }
        Some(
            reply["capabilities"]
                .as_array()
                .map(|caps| caps.iter().filter_map(|cap| cap.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
        )
    };
    tokio::time::timeout(std::time::Duration::from_millis(500), probe)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Fresh source-daemon connections. A disposable Ping never subscribes,
/// resumes, or creates a session. Failed snapshots clear connection presence.
async fn daemon_user_connections(socket: &std::path::Path) -> std::collections::HashSet<String> {
    let probe = async {
        let mut stream = Stream::connect(socket).await.context("connect presence probe")?;
        write_json_line(&mut stream, &serde_json::json!({"type":"ping", "id":0})).await?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        read_frame(&mut reader, &mut line).await?;
        let reply: Value = serde_json::from_str(&line).context("parse presence pong")?;
        if reply["type"] != "pong" || reply["id"] != 0 {
            anyhow::bail!("presence probe received an uncorrelated reply");
        }
        // Older daemons do not advertise connection presence.
        if reply["user_connected_session_ids"].is_null() {
            return Ok(std::collections::HashSet::new());
        }
        let ids: Vec<String> = serde_json::from_value(reply["user_connected_session_ids"].clone())
            .context("parse connected session IDs")?;
        Ok::<_, anyhow::Error>(ids.into_iter().filter(|id| !id.is_empty()).collect())
    };
    match tokio::time::timeout(std::time::Duration::from_millis(500), probe).await {
        Ok(Ok(ids)) => ids,
        Ok(Err(error)) => {
            eprintln!("Harness presence snapshot unavailable: {error:#}");
            std::collections::HashSet::new()
        }
        Err(_) => {
            eprintln!("Harness presence probe timed out, clearing connection presence");
            std::collections::HashSet::new()
        }
    }
}

/// Largest single request frame accepted from an API client, in bytes.
///
/// `read_line` grows its buffer until it finds a newline, so a client that
/// never sends one makes the bridge allocate without bound: one connection can
/// exhaust the host's memory, and the bridge serves every client on the
/// machine. A 32 MiB aggregate PDF budget expands to ~43 MiB of base64,
/// so 64 MiB leaves room for metadata while retaining a finite frame bound.
const MAX_FRAME_BYTES: u64 = 64 * 1024 * 1024;

/// Read one newline-delimited frame, refusing to buffer more than
/// `MAX_FRAME_BYTES`. Returns `Ok(0)` at end of stream, like `read_line`.
async fn read_frame<R>(reader: &mut R, line: &mut String) -> std::io::Result<usize>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    line.clear();
    let mut limited = tokio::io::AsyncReadExt::take(reader, MAX_FRAME_BYTES);
    let read = limited.read_line(line).await?;
    // A full buffer with no terminator means the frame exceeded the cap (or is
    // exactly at it and unterminated); either way it cannot be trusted.
    if read as u64 == MAX_FRAME_BYTES && !line.ends_with('\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame exceeds {MAX_FRAME_BYTES} byte limit"),
        ));
    }
    Ok(read)
}

/// Cancellation-safe variant for the multiplexed client loop. Tokio guarantees
/// `read_until` retains bytes already appended to `frame` when another select
/// branch wins, unlike `read_line` with a temporary String.
async fn read_frame_bytes<R>(reader: &mut R, frame: &mut Vec<u8>) -> std::io::Result<usize>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    // A cancelled read retains its prefix. Limit the remaining bytes, not
    // each invocation separately, or repeated cancellation defeats the cap.
    let remaining = MAX_FRAME_BYTES.saturating_sub(frame.len() as u64);
    let mut limited = tokio::io::AsyncReadExt::take(reader, remaining);
    limited.read_until(b'\n', frame).await?;
    if frame.len() as u64 >= MAX_FRAME_BYTES && !frame.ends_with(b"\n") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame exceeds {MAX_FRAME_BYTES} byte limit"),
        ));
    }
    Ok(frame.len())
}

/// Take the exclusive bridge lock beside the API socket, or report that a live
/// bridge already holds it.
///
/// The lock is released when the holder exits, however it exits, so a crashed
/// bridge never wedges the next one out.
pub(crate) fn single_instance_lock(
    api_socket: &std::path::Path,
) -> Result<Option<horon_agent_platform::ipc::InstanceLock>> {
    let path = api_socket.with_extension("lock");
    horon_agent_platform::ipc::lock_instance_file(&path).with_context(|| format!("open bridge lock {}", path.display()))
}

pub async fn run_bridge(api_socket: PathBuf, legacy_socket: PathBuf) -> Result<()> {
    run_bridge_with_deadline(api_socket, legacy_socket, writer::WRITE_STALL_DEADLINE).await
}

/// [`run_bridge`] with the write deadline injected, so a test can watch a
/// stalled peer close its connection without waiting the production interval.
async fn run_bridge_with_deadline(
    api_socket: PathBuf,
    legacy_socket: PathBuf,
    write_deadline: std::time::Duration,
) -> Result<()> {
    // Only one bridge may own the socket.
    //
    // This is the fix for clients seeing "disconnected: harness API stream
    // closed" at random: every desktop client spawned a bridge on demand, and
    // each new bridge unlinked the live socket and bound its own. The older
    // bridges kept running with their connected clients, but the *pathname*
    // now pointed at the newest one, and each reconnect churned the same way.
    // Whoever lost the race had its clients dropped. Refusing to start when a
    // live bridge holds the lock makes on-demand spawning idempotent, which is
    // what every caller already assumes.
    let _lock = match single_instance_lock(&api_socket) {
        Ok(Some(lock)) => Some(lock),
        Ok(None) => {
            eprintln!(
                "harness API bridge: another bridge already owns {}; exiting",
                api_socket.display()
            );
            return Ok(());
        }
        // The lock is advisory. A platform that cannot open it right now - a
        // filter driver holding the file, a directory the caller may not write -
        // must not stop the only bridge serving this desktop, so the launch
        // continues and reports why the single-owner check was skipped.
        Err(error) => {
            eprintln!(
                "harness API bridge: single-instance lock for {} is unavailable ({error:#}); continuing",
                api_socket.display()
            );
            None
        }
    };
    // A stale socket file blocks bind. Only a platform whose listening
    // endpoints are filesystem nodes has one to remove: on Windows the pipe
    // namespace is not the filesystem, and probing the pipe would consume one
    // published instance of a server that is already gone. Unlinking is safe
    // because a live bridge would have made this process exit above, whatever
    // the advisory lock answered.
    if horon_agent_platform::ipc::socket_nodes_can_be_stale() {
        horon_agent_platform::ipc::remove_socket(&api_socket);
    }
    if let Some(parent) = api_socket.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    // Accepting takes `&mut self` on both platforms: a Windows named-pipe
    // listener republishes a pipe instance for every accept, and the adapter
    // gives the Unix socket the same shape so callers never branch.
    let mut listener =
        Listener::bind(&api_socket).with_context(|| format!("bind API socket {}", api_socket.display()))?;
    // Restrict the socket to its owner, matching the daemon socket it fronts.
    //
    // Without this the bridge widens access to everything behind it: the
    // daemon socket is 0600, but a default-umask bind here produced 0755, so
    // any local user could drive sessions, read transcripts, and spend the
    // owner's provider tokens. A bridge must never be more permissive than
    // the thing it bridges to.
    //
    // A Windows named pipe carries an ACL rather than a file mode, so the
    // adapter's restriction is empty there and the pipe keeps its own access.
    horon_agent_platform::ipc::restrict_socket_to_owner(&api_socket)
        .with_context(|| format!("restrict API socket {}", api_socket.display()))?;
    eprintln!(
        "harness API bridge: listening on {} -> {}",
        api_socket.display(),
        legacy_socket.display()
    );
    loop {
        let (stream, _) = listener.accept().await?;
        let legacy = legacy_socket.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_api_client(stream, legacy, write_deadline).await {
                eprintln!("harness API bridge: client ended: {error:#}");
            }
            // A closed client drops its translation state and buffers.
            allocator::trim();
        });
    }
}

async fn handle_api_client(stream: Stream, legacy_socket: PathBuf, write_deadline: std::time::Duration) -> Result<()> {
    let (read_half, write_half) = stream.into_split();
    run_bridge_stream_with_deadline(read_half, write_half, legacy_socket, write_deadline).await
}

/// Serve the process's stdin/stdout as one API connection.
///
/// Read stdin on a plain thread, not Tokio's blocking pool: Tokio stdin cannot
/// be cancelled and otherwise prevents process exit when the daemon disconnects
/// while SSH still holds stdin open. This entrypoint is for a CLI process only.
#[cfg(unix)]
pub async fn run_bridge_stdio(legacy_socket: PathBuf) -> Result<()> {
    let (input, mut writer) = std::os::unix::net::UnixStream::pair()?;
    input.set_nonblocking(true)?;
    let input = tokio::net::UnixStream::from_std(input)?;
    std::thread::Builder::new().name("api-stdin".into()).spawn(move || {
        let _ = std::io::copy(&mut std::io::stdin().lock(), &mut writer);
        let _ = writer.shutdown(std::net::Shutdown::Write);
    })?;
    run_bridge_stream(input, tokio::io::stdout(), legacy_socket).await
}

/// Serve one stable API connection over a duplex byte stream, including SSH
/// stdio. No API listener or filesystem socket is created for this connection.
pub async fn run_bridge_stream<R, W>(read_half: R, write_half: W, legacy_socket: PathBuf) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Send + 'static + Unpin,
{
    run_bridge_stream_with_deadline(read_half, write_half, legacy_socket, writer::WRITE_STALL_DEADLINE).await
}

/// [`run_bridge_stream`] with the write deadline injected, so a test can watch a
/// stalled peer close the connection without waiting the production interval.
async fn run_bridge_stream_with_deadline<R, W>(
    read_half: R,
    mut write_half: W,
    legacy_socket: PathBuf,
    write_deadline: std::time::Duration,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Send + 'static + Unpin,
{
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    // 1. Handshake: first frame must be hello with a compatible version.
    read_frame(&mut reader, &mut line).await?;
    // A malformed first frame used to abort the task, closing the connection
    // with no reply at all: the client saw only an EOF and could not tell a
    // protocol mistake from a crashed bridge. Say what was wrong, then close.
    let hello: Value = match serde_json::from_str(line.trim()) {
        Ok(value) => value,
        Err(error) => {
            let frame = ServerFrame::event(ApiEvent::Error {
                code: ErrorCode::InvalidRequest,
                message: format!("first frame must be a JSON `hello`: {error}"),
                details: None,
            });
            write_json_line(&mut write_half, &frame).await?;
            return Ok(());
        }
    };
    let reply_to = hello["id"].as_u64().unwrap_or(0);
    let client_name = hello["client"].as_str().unwrap_or_default();
    let compatible = hello["req"] == "hello"
        && hello["min_version"].as_u64().unwrap_or(0) <= u64::from(API_VERSION_MAJOR)
        && hello["max_version"].as_u64().unwrap_or(0) >= u64::from(API_VERSION_MAJOR);
    if !compatible {
        let frame = ServerFrame::reply(
            reply_to,
            ApiEvent::Error {
                code: ErrorCode::UnsupportedVersion,
                message: format!(
                    "bridge speaks API v{API_VERSION_MAJOR}; this client asked for v{}..=v{}",
                    hello["min_version"].as_u64().unwrap_or(0),
                    hello["max_version"].as_u64().unwrap_or(0),
                ),
                details: None,
            },
        );
        write_json_line(&mut write_half, &frame).await?;
        return Ok(());
    }
    // Do not claim a usable connection before the native daemon is reachable.
    let legacy = Stream::connect(&legacy_socket)
        .await
        .with_context(|| format!("connect legacy socket {}", legacy_socket.display()))?;
    let daemon_caps = daemon_capabilities(&legacy_socket).await;
    let user_connections_supported = daemon_caps.iter().any(|cap| cap == "session_connections");
    let runtime_commands_supported = daemon_caps.iter().any(|cap| cap == "runtime_commands");
    let session_tools_supported = daemon_caps.iter().any(|cap| cap == "session_tools");
    let soft_interrupt_ids_supported = daemon_caps.iter().any(|cap| cap == "soft_interrupt_ids");
    let durable_runtime_api_supported = daemon_caps.iter().any(|cap| cap == "durable_runtime_api");
    let hello_ok = ServerFrame::reply(
        reply_to,
        ApiEvent::HelloOk {
            version: API_VERSION_MAJOR,
            server: format!("horon-agent-harness-api-bridge/{}", env!("CARGO_PKG_VERSION")),
            capabilities: BRIDGE_CAPABILITIES
                .iter()
                .copied()
                .map(str::to_string)
                .chain(user_connections_supported.then(|| "session_connections".to_string()))
                .chain(runtime_commands_supported.then(|| "runtime_commands".to_string()))
                .chain(session_tools_supported.then(|| "session_tools".to_string()))
                .chain(
                    ["soft_interrupt_ids", "background_tool_result"]
                        .into_iter()
                        .filter(|_| soft_interrupt_ids_supported)
                        .map(str::to_string),
                )
                .collect(),
        },
    );
    write_json_line(&mut write_half, &hello_ok).await?;

    // 2. Dial the legacy daemon for this client.
    let (legacy_read, mut legacy_write) = legacy.into_split();
    let mut legacy_reader = BufReader::new(legacy_read);

    let mut state = translate::BridgeState::with_crash_on_disconnect(client_name.starts_with("horon-agent-desktop-"));
    state.session_tools_supported = session_tools_supported;
    state.supports_user_response = hello["supports_user_response"].as_bool().unwrap_or(false);
    state.supports_ask_requests = hello["supports_ask_requests"].as_bool().unwrap_or(false);
    state.runtime_commands_supported = runtime_commands_supported;
    state.soft_interrupt_ids_supported = soft_interrupt_ids_supported;
    state.durable_runtime_api_supported = durable_runtime_api_supported;

    // 3. Translate both directions in one loop so translation state stays
    //    single-threaded, and never await a write to the client from it: one
    //    writer task owns that socket behind a bounded queue. A client that
    //    stops reading used to freeze this loop in `write_all`, so its own
    //    cancel or approval was never translated at all.
    let (queue, mut closed) = writer::OutboundQueue::new();
    // The writer publishes its own outcome before it exits. Without that, a
    // write that died of its deadline left this loop waiting for a close that
    // only this loop could announce, and the connection hung until the client
    // happened to leave.
    let writer_task = {
        let queue = queue.clone();
        tokio::spawn(async move {
            let outcome = writer::write_loop(write_half, queue.clone(), write_deadline).await;
            queue.close(outcome);
            outcome
        })
    };
    let mut pending: std::collections::VecDeque<Vec<u8>> = std::collections::VecDeque::new();
    let mut pending_bytes = 0usize;
    let mut api_frame = Vec::new();
    let mut legacy_frame = Vec::new();

    // A frame is a control frame exactly when it answers a client request, the
    // same distinction the SDKs make on `reply_to`. Everything else is stream
    // output, however small: routing a delta into the control queue would let a
    // normal busy session fill it and close the connection.
    //
    // A reply too large for the control queue is bulk data (a transcript, a
    // file read): it takes the ordinary queue and its byte budget instead, so
    // it cannot park megabytes of replies behind it. The label is a parameter
    // because a macro's own label is hygienic and cannot name this block.
    //
    // The queueing decision is separate from the serialization so bytes that
    // were serialized earlier, to measure them, take the same path.
    //
    // Ordinary output goes to the queue whenever it fits, because the queue is
    // what the writer drains and waiting behind it is what backpressures the
    // reader. `pending` is the fallback for frames that arrive while the queue
    // is full, never the first stop: measuring every reply against that cap
    // closed the connection on a transcript larger than `PENDING_BYTES`, which
    // the inline write this replaced had delivered.
    //
    // A frame larger than the pending cap takes the queue even when the queue
    // already holds data, because `pending` can never hold it at all and
    // closing is what the client cannot recover from; the queue then holds the
    // ordinary budget plus that one frame, and the reader waits for it to
    // drain.
    //
    // The empty `pending` requirement is what keeps stream order: while it
    // holds a frame, that frame is older than anything translated now, and
    // queueing the newer one directly would put it on the wire first.
    macro_rules! send_bytes {
        ($label:lifetime, $bytes:expr, $control:expr) => {
            if $control {
                if let Err(outcome) = queue.push_control($bytes) {
                    break $label Ok(outcome);
                }
            } else if pending.is_empty()
                && (queue.has_room_for($bytes.len()) || $bytes.len() > writer::PENDING_BYTES)
            {
                queue.push_data($bytes);
            } else {
                if pending_bytes + $bytes.len() > writer::PENDING_BYTES {
                    break $label Ok(writer::CloseOutcome::OutputQueueFull);
                }
                pending_bytes += $bytes.len();
                pending.push_back($bytes);
            }
        };
    }

    macro_rules! send_frame {
        ($label:lifetime, $frame:expr) => {
            match writer::frame_bytes(&$frame) {
                Ok(bytes) => {
                    let control = $frame.reply_to.is_some()
                        && bytes.len() <= writer::CONTROL_FRAME_MAX_BYTES;
                    send_bytes!($label, bytes, control);
                }
                Err(error) => break $label Err(error),
            }
        };
    }

    // A stream frame carries the daemon's delivery sequence so a client can
    // prove that everything after its snapshot's `seq` is new. A reply is
    // correlated by `reply_to` instead and keeps its plain encoding: stamping
    // it would put an ordering claim on a frame that makes none.
    macro_rules! send_stream_frame {
        ($label:lifetime, $frame:expr, $seq:expr) => {
            match $seq {
                Some(seq) => match writer::frame_bytes_stamped(&$frame, seq) {
                    Ok(bytes) => send_bytes!($label, bytes, false),
                    Err(error) => break $label Err(error),
                },
                None => send_frame!($label, $frame),
            }
        };
    }

    // A snapshot and the marker that bounds it must stay adjacent and in that
    // order, so both take the ordinary queue. The control queue drains ahead of
    // stream output on purpose, so a reply carrying `reply_to` would otherwise
    // overtake the marker that has to precede it.
    macro_rules! send_snapshot_frame {
        ($label:lifetime, $frame:expr) => {
            match writer::frame_bytes(&$frame) {
                Ok(bytes) => send_bytes!($label, bytes, false),
                Err(error) => break $label Err(error),
            }
        };
    }

    // The notice that explains a close is the last thing this connection says,
    // and the client is owed it even when the ordinary buffer is already full:
    // a bare EOF cannot tell a protocol mistake from a crashed bridge. It
    // carries no `reply_to`, so it still takes the ordinary path and keeps its
    // place at the end of the stream; it only skips that buffer's byte cap,
    // which one small frame cannot meaningfully exceed.
    macro_rules! send_terminal_frame {
        ($label:lifetime, $frame:expr) => {
            match writer::frame_bytes(&$frame) {
                Ok(bytes) => {
                    pending_bytes += bytes.len();
                    pending.push_back(bytes);
                }
                Err(error) => break $label Err(error),
            }
        };
    }

    let translated: Result<writer::CloseOutcome> = async {
        'translate: {
            loop {
                while let Some(front) = pending.front() {
                    if !queue.has_room_for(front.len()) {
                        break;
                    }
                    let bytes = pending.pop_front().expect("the front frame was just measured");
                    pending_bytes -= bytes.len();
                    queue.push_data(bytes);
                }
                // Only the daemon stream waits for room; the client's own requests
                // are translated whatever the queue holds, which is the point of
                // moving the writes out of this loop.
                let can_read_legacy = pending.is_empty() && queue.data_has_room();
                tokio::select! {
                    biased;
                    // The writer closed the socket (stalled peer, write failure).
                    _ = closed.changed() => {
                        let outcome = (*closed.borrow()).unwrap_or(writer::CloseOutcome::Aborted);
                        break 'translate Ok(outcome);
                    }
                    n = read_frame_bytes(&mut reader, &mut api_frame) => {
                        let n = match n {
                            Ok(n) => n,
                            // An oversized frame is unrecoverable: the stream is now
                            // mid-frame with no way to resynchronise. Report and close.
                            Err(error) => {
                                let frame = ServerFrame::event(ApiEvent::Error {
                                    code: ErrorCode::InvalidRequest,
                                    message: error.to_string(),
                                    details: None,
                                });
                                send_terminal_frame!('translate, frame);
                                break 'translate Ok(writer::CloseOutcome::Complete);
                            }
                        };
                        if n == 0 { break 'translate Ok(writer::CloseOutcome::Aborted); }
                        if api_frame.iter().all(u8::is_ascii_whitespace) {
                            api_frame.clear();
                            continue;
                        }
                        let parsed = serde_json::from_slice(&api_frame);
                        // The bytes that actually arrived bound a replication
                        // request, so their length travels with the request
                        // instead of a second serialization of the parsed value.
                        let envelope = api_frame.len()
                            - usize::from(api_frame.last() == Some(&b'\n'));
                        api_frame.clear();
                        release_large_frame(&mut api_frame);
                        let mut request: Value = match parsed {
                            Ok(value) => value,
                            Err(error) => {
                                // No `reply_to`: the id lived in the frame that failed
                                // to parse, so there is nothing to correlate against.
                                let frame = ServerFrame::event(ApiEvent::Error {
                                    code: ErrorCode::InvalidRequest,
                                    message: error.to_string(),
                                    details: None,
                                });
                                send_frame!('translate, frame);
                                continue;
                            }
                        };
                        // The `[messages]` modes are daemon configuration rather
                        // than session state, so the bridge answers them here and
                        // the daemon never sees the request.
                        if let Some(frame) = message_modes::reply(&request) {
                            send_frame!('translate, frame);
                            continue;
                        }
                        if request["req"] == "list_sessions" {
                            state.user_connected_sessions = if user_connections_supported {
                                daemon_user_connections(&legacy_socket).await
                            } else { Default::default() };
                        }
                        // A session id prefix names one session when it is
                        // unique, so an operator does not have to paste all of
                        // an id. This runs before translation because every
                        // session-addressed request forwards its id verbatim.
                        if let Some(frame) = state.resolve_session_prefix(&mut request) {
                            send_frame!('translate, frame);
                            continue;
                        }
                        // Translation may inspect persisted session/archive files. Tell
                        // Tokio before entering that synchronous region so it can keep
                        // the accept loop and fresh-client handshakes scheduled.
                        state.request_envelope_bytes = Some(envelope);
                        let outbound = tokio::task::block_in_place(|| {
                            state.api_request_to_legacy(&request)
                        });
                        // The measurement belongs to the request just translated:
                        // leaving it set would let a later caller read a stale
                        // envelope from an unrelated frame.
                        state.request_envelope_bytes = None;
                        let heavy = allocator::request_is_heavy(&request);
                        drop(request);
                        for out in outbound {
                            match out {
                                translate::Outbound::Legacy(value) => {
                                    // The daemon is a local peer that always reads,
                                    // but an open socket with a wedged process
                                    // behind it must not park this loop either: the
                                    // same deadline bounds a daemon write, and its
                                    // expiry ends the connection with a named
                                    // outcome.
                                    if let Err(outcome) =
                                        write_json_line_bounded(&mut legacy_write, &value, write_deadline)
                                            .await
                                    {
                                        break 'translate Ok(outcome);
                                    }
                                }
                                translate::Outbound::Reply(frame) => {
                                    send_frame!('translate, frame);
                                }
                                translate::Outbound::Serialized { bytes, .. } => {
                                    let control = bytes.len() <= writer::CONTROL_FRAME_MAX_BYTES;
                                    send_bytes!('translate, bytes, control);
                                }
                            }
                        }
                        if heavy {
                            allocator::trim();
                        }
                    }
                    // Wakes the loop when the ordinary queue drains, so a daemon
                    // stream resumes without polling.
                    _ = queue.wait_for_data_room(), if !can_read_legacy => {}
                    n = read_frame_bytes(&mut legacy_reader, &mut legacy_frame), if can_read_legacy => {
                        if n? == 0 {
                            let frame = ServerFrame::event(ApiEvent::Error {
                                code: ErrorCode::Internal,
                                message: "daemon connection closed".into(),
                                details: None,
                            });
                            send_terminal_frame!('translate, frame);
                            break 'translate Ok(writer::CloseOutcome::Complete);
                        }
                        if legacy_frame.iter().all(u8::is_ascii_whitespace) {
                            legacy_frame.clear();
                            continue;
                        }
                        let parsed = serde_json::from_slice(&legacy_frame);
                        let large = legacy_frame.capacity() > LARGE_FRAME_BYTES;
                        legacy_frame.clear();
                        let event: Value = match parsed {
                            Ok(value) => value,
                            Err(_) => {
                                release_large_frame(&mut legacy_frame);
                                continue;
                            }
                        };
                        // Only a few replies read persisted session files. Streaming
                        // deltas are pure translation, and routing each one through
                        // block_in_place handed the worker core to a fresh blocking
                        // thread per event: a busy session grew the bridge to 69 idle
                        // threads, each pinning its own allocator arena.
                        let frames = if translate::legacy_event_may_block(&event) {
                            tokio::task::block_in_place(|| state.legacy_event_to_api(&event))
                        } else {
                            state.legacy_event_to_api(&event)
                        };
                        // The daemon stamps every event it writes with the
                        // observation owner's sequence. It travels with the
                        // frames this event produced, so a client can order
                        // live events against the snapshot it was given.
                        let seq = event["seq"].as_u64();
                        drop(event);
                        for frame in frames {
                            if matches!(
                                frame.event,
                                ApiEvent::Snapshot { .. } | ApiEvent::SnapshotMarker { .. }
                            ) {
                                send_snapshot_frame!('translate, frame);
                            } else if frame.reply_to.is_some() {
                                send_frame!('translate, frame);
                            } else {
                                send_stream_frame!('translate, frame, seq);
                            }
                        }
                        if large {
                            release_large_frame(&mut legacy_frame);
                        }
                    }
                }
            }
        }
    }
    .await;

    // The writer is joined, never left running: a connection that ended must
    // not leave a task holding the socket. When the writer itself closed the
    // socket, the join returns immediately; when the loop closed it, the join
    // waits only for the frame in flight, bounded by the write deadline.
    let outcome = match translated {
        Ok(outcome) => outcome,
        Err(error) => {
            queue.close(writer::CloseOutcome::Aborted);
            let _ = writer_task.await;
            return Err(error);
        }
    };
    // Frames the loop held back under backpressure are still owed to the
    // client, and the notice that explains this close is one of them: it
    // carries no `reply_to`, so it took the ordinary path into `pending` and
    // exiting the loop dropped it. Handing them to the queue before the close
    // keeps `Complete` honest and lets the writer deliver the reason, in
    // stream order, instead of ending the connection with a bare EOF.
    if outcome.drains() {
        for bytes in pending.drain(..) {
            queue.push_data(bytes);
        }
    }
    queue.close(outcome);
    let written = match writer_task.await {
        Ok(outcome) => outcome,
        Err(error) => anyhow::bail!("harness API writer task failed: {error}"),
    };
    // Report the reason the connection first closed, not what the writer was
    // doing when it noticed. A client that outran its replies closes the
    // connection with that reason, and the writer then returns its own stall:
    // naming the stall would hide the cause an operator needs.
    let outcome = queue.close_outcome().unwrap_or(written);
    if outcome.is_failure() {
        anyhow::bail!("harness API connection closed: {}", outcome.as_str());
    }
    Ok(())
}

async fn write_json_line<W, T>(writer: &mut W, value: &T) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
    T: ?Sized + serde::Serialize,
{
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// [`write_json_line`] under the connection's write deadline.
///
/// `write_all` waits forever on a peer that never drains, which is the stall
/// this connection no longer tolerates on either socket. The error names the
/// outcome so the caller can close the connection with it.
async fn write_json_line_bounded<W, T>(
    writer: &mut W,
    value: &T,
    deadline: std::time::Duration,
) -> std::result::Result<(), writer::CloseOutcome>
where
    W: AsyncWriteExt + Unpin,
    T: ?Sized + serde::Serialize,
{
    match tokio::time::timeout(deadline, write_json_line(writer, value)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(writer::CloseOutcome::WriteFailed),
        Err(_) => Err(writer::CloseOutcome::WriteStalled),
    }
}

#[cfg(test)]
mod public_acceptance_tests {
    use super::*;
    use horon_agent_platform::ipc::{Listener, Stream};
    use serde_json::json;
    use std::ffi::OsString;
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[tokio::test(flavor = "multi_thread")]
    async fn stdio_stream_handshake_ping_and_eof() {
        let path = std::env::temp_dir().join(format!(
            "horon-agent-stdio-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut listener = Listener::bind(&path).unwrap();
        let (client, bridge) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(bridge);
        let task = tokio::spawn(run_bridge_stream(read, write, path.clone()));
        let (client_read, mut client_write) = tokio::io::split(client);
        let mut reader = BufReader::new(client_read);
        write_json_line(
            &mut client_write,
            &json!({"v":1,"id":1,"req":"hello","min_version":1,"max_version":1,"client":"stdio-test"}),
        )
        .await
        .unwrap();
        let (_daemon, _) = listener.accept().await.unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["ev"], "hello_ok");
        write_json_line(&mut client_write, &json!({"v":1,"id":2,"req":"ping"}))
            .await
            .unwrap();
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        let pong: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(pong["ev"], "pong");
        assert_eq!(pong["reply_to"], 2);
        drop(client_write);
        drop(reader);
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        drop(listener);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn stdio_does_not_confirm_handshake_when_native_daemon_is_missing() {
        let (mut client, bridge) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(bridge);
        let task = tokio::spawn(run_bridge_stream(
            read,
            write,
            PathBuf::from("/nonexistent-horon-agent-stdio-test/daemon.sock"),
        ));
        write_json_line(
            &mut client,
            &json!({"v":1,"id":1,"req":"hello","min_version":1,"max_version":1}),
        )
        .await
        .unwrap();
        let error = task.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("connect legacy socket"));
    }

    struct HoronAgentHomeGuard(Option<OsString>);

    impl Drop for HoronAgentHomeGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => unsafe { std::env::set_var("HORON_HOME", value) },
                None => unsafe { std::env::remove_var("HORON_HOME") },
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn public_socket_keeps_its_attachment_after_another_sessions_state() {
        let _home_lock = translate::horon_agent_home_test_lock();
        let root = std::env::temp_dir().join(format!(
            "horon-api-attachment-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                % 1_000_000_000
        ));
        std::fs::create_dir_all(&root).unwrap();
        let previous_home = std::env::var_os("HORON_HOME");
        unsafe { std::env::set_var("HORON_HOME", &root) };
        let _home_guard = HoronAgentHomeGuard(previous_home);
        let sessions = root.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join("session_alpha.json"),
            json!({"working_dir": "/workspace/alpha", "messages": []}).to_string(),
        )
        .unwrap();
        let api_path = root.join("api.sock");
        let legacy_path = root.join("legacy.sock");
        let mut legacy_listener = Listener::bind(&legacy_path).unwrap();

        let fake_daemon = tokio::spawn(async move {
            let (stream, _) = legacy_listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let mut attached_state_id = None;
            while let Some(line) = lines.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                match request["type"].as_str() {
                    Some("state") if attached_state_id.is_none() => {
                        let id = request["id"].as_u64().unwrap();
                        attached_state_id = Some(id);
                        write_json_line(
                            &mut write,
                            &json!({"type":"state", "id":id, "session_id":"session_alpha"}),
                        )
                        .await
                        .unwrap();
                    }
                    Some("message") => {
                        assert_eq!(request["content"], "public boundary prompt");
                        return;
                    }
                    _ => {}
                }
                if attached_state_id.is_some() {
                    write_json_line(
                        &mut write,
                        &json!({"type":"state", "id":999_999, "session_id":"session_beta"}),
                    )
                    .await
                    .unwrap();
                    attached_state_id = None;
                }
            }
            panic!("API connection closed before the message reached the attached session");
        });

        let bridge = tokio::spawn(run_bridge(api_path.clone(), legacy_path));
        let stream = loop {
            match Stream::connect(&api_path).await {
                Ok(stream) => break stream,
                Err(_) => tokio::task::yield_now().await,
            }
        };
        let (read, mut write) = stream.into_split();
        let mut replies = BufReader::new(read).lines();
        for request in [
            json!({"id":1, "req":"hello", "min_version":1, "max_version":1, "client":"acceptance-test"}),
            json!({"id":2, "req":"attach_session", "session_id":"session_alpha"}),
        ] {
            write_json_line(&mut write, &request).await.unwrap();
            replies.next_line().await.unwrap().expect("public API reply");
        }
        write_json_line(
            &mut write,
            &json!({"id":3, "req":"send_message", "session_id":"session_alpha", "content":"public boundary prompt"}),
        )
        .await
        .unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(2), fake_daemon)
            .await
            .expect("message should cross the API/legacy boundary")
            .unwrap();
        bridge.abort();
        let _ = std::fs::remove_dir_all(root);
    }
}

#[cfg(test)]
#[path = "framing_tests.rs"]
mod framing_tests;

#[cfg(test)]
mod single_instance_tests {
    /// Two bridges must never both own the API socket.
    ///
    /// They used to: `run_bridge` unlinked whatever socket file was there and
    /// bound its own, so every on-demand spawn silently evicted the live
    /// bridge and its clients reported "harness API stream closed".
    #[test]
    fn a_second_bridge_cannot_take_the_socket() {
        let dir = std::env::temp_dir().join(format!("horon-agent-bridge-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("horon-agent-api.sock");

        let first = super::single_instance_lock(&socket).unwrap();
        assert!(first.is_some(), "the first bridge must take the lock");
        assert!(
            super::single_instance_lock(&socket).unwrap().is_none(),
            "a second bridge must be refused while the first is alive"
        );

        // Once the owner is gone the lock is available again, so a crashed
        // bridge never wedges its replacement out.
        drop(first);
        assert!(super::single_instance_lock(&socket).unwrap().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod socket_permission_tests {
    /// The API socket must never be more permissive than the daemon socket it
    /// fronts.
    ///
    /// This regressed once: `UnixListener::bind` applies the process umask, so
    /// the socket landed at 0755 while the daemon socket it bridges to is
    /// 0600. Every guarantee behind the daemon socket was then reachable by
    /// any local user, including reading transcripts and spending the owner's
    /// provider tokens.
    #[tokio::test]
    async fn the_api_socket_is_owner_only() {
        let dir = std::env::temp_dir().join(format!("horon-agent-api-perm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let api_socket = dir.join("api.sock");
        let legacy_socket = dir.join("daemon.sock");

        let bridge_socket = api_socket.clone();
        let handle = tokio::spawn(async move {
            let _ = super::run_bridge(bridge_socket, legacy_socket).await;
        });

        // Wait for the bind, which happens before the accept loop.
        let mut owner_only = None;
        for _ in 0..100 {
            if horon_agent_platform::ipc::is_socket_path(&api_socket) {
                owner_only = Some(horon_agent_platform::ipc::socket_is_owner_only(&api_socket));
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        handle.abort();
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(
            owner_only,
            Some(true),
            "API socket must be owner-only (0600); a wider mode exposes every \
             session behind the bridge to other local users"
        );
    }
}

#[cfg(test)]
mod fragmented_reply_tests {
    use super::*;
    use horon_agent_platform::ipc::Listener;
    use serde_json::json;

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_history_replies_survive_api_requests_between_fragments() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let path = std::env::temp_dir().join(format!("horon-agent-fragments-{}-{}.sock", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
            let mut listener = Listener::bind(&path).unwrap();
            let (client, bridge) = tokio::io::duplex(4 * 1024 * 1024);
            let (bridge_read, bridge_write) = tokio::io::split(bridge);
            let task = tokio::spawn(run_bridge_stream(bridge_read, bridge_write, path.clone()));
            let (client_read, mut client_write) = tokio::io::split(client);
            let mut replies = BufReader::new(client_read).lines();
            write_json_line(&mut client_write, &json!({"v":1,"id":1,"req":"hello","min_version":1,"max_version":1})).await.unwrap();
            let (daemon, _) = listener.accept().await.unwrap();
            let (daemon_read, mut daemon_write) = daemon.into_split();
            let mut requests = BufReader::new(daemon_read).lines();
            assert_eq!(serde_json::from_str::<Value>(&replies.next_line().await.unwrap().unwrap()).unwrap()["ev"], "hello_ok");
            write_json_line(&mut client_write, &json!({"v":1,"id":2,"req":"attach_session","session_id":"fragment-session"})).await.unwrap();
            for _ in 0..3 {
                let request: Value = serde_json::from_str(&requests.next_line().await.unwrap().unwrap()).unwrap();
                if request["type"] == "state" {
                    write_json_line(&mut daemon_write, &json!({"type":"state","id":request["id"],"session_id":"fragment-session"})).await.unwrap();
                }
            }
            loop {
                let frame: Value = serde_json::from_str(&replies.next_line().await.unwrap().unwrap()).unwrap();
                if frame["ev"] == "attached" { break; }
            }
            for id in 3..6 {
                write_json_line(&mut client_write, &json!({"v":1,"id":id,"req":"get_history","session_id":"fragment-session"})).await.unwrap();
            }
            let mut history_ids = Vec::new();
            for _ in 0..3 {
                let request: Value = serde_json::from_str(&requests.next_line().await.unwrap().unwrap()).unwrap();
                assert_eq!(request["type"], "get_history");
                history_ids.push(request["id"].clone());
            }
            for (index, id) in history_ids.into_iter().enumerate() {
                // Larger than the socket send buffer, forcing the bridge to
                // consume an unterminated prefix before API activity cancels it.
                let text = "x".repeat(1024 * 1024);
                let prefix = format!("{{\"type\":\"history\",\"id\":{id},\"session_id\":\"fragment-session\",\"messages\":[{{\"role\":\"assistant\",\"content\":\"{text}");
                daemon_write.write_all(prefix.as_bytes()).await.unwrap();
                let ping_id = 20 + index;
                write_json_line(&mut client_write, &json!({"v":1,"id":ping_id,"req":"ping"})).await.unwrap();
                let ping: Value = serde_json::from_str(&requests.next_line().await.unwrap().unwrap()).unwrap();
                assert_eq!(ping["type"], "ping");
                daemon_write.write_all(b"\"}]}\n").await.unwrap();
                write_json_line(&mut daemon_write, &json!({"type":"pong","id":ping["id"]})).await.unwrap();
                let mut history_seen = false;
                let mut pong_seen = false;
                while !history_seen || !pong_seen {
                    let reply: Value = serde_json::from_str(&replies.next_line().await.unwrap().unwrap()).unwrap();
                    if reply["reply_to"] == index + 3 {
                        assert_eq!(reply["ev"], "history");
                        assert_eq!(reply["messages"][0]["content"], text);
                        history_seen = true;
                    }
                    if reply["reply_to"] == ping_id {
                        assert_eq!(reply["ev"], "pong");
                        pong_seen = true;
                    }
                }
            }
            drop(client_write);
            drop(replies);
            task.await.unwrap().unwrap();
            drop(listener);
            std::fs::remove_file(path).unwrap();
        }).await.expect("fragmented history requests must all receive correlated replies");
    }
}
