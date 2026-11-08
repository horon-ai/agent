//! Per-session daemon connection with one background reader (the pump).
//!
//! Only the pump reads the daemon socket. Every event goes to exactly one
//! place: the waiter registered for its request id, the foreground stream of
//! the running prompt (or attach replay), or the session's side channel.
//! Sends to all three are unbounded channel sends, so the pump never waits on
//! a consumer or a client round trip.

use super::*;
use crate::protocol::TurnStopReason;
use routes::*;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex as StdMutex;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

mod routes;

pub(super) const DAEMON_DISCONNECTED: &str = "HoronAgent daemon disconnected";

/// How long a prompt waits for the previous turn's cancel tail before it writes
/// its own message.
///
/// The daemon rejects a message for a session that is still processing
/// ("Already processing a message", crates/app-core/src/server/client_lifecycle.rs:3872-3878),
/// and its cancel handler waits up to 500 ms for a cooperative stop and up to
/// 2 s for an aborted task before it clears that flag and emits `Interrupted`
/// and the turn's `Done`. The tail's `Done` therefore proves the session is free
/// again, and this bound only stops a daemon that never sends it from delaying
/// the next prompt for long.
pub(super) const TURN_TAIL_WAIT: Duration = Duration::from_millis(3000);

/// The idle gate a prompt runs after a cancelled turn, before it writes its own
/// message (see [`DaemonSession::settle_cancelled_turn`]).
///
/// At most `IDLE_PROBES` `GetState` probes, all inside `IDLE_GATE_BOUND`. A
/// probe whose reply does not arrive inside the bound counts as busy: the
/// daemon answers `GetState` from the same request loop that runs a cancel
/// (crates/app-core/src/server/client_lifecycle.rs:1011-1040, :1656-1670), so
/// a silent probe means the cancel is still running. A busy reply is retried
/// once the released turn's drain ends or after `IDLE_PROBE_GAP`, whichever
/// comes first; nothing waits while the daemon reports idle.
const IDLE_PROBES: usize = 6;
const IDLE_PROBE_GAP: Duration = Duration::from_millis(50);
pub(super) const IDLE_GATE_BOUND: Duration = Duration::from_millis(2000);

/// How long a released turn keeps its successor's stream clean.
///
/// A turn that answered before its own terminal reply is still streaming on
/// the daemon connection: cancelling a turn publishes `TurnStopped` before the
/// worker is signalled, and `Interrupted` plus the turn's `Done` follow once
/// the worker has ended (crates/app-core/src/server/client_lifecycle.rs). The
/// bound keeps a daemon that never sends that terminal from blinding the
/// session for good, and matches the wait a cancelled prompt itself gets.
pub(super) const STALE_TURN_BOUND: Duration = PROMPT_CANCEL_BOUND;

/// The bottom of the id range ACP v2 `message` requests use.
///
/// The daemon fans every turn of a session out to every attached connection,
/// terminal `Done`/`Error` included, and those carry the request id of the
/// connection that started the turn. Every connection counts its own ids up
/// from a small number, so a v2 prompt taking its id from that counter could be
/// failed by another connection's `Error` with the same number. Ids from this
/// range never meet one of those counters.
pub(super) const V2_MESSAGE_ID_BASE: u64 = 1 << 40;

/// Where this process starts its v2 message ids.
///
/// Two adapters attached to one session must not allocate the same id, because
/// another connection's `Error`/`Done` for its own request reaches this one
/// too. The full process id gives each process its own slice of the range: a
/// process's slice begins at `pid << ID_SALT_BITS`, so slices never overlap
/// even when pids differ by a multiple of 2^16 (the collision the old
/// `pid & 0xFFFF` slice allowed). The per-process salt lands in the lower half
/// of the slice, which moves a reused pid away from the ids its previous holder
/// used and still leaves 2^19 ids before the next pid's slice.
pub(super) fn v2_message_id_start() -> u64 {
    v2_message_id_start_from(std::process::id(), process_salt())
}

/// Bits of a process's id slice reserved for its start salt.
const ID_SALT_BITS: u32 = 20;
pub(super) const ID_SALT_MASK: u64 = (1 << ID_SALT_BITS) - 1;

/// The start of process `pid`'s slice; only the lower half of the slice takes
/// `salt`, so the ids counted up from the start stay inside the slice.
pub(super) fn v2_message_id_start_from(pid: u32, salt: u64) -> u64 {
    V2_MESSAGE_ID_BASE + ((u64::from(pid) << ID_SALT_BITS) | (salt & (ID_SALT_MASK >> 1)))
}

/// A per-process value that is unlikely to repeat: the wall clock at this
/// process's start, mixed with the pid and an address, hashed by `splitmix64`.
fn process_salt() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos() as u64);
    let stack = &nanos as *const u64 as u64;
    splitmix64(nanos ^ (u64::from(std::process::id()) << 32) ^ stack)
}

