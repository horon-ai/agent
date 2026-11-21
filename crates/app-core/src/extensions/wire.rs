//! Bounded JSON-RPC transport for a user-installed Rust package child.
//!
//! Frames, payloads, protocol ranges, capability sets and the declaration
//! digest check are the shared contract in
//! [`horon_agent_extension_types::wire`]. This module adds the parts that need
//! a runtime:
//!
//! - [`FrameReader`] reads one newline-terminated UTF-8 frame at a time and
//!   refuses to buffer past [`WireLimits::max_frame_bytes`], so a peer that
//!   never sends a newline cannot make the host allocate without bound. Every
//!   read is bounded by a deadline.
//! - [`write_frame`] writes one frame under a deadline, so a peer that stops
//!   reading cannot stall the host either.
//! - [`ExtensionWire`] owns request ids, the pending-request map, the bounded
//!   observation queue and the writer task. Ids are monotonic and never
//!   recycled, so an answer that arrives after its request timed out or was
//!   cancelled can never be handed to a later request: it is unregistered,
//!   reported and dropped.
//!
//! One `oneshot` carries answer, cancellation and connection loss, so a waiter
//! cannot observe "channel closed" and report a loss when the real outcome was
//! a cancellation. The handshake is reserved before any await, so two callers
//! cannot both send `initialize`, and `initialize`, `cancel` and `shutdown`
//! have their own entry points: a raw request cannot smuggle a control method.
//!
//! A failed exchange is never retried here. A second attempt could duplicate a
//! side effect the child already performed, so the wire reports the ambiguity
//! and the caller decides.
//!
//! The read loop belongs to the supervisor that owns the child (a later step):
//! it calls [`ExtensionWire::dispatch`] for every inbound frame and
//! [`ExtensionWire::handle_eof`] once the stream ends. That split is the
//! boundary a spawn and lifetime stage plugs into without changing this file.

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use horon_agent_extension_types::wire::{
    CancelParams, Capability, FrameError, HandshakeError, HostExpectation, InitializeParams, InitializeResult,
    MAX_FRAME_BYTES, MAX_PENDING_REQUESTS, MAX_QUEUED_FRAMES, Negotiated, RequestIds, ShutdownParams, WireFrame,
    WireMethod, WireNotification, WireRequest, WireResponse, encode_frame, negotiate, parse_frame, verify_offer,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

/// Bounds one connection must honour. The defaults are the shared wire
/// contract; a test narrows them so a limit is reached in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireLimits {
    pub max_frame_bytes: usize,
    pub max_pending_requests: usize,
    pub max_queued_frames: usize,
    /// Round-trip deadline for one request. On expiry the pending entry is
    /// dropped, so a late answer is reported as unregistered.
    pub reply_deadline: Duration,
    /// Deadline for one inbound frame.
    pub read_deadline: Duration,
    /// Deadline for one frame to leave the host, flush included.
    pub write_deadline: Duration,
}

impl Default for WireLimits {
    fn default() -> Self {
        Self {
            max_frame_bytes: MAX_FRAME_BYTES,
            max_pending_requests: MAX_PENDING_REQUESTS,
            max_queued_frames: MAX_QUEUED_FRAMES,
            reply_deadline: Duration::from_secs(30),
            read_deadline: Duration::from_secs(30),
            write_deadline: Duration::from_secs(10),
        }
    }
}

