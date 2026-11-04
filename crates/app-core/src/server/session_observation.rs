//! Source-ordered observation state for one session.
//!
//! # Contract
//!
//! * One [`SessionObservation`] per session owns the restart-scoped `generation`,
//!   the monotonic `seq`, the latest immutable observation value, and the bounded
//!   history tail. The process session map is not the atomicity point; this type
//!   is.
//! * Every externally visible state change, conversation change, and published
//!   event advances `seq` through `observe_*`. Nothing else mints a sequence
//!   number, and no caller reconstructs the ordering from a broadcast tap after
//!   the fact.
//! * Two mechanisms classify a live event, and they cover different failures. The
//!   `seq` in a snapshot reply is the state boundary: a change observed before
//!   the capture is inside the view, a change observed after it is newer. The
//!   per-connection FIFO is the delivery boundary: the marker is enqueued on it
//!   inside the same critical section that fixed `seq`, so an event that reaches
//!   the same connection later is after the marker even when it was not observed
//!   (a reply written straight to one connection does not advance `seq`).
//! * A snapshot captures the value, mints its marker, and enqueues that marker on
//!   the requesting connection's FIFO inside **one** critical section that does no
//!   disk I/O, awaits nothing, and never touches the network. A marker the FIFO
//!   cannot take fails the request, because a client told it holds a snapshot must
//!   be able to observe that snapshot's boundary.
//! * `seq` is meaningful only inside one `generation`. A restart mints a new
//!   generation and starts again at 1; nothing here replays or compares `seq`
//!   across generations.
//!
//! The wire types for the marker and the snapshot reply belong to the harness API
//! (plan task 12), so this module carries its own [`SnapshotMarker`],
//! [`ConnectionRecord`], and [`SnapshotView`] plus a handler that frames them.

use crate::protocol::HistoryMessage;
use crate::protocol::ServerEvent;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use tokio::sync::mpsc;

pub(crate) const DEFAULT_TAIL: u32 = 20;
pub(crate) const MAX_TAIL: u32 = 100;
pub(crate) const DEFAULT_MAX_BYTES: u64 = 262_144;
pub(crate) const MAX_MAX_BYTES: u64 = 1_048_576;
const RETAINED_MESSAGES: usize = 512;

/// The boundary record a snapshot request puts on its own connection: a client
/// that holds it classifies every live event that follows as newer than the
/// snapshot, whatever the socket interleaving turns out to be.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SnapshotMarker {
    pub session_id: String,
    pub generation: String,
    pub seq: u64,
    pub marker_id: String,
}