/// The finalizer of SplitMix64 (`seed ++ 0x9E3779B97F4A7C15`, two mixes).
fn splitmix64(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

pub(super) type SideFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type EventTx = mpsc::UnboundedSender<Routed>;
type EventRx = mpsc::UnboundedReceiver<Routed>;

/// One item on a session stream: a daemon event, or an update a side-channel
/// handler needs written in order with the running prompt's own updates.
pub(super) enum Routed {
    Daemon(Box<ServerEvent>),
    Local(LocalUpdate),
}

/// Updates the side channel hands to the running prompt's ordered stream.
pub(super) enum LocalUpdate {
    /// The client created a terminal for a tool call. `written` resolves once
    /// the prompt has written the `tool_call_update` that embeds it; it is
    /// dropped unanswered when no prompt consumed the update.
    TerminalAttached {
        tool_call_id: String,
        terminal_id: String,
        written: tokio::sync::oneshot::Sender<()>,
    },
}

/// Writer half of a daemon connection plus its request id allocator, shared
/// by the session handlers and side-channel handlers.
pub(super) struct DaemonLink {
    writer: Mutex<WriteHalf>,
    next_request_id: AtomicU64,
    /// Ids of v2 `message` requests, from [`V2_MESSAGE_ID_BASE`] up.
    next_message_id: AtomicU64,
}

impl DaemonLink {
    pub(super) fn next_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::Relaxed)
    }

    pub(super) fn next_message_id(&self) -> u64 {
        self.next_message_id.fetch_add(1, Ordering::Relaxed)
    }

    pub(super) async fn send(&self, request: &Request) -> Result<()> {
        let mut json = serde_json::to_string(request)?;
        json.push('\n');
        let mut writer = self.writer.lock().await;
        writer.write_all(json.as_bytes()).await?;
        writer.flush().await?;
        Ok(())
    }
}

/// Handles daemon events that arrive outside any request/response exchange.
/// Later todos install handlers that bridge these to ACP client requests.
pub(super) trait SideChannel: Send + Sync {
    fn handle(&self, link: Arc<DaemonLink>, event: ServerEvent) -> SideFuture;
}

pub(super) struct DefaultSideChannel;

impl SideChannel for DefaultSideChannel {
    fn handle(&self, link: Arc<DaemonLink>, event: ServerEvent) -> SideFuture {
        match event {
            ServerEvent::ClientRequest { request_id, method, .. } => Box::pin(async move {
                let response = Request::ClientResponse {
                    id: link.next_id(),
                    request_id,
                    result: None,
                    error: Some(crate::protocol::ClientRpcError {
                        code: JSONRPC_METHOD_NOT_FOUND,
                        message: "client capability not wired".to_string(),
                    }),
                };
                if let Err(err) = link.send(&response).await {
                    crate::logging::warn(&format!("ACP: failed to answer client_request {method}: {err:#}"));
                }
            }),
            // A session with no ACP side channel installed has no editor to ask.
            ServerEvent::ToolPermissionRequested {
                request_id, tool_name, ..
            } => {
                crate::logging::warn(&format!(
                    "ACP: skipping tool_permission_requested {request_id} for {tool_name}: permission bridge not wired"
                ));
                Box::pin(async {})
            }
            ServerEvent::ToolPermissionResolved { request_id, .. } => {
                crate::logging::info(&format!(
                    "ACP: skipping tool_permission_resolved {request_id}: permission bridge not wired"
                ));
                Box::pin(async {})
            }
            _ => Box::pin(async {}),
        }
    }
}

/// What one `GetState` probe learned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Probe {
    Idle,
    /// The daemon reported `is_processing`, or the probe could not be written.
    Busy,
    /// No reply inside the gate's bound: the daemon is still busy with the
    /// cancel, and the bound is spent.
    Silent,
}

/// How the daemon ends a turn a consumer released early.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StaleTail {
    /// One `Done`/`Error` for the turn's request.
    #[cfg(test)]
    Terminal,
    /// The tail of a turn the daemon interrupted: the worker may send its own
    /// `Done` first, and the cancel path closes the turn with `Interrupted`
    /// and then its `Done` (crates/app-core/src/server/client_lifecycle.rs:
    /// :4010-4011, :4132-4133), so only a terminal after that pair ends the
    /// drain.
    AfterInterrupt,
    /// A turn the prompt answered on a `TurnStopped` after its cancel was
    /// recorded: the cancel's own `Interrupted`, a peer connection's interrupt
    /// (client_lifecycle.rs:4166-4174, whose `Interrupted` goes to the peer
    /// only), a worker failure (:3984-4011) or a limit
    /// (turn_streaming_mpsc.rs:1652, :2349). The worker - or the cancel path,
    /// when it aborted the worker - still sends one `Done`/`Error` for the turn
    /// id; whether `Interrupted` and a second `Done` follow depends on who
    /// interrupted and whether the worker had already finished. So the drain
    /// ends at that first terminal and then turns into
    /// [`StaleTail::StrayCancelNote`], which drops at most one note plus any
    /// later terminal of the old id and never holds content back.
    WorkerTerminal,
    /// A cancel that found the turn already finished sends no turn events at
    /// all (`SERVER_INTERRUPT_CANCEL_IGNORED_FINISHED`) or a lone `Interrupted`
    /// without a `Done` (`SERVER_INTERRUPT_CANCEL_IDLE_NOOP`). Only that note
    /// is dropped - at most one of them, and never a content frame.
    ///
    /// The daemon queues that note on its ordered per-connection event channel
    /// while it handles the cancel, before it reads the next message
    /// (client_lifecycle.rs:671-679, :4154-4164), so it precedes every event of
    /// the next turn. The drain therefore ends at the first turn content it
    /// sees ([`is_turn_content`]) or at a second note; request replies
    /// (`State`, `Ack`) and the cancel's bookkeeping pass through without
    /// ending it. A `Done`/`Error` that carries the released turn's id can
    /// never be the next turn's, so it is dropped without ending the drain.
    StrayCancelNote,
}