/// Why a frame, a request or the connection was refused.
///
/// No variant carries a frame body, so a log line built from one cannot leak
/// tool output, model text or a secret the child sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// The frame itself was refused (size, UTF-8, shape, parameters).
    Frame(FrameError),
    /// The peer sent nothing before a deadline, or ended inside a frame.
    ReadTimeout,
    /// One frame did not leave the host before the write deadline.
    WriteTimeout,
    /// The stream ended, or the writer task stopped after a write failure.
    Closed,
    /// The writer queue is full.
    QueueFull,
    /// Too many requests already wait for an answer.
    TooManyPending { limit: usize },
    /// `initialize` was called twice, or its first attempt is still running.
    AlreadyInitialized,
    /// The method was sent before `initialize` settled.
    NotInitialized,
    /// `initialize` refused: version range, digest, kind or capabilities.
    Handshake(HandshakeError),
    /// The negotiated capabilities do not cover this method.
    CapabilityUnavailable(WireMethod),
    /// A raw request named a method that has its own entry point.
    ControlMethod(WireMethod),
    /// No request with this id is waiting.
    UnknownRequest(u64),
    /// An answer for an id nobody waits for: a late, duplicate or stale frame.
    UnexpectedResponse(u64),
    /// The child asked something only the host may ask.
    UnexpectedRequest(WireMethod),
    /// The child answered with a JSON-RPC error code.
    Refused(i32),
    /// The request was cancelled through [`ExtensionWire::cancel`].
    Cancelled,
    /// The host issued `shutdown`; nothing may be sent afterwards.
    Shutdown,
    /// Request ids ran out. A session stops instead of reusing one.
    IdsExhausted,
}

impl fmt::Display for WireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => write!(formatter, "{error}"),
            Self::ReadTimeout => formatter.write_str("peer sent no frame before the deadline"),
            Self::WriteTimeout => formatter.write_str("frame did not leave before the write deadline"),
            Self::Closed => formatter.write_str("connection closed"),
            Self::QueueFull => formatter.write_str("frame queue is full"),
            Self::TooManyPending { limit } => write!(formatter, "more than {limit} requests are waiting"),
            Self::AlreadyInitialized => formatter.write_str("connection is already initialized"),
            Self::NotInitialized => formatter.write_str("connection is not initialized"),
            Self::Handshake(error) => write!(formatter, "initialize refused: {error}"),
            Self::CapabilityUnavailable(method) => write!(formatter, "{method:?} is not negotiated"),
            Self::ControlMethod(method) => {
                write!(
                    formatter,
                    "{method:?} has its own entry point and cannot be a raw request"
                )
            }
            Self::UnknownRequest(id) => write!(formatter, "no request {id} is waiting"),
            Self::UnexpectedResponse(id) => write!(formatter, "answer for request {id}, which is not waiting"),
            Self::UnexpectedRequest(method) => write!(formatter, "child sent {method:?}, which only the host issues"),
            Self::Refused(code) => write!(formatter, "child refused the request (JSON-RPC code {code})"),
            Self::Cancelled => formatter.write_str("request was cancelled"),
            Self::Shutdown => formatter.write_str("connection is shut down"),
            Self::IdsExhausted => formatter.write_str("request id space is exhausted"),
        }
    }
}

impl std::error::Error for WireError {}

impl From<FrameError> for WireError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

/// Reads one newline-terminated frame at a time, bounded by the frame cap and
/// a per-frame deadline.
pub struct FrameReader<R> {
    reader: tokio::io::BufReader<R>,
    limits: WireLimits,
    frame: Vec<u8>,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    pub fn new(reader: R, limits: WireLimits) -> Self {
        Self {
            reader: tokio::io::BufReader::new(reader),
            limits,
            frame: Vec::new(),
        }
    }