/// One record on a marker-aware connection's FIFO.
///
/// TODO(task 12): add a `Live { seq, generation, event }` arm once the harness
/// API carries a sequence on live events, so a connection can classify an event
/// that arrives before the marker without relying on this FIFO's order.
#[derive(Clone, Debug)]
#[allow(
    dead_code,
    reason = "the per-connection forwarder that drains this FIFO is task 12's dispatcher wiring"
)]
pub(crate) enum ConnectionRecord {
    Marker(SnapshotMarker),
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SnapshotRequest {
    /// Opaque continuation returned by an earlier reply, for an older page.
    pub cursor: Option<String>,
    pub tail: Option<u32>,
    pub max_bytes: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ObservationState {
    pub message_count: usize,
    pub last_seq: u64,
    pub last_change: String,
    pub processing: bool,
}

/// One consistent view of a session at a fixed marker.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct SnapshotView {
    pub session_id: String,
    pub generation: String,
    pub seq: u64,
    pub marker_id: String,
    pub state: ObservationState,
    /// Oldest first, like `History`.
    pub messages: Vec<HistoryMessage>,
    pub next_history_cursor: Option<String>,
    /// Cut by the byte budget or by what is still retained.
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SnapshotError {
    /// The connection FIFO could not take the marker, so no boundary exists.
    MarkerRejected,
}

#[derive(Clone, Debug)]
struct ObservationValue {
    seq: u64,
    messages: Arc<Vec<HistoryMessage>>,
    message_count: usize,
    last_change: &'static str,
    processing: bool,
}

impl Default for ObservationValue {
    fn default() -> Self {
        Self {
            seq: 0,
            messages: Arc::new(Vec::new()),
            message_count: 0,
            last_change: "start",
            processing: false,
        }
    }
}

struct Inner {
    seq: u64,
    value: Arc<ObservationValue>,
}

/// One session's observation owner. `generation` is minted when the owner is
/// created and never reused, so a restart cannot be mistaken for a continuation.
pub(crate) struct SessionObservation {
    session_id: String,
    generation: String,
    inner: Mutex<Inner>,
}

static OBSERVATIONS: LazyLock<Mutex<HashMap<String, Arc<SessionObservation>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn registry() -> MutexGuard<'static, HashMap<String, Arc<SessionObservation>>> {
    OBSERVATIONS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The owner for `session_id`, created on first use: a fresh process creates a
/// fresh owner, so its `generation` differs from the previous process's even for
/// the same session id.
pub(crate) fn observation_for(session_id: &str) -> Arc<SessionObservation> {
    let mut sessions = registry();
    if let Some(existing) = sessions.get(session_id) {
        return Arc::clone(existing);
    }
    let created = Arc::new(SessionObservation {
        session_id: session_id.to_string(),
        generation: uuid::Uuid::new_v4().to_string(),
        inner: Mutex::new(Inner {
            seq: 0,
            value: Arc::new(ObservationValue::default()),
        }),
    });
    sessions.insert(session_id.to_string(), Arc::clone(&created));
    created
}

/// Record a published event as an observable change. Called from the single event
/// funnel, so a snapshot taken afterwards has a strictly greater `seq` than any
/// snapshot captured before it. Returns the sequence this change was recorded
/// at, which is what a live event carries so a client can place it against a
/// snapshot boundary.
pub(crate) fn observe_published(session_id: &str, event: &ServerEvent) -> u64 {
    observation_for(session_id).observe_event(event_label(event))
}

/// Issue the sequence number one event is written to a connection at.
///
/// The delivery boundary is what a client actually observes, so the number a
/// live event carries must advance per event rather than repeat the last state
/// change: two events delivered after the same publish still have to be
/// ordered, and a client proving "everything after seq N is new" needs the
/// numbers to increase strictly. The counter is the owner's single one, so a
/// number minted here is still comparable with a snapshot's `seq`.
pub(crate) fn stamp_event_delivery(session_id: &str) -> u64 {
    observation_for(session_id).advance_delivery()
}

/// Record the conversation a client is being shown as the observable state.
pub(crate) fn observe_conversation(session_id: &str, messages: &[HistoryMessage]) -> u64 {
    observation_for(session_id).replace_conversation(messages)
}

fn event_label(event: &ServerEvent) -> &'static str {
    match event {
        ServerEvent::TextDelta { .. } => "text_delta",
        ServerEvent::ToolStart { .. } => "tool_start",
        ServerEvent::ToolDone { .. } => "tool_done",
        ServerEvent::Done { .. } => "done",
        ServerEvent::Error { .. } => "error",
        ServerEvent::AskRequested { .. } => "ask_requested",
        _ => "event",
    }
}

impl SessionObservation {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Replace the immutable value and advance `seq` in one critical section.
    fn mutate(&self, change: &'static str, apply: impl FnOnce(&mut ObservationValue)) -> u64 {
        let mut inner = self.lock();
        let seq = inner.seq.saturating_add(1);
        let mut value = (*inner.value).clone();
        value.seq = seq;
        value.last_change = change;
        apply(&mut value);
        inner.seq = seq;
        inner.value = Arc::new(value);
        seq
    }

    fn observe_event(&self, label: &'static str) -> u64 {
        self.mutate(label, |_| {})
    }