/// A turn a consumer released before its terminal reply arrived. Everything
/// the daemon still sends for it belongs to that turn, never to the next one.
#[derive(Clone, Copy, Debug)]
struct StaleTurn {
    id: u64,
    tail: StaleTail,
    /// A cancel note of this turn was seen (and, for a stray drain, dropped).
    interrupted: bool,
    released: std::time::Instant,
}

#[derive(Default)]
struct Routes {
    waiters: HashMap<u64, EventTx>,
    foreground: Option<(u64, EventTx)>,
    next_foreground_token: u64,
    closed: bool,
    stale: Option<StaleTurn>,
    /// The ordered stream of an ACP v2 session (v2/state.rs): every stream
    /// event no waiter or foreground claims goes here instead of being
    /// dropped, whoever started the turn.
    v2_stream: Option<EventTx>,
    /// Signals that the release of a turn's tail is over, so a waiting prompt
    /// may start its own turn.
    tail_done: Arc<tokio::sync::Notify>,
}

impl Routes {
    /// End the drain and wake any prompt waiting to start behind it.
    fn clear_stale(&mut self) {
        self.stale = None;
        self.tail_done.notify_waiters();
    }

    /// How long a prompt should wait before writing its message while a drain
    /// is armed, or `None` when nothing has to be waited for.
    fn tail_wait(&self) -> Option<Duration> {
        match self.stale?.tail {
            StaleTail::AfterInterrupt | StaleTail::WorkerTerminal => Some(TURN_TAIL_WAIT),
            StaleTail::StrayCancelNote => None,
            #[cfg(test)]
            StaleTail::Terminal => None,
        }
    }
    /// Discard an event that cannot belong to the turn a consumer is waiting
    /// for. True when the event was dropped instead of routed.
    ///
    /// The daemon serializes the turns of one session on one connection, so
    /// while a released turn has not sent its terminal reply every event that
    /// no waiter claims is still that turn's: its trailing `Interrupted`, its
    /// late text or tool frames, and its `Done`/`Error`. The terminal itself is
    /// dropped too, and ends the drain.
    fn drain_stale(&mut self, event: &ServerEvent) -> bool {
        let Some(stale) = self.stale else {
            return false;
        };
        if stale.released.elapsed() >= STALE_TURN_BOUND {
            self.clear_stale();
            return false;
        }
        let cancel_note = matches!(
            event,
            ServerEvent::Interrupted
                | ServerEvent::TurnStopped {
                    reason: TurnStopReason::Interrupted,
                    ..
                }
        );
        let stale_terminal =
            matches!(event, ServerEvent::Done { id } | ServerEvent::Error { id, .. } if *id == stale.id);
        match stale.tail {
            StaleTail::StrayCancelNote => {
                if is_request_reply(event) {
                    return false;
                }
                if stale_terminal {
                    return true;
                }
                if cancel_note && !stale.interrupted {
                    if let Some(stale) = self.stale.as_mut() {
                        stale.interrupted = true;
                    }
                    return true;
                }
                if cancel_note || is_turn_content(event) {
                    self.clear_stale();
                }
                return false;
            }
            StaleTail::WorkerTerminal => {
                if stale_terminal {
                    self.stale = Some(StaleTurn {
                        tail: StaleTail::StrayCancelNote,
                        interrupted: false,
                        ..stale
                    });
                    self.tail_done.notify_waiters();
                }
                return true;
            }
            StaleTail::AfterInterrupt => {}
            #[cfg(test)]
            StaleTail::Terminal => {}
        }
        if cancel_note {
            if let Some(stale) = self.stale.as_mut() {
                stale.interrupted = true;
            }
            return true;
        }
        if stale_terminal {
            let ends_turn = match stale.tail {
                StaleTail::AfterInterrupt => stale.interrupted,
                #[cfg(test)]
                StaleTail::Terminal => true,
                StaleTail::StrayCancelNote | StaleTail::WorkerTerminal => false,
            };
            if ends_turn {
                self.clear_stale();
            }
        }
        true
    }
}

type SharedRoutes = Arc<StdMutex<Routes>>;
type SharedSideChannel = Arc<StdMutex<Arc<dyn SideChannel>>>;

fn lock_routes(routes: &StdMutex<Routes>) -> std::sync::MutexGuard<'_, Routes> {
    routes.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Sends daemon requests whose replies come back by id, for code that does not
/// hold the [`DaemonSession`] (the side channel).
#[derive(Clone)]
pub(super) struct DaemonRequester {
    link: Arc<DaemonLink>,
    routes: SharedRoutes,
}