    /// Read and parse the next frame. `Ok(None)` ends a clean stream.
    ///
    /// A frame must end with a newline; a stream that ends inside one is
    /// [`FrameError::Unterminated`] instead of a partial parse. Any error
    /// leaves the connection unusable, so the caller ends it rather than
    /// retrying the read.
    pub async fn next(&mut self) -> Result<Option<WireFrame>, WireError> {
        self.frame.clear();
        let Self { reader, limits, frame } = self;
        let cap = limits.max_frame_bytes as u64;
        let read = tokio::time::timeout(
            limits.read_deadline,
            tokio::io::AsyncReadExt::take(reader, cap).read_until(b'\n', frame),
        )
        .await
        .map_err(|_| WireError::ReadTimeout)?
        .map_err(|_| WireError::Closed)?;
        if read == 0 {
            return Ok(None);
        }
        if !frame.ends_with(b"\n") {
            // Either the cap was reached without a terminator, or the stream
            // ended inside the frame; both are refusals, not partial frames.
            return Err(WireError::Frame(if frame.len() >= limits.max_frame_bytes {
                FrameError::TooLarge {
                    limit: limits.max_frame_bytes,
                }
            } else {
                FrameError::Unterminated
            }));
        }
        parse_frame(frame).map(Some).map_err(WireError::Frame)
    }
}

/// Write one frame and flush it under a deadline.
///
/// Used by the writer task here, by a child answering on its own stream, and
/// by any side that owns only a write half.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &impl Serialize,
    deadline: Duration,
) -> Result<(), WireError> {
    let bytes = encode_frame(frame)?;
    match tokio::time::timeout(deadline, async {
        writer.write_all(&bytes).await?;
        writer.flush().await
    })
    .await
    {
        Ok(Ok(())) => Ok(()),
        // A write that failed or stalled leaves the stream unusable; retrying
        // could duplicate an effect the child already ran.
        Ok(Err(_)) => Err(WireError::Closed),
        Err(_) => Err(WireError::WriteTimeout),
    }
}

/// The outcome of one request, delivered through one channel.
///
/// An answer, a cancellation and the end of the connection all arrive here, so
/// a waiter never has to guess from a closed channel which of them happened.
type Outcome = Result<WireResponse, WireError>;

/// One request that is waiting for its answer.
struct PendingRequest {
    outcome: oneshot::Sender<Outcome>,
}

/// One frame queued for the writer, with the deadline of the request it
/// belongs to. A notification carries none.
struct QueuedFrame {
    bytes: Vec<u8>,
    deadline: Option<Instant>,
}

/// Where one connection stands. A later supervisor moves this through its
/// lifetime stages; the wire only needs to know whether request slots are
/// open, which capabilities were negotiated, and whether it is over.
///
/// `Initializing` is a reservation: it is taken under the lifecycle lock
/// before the handshake frame is built, so a second caller cannot pass the
/// `New` check while the first one is awaiting its answer.
#[derive(Clone)]
enum Lifecycle {
    New,
    Initializing,
    Ready(Negotiated),
    Finished,
}

impl Lifecycle {
    fn negotiated(&self) -> Option<Negotiated> {
        match self {
            Self::Ready(negotiated) => Some(negotiated.clone()),
            Self::New | Self::Initializing | Self::Finished => None,
        }
    }
}

/// Host half of one extension connection: ids, correlation and the writer.
///
/// The read half is driven by the caller, which feeds frames to
/// [`Self::dispatch`]. Keeping the byte-level read policy with the supervisor
/// that owns the process is what lets a later step add spawn and lifetime
/// stages without changing this contract.
pub struct ExtensionWire {
    limits: WireLimits,
    ids: RequestIds,
    pending: Mutex<HashMap<u64, PendingRequest>>,
    lifecycle: Mutex<Lifecycle>,
    writer: Mutex<Option<mpsc::Sender<QueuedFrame>>>,
    /// Closing this sender (by taking it in [`Self::finish`]) is what lets a
    /// reader drain the queue and then end.
    observation_sender: Mutex<Option<mpsc::Sender<WireNotification>>>,
    observations: tokio::sync::Mutex<mpsc::Receiver<WireNotification>>,
    /// Level-triggered so a wait that starts after the connection ended still
    /// sees it. The writer task shares the sender and sets it when it exits.
    closed: tokio::sync::watch::Sender<bool>,
    closed_state: tokio::sync::watch::Receiver<bool>,
    drained: AtomicBool,
}