    /// Advance the sequence for one delivered event, leaving the observed
    /// value's own description untouched.
    ///
    /// `last_change` keeps naming the last state change rather than the last
    /// delivery, so a snapshot taken after delivery still reports what actually
    /// changed, while `seq` stays the one monotonic counter both sides compare.
    fn advance_delivery(&self) -> u64 {
        let mut inner = self.lock();
        let seq = inner.seq.saturating_add(1);
        let mut value = (*inner.value).clone();
        value.seq = seq;
        inner.seq = seq;
        inner.value = Arc::new(value);
        seq
    }

    fn replace_conversation(&self, messages: &[HistoryMessage]) -> u64 {
        let retained_start = messages.len().saturating_sub(RETAINED_MESSAGES);
        let retained: Vec<HistoryMessage> = messages[retained_start..].to_vec();
        self.mutate("conversation", |value| {
            value.message_count = messages.len();
            value.messages = Arc::new(retained);
        })
    }

    /// Append one message, keeping only the newest [`RETAINED_MESSAGES`] so this
    /// stays a bounded copy rather than an unbounded transcript in memory.
    #[cfg(test)]
    pub(crate) fn append_message(&self, message: HistoryMessage) -> u64 {
        self.mutate("conversation", |value| {
            let mut messages = (*value.messages).clone();
            messages.push(message);
            let drop_count = messages.len().saturating_sub(RETAINED_MESSAGES);
            if drop_count > 0 {
                messages.drain(..drop_count);
            }
            value.message_count = value.message_count.saturating_add(1);
            value.messages = Arc::new(messages);
        })
    }

    #[cfg(test)]
    pub(crate) fn set_processing(&self, processing: bool) -> u64 {
        self.mutate("processing", |value| value.processing = processing)
    }

    #[cfg(test)]
    pub(crate) fn seq(&self) -> u64 {
        self.lock().seq
    }

