//! Outbound writes for one API connection.
//!
//! The connection loop used to await every socket write inline, inside the same
//! `select!` that carried the client's requests. One client that stopped
//! reading therefore froze the whole connection: a cancel or an approval
//! arriving on it was never even translated, because the loop was parked in
//! `write_all` on a frame the peer would not read.
//!
//! Now the loop only reads and translates, and one writer task owns the socket:
//!
//! - **One writer.** A single task drains the queue, so two frames can never
//!   interleave on the wire. Translation state stays in the loop, so the frames
//!   are still built in one place and in one order.
//! - **Bounded.** Ordinary (event-stream) frames share a byte budget. A frame
//!   larger than the whole budget is admitted only when the queue is empty, so
//!   a huge history reply cannot be starved and the queue never holds more than
//!   that one frame plus the frame currently being written.
//! - **Control capacity.** A frame that answers a client request (it carries
//!   `reply_to`) travels a separate, small queue, and the writer drains up to
//!   [`CONTROL_BURST`] of them between two ordinary frames. A client that is
//!   not reading the event stream still gets its own replies as soon as the
//!   frame already in flight finishes, instead of waiting behind megabytes of
//!   queued output. Stream output goes to the ordinary queue however small it
//!   is: only replies are scarce.
//! - **Deadline.** A write that makes no progress for
//!   [`WRITE_STALL_DEADLINE`] closes the connection with a named outcome
//!   instead of waiting forever. The deadline measures *no progress*, not frame
//!   duration: every accepted chunk of bytes restarts it, so a large frame to a
//!   slow but reading peer is never cut short.
//!
//! Per-session order is preserved: every frame derived from the daemon stream
//! enters the ordinary queue in translation order and leaves it in that order.
//! Control replies are correlated by `reply_to`, so moving one ahead of a queued
//! event reorders nothing a client reads as a stream.
//!
//! A deadline is a statement about the socket, never about the work: bytes
//! already accepted by the kernel may still be delivered, and a frame that was
//! cut short is simply never terminated with a newline, so an NDJSON reader
//! discards the fragment at end of stream rather than misreading it as a frame.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{Notify, watch};

/// Control frames held at once before the connection is closed as overloaded.
///
/// The queue is small on purpose: it exists to keep a request's reply from
/// waiting behind stream output, not to absorb a client that never reads.
pub(crate) const CONTROL_QUEUE_FRAMES: usize = 128;

/// Bytes of control frames held at once.
pub(crate) const CONTROL_QUEUE_BYTES: usize = 2 * 1024 * 1024;

/// Largest reply treated as a control frame.
///
/// A reply above this is bulk data (a transcript, a file read) and joins the
/// ordinary queue under the byte budget, where it cannot push stream events
/// out of the way or be starved by them.
pub(crate) const CONTROL_FRAME_MAX_BYTES: usize = 64 * 1024;

/// Bytes of ordinary frames held at once.
pub(crate) const DATA_QUEUE_BYTES: usize = 8 * 1024 * 1024;

/// Bytes of frames a request produced that the queue could not take yet.
///
/// A frame larger than the whole queue budget waits here rather than being
/// dropped or pushed into a queue that would then hold two of it. The cap is
/// generous for a burst of history pages and still finite: a client that asks
/// for more output than it will read is closed, not spooled.
pub(crate) const PENDING_BYTES: usize = 32 * 1024 * 1024;

/// Control frames written between two ordinary frames, so a request flood
/// cannot starve the event stream.
pub(crate) const CONTROL_BURST: usize = 8;

/// How long one write may make no progress before the connection closes.
///
/// Ten seconds sits well inside the SDK's 30 second request timeout, so a
/// client learns the connection is dead from the close rather than from a
/// request that silently ran out of time.
pub(crate) const WRITE_STALL_DEADLINE: Duration = Duration::from_secs(10);

/// How a connection's writer finished.
///
/// Named rather than a bare `bool` because it is logged: an operator reading
/// the bridge's stderr has to tell a peer that stopped reading from a socket
/// that failed or a client that outran its own replies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseOutcome {
    /// The connection ended normally; everything queued was written.
    Complete,
    /// The peer accepted no bytes for the whole deadline.
    WriteStalled,
    /// The socket refused the write.
    WriteFailed,
    /// The client sent requests faster than their replies could be written.
    ControlQueueFull,
    /// The client asked for more output than it was reading.
    OutputQueueFull,
    /// The connection ended and queued frames were dropped.
    Aborted,
}