impl DaemonRequester {
    pub(super) fn next_id(&self) -> u64 {
        self.link.next_id()
    }

    pub(super) async fn request(&self, id: u64, request: &Request) -> Result<EventStream> {
        let reply = expect_reply(&self.routes, id);
        self.link.send(request).await?;
        Ok(reply)
    }

    /// Queue `update` behind the daemon events already on the foreground
    /// stream. Gives it back when no stream holds the foreground.
    pub(super) fn send_to_foreground(&self, update: LocalUpdate) -> std::result::Result<(), LocalUpdate> {
        let routes = lock_routes(&self.routes);
        let Some((_, foreground)) = &routes.foreground else {
            return Err(update);
        };
        foreground
            .send(Routed::Local(update))
            .map_err(|mpsc::error::SendError(unsent)| match unsent {
                Routed::Local(update) => update,
                Routed::Daemon(_) => unreachable!("only a local update was sent"),
            })
    }
}

fn expect_reply(routes: &SharedRoutes, id: u64) -> EventStream {
    let (tx, rx) = mpsc::unbounded_channel();
    {
        let mut routes = lock_routes(routes);
        if !routes.closed {
            routes.waiters.insert(id, tx);
        }
    }
    EventStream {
        rx,
        routes: Arc::clone(routes),
        reply_id: Some(id),
        foreground_token: None,
        released: false,
    }
}

pub(super) struct DaemonSession {
    pub(super) session_id: String,
    link: Arc<DaemonLink>,
    routes: SharedRoutes,
    side_channel: SharedSideChannel,
    pub(super) prompt_running: AtomicBool,
    /// Set when this session's last turn was cancelled or interrupted, so the
    /// next prompt runs the idle gate before it writes its message. Every other
    /// prompt skips the gate.
    settle_pending: AtomicBool,
    pub(super) ui_state: Arc<Mutex<SessionUiState>>,
    /// The mode this connection set for the session. Re-sent when the session
    /// is loaded again on the same connection.
    pub(super) mode: Arc<Mutex<AcpMode>>,
    pub(super) permissions: Arc<PermissionState>,
    pub(super) client_calls: Arc<ClientCalls>,
    /// The absolute cwd the client gave at session/new, load or resume. Shared
    /// with the side channel, whose v2 command permission subject names it.
    pub(super) cwd: Arc<Mutex<Option<PathBuf>>>,
    /// Prompt and foreground state of an ACP v2 session; unset on v1.
    pub(super) v2: std::sync::OnceLock<Arc<v2::state::V2Session>>,
    /// The tool calls this connection advertises `capabilities.cancel` for.
    cancelable: Arc<v2::cancel::CancelRegistry>,
    tasks: [JoinHandle<()>; 2],
    #[cfg(test)]
    send_log: StdMutex<Vec<SendRecord>>,
}

/// What the routes held at the moment a request was written to the daemon.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SendRecord {
    pub(super) id: u64,
    pub(super) waiter_registered: bool,
    pub(super) foreground_registered: bool,
}

impl Drop for DaemonSession {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl DaemonSession {
    pub(super) fn new(session_id: String, reader: ReadHalf, writer: WriteHalf, next_request_id: u64) -> Self {
        let link = Arc::new(DaemonLink {
            writer: Mutex::new(writer),
            next_request_id: AtomicU64::new(next_request_id),
            next_message_id: AtomicU64::new(v2_message_id_start()),
        });
        let cancelable = Arc::new(v2::cancel::CancelRegistry::default());
        let routes: SharedRoutes = Arc::default();
        let side_channel: SharedSideChannel = Arc::new(StdMutex::new(Arc::new(DefaultSideChannel)));
        let (side_tx, side_rx) = mpsc::unbounded_channel();
        let pump = tokio::spawn(pump_events(
            reader,
            Arc::clone(&routes),
            side_tx,
            Arc::clone(&cancelable),
        ));
        let side = tokio::spawn(run_side_channel(side_rx, Arc::clone(&side_channel), Arc::clone(&link)));
        Self {
            session_id,
            link,
            routes,
            side_channel,
            prompt_running: AtomicBool::new(false),
            settle_pending: AtomicBool::new(false),
            ui_state: Arc::default(),
            mode: Arc::default(),
            permissions: Arc::default(),
            client_calls: Arc::default(),
            cwd: Arc::default(),
            cancelable,
            v2: std::sync::OnceLock::new(),
            tasks: [pump, side],
            #[cfg(test)]
            send_log: StdMutex::default(),
        }
    }

    pub(super) fn next_id(&self) -> u64 {
        self.link.next_id()
    }

    /// The tool calls this connection advertises `capabilities.cancel` for.
    pub(super) fn cancel_registry(&self) -> Arc<v2::cancel::CancelRegistry> {
        Arc::clone(&self.cancelable)
    }

    /// The id of the next v2 `message` request (see [`V2_MESSAGE_ID_BASE`]).
    pub(super) fn next_message_id(&self) -> u64 {
        self.link.next_message_id()
    }