impl ExtensionWire {
    /// Start the writer task over `writer` and return the host half.
    ///
    /// The task owns the write half for the life of the connection: it applies
    /// the write deadline to every queued frame, skips a frame whose request
    /// already gave up, and closes the connection when a write fails or stalls.
    pub fn new<W: AsyncWrite + Send + 'static + Unpin>(writer: W, limits: WireLimits) -> Self {
        let (frames, mut queue) = mpsc::channel::<QueuedFrame>(limits.max_queued_frames);
        let (observation_sender, observations) = mpsc::channel::<WireNotification>(limits.max_queued_frames);
        let (closed, closed_state) = tokio::sync::watch::channel(false);
        let task_closed = closed.clone();
        tokio::spawn(async move {
            let mut writer = writer;
            while let Some(frame) = queue.recv().await {
                // A frame that waited past its own reply deadline must not go
                // out now: nobody is waiting for the answer, and a late send
                // could duplicate an effect the caller already retried
                // elsewhere.
                if frame.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    continue;
                }
                let sent = tokio::time::timeout(limits.write_deadline, async {
                    writer.write_all(&frame.bytes).await?;
                    writer.flush().await
                })
                .await;
                if !matches!(sent, Ok(Ok(()))) {
                    break;
                }
            }
            queue.close();
            task_closed.send_replace(true);
            if let Err(error) = writer.shutdown().await {
                crate::logging::debug(&format!("extension wire: closing the write half failed: {error}"));
            }
        });
        Self {
            limits,
            ids: RequestIds::default(),
            pending: Mutex::new(HashMap::new()),
            lifecycle: Mutex::new(Lifecycle::New),
            writer: Mutex::new(Some(frames)),
            observation_sender: Mutex::new(Some(observation_sender)),
            observations: tokio::sync::Mutex::new(observations),
            closed,
            closed_state,
            drained: AtomicBool::new(false),
        }
    }

    /// Number of requests waiting for an answer.
    pub fn pending_requests(&self) -> usize {
        self.lock_pending().len()
    }

    /// Whether the writer task stopped or the stream ended.
    pub fn is_closed(&self) -> bool {
        *self.closed_state.borrow()
    }

    /// The version and capabilities `initialize` settled, if it did.
    pub fn negotiated(&self) -> Option<Negotiated> {
        self.lock_lifecycle().negotiated()
    }

    /// Send `initialize` and settle the version and capability set.
    ///
    /// Both package kinds travel this one path. The kind and id in `params` are
    /// compared with the installation record, and the reply is checked against
    /// the same record's protocol range, declared capabilities and declaration
    /// digest.
    ///
    /// The handshake slot is reserved before anything is awaited, so one
    /// connection can never put two `initialize` frames on the wire.
    pub async fn initialize(
        &self,
        params: InitializeParams,
        expectation: &HostExpectation<'_>,
    ) -> Result<Negotiated, WireError> {
        // The identity check reads no shared state, so a caller that names
        // another package gets that answer without spending the one slot.
        verify_offer(expectation, &params).map_err(WireError::Handshake)?;
        if self.is_closed() {
            return Err(WireError::Closed);
        }
        {
            let mut lifecycle = self.lock_lifecycle();
            match *lifecycle {
                Lifecycle::New => *lifecycle = Lifecycle::Initializing,
                Lifecycle::Initializing | Lifecycle::Ready(_) => return Err(WireError::AlreadyInitialized),
                Lifecycle::Finished => return Err(WireError::Shutdown),
            }
        }
        let deadline = Instant::now() + self.limits.reply_deadline;
        let answer = self
            .exchange_once(WireMethod::Initialize, Some(encode_params(&params)?), deadline)
            .await
            .and_then(decode::<InitializeResult>);
        let result = match answer {
            Ok(result) => result,
            Err(error) => {
                // A connection whose handshake did not complete is not usable
                // for anything else, and the slot never reopens.
                self.finish();
                return Err(error);
            }
        };
        match negotiate(expectation, &result) {
            Ok(negotiated) => {
                *self.lock_lifecycle() = Lifecycle::Ready(negotiated.clone());
                Ok(negotiated)
            }
            Err(error) => {
                self.finish();
                Err(WireError::Handshake(error))
            }
        }
    }

    /// Issue one request and decode its result as `T`.
    pub async fn request<T: DeserializeOwned>(
        &self,
        method: WireMethod,
        params: Option<Value>,
    ) -> Result<T, WireError> {
        decode(self.request_value(method, params).await?)
    }

    /// Issue one request and return its raw result.
    pub async fn request_value(&self, method: WireMethod, params: Option<Value>) -> Result<Value, WireError> {
        self.ensure_request_allowed(method)?;
        let deadline = Instant::now() + self.limits.reply_deadline;
        self.exchange_once(method, params, deadline).await
    }

    /// Report one observed event to the child. A notification expects no
    /// answer, so it only has to reach the wire.
    pub fn notify(&self, params: impl Serialize) -> Result<(), WireError> {
        self.require_capability(Capability::ObserverNotify, WireMethod::ObserverNotify)?;
        let params = encode_params(&params)?;
        self.send(&WireNotification::new(WireMethod::ObserverNotify, Some(params)), None)
    }

    fn require_capability(&self, capability: Capability, method: WireMethod) -> Result<(), WireError> {
        if self.is_closed() {
            return Err(WireError::Closed);
        }
        match &*self.lock_lifecycle() {
            Lifecycle::Ready(negotiated) if negotiated.capabilities.contains(&capability) => Ok(()),
            Lifecycle::Ready(_) => Err(WireError::CapabilityUnavailable(method)),
            Lifecycle::Finished => Err(WireError::Shutdown),
            Lifecycle::New | Lifecycle::Initializing => Err(WireError::NotInitialized),
        }
    }

    /// Cancel one waiting request.
    ///
    /// The pending entry is removed and the outcome delivered through the same
    /// channel the answer would use, so the waiter reports `Cancelled` rather
    /// than a closed channel, and an answer that arrives afterwards is
    /// unregistered and cannot be handed to anyone. Ids are never reused, which
    /// is the whole stale-result story. `cancel` is itself a notification: it
    /// names the request to drop, so it needs no id of its own.
    pub fn cancel(&self, request_id: u64) -> Result<(), WireError> {
        if self.is_closed() {
            return Err(WireError::Closed);
        }
        let pending = self
            .lock_pending()
            .remove(&request_id)
            .ok_or(WireError::UnknownRequest(request_id))?;
        match pending.outcome.send(Err(WireError::Cancelled)) {
            Ok(()) => {}
            Err(_gone) => {
                // The waiter already timed out; the id stays spent.
            }
        }
        let params = encode_params(&CancelParams { request_id })?;
        self.send(&WireNotification::new(WireMethod::Cancel, Some(params)), None)
    }

    /// Send `shutdown`, fail every waiting request and stop sending.
    ///
    /// A child that exits on `shutdown` before answering is not an error: the
    /// host has nothing left to deliver, and it never retries.
    pub async fn shutdown(&self, reason: Option<String>) -> Result<(), WireError> {
        if self.is_closed() {
            return Err(WireError::Closed);
        }
        if matches!(*self.lock_lifecycle(), Lifecycle::Finished) {
            return Err(WireError::Shutdown);
        }
        let params = encode_params(&ShutdownParams { reason })?;
        let deadline = Instant::now() + self.limits.reply_deadline;
        let answered = self.exchange_once(WireMethod::Shutdown, Some(params), deadline).await;
        self.finish();
        match answered {
            Ok(_) => Ok(()),
            // An EOF or a silent peer after `shutdown` was sent is the expected
            // end of the connection, not a failure to report.
            Err(WireError::Closed) | Err(WireError::ReadTimeout) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Route one inbound frame.
    ///
    /// An answer goes to the request waiting for that id; anything else — a
    /// duplicate, a late answer, a method only the host may issue — is reported
    /// so the read loop can log it and continue. A refusal here never ends the
    /// connection.
    pub fn dispatch(&self, frame: WireFrame) -> Result<(), WireError> {
        match frame {
            WireFrame::Response(response) => {
                let id = response.id;
                if !response.is_exclusive() {
                    return Err(WireError::Frame(FrameError::InvalidResponse));
                }
                let pending = self
                    .lock_pending()
                    .remove(&id)
                    .ok_or(WireError::UnexpectedResponse(id))?;
                match pending.outcome.send(Ok(response)) {
                    Ok(()) => Ok(()),
                    // The entry existed, so the waiter must too; treat a
                    // vanished one as the stale answer it is.
                    Err(_gone) => Err(WireError::UnexpectedResponse(id)),
                }
            }
            WireFrame::Notification(notification) => {
                if notification.method != WireMethod::ObserverNotify {
                    return Err(WireError::UnexpectedRequest(notification.method));
                }
                self.require_capability(Capability::ObserverNotify, notification.method)?;
                let sender = self
                    .observation_sender
                    .lock()
                    .map_err(|_| WireError::Closed)?
                    .as_ref()
                    .cloned()
                    .ok_or(WireError::Closed)?;
                // A bounded channel: when the caller is not draining
                // observations fast enough the event is dropped, never buffered
                // without bound.
                sender.try_send(notification).map_err(|error| match error {
                    mpsc::error::TrySendError::Full(_) => WireError::QueueFull,
                    mpsc::error::TrySendError::Closed(_) => WireError::Closed,
                })
            }
            WireFrame::Request(request) => Err(WireError::UnexpectedRequest(request.method)),
        }
    }

    /// The child's stream ended: every waiting request fails and nothing more
    /// may be sent.
    pub fn handle_eof(&self) {
        self.finish();
    }

    /// The next observation the child reported, in arrival order.
    ///
    /// `None` once the connection is closed and the queue is drained.
    pub async fn next_observation(&self) -> Option<WireNotification> {
        // One consumer, so holding the receiver across the await is exactly the
        // ordering guarantee callers want. `None` once the queue is drained and
        // `handle_eof` or `shutdown` closed the sender.
        self.observations.lock().await.recv().await
    }

    /// Send one request and wait for its outcome.
    ///
    /// Lifecycle gating happens in the entry points, not here: `initialize`
    /// runs while the slot is reserved, so this cannot ask whether the
    /// connection is ready.
    async fn exchange_once(
        &self,
        method: WireMethod,
        params: Option<Value>,
        deadline: Instant,
    ) -> Result<Value, WireError> {
        if self.is_closed() {
            return Err(WireError::Closed);
        }
        // A request whose deadline has already passed must not be queued: its
        // callers have stopped waiting, and sending it later could duplicate an
        // effect.
        if Instant::now() >= deadline {
            return Err(WireError::WriteTimeout);
        }
        let id = self.next_id()?;
        let (outcome, receiver) = oneshot::channel();
        {
            let mut pending = self.lock_pending();
            if pending.len() >= self.limits.max_pending_requests {
                return Err(WireError::TooManyPending {
                    limit: self.limits.max_pending_requests,
                });
            }
            pending.insert(id, PendingRequest { outcome });
        }
        if let Err(error) = self.send(&WireRequest::new(id, method, params), Some(deadline)) {
            self.lock_pending().remove(&id);
            return Err(error);
        }
        let mut closed = self.closed_state.clone();
        let answer = tokio::select! {
            biased;
            outcome = receiver => match outcome {
                Ok(outcome) => outcome,
                Err(_dropped) => Err(WireError::Closed),
            },
            // `wait_for` is level-triggered: a connection that ended before this
            // wait began still reports `Closed` instead of hanging to the
            // deadline.
            _ = closed.wait_for(|ended| *ended) => Err(WireError::Closed),
            () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => Err(WireError::ReadTimeout),
        };
        // `dispatch` and `cancel` already removed the entry; removing twice is
        // how this stays idempotent on every path.
        self.lock_pending().remove(&id);
        answer?
            .into_outcome()
            .map_err(WireError::Frame)?
            .map_err(|error| WireError::Refused(error.code))
    }

    /// A raw request may only carry a capability method. `initialize`,
    /// `cancel` and `shutdown` own their state and have their own entry points,
    /// so they cannot be smuggled through here.
    fn ensure_request_allowed(&self, method: WireMethod) -> Result<(), WireError> {
        match method {
            WireMethod::ToolCall | WireMethod::CommandInvoke => {}
            WireMethod::Initialize | WireMethod::Cancel | WireMethod::Shutdown | WireMethod::ObserverNotify => {
                return Err(WireError::ControlMethod(method));
            }
        }
        if self.is_closed() {
            return Err(WireError::Closed);
        }
        match self.negotiated() {
            Some(negotiated) => match method.capability() {
                Some(capability) if !negotiated.capabilities.contains(&capability) => {
                    Err(WireError::CapabilityUnavailable(method))
                }
                _ => Ok(()),
            },
            None => Err(WireError::NotInitialized),
        }
    }

    fn send(&self, frame: &impl Serialize, deadline: Option<Instant>) -> Result<(), WireError> {
        if self.is_closed() {
            return Err(WireError::Closed);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(WireError::WriteTimeout);
        }
        let bytes = encode_frame(frame)?;
        let sender = self.writer.lock().map_err(|_| WireError::Closed)?;
        let sender = sender.as_ref().ok_or(WireError::Shutdown)?;
        sender
            .try_send(QueuedFrame { bytes, deadline })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => WireError::QueueFull,
                mpsc::error::TrySendError::Closed(_) => WireError::Closed,
            })
    }

    fn next_id(&self) -> Result<u64, WireError> {
        self.ids.allocate().ok_or(WireError::IdsExhausted)
    }

    /// Mark the connection finished and wake everything still waiting.
    ///
    /// Each waiting request receives [`WireError::Closed`] through its own
    /// channel, so a waiter tells a lost connection from a cancellation.
    fn finish(&self) {
        self.closed.send_replace(true);
        if self.drained.swap(true, Ordering::SeqCst) {
            // A second end (EOF after shutdown, say) has nothing left to drain.
            return;
        }
        *self.lock_lifecycle() = Lifecycle::Finished;
        let drained: Vec<PendingRequest> = self.lock_pending().drain().map(|(_id, request)| request).collect();
        if !drained.is_empty() {
            crate::logging::debug(&format!(
                "extension wire: closed with {} request(s) still waiting",
                drained.len()
            ));
        }
        for request in drained {
            match request.outcome.send(Err(WireError::Closed)) {
                Ok(()) => {}
                Err(_gone) => {
                    // The waiter already timed out; the id stays spent.
                }
            }
        }
        if let Ok(mut writer) = self.writer.lock() {
            writer.take();
        }
        if let Ok(mut sender) = self.observation_sender.lock() {
            sender.take();
        }
    }

    fn lock_pending(&self) -> std::sync::MutexGuard<'_, HashMap<u64, PendingRequest>> {
        self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_lifecycle(&self) -> std::sync::MutexGuard<'_, Lifecycle> {
        self.lifecycle.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn encode_params(params: &impl Serialize) -> Result<Value, WireError> {
    serde_json::to_value(params)
        .map_err(|_| FrameError::InvalidParams)
        .map_err(WireError::Frame)
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, WireError> {
    serde_json::from_value(value)
        .map_err(|_| FrameError::InvalidParams)
        .map_err(WireError::Frame)
}

#[cfg(test)]
#[path = "wire/tests.rs"]
mod tests;