impl CloseOutcome {
    /// The word that goes into the bridge's log line.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            CloseOutcome::Complete => "connection closed after writing every queued frame",
            CloseOutcome::WriteStalled => "peer stopped reading; write deadline expired",
            CloseOutcome::WriteFailed => "socket write failed",
            CloseOutcome::ControlQueueFull => "client outran its replies; control queue full",
            CloseOutcome::OutputQueueFull => "client outran its replies; output buffer full",
            CloseOutcome::Aborted => "connection closed without flushing",
        }
    }

    /// Whether the writer finishes what is already queued before it exits.
    pub(crate) fn drains(self) -> bool {
        matches!(self, CloseOutcome::Complete)
    }

    /// Whether the outcome is worth a log line of its own.
    pub(crate) fn is_failure(self) -> bool {
        matches!(
            self,
            CloseOutcome::WriteStalled
                | CloseOutcome::WriteFailed
                | CloseOutcome::ControlQueueFull
                | CloseOutcome::OutputQueueFull
        )
    }
}

/// Serialize one frame into the exact bytes that go on the wire.
///
/// A frame is always a whole line: the newline is part of the unit the writer
/// hands to the socket, which is what keeps a frame from being split across two
/// queue entries.
pub(crate) fn frame_bytes<T>(value: &T) -> anyhow::Result<Vec<u8>>
where
    T: ?Sized + serde::Serialize,
{
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    Ok(line.into_bytes())
}

/// Serialize one stream frame with the daemon's delivery sequence attached.
///
/// The sequence is additive and top-level, so a client that does not read it
/// ignores an unknown key and an older client is unaffected. It is attached
/// here rather than carried in [`ServerFrame`](horon_agent_harness_api::ServerFrame)
/// because it describes one delivery of one event, and because a reply is
/// correlated by `reply_to` rather than ordered against a snapshot boundary.
pub(crate) fn frame_bytes_stamped<T>(value: &T, seq: u64) -> anyhow::Result<Vec<u8>>
where
    T: ?Sized + serde::Serialize,
{
    let mut value = serde_json::to_value(value)?;
    if let Some(object) = value.as_object_mut() {
        object.insert("seq".to_string(), serde_json::Value::from(seq));
    }
    let mut line = value.to_string();
    line.push('\n');
    Ok(line.into_bytes())
}

#[derive(Default)]
struct QueueState {
    control: VecDeque<Vec<u8>>,
    control_bytes: usize,
    data: VecDeque<Vec<u8>>,
    data_bytes: usize,
    closed: Option<CloseOutcome>,
}

/// What the writer should send next.
enum Item {
    Control(Vec<u8>),
    Data(Vec<u8>),
}

/// The bounded hand-off between the translating loop and the writing task.
pub(crate) struct OutboundQueue {
    state: Mutex<QueueState>,
    /// Signals "there is something to write" to the writer.
    ///
    /// `notify_one` stores a permit, so a frame queued between the writer's
    /// emptiness check and its wait cannot be missed.
    item: Notify,
    /// Signals "the ordinary queue has room" to the translating loop.
    space: Notify,
    /// Publishes the close outcome to the loop, which selects on it.
    closed: watch::Sender<Option<CloseOutcome>>,
}

impl OutboundQueue {
    /// Create the queue and the receiver that reports how the connection closed.
    pub(crate) fn new() -> (Arc<Self>, watch::Receiver<Option<CloseOutcome>>) {
        let (closed, closed_rx) = watch::channel(None);
        let queue = Arc::new(Self {
            state: Mutex::new(QueueState::default()),
            item: Notify::new(),
            space: Notify::new(),
            closed,
        });
        (queue, closed_rx)
    }

    /// A poisoned lock is still consistent: every critical section here only
    /// pushes or pops a `VecDeque`, so recover the guard instead of turning one
    /// panic elsewhere into a dead connection.
    fn lock(&self) -> MutexGuard<'_, QueueState> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Close the connection with `outcome`. Idempotent: the first outcome wins,
    /// so a writer that closed the socket is not relabelled by the loop that
    /// then notices it.
    pub(crate) fn close(&self, outcome: CloseOutcome) {
        {
            let mut state = self.lock();
            if state.closed.is_some() {
                return;
            }
            state.closed = Some(outcome);
        }
        let _ = self.closed.send(Some(outcome));
        self.item.notify_one();
        self.space.notify_one();
    }

    /// Queue a reply to a client request, or report why the connection has to
    /// close instead. Never waits: a full control queue means the client is not
    /// reading its own replies, and waiting would be the stall this module
    /// exists to remove.
    pub(crate) fn push_control(&self, bytes: Vec<u8>) -> Result<(), CloseOutcome> {
        let mut state = self.lock();
        if state.closed.is_some() {
            return Err(CloseOutcome::Aborted);
        }
        if state.control.len() >= CONTROL_QUEUE_FRAMES || state.control_bytes + bytes.len() > CONTROL_QUEUE_BYTES {
            return Err(CloseOutcome::ControlQueueFull);
        }
        state.control_bytes += bytes.len();
        state.control.push_back(bytes);
        drop(state);
        self.item.notify_one();
        Ok(())
    }