    /// Route every stream event nothing else claims to the returned receiver,
    /// for the session's v2 stream task. A closed daemon connection hands back
    /// a receiver that is already at its end.
    pub(super) fn open_v2_stream(&self) -> EventRx {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut routes = lock_routes(&self.routes);
        if !routes.closed {
            routes.v2_stream = Some(tx);
        }
        rx
    }

    pub(super) async fn send(&self, request: &Request) -> Result<()> {
        #[cfg(test)]
        self.record_send(request);
        self.link.send(request).await
    }

    /// Send a request whose replies come back by id: the waiter is registered
    /// first, so a fast reply cannot be dropped.
    pub(super) async fn request(&self, id: u64, request: &Request) -> Result<EventStream> {
        let reply = self.expect_reply(id);
        self.send(request).await?;
        Ok(reply)
    }

    /// Take the foreground, then send `request`. Every event without a
    /// matching waiter comes to the returned stream until it is dropped; with
    /// `reply_id`, replies to that request come here too.
    pub(super) async fn foreground_request(&self, reply_id: Option<u64>, request: &Request) -> Result<EventStream> {
        let events = self.take_foreground(reply_id);
        self.send(request).await?;
        Ok(events)
    }

    /// Record that this session's turn was cancelled or interrupted: the next
    /// prompt runs [`Self::settle_cancelled_turn`] first.
    pub(super) fn mark_cancelled_turn(&self) {
        self.settle_pending.store(true, Ordering::Release);
    }

    /// Before the turn after a cancelled one is written, let the daemon finish
    /// the cancel. A prompt whose previous turn was not cancelled returns at
    /// once and sends nothing.
    ///
    /// The daemon rejects a message for a session that is still winding a turn
    /// down (`Already processing a message`,
    /// crates/app-core/src/server/client_lifecycle.rs:3872-3878), and a cancel
    /// this connection sent may not have been handled yet when the next prompt
    /// arrives. So the gate first waits for the released turn's tail (event
    /// driven, bounded by [`TURN_TAIL_WAIT`]), then asks `GetState` - which
    /// reports the same flag the rejection tests - until it says idle.
    ///
    /// Whole bound: [`TURN_TAIL_WAIT`] plus [`IDLE_GATE_BOUND`]. When the daemon
    /// still reports busy (or stays silent) at the end, the message is written
    /// anyway: the daemon then refuses it with `Already processing a message`,
    /// which the prompt answers as `-32603`, exactly as without the gate.
    pub(super) async fn settle_cancelled_turn(&self) {
        if !self.settle_pending.swap(false, Ordering::AcqRel) {
            return;
        }
        self.wait_for_turn_tail().await;
        if !self.wait_for_idle().await {
            crate::logging::warn(&format!(
                "ACP: session {} still busy after a cancelled turn; writing the next message anyway",
                self.session_id
            ));
        }
    }

    /// Probe `GetState` until the daemon reports the session idle. False when
    /// the probes or the bound ran out first.
    async fn wait_for_idle(&self) -> bool {
        let deadline = tokio::time::Instant::now() + IDLE_GATE_BOUND;
        for attempt in 0..IDLE_PROBES {
            match self.probe_state(deadline).await {
                Probe::Idle => return true,
                Probe::Silent => return false,
                Probe::Busy => {}
            }
            if attempt + 1 == IDLE_PROBES {
                break;
            }
            // Retry only while busy: as soon as the released turn's drain
            // ends, or after the gap, never past the bound.
            let gate = Arc::clone(&lock_routes(&self.routes).tail_done);
            let notified = gate.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let retry_at = deadline.min(tokio::time::Instant::now() + IDLE_PROBE_GAP);
            let _ = tokio::time::timeout_at(retry_at, notified).await;
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
        }
        false
    }

    async fn probe_state(&self, deadline: tokio::time::Instant) -> Probe {
        let id = self.next_id();
        let Ok(mut reply) = self.request(id, &Request::GetState { id }).await else {
            return Probe::Busy;
        };
        loop {
            match tokio::time::timeout_at(deadline, reply.next()).await {
                Err(_) => return Probe::Silent,
                Ok(Err(_)) => return Probe::Busy,
                Ok(Ok(ServerEvent::State {
                    id: reply_id,
                    is_processing,
                    ..
                })) if reply_id == id => {
                    return if is_processing { Probe::Busy } else { Probe::Idle };
                }
                Ok(Ok(_)) => {}
            }
        }
    }

    /// Let a cancelled turn's tail finish before this session's next turn is
    /// written. The tail's own `Done` follows the cancel path clearing the
    /// daemon's processing flag, so it is the first sign the session is free.
    ///
    /// When the tail does not arrive within [`TURN_TAIL_WAIT`] the drain stays
    /// armed: the daemon still owes it (its cancel path always ends with
    /// `Interrupted` and the turn's `Done`, client_lifecycle.rs:4131-4133) and
    /// queues it ahead of anything of the next turn, so it ends at that
    /// terminal or at [`STALE_TURN_BOUND`].
    async fn wait_for_turn_tail(&self) {
        loop {
            let (gate, bound) = {
                let routes = lock_routes(&self.routes);
                (Arc::clone(&routes.tail_done), routes.tail_wait())
            };
            let Some(bound) = bound else {
                return;
            };
            let notified = gate.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if lock_routes(&self.routes).tail_wait().is_none() {
                return;
            }
            if tokio::time::timeout(bound, notified).await.is_err() {
                return;
            }
        }
    }