    /// Capture one consistent view and enqueue its marker on `connection`.
    ///
    /// The capture, the marker identity, and the FIFO enqueue happen under the
    /// owner lock, so a change observed before this call is inside the returned
    /// view and a change observed after it is after the marker. Nothing inside
    /// this section reads a file, awaits, or touches the network.
    pub(crate) fn take_snapshot(
        &self,
        request: &SnapshotRequest,
        connection: &mpsc::UnboundedSender<ConnectionRecord>,
    ) -> Result<SnapshotView, SnapshotError> {
        let inner = self.lock();
        let value = Arc::clone(&inner.value);
        let marker_id = uuid::Uuid::new_v4().to_string();
        let marker = SnapshotMarker {
            session_id: self.session_id.clone(),
            generation: self.generation.clone(),
            seq: value.seq,
            marker_id: marker_id.clone(),
        };
        // Enqueued inside the same critical section that fixed the boundary, so
        // the marker cannot land on either side of the value it names.
        if connection.send(ConnectionRecord::Marker(marker)).is_err() {
            return Err(SnapshotError::MarkerRejected);
        }
        let (messages, next_history_cursor, truncated) = select_tail(&value, request);
        Ok(SnapshotView {
            session_id: self.session_id.clone(),
            generation: self.generation.clone(),
            seq: value.seq,
            marker_id,
            state: ObservationState {
                message_count: value.message_count,
                last_seq: value.seq,
                last_change: value.last_change.to_string(),
                processing: value.processing,
            },
            messages,
            next_history_cursor,
            truncated,
        })
    }
}

/// Drop the owner for `session_id`, as a process restart would: the next
/// [`observation_for`] mints a new generation and starts `seq` at 1.
pub(crate) fn forget_session(session_id: &str) {
    registry().remove(session_id);
}

fn cursor_index(cursor: Option<&str>) -> Option<usize> {
    cursor
        .and_then(|cursor| cursor.strip_prefix("before:"))
        .and_then(|index| index.parse::<usize>().ok())
}

/// Pick the tail a reply carries, bounded by count and bytes.
fn select_tail(value: &ObservationValue, request: &SnapshotRequest) -> (Vec<HistoryMessage>, Option<String>, bool) {
    let total = value.message_count;
    let tail = request.tail.unwrap_or(DEFAULT_TAIL).clamp(1, MAX_TAIL) as usize;
    let budget = request.max_bytes.unwrap_or(DEFAULT_MAX_BYTES).min(MAX_MAX_BYTES);
    let retained_start = total.saturating_sub(value.messages.len());
    let end = cursor_index(request.cursor.as_deref()).map_or(total, |end| end.min(total));
    let start = end.saturating_sub(tail);

    // A cursor that points before what is still retained cannot be served from
    // memory; the oldest page we still hold is the honest answer.
    if end < retained_start {
        return (Vec::new(), Some(format!("before:{retained_start}")), true);
    }
    let from = start.max(retained_start);
    if from >= end {
        return (Vec::new(), (end > 0).then(|| format!("before:{end}")), true);
    }

    let mut used = 0u64;
    let mut effective_start = end;
    let mut truncated = from > start;
    while effective_start > from {
        let message = &value.messages[effective_start - 1 - retained_start];
        let size = u64::try_from(message.role.len() + message.content.len()).unwrap_or(u64::MAX);
        if used.saturating_add(size) > budget {
            truncated = true;
            break;
        }
        used = used.saturating_add(size);
        effective_start -= 1;
    }
    let messages = value.messages[effective_start - retained_start..end - retained_start].to_vec();
    // `truncated` covers both cuts a client cares about: the byte budget stopped
    // the page, and the requested tail was clamped below what was asked for. In
    // both cases older messages exist and `next_history_cursor` names them.
    let truncated = truncated || effective_start > 0;
    let next = (effective_start > 0).then(|| format!("before:{effective_start}"));
    (messages, next, truncated)
}

#[derive(Serialize)]
struct SnapshotFrame<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    id: u64,
    #[serde(flatten)]
    view: &'a SnapshotView,
}

async fn write_frame(writer: &mut (impl tokio::io::AsyncWrite + Unpin), frame: &impl Serialize) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut buf = serde_json::to_vec(frame)?;
    buf.push(b'\n');
    writer.write_all(&buf).await?;
    Ok(())
}

/// Answer one snapshot request: the marker is on the connection FIFO before the
/// reply is written.
///
/// TODO(task 12): dispatch `ApiRequest::GetSnapshot` here and replace
/// [`SnapshotFrame`] with `ApiEvent::Snapshot` once that type reaches the daemon.
#[allow(
    dead_code,
    reason = "no production caller until task 12 wires the get_snapshot request into the dispatcher"
)]
pub(crate) async fn handle_get_snapshot(
    id: u64,
    session_id: &str,
    request: SnapshotRequest,
    connection: &mpsc::UnboundedSender<ConnectionRecord>,
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> anyhow::Result<()> {
    let observation = observation_for(session_id);
    match observation.take_snapshot(&request, connection) {
        Ok(view) => {
            // The reply carries the same `marker_id`, so a client that reads it
            // before the marker can buffer instead of guessing from order.
            write_frame(
                writer,
                &SnapshotFrame {
                    kind: "snapshot",
                    id,
                    view: &view,
                },
            )
            .await
        }
        Err(SnapshotError::MarkerRejected) => {
            // A snapshot the client cannot place on its own stream is a false
            // success, so this refuses with the daemon's existing error frame.
            let refusal = ServerEvent::Error {
                id,
                message: format!(
                    "get_snapshot for session `{session_id}` could not enqueue its marker; the connection cannot hold the snapshot boundary"
                ),
                retry_after_secs: None,
            };
            write_frame(writer, &refusal).await
        }
    }
}

#[cfg(test)]
#[path = "session_observation_tests.rs"]
mod tests;