    /// Whether one more ordinary frame of `len` bytes fits right now.
    ///
    /// A frame larger than the whole budget fits only into an empty queue: that
    /// keeps a 20 MiB transcript writable without letting the queue grow to two
    /// of them.
    pub(crate) fn has_room_for(&self, len: usize) -> bool {
        let state = self.lock();
        state.data_bytes == 0 || state.data_bytes + len <= DATA_QUEUE_BYTES
    }

    /// Whether the ordinary queue can take another frame at all.
    pub(crate) fn data_has_room(&self) -> bool {
        let state = self.lock();
        state.data_bytes == 0 || state.data_bytes < DATA_QUEUE_BYTES
    }

    /// Queue an ordinary frame. The caller checks [`Self::has_room_for`] first;
    /// a frame that no longer fits is dropped rather than blocking the loop.
    pub(crate) fn push_data(&self, bytes: Vec<u8>) {
        {
            let mut state = self.lock();
            if state.closed.is_some() {
                return;
            }
            state.data_bytes += bytes.len();
            state.data.push_back(bytes);
        }
        self.item.notify_one();
    }

    /// Resolves when the ordinary queue has room again, or when the connection
    /// closes. Losing the race to a notification is harmless: the loop
    /// re-checks capacity before it waits.
    pub(crate) async fn wait_for_data_room(&self) {
        self.space.notified().await;
    }

    async fn wait_for_item(&self) {
        self.item.notified().await;
    }

    /// Take the next frame. Control frames win while `prefer_control` holds,
    /// otherwise an ordinary frame goes first so the event stream keeps moving.
    fn pop(&self, prefer_control: bool) -> Option<Item> {
        let (item, freed) = {
            let mut state = self.lock();
            let take_control = if prefer_control {
                !state.control.is_empty()
            } else {
                state.data.is_empty()
            };
            if take_control {
                match state.control.pop_front() {
                    Some(bytes) => {
                        state.control_bytes -= bytes.len();
                        (Some(Item::Control(bytes)), false)
                    }
                    None => (None, false),
                }
            } else {
                match state.data.pop_front() {
                    Some(bytes) => {
                        state.data_bytes -= bytes.len();
                        (Some(Item::Data(bytes)), true)
                    }
                    // Ordinary frames ran out; fall back to control rather
                    // than idling with replies still queued.
                    None => match state.control.pop_front() {
                        Some(bytes) => {
                            state.control_bytes -= bytes.len();
                            (Some(Item::Control(bytes)), false)
                        }
                        None => (None, false),
                    },
                }
            }
        };
        if freed {
            self.space.notify_one();
        }
        item
    }

    /// The outcome the connection was closed with, if any. The first close
    /// wins, so this is the cause rather than the last symptom.
    pub(crate) fn close_outcome(&self) -> Option<CloseOutcome> {
        self.lock().closed
    }
}

/// Own one connection's write half until the connection ends.
///
/// Returns the outcome so the caller can log it: a stall is an operator-visible
/// event, not an incidental error.
pub(crate) async fn write_loop<W>(mut write: W, queue: Arc<OutboundQueue>, deadline: Duration) -> CloseOutcome
where
    W: AsyncWrite + Unpin,
{
    let mut control_streak = 0usize;
    loop {
        let prefer_control = control_streak < CONTROL_BURST;
        let Some(item) = queue.pop(prefer_control) else {
            match queue.close_outcome() {
                // Closed and nothing left: a normal end, or the loop gave up.
                Some(outcome) if !outcome.drains() => return outcome,
                Some(_) => return CloseOutcome::Complete,
                None => {
                    queue.wait_for_item().await;
                    continue;
                }
            }
        };
        let (bytes, is_control) = match item {
            Item::Control(bytes) => (bytes, true),
            Item::Data(bytes) => (bytes, false),
        };
        control_streak = if is_control { control_streak + 1 } else { 0 };
        if let Err(outcome) = write_frame(&mut write, &bytes, deadline).await {
            return outcome;
        }
    }
}

/// Write one whole frame, restarting the deadline on every accepted chunk.
///
/// `write_all` has no deadline and no progress signal, which is exactly the
/// stall this module removes. Writing by hand also keeps the frame atomic from
/// the queue's point of view: a frame is never interleaved with another, it is
/// only ever finished or abandoned at end of stream.
async fn write_frame<W>(write: &mut W, bytes: &[u8], deadline: Duration) -> Result<(), CloseOutcome>
where
    W: AsyncWrite + Unpin,
{
    let mut written = 0;
    while written < bytes.len() {
        match tokio::time::timeout(deadline, write.write(&bytes[written..])).await {
            Err(_) => return Err(CloseOutcome::WriteStalled),
            Ok(Ok(0)) => return Err(CloseOutcome::WriteFailed),
            Ok(Ok(count)) => written += count,
            Ok(Err(_)) => return Err(CloseOutcome::WriteFailed),
        }
    }
    match tokio::time::timeout(deadline, write.flush()).await {
        Err(_) => Err(CloseOutcome::WriteStalled),
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(CloseOutcome::WriteFailed),
    }
}