    pub(super) fn install_side_channel(&self, handler: Arc<dyn SideChannel>) {
        *self
            .side_channel
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = handler;
    }

    pub(super) fn requester(&self) -> DaemonRequester {
        DaemonRequester {
            link: Arc::clone(&self.link),
            routes: Arc::clone(&self.routes),
        }
    }

    fn expect_reply(&self, id: u64) -> EventStream {
        expect_reply(&self.routes, id)
    }

    fn take_foreground(&self, reply_id: Option<u64>) -> EventStream {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut token = None;
        {
            let mut routes = lock_routes(&self.routes);
            if !routes.closed {
                routes.next_foreground_token += 1;
                let next = routes.next_foreground_token;
                if let Some(id) = reply_id {
                    routes.waiters.insert(id, tx.clone());
                }
                routes.foreground = Some((next, tx));
                token = Some(next);
            }
        }
        EventStream {
            rx,
            routes: Arc::clone(&self.routes),
            reply_id,
            foreground_token: token,
            released: false,
        }
    }

    #[cfg(test)]
    pub(super) fn foreground(&self, reply_id: Option<u64>) -> EventStream {
        self.take_foreground(reply_id)
    }

    #[cfg(test)]
    pub(super) fn has_waiter(&self, id: u64) -> bool {
        lock_routes(&self.routes).waiters.contains_key(&id)
    }

    #[cfg(test)]
    fn record_send(&self, request: &Request) {
        let Some(id) = serde_json::to_value(request)
            .ok()
            .and_then(|value| value.get("id").and_then(Value::as_u64))
        else {
            return;
        };
        let record = {
            let routes = lock_routes(&self.routes);
            SendRecord {
                id,
                waiter_registered: routes.waiters.contains_key(&id),
                foreground_registered: routes.foreground.is_some(),
            }
        };
        self.send_log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(record);
    }

    #[cfg(test)]
    pub(super) fn sent(&self, id: u64) -> Option<SendRecord> {
        self.send_log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .rev()
            .find(|record| record.id == id)
            .copied()
    }
}

pub(super) struct EventStream {
    rx: EventRx,
    routes: SharedRoutes,
    reply_id: Option<u64>,
    foreground_token: Option<u64>,
    released: bool,
}

impl EventStream {
    /// The next daemon event. A local update reaching a stream that does not
    /// write updates is dropped, which tells its sender it was not written.
    pub(super) async fn next(&mut self) -> Result<ServerEvent> {
        loop {
            if let Routed::Daemon(event) = self.next_routed().await? {
                return Ok(*event);
            }
        }
    }

    /// The next item, local updates included, for the prompt that writes them.
    pub(super) async fn next_routed(&mut self) -> Result<Routed> {
        self.rx.recv().await.ok_or_else(|| anyhow::anyhow!(DAEMON_DISCONNECTED))
    }

    /// Release the stream and keep the rest of its turn away from the next
    /// consumer: the daemon is still winding that turn down, so every event it
    /// sends until the turn's own terminal reply is discarded rather than
    /// handed to whoever takes the foreground next. `turn_id` is the daemon's
    /// id for the turn's request, the id its `Done`/`Error` carries.
    ///
    /// Only the pump's own tests release a turn this way: every turn a prompt
    /// answers early was cancelled, which is [`Self::drain_cancelled_turn`].
    #[cfg(test)]
    pub(super) fn drain_turn(mut self, turn_id: u64) {
        self.release(Some((turn_id, StaleTail::Terminal)));
    }

    /// [`Self::drain_turn`] for a turn that was cancelled: the daemon closes it
    /// with `Interrupted` and then its `Done`, after whatever the worker sent,
    /// so a terminal before that pair does not end the drain.
    pub(super) fn drain_cancelled_turn(mut self, turn_id: u64) {
        self.release(Some((turn_id, StaleTail::AfterInterrupt)));
    }

    /// Release a stream whose turn already reported its own terminal, for a
    /// cancel that may still send one `Interrupted` and nothing else. Only that
    /// note is dropped; the next turn's frames are never held back.
    pub(super) fn discard_stray_cancel_note(mut self, turn_id: u64) {
        self.release(Some((turn_id, StaleTail::StrayCancelNote)));
    }

    /// Release a cancelled turn the prompt answered on a `TurnStopped`: drop everything up to the worker's own `Done`/`Error`,
    /// then at most one stray cancel note (see [`StaleTail::WorkerTerminal`]).
    pub(super) fn drain_worker_turn(mut self, turn_id: u64) {
        self.release(Some((turn_id, StaleTail::WorkerTerminal)));
    }

    /// Release the foreground to the session's v2 stream, which must already
    /// be open ([`DaemonSession::open_v2_stream`]), and return what was routed
    /// here but never read, oldest first. Done under the routes lock, so every
    /// later event reaches the v2 stream and none falls between the two.
    pub(super) fn hand_over(mut self) -> Vec<ServerEvent> {
        self.released = true;
        let routes_handle = Arc::clone(&self.routes);
        let mut routes = lock_routes(&routes_handle);
        if let Some(id) = self.reply_id {
            routes.waiters.remove(&id);
        }
        if let Some(token) = self.foreground_token
            && routes.foreground.as_ref().is_some_and(|(current, _)| *current == token)
        {
            routes.foreground = None;
        }
        let mut unread = Vec::new();
        while let Ok(routed) = self.rx.try_recv() {
            if let Routed::Daemon(event) = routed {
                unread.push(*event);
            }
        }
        unread
    }

    fn release(&mut self, stale_turn: Option<(u64, StaleTail)>) {
        if self.released {
            return;
        }
        self.released = true;
        let routes_handle = Arc::clone(&self.routes);
        let mut routes = lock_routes(&routes_handle);
        if let Some(id) = self.reply_id {
            routes.waiters.remove(&id);
        }
        if let Some(token) = self.foreground_token
            && routes.foreground.as_ref().is_some_and(|(current, _)| *current == token)
        {
            routes.foreground = None;
            if let Some((id, tail)) = stale_turn
                && !routes.closed
            {
                routes.stale = Some(StaleTurn {
                    id,
                    tail,
                    interrupted: false,
                    released: std::time::Instant::now(),
                });
                // Events the pump already routed here, but the prompt never
                // read, are the oldest part of the turn's tail: a fast cancel
                // delivers `TurnStopped`, the worker's `Done`, `Interrupted`
                // and the final `Done` back to back. Run them through the
                // drain in arrival order, under the routes lock, before any
                // later event can be routed. A local update is not part of the
                // daemon's tail, so it stays out of the drain.
                while let Ok(event) = self.rx.try_recv() {
                    if let Routed::Daemon(event) = event {
                        routes.drain_stale(&event);
                    }
                }
            } else if routes
                .stale
                .is_some_and(|stale| stale.tail == StaleTail::StrayCancelNote)
            {
                // A whole turn went by after the cancel, and the daemon queues
                // its note ahead of every event of that turn: no note is coming.
                routes.clear_stale();
            }
        }
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.release(None);
    }
}

async fn pump_events(
    reader: ReadHalf,
    routes: SharedRoutes,
    side: mpsc::UnboundedSender<ServerEvent>,
    cancelable: Arc<v2::cancel::CancelRegistry>,
) {
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(err) => {
                crate::logging::warn(&format!("ACP: daemon read failed: {err}"));
                break;
            }
        }
        match serde_json::from_str::<ServerEvent>(&line) {
            Ok(event) => route_event(&routes, &side, &cancelable, event),
            Err(err) => crate::logging::warn(&format!(
                "ACP: skipping undecodable HoronAgent daemon event ({err}): {}",
                crate::logging::truncate_for_log(line.trim_end(), 200)
            )),
        }
    }
    // Daemon EOF: dropping every sender ends each waiting stream with
    // "daemon disconnected" at once. The tail a cancelled turn still owed can
    // never arrive on a closed connection either, so a prompt waiting behind
    // it stops waiting instead of spending its tail bound.
    let mut routes = lock_routes(&routes);
    routes.closed = true;
    routes.waiters.clear();
    routes.foreground = None;
    routes.v2_stream = None;
    routes.stale = None;
    routes.tail_done.notify_waiters();
}

fn route_event(
    routes: &StdMutex<Routes>,
    side: &mpsc::UnboundedSender<ServerEvent>,
    cancelable: &v2::cancel::CancelRegistry,
    event: ServerEvent,
) {
    track_cancelable(cancelable, &event);
    if bypasses_prompt_stream(&event) {
        let _ = side.send(event);
        return;
    }
    let mut routes = lock_routes(routes);
    let mut event = event;
    if let Some(id) = reply_id(&event)
        && let Some(waiter) = routes.waiters.get(&id)
    {
        match waiter.send(Routed::Daemon(Box::new(event))) {
            Ok(()) => return,
            Err(mpsc::error::SendError(unsent)) => {
                routes.waiters.remove(&id);
                event = unrouted(unsent);
            }
        }
    }
    if routes.drain_stale(&event) {
        return;
    }
    if let Some((_, foreground)) = &routes.foreground {
        match foreground.send(Routed::Daemon(Box::new(event))) {
            Ok(()) => return,
            Err(mpsc::error::SendError(unsent)) => {
                routes.foreground = None;
                event = unrouted(unsent);
            }
        }
    }
    // A v2 session reports every turn, so its stream events reach the v2
    // stream rather than being dropped. A model change made outside any request
    // stays with the side channel, which owns the selector update.
    if belongs_to_v2_stream(&event)
        && let Some(stream) = &routes.v2_stream
    {
        match stream.send(Routed::Daemon(Box::new(event))) {
            Ok(()) => return,
            Err(mpsc::error::SendError(unsent)) => {
                routes.v2_stream = None;
                event = unrouted(unsent);
            }
        }
    }
    drop(routes);
    if is_side_event(&event) {
        let _ = side.send(event);
    }
}

/// The daemon event a failed send handed back.
fn unrouted(routed: Routed) -> ServerEvent {
    match routed {
        Routed::Daemon(event) => *event,
        Routed::Local(_) => unreachable!("the pump only routes daemon events"),
    }
}

/// Move the connection's advertised cancellable calls with `event`.
///
/// The mapper writes the update that advertises `capabilities.cancel` for a
/// call at its `ToolExec`, and a final update for it at its `ToolDone` or
/// `ToolCancelled`, so the registry follows the same events. A turn reporting
/// its end clears the rest: nothing of a finished turn is cancellable.
fn track_cancelable(cancelable: &v2::cancel::CancelRegistry, event: &ServerEvent) {
    match event {
        ServerEvent::ToolExec { id, .. } => cancelable.advertise(id),
        ServerEvent::ToolDone { id, .. } | ServerEvent::ToolCancelled { id } => cancelable.withdraw(id),
        ServerEvent::ForegroundState {
            running: true, run_id, ..
        } => cancelable.run_started(run_id),
        ServerEvent::ForegroundState {
            running: false, run_id, ..
        } => cancelable.run_ended(run_id),
        _ => {}
    }
}

async fn run_side_channel(
    mut events: mpsc::UnboundedReceiver<ServerEvent>,
    handler: SharedSideChannel,
    link: Arc<DaemonLink>,
) {
    while let Some(event) = events.recv().await {
        let current = Arc::clone(&*handler.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        let round_trip = needs_client_round_trip(&event);
        let work = current.handle(Arc::clone(&link), event);
        if round_trip {
            tokio::spawn(work);
        } else {
            work.await;
        }
    }
}

pub(super) async fn wait_for_done(reply: &mut EventStream, request_id: u64) -> Result<()> {
    loop {
        match reply.next().await? {
            ServerEvent::Done { id } if id == request_id => return Ok(()),
            ServerEvent::Error { id, message, .. } if id == request_id => anyhow::bail!(message),
            _ => {}
        }
    }
}

/// Resolve on the first terminal reply for `request_id`: `Ack` (the daemon
/// took the request without a completion event), `Done`, or `Error`.
///
/// Session configuration requests split on this: the daemon answers
/// `set_session_approval_mode` with `Ack` and `configure_session_mcp` with
/// `Done`, so waiting for `Done` alone would sit on an `Ack` that already
/// settled the request.
pub(super) async fn wait_for_accepted(reply: &mut EventStream, request_id: u64) -> Result<()> {
    loop {
        match reply.next().await? {
            ServerEvent::Ack { id } | ServerEvent::Done { id } if id == request_id => return Ok(()),
            ServerEvent::Error { id, message, .. } if id == request_id => anyhow::bail!(message),
            _ => {}
        }
    }
}

pub(super) async fn request_history(session: &DaemonSession) -> Result<ServerEvent> {
    let id = session.next_id();
    let mut reply = session.request(id, &Request::GetHistory { id }).await?;
    wait_for_history(&mut reply, id).await
}

pub(super) async fn request_model_catalog(session: &DaemonSession) -> Result<ServerEvent> {
    let id = session.next_id();
    let request = Request::GetModelCatalog {
        id,
        subscribe_usage_updates: false,
    };
    let mut reply = session.request(id, &request).await?;
    wait_for_history(&mut reply, id).await
}

async fn wait_for_history(reply: &mut EventStream, request_id: u64) -> Result<ServerEvent> {
    loop {
        match reply.next().await? {
            event @ ServerEvent::History { id, .. } if id == request_id => return Ok(event),
            ServerEvent::Error { id, message, .. } if id == request_id => anyhow::bail!(message),
            _ => {}
        }
    }
}

pub(super) async fn apply_model(session: &DaemonSession, model: String) -> Result<()> {
    let request_id = session.next_id();
    let mut reply = session
        .request(request_id, &Request::SetModel { id: request_id, model })
        .await?;
    loop {
        match reply.next().await? {
            ServerEvent::ModelChanged {
                id,
                model,
                provider_name,
                error,
                ..
            } if id == request_id => {
                if let Some(error) = error {
                    anyhow::bail!(error);
                }
                let mut state = session.ui_state.lock().await;
                state.model = Some(model);
                if provider_name.is_some() {
                    state.provider_name = provider_name;
                }
                return Ok(());
            }
            ServerEvent::Error { id, message, .. } if id == request_id => anyhow::bail!(message),
            _ => {}
        }
    }
}

pub(super) async fn apply_effort(session: &DaemonSession, effort: String) -> Result<()> {
    let request_id = session.next_id();
    let request = Request::SetReasoningEffort {
        id: request_id,
        effort,
        target_session_id: None,
    };
    let mut reply = session.request(request_id, &request).await?;
    loop {
        match reply.next().await? {
            ServerEvent::ReasoningEffortChanged { id, effort, error } if id == request_id => {
                if let Some(error) = error {
                    anyhow::bail!(error);
                }
                session.ui_state.lock().await.reasoning_effort = effort;
                return Ok(());
            }
            ServerEvent::Error { id, message, .. } if id == request_id => anyhow::bail!(message),
            _ => {}
        }
    }
}
