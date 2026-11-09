use crate::bus::FileOp;
use crate::plan::VersionedPlan;
use crate::protocol::ServerEvent;
use horon_agent_agent_runtime::{InterruptSignal, SoftInterruptMessage, SoftInterruptQueue, SoftInterruptSource};
use horon_agent_swarm_core::{SwarmLifecycleStatus, SwarmMemberRecord, SwarmRole};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex as StdMutex, RwLock as StdRwLock};
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, mpsc};

/// Process-global registry mapping session id -> background-tool signal.
///
/// The background-tool ("move tool to background", Alt+B/Ctrl+B) signal lives on
/// the `Agent`, so a `SessionControlHandle` can normally only obtain it by
/// locking the agent mutex. When a turn is busy (e.g. running `await_members`),
/// `refresh_session_control_handle` falls back to a lock-free `cancel_only`
/// handle that historically dropped the background signal entirely, which made
/// Alt+B/Ctrl+B silently no-op (`BACKGROUND_TOOL_SIGNAL_FIRE result=no_signal_handle`).
///
/// This registry is populated every time a full `SessionControlHandle` is built
/// (which always has both the session id and the correct signal), so the
/// lock-free fallback can still fire the background signal without the agent
/// lock. Entries are keyed by session id; renames/removals reuse
/// [`rename_background_tool_signal`]/[`remove_background_tool_signal`] alongside
/// the existing shutdown-signal lifecycle.
static BACKGROUND_TOOL_SIGNALS: LazyLock<StdMutex<HashMap<String, InterruptSignal>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

/// Register (or replace) the background-tool signal for a session.
pub(super) fn register_background_tool_signal(session_id: &str, signal: InterruptSignal) {
    if let Ok(mut map) = BACKGROUND_TOOL_SIGNALS.lock() {
        map.insert(session_id.to_string(), signal);
    }
}

/// Look up the registered background-tool signal for a session, if any.
pub(super) fn background_tool_signal_for_session(session_id: &str) -> Option<InterruptSignal> {
    BACKGROUND_TOOL_SIGNALS
        .lock()
        .ok()
        .and_then(|map| map.get(session_id).cloned())
}

/// Move a session's background-tool signal registration to a new session id.
pub(super) fn rename_background_tool_signal(old_session_id: &str, new_session_id: &str) {
    if old_session_id == new_session_id {
        return;
    }
    if let Ok(mut map) = BACKGROUND_TOOL_SIGNALS.lock()
        && let Some(signal) = map.remove(old_session_id)
    {
        map.insert(new_session_id.to_string(), signal);
    }
}

/// Drop a session's background-tool signal registration.
pub(super) fn remove_background_tool_signal(session_id: &str) {
    if let Ok(mut map) = BACKGROUND_TOOL_SIGNALS.lock() {
        map.remove(session_id);
    }
}

/// Process-global session event sinks: session id -> the fanout sender that
/// reaches every live attachment of that session.
///
/// Tools and blocking turn loops have no event sender of their own, and adding
/// one to `ToolContext` is forbidden, so approval and failover paths reach
/// clients through this registry instead. Both fanout factories register their
/// channel here, which is what makes every existing caller of them reachable
/// from [`emit_session_event`] — including a turn started by a connected
/// client, whose ordered stream is built with the fallback factory.
///
/// A `std` lock, not `tokio`'s: registration happens inside the synchronous
/// fanout factory, and an async lock there would force registration onto a
/// spawned task that an immediate emit could outrun. Both critical sections
/// are short and never await.
static SESSION_EVENT_SINKS: LazyLock<StdRwLock<HashMap<String, mpsc::UnboundedSender<ServerEvent>>>> =
    LazyLock::new(|| StdRwLock::new(HashMap::new()));

/// Sessions with at least one live client attachment, mapped to the
/// connection ids attached to them.
///
/// A per-session set rather than a bare set of session ids: detaching one of two
/// windows must not report the session as clientless while the other is still
/// attached.
static ATTACHED_CLIENTS: LazyLock<StdMutex<HashMap<String, HashSet<String>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

/// What happened to an event handed to a session sink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EmitOutcome {
    /// Queued on the session's fanout channel. This is a memory handoff, not a
    /// durability guarantee: the caller keeps its own record until the client
    /// acknowledges the event.
    Delivered,
    /// No sink is registered, so nobody is live for this session right now.
    /// The caller must keep the event and emit it again on the next attach.
    NoSink,
    /// A sink existed but its receiver is gone; the stale entry was pruned.
    Closed,
}

/// Register (or replace) the sink that carries events to a session's clients.
///
/// Registering also re-announces any `FailoverSwitched` this session still owes
/// a `FailoverAck` for, which is what makes a switch that happened while no
/// client was attached survive a restart.
pub(crate) fn register_session_event_sink(session_id: &str, tx: mpsc::UnboundedSender<ServerEvent>) {
    match SESSION_EVENT_SINKS.write() {
        Ok(mut sinks) => {
            sinks.insert(session_id.to_string(), tx);
        }
        Err(poisoned) => {
            // A panic elsewhere must not permanently disable event delivery.
            poisoned.into_inner().insert(session_id.to_string(), tx);
        }
    }
    reemit_pending_failover_events(session_id);
}

/// Drop the sink registered for `session_id`, but only while `retired` is
/// still the channel on file.
///
/// The registry holds the sender, and the sender keeps its fanout task alive, so
/// an entry that is never removed keeps a task and its `swarm_members` handle for
/// the life of the daemon. [`deliver_session_event`] only prunes an entry when a
/// send fails, which cannot happen while that task is alive; session teardown
/// therefore has to drop the entry explicitly.
///
/// `retired` is the channel the registry held when the teardown was decided,
/// which the caller reads before it removes the session entry. The identity
/// check is what keeps a client that attaches in the window between the two
/// steps alive: its sink is a different channel, so it survives and the client
/// still receives approvals. The same rule as the prune in
/// [`deliver_session_event`].
///
/// Idempotent: removing an absent entry, or one a newer registration replaced,
/// is a no-op.
pub(crate) fn unregister_session_event_sink(session_id: &str, retired: &mpsc::UnboundedSender<ServerEvent>) {
    match SESSION_EVENT_SINKS.write() {
        Ok(mut sinks) => {
            if sinks
                .get(session_id)
                .is_some_and(|current| current.same_channel(retired))
            {
                sinks.remove(session_id);
            }
        }
        Err(poisoned) => {
            let mut sinks = poisoned.into_inner();
            if sinks
                .get(session_id)
                .is_some_and(|current| current.same_channel(retired))
            {
                sinks.remove(session_id);
            }
        }
    }
}

/// How many unacknowledged `FailoverSwitched` announcements the file keeps.
///
/// A crash loop that never acks must not grow this file without bound. Dropping
/// the oldest announcement costs a duplicate notification at worst, which is the
/// same cost as losing the file entirely.
const MAX_PENDING_FAILOVER_EVENTS: usize = 100;

/// Attempts to take the failover-announcement lock before giving up.
const FAILOVER_EVENTS_LOCK_ATTEMPTS: u32 = 100;

/// Delay between failover-announcement lock attempts (20ms x 100 = ~2s).
const FAILOVER_EVENTS_LOCK_RETRY_DELAY: Duration = Duration::from_millis(20);

/// One `FailoverSwitched` the daemon emitted and no client has acknowledged.
///
/// The fields are exactly the `FailoverAck` key: an ack clears the entry whose
/// `(session_id, chain_id, from, to, attempt)` matches, so a late ack from an
/// older chain cannot clear a newer announcement.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PendingFailoverEvent {
    pub session_id: String,
    pub chain_id: String,
    pub from: String,
    pub to: String,
    pub attempt: u32,
    pub max: u32,
    pub reason: String,
    pub emitted_at: chrono::DateTime<chrono::Utc>,
}

impl PendingFailoverEvent {
    /// The key an `FailoverAck` must carry to clear this entry.
    fn matches(&self, session_id: &str, chain_id: &str, from: &str, to: &str, attempt: u32) -> bool {
        self.session_id == session_id
            && self.chain_id == chain_id
            && self.from == from
            && self.to == to
            && self.attempt == attempt
    }
}

/// `~/.horon/failover-events.json`, the durable announcement log.
///
/// Sibling of the approvals directory rather than a per-session file: one write
/// per switch, and the session id inside each entry is what scopes a re-emit.
fn failover_events_path() -> anyhow::Result<PathBuf> {
    Ok(crate::storage::horon_agent_dir()?.join("failover-events.json"))
}

fn failover_events_lock_path() -> anyhow::Result<PathBuf> {
    Ok(crate::storage::horon_agent_dir()?.join("failover-events.lock"))
}

/// Run `f` holding the cross-process failover-announcement lock.
///
/// Returns `None` when the lock could not be taken in bounded time. Callers
/// treat that as "the announcement could not be recorded": the switch still
/// proceeds and still emits. This is an announcement log, not an attempt
/// counter, so a lost write costs a duplicate or missing notification, never a
/// refund or an unspent attempt. Do not turn this into a blocking error.
fn with_failover_events_lock<T>(f: impl FnOnce() -> T) -> Option<T> {
    let path = match failover_events_lock_path() {
        Ok(path) => path,
        Err(error) => {
            crate::logging::warn(&format!(
                "failover-events: no lock path ({error}); recording without the lock"
            ));
            return Some(f());
        }
    };
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) => {
            crate::logging::warn(&format!(
                "failover-events: cannot open lock {} ({error}); recording without the lock",
                path.display()
            ));
            return Some(f());
        }
    };
    let mut attempt = 0;
    loop {
        attempt += 1;
        match file.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if attempt < FAILOVER_EVENTS_LOCK_ATTEMPTS => {
                std::thread::sleep(FAILOVER_EVENTS_LOCK_RETRY_DELAY);
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                crate::logging::warn(&format!(
                    "failover-events: lock {} still held after {attempt} attempts; skipping this write",
                    path.display()
                ));
                return None;
            }
            Err(std::fs::TryLockError::Error(error)) => {
                crate::logging::warn(&format!(
                    "failover-events: cannot lock {} ({error}); recording without the lock",
                    path.display()
                ));
                return Some(f());
            }
        }
    }
    let result = f();
    let _ = file.unlock();
    Some(result)
}

/// Read the pending announcements, reporting why a read failed.
///
/// A missing file is `Ok(empty)`: nothing was recorded yet. Any other failure
/// is returned rather than flattened, because a caller that clears an
/// acknowledged announcement must be able to tell "nothing matched" from "the
/// file could not be read".
fn try_read_pending_failover_events() -> anyhow::Result<Vec<PendingFailoverEvent>> {
    let path = failover_events_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    crate::storage::read_json::<Vec<PendingFailoverEvent>>(&path)
        .map_err(|error| anyhow::anyhow!("cannot read {} ({error})", path.display()))
}

/// Read the pending announcements. A missing or malformed file reads as empty.
///
/// Never returns an error and never panics: a torn or hand-edited file must not
/// take down a turn or the server. The next successful append rewrites the file
/// in the current shape, so the damage repairs itself. Callers that must prove
/// an entry is gone use [`try_read_pending_failover_events`] instead.
fn read_pending_failover_events() -> Vec<PendingFailoverEvent> {
    match try_read_pending_failover_events() {
        Ok(entries) => entries,
        Err(error) => {
            crate::logging::warn(&format!("failover-events: {error}; treating it as empty"));
            Vec::new()
        }
    }
}

/// Rewrite the pending announcements.
///
/// Returns the failure instead of only logging it: a caller that clears an
/// acknowledged announcement must not report success when the rewrite did not
/// land, or the client stops retrying an entry that is still on disk.
fn write_pending_failover_events(entries: &[PendingFailoverEvent]) -> anyhow::Result<()> {
    let path = failover_events_path()?;
    crate::storage::write_json(&path, entries)
        .map_err(|error| anyhow::anyhow!("cannot write {} ({error})", path.display()))
}

/// Append one announcement, ignoring an entry with the same ack key.
///
/// The cap drops the oldest entries. Duplicate suppression is by key rather than
/// by value so a re-emitted event (restart, sink re-registration) cannot grow
/// the file.
fn record_pending_failover_event(entry: PendingFailoverEvent) {
    let recorded = with_failover_events_lock(|| -> anyhow::Result<()> {
        let mut entries = read_pending_failover_events();
        if entries.iter().any(|existing| {
            existing.matches(
                &entry.session_id,
                &entry.chain_id,
                &entry.from,
                &entry.to,
                entry.attempt,
            )
        }) {
            return Ok(());
        }
        entries.push(entry);
        if entries.len() > MAX_PENDING_FAILOVER_EVENTS {
            let overflow = entries.len() - MAX_PENDING_FAILOVER_EVENTS;
            entries.drain(0..overflow);
        }
        write_pending_failover_events(&entries)
    });
    // Recording stays best effort: a lost write costs a duplicate notification
    // at worst. Only the ack path has to treat a failed rewrite as a failure,
    // because there a lost write means the entry it reports as cleared survives.
    if let Some(Err(error)) = recorded {
        crate::logging::warn(&format!("failover-events: {error}; the announcement is not durable"));
    }
}

/// Drop the announcement a client just acknowledged.
///
/// The three outcomes are distinct on purpose:
///
/// - `Ok(0)` — the lock was taken, the file was read, and nothing matched. This
///   is success: the entry is already gone (a duplicate ack, or a clear that
///   already landed), so retrying cannot help.
/// - `Ok(n)` — `n` entries matched and the rewrite landed.
/// - `Err(..)` — the lock stayed held, or the read/write failed. The entry may
///   still be on disk, so the caller must not report the ack as applied.
///
/// Only an ack reaches this path: emitting the event alone never clears it, so a
/// clientless session keeps its announcement across a restart.
pub(crate) fn try_clear_acked_failover_event(
    session_id: &str,
    chain_id: &str,
    from: &str,
    to: &str,
    attempt: u32,
) -> anyhow::Result<usize> {
    let removed = with_failover_events_lock(|| -> anyhow::Result<usize> {
        let mut entries = try_read_pending_failover_events()?;
        let before = entries.len();
        entries.retain(|entry| !entry.matches(session_id, chain_id, from, to, attempt));
        let removed = before - entries.len();
        if removed > 0 {
            write_pending_failover_events(&entries)?;
        }
        Ok(removed)
    })
    .ok_or_else(|| {
        anyhow::anyhow!("the failover announcement lock is still held; the acknowledgement was not applied")
    })??;
    Ok(removed)
}

/// Count-only view of [`try_clear_acked_failover_event`] for the durability
/// tests, which assert on how many entries matched. Production code uses the
/// `Result` form: collapsing a failure into `0` is exactly the false success
/// this module now refuses to report. Here a failure panics instead, so a test
/// can never mistake a broken clear for a matching one.
#[cfg(test)]
pub(crate) fn clear_acked_failover_event(
    session_id: &str,
    chain_id: &str,
    from: &str,
    to: &str,
    attempt: u32,
) -> usize {
    try_clear_acked_failover_event(session_id, chain_id, from, to, attempt)
        .expect("the failover announcement lock and file are usable in this test")
}

/// Announce every pending `FailoverSwitched` for `session_id` once.
///
/// Called when a sink is registered. A duplicate delivery is fine: consumers
/// dedupe on the same key an ack uses. The entries are left in place, because
/// only an ack may clear them.
fn reemit_pending_failover_events(session_id: &str) {
    let Some(tx) = session_event_sink(session_id) else {
        return;
    };
    for entry in read_pending_failover_events()
        .into_iter()
        .filter(|entry| entry.session_id == session_id)
    {
        let event = ServerEvent::FailoverSwitched {
            chain_id: entry.chain_id,
            from: entry.from,
            to: entry.to,
            reason: entry.reason,
            attempt: entry.attempt,
            max: entry.max,
        };
        if tx.send(event).is_err() {
            return;
        }
    }
}

/// Hand `event` to `session_id`'s sink.
///
/// Never panics on an unknown session or a closed channel: those are ordinary
/// states of an unattended run, reported through the returned [`EmitOutcome`]
/// so the caller can decide whether to keep the event for a later attach.
/// `Delivered` is a hand-off to the in-memory fanout only, so a caller that needs
/// at-least-once delivery keeps its own pending record until a client acks.
pub(crate) fn emit_session_event(session_id: &str, event: ServerEvent) -> EmitOutcome {
    persist_then_deliver(session_id, event, deliver_session_event)
}

/// Record a `FailoverSwitched` announcement, then deliver it.
///
/// The order is the durability guarantee: a crash between the two steps must
/// leave the announcement on disk, so the next start re-announces it from
/// `failover-events.json`. Reversing them loses the announcement, and this event
/// is how an unattended run tells a later client that its route moved. The
/// delivery step is a parameter so a test can observe the file from inside it.
fn persist_then_deliver(
    session_id: &str,
    event: ServerEvent,
    deliver: impl FnOnce(&str, ServerEvent) -> EmitOutcome,
) -> EmitOutcome {
    if let ServerEvent::FailoverSwitched {
        chain_id,
        from,
        to,
        reason,
        attempt,
        max,
    } = &event
    {
        record_pending_failover_event(PendingFailoverEvent {
            session_id: session_id.to_string(),
            chain_id: chain_id.clone(),
            from: from.clone(),
            to: to.clone(),
            attempt: *attempt,
            max: *max,
            reason: reason.clone(),
            emitted_at: chrono::Utc::now(),
        });
    }
    deliver(session_id, event)
}

fn deliver_session_event(session_id: &str, event: ServerEvent) -> EmitOutcome {
    let Some(tx) = session_event_sink(session_id) else {
        return EmitOutcome::NoSink;
    };
    if tx.send(event).is_ok() {
        return EmitOutcome::Delivered;
    }
    // The receiver is gone. Prune the entry only while it is still the same
    // channel, so a sink registered in the meantime survives.
    if let Ok(mut sinks) = SESSION_EVENT_SINKS.write()
        && sinks.get(session_id).is_some_and(|current| current.same_channel(&tx))
    {
        sinks.remove(session_id);
    }
    EmitOutcome::Closed
}

/// Whether a live client is attached to `session_id` right now.
///
/// `false` is exactly what permits an unattended switch, so it must not be read
/// as "no client will ever connect": one may attach later and then needs the
/// event emitted meanwhile.
pub(crate) fn session_has_attached_client(session_id: &str) -> bool {
    match ATTACHED_CLIENTS.lock() {
        Ok(map) => map.get(session_id).is_some_and(|connections| !connections.is_empty()),
        Err(poisoned) => poisoned
            .into_inner()
            .get(session_id)
            .is_some_and(|connections| !connections.is_empty()),
    }
}

pub(super) fn session_event_sink(session_id: &str) -> Option<mpsc::UnboundedSender<ServerEvent>> {
    match SESSION_EVENT_SINKS.read() {
        Ok(sinks) => sinks.get(session_id).cloned(),
        Err(poisoned) => poisoned.into_inner().get(session_id).cloned(),
    }
}

fn mark_client_attached(session_id: &str, connection_id: &str) {
    let mut map = match ATTACHED_CLIENTS.lock() {
        Ok(map) => map,
        Err(poisoned) => poisoned.into_inner(),
    };
    map.entry(session_id.to_string())
        .or_default()
        .insert(connection_id.to_string());
}

fn mark_client_detached(session_id: &str, connection_id: &str) {
    let mut map = match ATTACHED_CLIENTS.lock() {
        Ok(map) => map,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(connections) = map.get_mut(session_id) {
        connections.remove(connection_id);
        if connections.is_empty() {
            map.remove(session_id);
        }
    }
}

/// Record of a file access by an agent
#[derive(Clone, Debug)]
pub struct FileAccess {
    pub session_id: String,
    pub op: FileOp,
    pub timestamp: Instant,
    pub absolute_time: std::time::SystemTime,
    pub intent: Option<String>,
    pub summary: Option<String>,
    pub detail: Option<String>,
}

pub(super) fn latest_peer_touches(
    accesses: &[FileAccess],
    current_session_id: &str,
    swarm_session_ids: &HashSet<String>,
) -> Vec<FileAccess> {
    let mut latest_by_session: HashMap<&str, &FileAccess> = HashMap::new();

    for access in accesses.iter().filter(|access| {
        access.session_id != current_session_id
            && swarm_session_ids.contains(&access.session_id)
            && access.op.is_modification()
    }) {
        latest_by_session
            .entry(&access.session_id)
            .and_modify(|existing| {
                if access.timestamp > existing.timestamp {
                    *existing = access;
                }
            })
            .or_insert(access);
    }

    let mut latest: Vec<FileAccess> = latest_by_session.into_values().cloned().collect();
    latest.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    latest
}

/// Shared ownership of the core persisted swarm coordination state.
#[derive(Clone)]
pub struct SwarmState {
    pub members: Arc<RwLock<HashMap<String, SwarmMember>>>,
    pub swarms_by_id: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    pub plans: Arc<RwLock<HashMap<String, VersionedPlan>>>,
    pub coordinators: Arc<RwLock<HashMap<String, String>>>,
}

/// First-class snapshot of a single swarm's logical runtime state.
#[derive(Clone, Debug)]
pub struct SwarmRuntime {
    pub swarm_id: String,
    pub coordinator_session_id: Option<String>,
    pub member_session_ids: HashSet<String>,
    pub members: Vec<SwarmMember>,
    pub plan: Option<VersionedPlan>,
}

impl SwarmRuntime {
    pub fn has_any_state(&self) -> bool {
        self.plan.is_some() || self.coordinator_session_id.is_some() || !self.members.is_empty()
    }
}

/// Live transport attachment for a connected session.
#[derive(Clone, Debug)]
pub struct LiveSessionAttachment {
    pub connection_id: String,
    pub event_tx: mpsc::UnboundedSender<ServerEvent>,
}

impl SwarmState {
    pub fn new(
        members: HashMap<String, SwarmMember>,
        swarms_by_id: HashMap<String, HashSet<String>>,
        plans: HashMap<String, VersionedPlan>,
        coordinators: HashMap<String, String>,
    ) -> Self {
        Self {
            members: Arc::new(RwLock::new(members)),
            swarms_by_id: Arc::new(RwLock::new(swarms_by_id)),
            plans: Arc::new(RwLock::new(plans)),
            coordinators: Arc::new(RwLock::new(coordinators)),
        }
    }

    pub async fn load_runtime(&self, swarm_id: &str) -> SwarmRuntime {
        let plan = {
            let plans = self.plans.read().await;
            plans.get(swarm_id).cloned()
        };
        let coordinator_session_id = {
            let coordinators = self.coordinators.read().await;
            coordinators.get(swarm_id).cloned()
        };
        let member_session_ids = {
            let swarms = self.swarms_by_id.read().await;
            swarms.get(swarm_id).cloned().unwrap_or_default()
        };
        let mut members = {
            let members = self.members.read().await;
            members
                .values()
                .filter(|member| member.swarm_id.as_deref() == Some(swarm_id))
                .cloned()
                .collect::<Vec<_>>()
        };
        members.sort_by(|left, right| left.session_id.cmp(&right.session_id));

        SwarmRuntime {
            swarm_id: swarm_id.to_string(),
            coordinator_session_id,
            member_session_ids,
            members,
            plan,
        }
    }
}

/// Information about a session in a swarm
#[derive(Clone, Debug)]
pub struct SwarmMember {
    pub session_id: String,
    /// Primary channel to send events to this session.
    ///
    /// This remains for backward-compatible single-sender call sites and for
    /// headless sessions that do not maintain a live attachment map.
    pub event_tx: mpsc::UnboundedSender<ServerEvent>,
    /// Live client attachments for this session keyed by connection id.
    pub event_txs: HashMap<String, mpsc::UnboundedSender<ServerEvent>>,
    /// Working directory (used to derive swarm id)
    pub working_dir: Option<PathBuf>,
    /// Swarm identifier (shared across worktrees)
    pub swarm_id: Option<String>,
    /// Whether swarm coordination is enabled for this member
    pub swarm_enabled: bool,
    /// Lifecycle status (ready, running, completed, failed, stopped, etc.)
    pub status: String,
    /// Optional detail (current task, error, etc.)
    pub detail: Option<String>,
    /// Stable, human-readable label of the task/role this member was spawned
    /// or assigned for (compacted from the spawn prompt or plan item). Unlike
    /// `detail`, this is not overwritten by transient status updates.
    pub task_label: Option<String>,
    /// Friendly name like "fox"
    pub friendly_name: Option<String>,
    /// Session that should receive direct completion report-back for this member, if any.
    pub report_back_to_session_id: Option<String>,
    /// Latest explicit completion report submitted by this member.
    pub latest_completion_report: Option<String>,
    /// Role: "agent" or "coordinator"
    pub role: String,
    /// When this member joined the swarm
    pub joined_at: Instant,
    /// When status was last changed
    pub last_status_change: Instant,
    /// Whether this is a headless (spawned) session vs a TUI-connected session.
    /// Headless sessions should not be automatically elected as coordinator.
    pub is_headless: bool,
    /// Recent streamed output tail (last few lines of in-progress assistant
    /// text), captured for inline swarm gallery rendering. Updated by the bus
    /// monitor from worker streaming taps; not persisted.
    pub output_tail: Option<String>,
    /// Aggregate todo progress (completed, total) for this member's session,
    /// updated from `TodoUpdated` bus events. Surfaced on the inline swarm
    /// strip; not persisted.
    pub todo_progress: Option<(u32, u32)>,
    /// Compact snapshot of this member's todo list (content + status), capped
    /// at a few entries by the bus monitor. Rendered in the focused inline
    /// swarm panel; not persisted.
    pub todo_items: Vec<crate::protocol::SwarmTodoItem>,
    /// Ephemeral model/timing metadata for the inline swarm card.
    pub runtime: crate::protocol::SwarmMemberRuntime,
}

impl SwarmMember {
    pub fn durable_record(&self) -> SwarmMemberRecord {
        SwarmMemberRecord {
            session_id: self.session_id.clone(),
            working_dir: self.working_dir.clone(),
            swarm_id: self.swarm_id.clone(),
            swarm_enabled: self.swarm_enabled,
            status: SwarmLifecycleStatus::from(self.status.clone()),
            detail: self.detail.clone(),
            task_label: self.task_label.clone(),
            friendly_name: self.friendly_name.clone(),
            report_back_to_session_id: self.report_back_to_session_id.clone(),
            latest_completion_report: self.latest_completion_report.clone(),
            role: SwarmRole::from(self.role.clone()),
            is_headless: self.is_headless,
        }
    }

    pub fn live_attachments(&self) -> Vec<LiveSessionAttachment> {
        self.event_txs
            .iter()
            .map(|(connection_id, event_tx)| LiveSessionAttachment {
                connection_id: connection_id.clone(),
                event_tx: event_tx.clone(),
            })
            .collect()
    }

    pub fn from_record(record: SwarmMemberRecord, event_tx: mpsc::UnboundedSender<ServerEvent>) -> Self {
        Self {
            session_id: record.session_id,
            event_tx,
            event_txs: HashMap::new(),
            working_dir: record.working_dir,
            swarm_id: record.swarm_id,
            swarm_enabled: record.swarm_enabled,
            status: record.status.as_str().into_owned(),
            detail: record.detail,
            task_label: record.task_label,
            friendly_name: record.friendly_name,
            report_back_to_session_id: record.report_back_to_session_id,
            latest_completion_report: record.latest_completion_report,
            role: record.role.as_str().into_owned(),
            joined_at: Instant::now(),
            last_status_change: Instant::now(),
            is_headless: record.is_headless,
            output_tail: None,
            todo_progress: None,
            todo_items: Vec::new(),
            runtime: crate::protocol::SwarmMemberRuntime::default(),
        }
    }
}

/// A shared context entry stored by the server
#[derive(Clone, Debug)]
pub struct SharedContext {
    pub key: String,
    pub value: String,
    pub from_session: String,
    pub from_name: Option<String>,
    /// When this context was created
    pub created_at: Instant,
    /// When this context was last updated
    pub updated_at: Instant,
}

/// Event types for real-time event subscription
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SwarmEventType {
    /// A file was touched (read/write/edit)
    FileTouch {
        path: String,
        op: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        intent: Option<String>,
        summary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// A notification was broadcast
    Notification { notification_type: String, message: String },
    /// A swarm plan was updated
    PlanUpdate { swarm_id: String, item_count: usize },
    /// A plan proposal was submitted
    PlanProposal {
        swarm_id: String,
        proposer_session: String,
        item_count: usize,
    },
    /// Shared context was updated
    ContextUpdate { swarm_id: String, key: String },
    /// Session status changed
    StatusChange { old_status: String, new_status: String },
    /// Session joined/left swarm
    MemberChange {
        action: String, // "joined" or "left"
    },
}

/// A swarm event with metadata
#[derive(Clone, Debug)]
pub struct SwarmEvent {
    pub id: u64,
    pub session_id: String,
    pub session_name: Option<String>,
    pub swarm_id: Option<String>,
    pub event: SwarmEventType,
    pub timestamp: Instant,
    pub absolute_time: std::time::SystemTime,
}

/// Ring buffer for recent swarm events
pub(super) const MAX_EVENT_HISTORY: usize = 5000;

pub(super) type SessionInterruptQueues = Arc<RwLock<HashMap<String, SoftInterruptQueue>>>;

#[cfg(test)]
tokio::task_local! {
    pub(super) static SESSION_REGISTRATION_OBSERVER:
        std::cell::RefCell<Option<tokio::sync::oneshot::Sender<()>>>;
}

pub(super) async fn register_session_event_sender(
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    session_id: &str,
    connection_id: &str,
    event_tx: mpsc::UnboundedSender<ServerEvent>,
) {
    let mut members = swarm_members.write().await;
    if let Some(member) = members.get_mut(session_id) {
        member.event_tx = event_tx.clone();
        member.event_txs.insert(connection_id.to_string(), event_tx);
        #[cfg(test)]
        let _ = SESSION_REGISTRATION_OBSERVER.try_with(|observer| {
            if let Some(sender) = observer.borrow_mut().take() {
                let _ = sender.send(());
            }
        });
    }
    mark_client_attached(session_id, connection_id);
}

pub(super) async fn unregister_session_event_sender(
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    session_id: &str,
    connection_id: &str,
) {
    {
        let mut members = swarm_members.write().await;
        if let Some(member) = members.get_mut(session_id) {
            member.event_txs.remove(connection_id);
            if let Some((_, tx)) = member.event_txs.iter().next() {
                member.event_tx = tx.clone();
            }
        }
    }
    mark_client_detached(session_id, connection_id);
}

/// Deliver `event` to every live attachment of `session_id`.
///
/// This is the single publish funnel: every session event reaches a connection
/// through here (the sink registered by [`emit_session_event`] feeds it, and the
/// callers that fan out directly enter here), so the session's observation owner
/// advances exactly once per publish, before any connection queue takes the
/// event. A snapshot captured afterwards therefore carries a `seq` at least as
/// high as this event's.
pub(super) async fn fanout_session_event(
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    session_id: &str,
    event: ServerEvent,
) -> usize {
    super::session_observation::observe_published(session_id, &event);
    let targets = {
        let mut members = swarm_members.write().await;
        let Some(member) = members.get_mut(session_id) else {
            return 0;
        };

        member.event_txs.retain(|_, tx| !tx.is_closed());

        if member.event_txs.is_empty() {
            vec![member.event_tx.clone()]
        } else {
            if let Some((_, tx)) = member.event_txs.iter().next() {
                member.event_tx = tx.clone();
            }
            member.event_txs.values().cloned().collect::<Vec<_>>()
        }
    };

    let mut delivered = 0;
    for tx in targets {
        if tx.send(event.clone()).is_ok() {
            delivered += 1;
        }
    }
    delivered
}

pub(super) async fn fanout_live_client_event(
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    session_id: &str,
    event: ServerEvent,
) -> usize {
    let targets = {
        let mut members = swarm_members.write().await;
        let Some(member) = members.get_mut(session_id) else {
            return 0;
        };

        member.event_txs.retain(|_, tx| !tx.is_closed());
        member.event_txs.values().cloned().collect::<Vec<_>>()
    };

    let mut delivered = 0;
    for tx in targets {
        if tx.send(event.clone()).is_ok() {
            delivered += 1;
        }
    }
    delivered
}

/// Deliver `event` to every live attachment of `session_id` except the
/// connection that caused it.
pub(super) async fn fanout_peer_client_event(
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    session_id: &str,
    exclude_connection_id: &str,
    event: ServerEvent,
) -> usize {
    super::session_observation::observe_published(session_id, &event);
    let targets = {
        let mut members = swarm_members.write().await;
        let Some(member) = members.get_mut(session_id) else {
            return 0;
        };
        member.event_txs.retain(|_, tx| !tx.is_closed());
        member
            .event_txs
            .iter()
            .filter(|(connection_id, _)| connection_id.as_str() != exclude_connection_id)
            .map(|(_, tx)| tx.clone())
            .collect::<Vec<_>>()
    };
    let mut delivered = 0;
    for tx in targets {
        if tx.send(event.clone()).is_ok() {
            delivered += 1;
        }
    }
    delivered
}

pub(super) fn session_event_fanout_sender(
    session_id: String,
    swarm_members: Arc<RwLock<HashMap<String, SwarmMember>>>,
) -> mpsc::UnboundedSender<ServerEvent> {
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();
    // One registration point: every existing caller of this factory becomes
    // reachable from `emit_session_event` without knowing about the registry.
    register_session_event_sink(&session_id, tx.clone());
    tokio::spawn(async move {
        // This task sees the turn's events in order, so it settles the stop
        // reason of the turn's closing foreground state.
        let mut stop_observer = super::foreground_state::StopObserver::default();
        while let Some(mut event) = rx.recv().await {
            stop_observer.apply(&mut event);
            let idle_run = super::foreground_state::idle_run_id(&event);
            let _ = fanout_session_event(&swarm_members, &session_id, event).await;
            if let Some(run_id) = idle_run {
                super::foreground_state::note_forwarded(&run_id);
            }
        }
    });
    tx
}

pub(super) fn session_event_fanout_sender_with_fallback(
    session_id: String,
    swarm_members: Arc<RwLock<HashMap<String, SwarmMember>>>,
    fallback_tx: mpsc::UnboundedSender<ServerEvent>,
) -> mpsc::UnboundedSender<ServerEvent> {
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();
    // The same single registration point as the plain factory. A client-driven
    // turn is where a connected client's channel is known, and without this the
    // approval and failover paths reach `NoSink` for exactly the sessions that
    // have a client watching. The registered sender fans out to the session's
    // live attachments at send time, so a later turn replacing it, or a client
    // attaching afterwards, still reaches whatever is live then.
    register_session_event_sink(&session_id, tx.clone());
    tokio::spawn(async move {
        let mut stop_observer = super::foreground_state::StopObserver::default();
        while let Some(mut event) = rx.recv().await {
            stop_observer.apply(&mut event);
            let idle_run = super::foreground_state::idle_run_id(&event);
            if fanout_session_event(&swarm_members, &session_id, event.clone()).await == 0 {
                let _ = fallback_tx.send(event);
            }
            if let Some(run_id) = idle_run {
                super::foreground_state::note_forwarded(&run_id);
            }
        }
    });
    tx
}

pub(super) fn enqueue_soft_interrupt(
    queue: &SoftInterruptQueue,
    content: String,
    images: Vec<(String, String)>,
    urgent: bool,
    source: SoftInterruptSource,
) -> bool {
    enqueue_soft_interrupt_with_id(queue, content, images, urgent, source, None)
}

pub(super) fn enqueue_soft_interrupt_with_id(
    queue: &SoftInterruptQueue,
    content: String,
    images: Vec<(String, String)>,
    urgent: bool,
    source: SoftInterruptSource,
    client_id: Option<String>,
) -> bool {
    let content_bytes = content.len();
    let content_chars = content.chars().count();
    if let Ok(mut pending) = queue.lock() {
        let pending_before = pending.len();
        pending.push(SoftInterruptMessage {
            content,
            images,
            urgent,
            source,
            client_id,
            input_id: None,
        });
        crate::logging::info(&format!(
            "SOFT_INTERRUPT_QUEUE_PUSH source={:?} urgent={} content_bytes={} content_chars={} pending_before={} pending_after={}",
            source,
            urgent,
            content_bytes,
            content_chars,
            pending_before,
            pending.len()
        ));
        drop(pending);
        queue.observe();
        true
    } else {
        crate::logging::warn(&format!(
            "SOFT_INTERRUPT_QUEUE_PUSH_FAILED source={:?} urgent={} content_bytes={} content_chars={} reason=queue_lock_poisoned",
            source, urgent, content_bytes, content_chars
        ));
        false
    }
}

/// Lock-free control-plane handles for a live session.
///
/// This intentionally exposes only out-of-band controls that are safe to use
/// while a turn owns the Agent mutex. Stateful operations such as history
/// mutation, model changes, or direct tool execution should continue to
/// coordinate through the Agent lock after the turn is idle/stopped.
#[derive(Clone)]
pub struct SessionControlHandle {
    pub session_id: String,
    soft_interrupt_queue: SoftInterruptQueue,
    background_tool_signal: Option<InterruptSignal>,
    stop_current_turn_signal: InterruptSignal,
}

impl SessionControlHandle {
    pub fn new(
        session_id: impl Into<String>,
        soft_interrupt_queue: SoftInterruptQueue,
        background_tool_signal: InterruptSignal,
        stop_current_turn_signal: InterruptSignal,
    ) -> Self {
        let session_id = session_id.into();
        // Mirror the signal into the process-global registry so the lock-free
        // `cancel_only` fallback (used while the agent mutex is busy, e.g. during
        // `await_members`) can still fire it. Without this, Alt+B/Ctrl+B silently
        // no-ops for busy turns.
        register_background_tool_signal(&session_id, background_tool_signal.clone());
        Self {
            session_id,
            soft_interrupt_queue,
            background_tool_signal: Some(background_tool_signal),
            stop_current_turn_signal,
        }
    }

    pub fn cancel_only(
        session_id: impl Into<String>,
        soft_interrupt_queue: SoftInterruptQueue,
        stop_current_turn_signal: InterruptSignal,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            soft_interrupt_queue,
            background_tool_signal: None,
            stop_current_turn_signal,
        }
    }

    pub fn queue_soft_interrupt(
        &self,
        content: String,
        images: Vec<(String, String)>,
        urgent: bool,
        source: SoftInterruptSource,
    ) -> bool {
        enqueue_soft_interrupt(&self.soft_interrupt_queue, content, images, urgent, source)
    }

    pub fn queue_soft_interrupt_with_id(
        &self,
        content: String,
        images: Vec<(String, String)>,
        urgent: bool,
        source: SoftInterruptSource,
        client_id: Option<String>,
    ) -> bool {
        enqueue_soft_interrupt_with_id(&self.soft_interrupt_queue, content, images, urgent, source, client_id)
    }

    /// Push a message the durable layer already accepted.
    ///
    /// The live queue never mints or drops a journal id: the caller hands over a
    /// message that already carries one, so the queue and the journal name the
    /// same input.
    pub fn enqueue_message(&self, message: SoftInterruptMessage) -> bool {
        enqueue_message(&self.soft_interrupt_queue, message)
    }

    /// Soft interrupts queued and not yet injected.
    pub fn pending_soft_interrupt_count(&self) -> usize {
        self.soft_interrupt_queue.lock().map(|queue| queue.len()).unwrap_or(0)
    }

    /// Remove the queued interrupts whose caller id is in `client_ids`.
    ///
    /// Injection drains the queue under the same lock, so each requested id is
    /// reported exactly once: in `cancelled` if it was still queued, otherwise
    /// in `not_queued` (already injected, or never queued here). Order follows
    /// the request, and duplicate requested ids are reported once.
    pub fn cancel_soft_interrupts_by_id(&self, client_ids: &[String]) -> (Vec<String>, Vec<String>) {
        let mut requested: Vec<String> = Vec::new();
        for id in client_ids {
            if !requested.contains(id) {
                requested.push(id.clone());
            }
        }
        let removed: Vec<String> = match self.soft_interrupt_queue.lock() {
            Ok(mut queue) => {
                let mut removed = Vec::new();
                queue.retain(|message| match &message.client_id {
                    Some(id) if requested.contains(id) => {
                        removed.push(id.clone());
                        false
                    }
                    _ => true,
                });
                removed
            }
            Err(_) => {
                crate::logging::warn(&format!(
                    "SOFT_INTERRUPT_QUEUE_CANCEL_BY_ID_FAILED session={} reason=queue_lock_poisoned",
                    self.session_id
                ));
                Vec::new()
            }
        };
        let (cancelled, not_queued): (Vec<String>, Vec<String>) =
            requested.into_iter().partition(|id| removed.contains(id));
        crate::logging::info(&format!(
            "SOFT_INTERRUPT_QUEUE_CANCEL_BY_ID session={} cancelled={} not_queued={}",
            self.session_id,
            cancelled.len(),
            not_queued.len()
        ));
        (cancelled, not_queued)
    }

    pub fn clear_soft_interrupts(&self) {
        if let Ok(mut queue) = self.soft_interrupt_queue.lock() {
            let cleared = queue.len();
            queue.clear();
            crate::logging::info(&format!(
                "SOFT_INTERRUPT_QUEUE_CLEAR session={} cleared={}",
                self.session_id, cleared
            ));
        } else {
            crate::logging::warn(&format!(
                "SOFT_INTERRUPT_QUEUE_CLEAR_FAILED session={} reason=queue_lock_poisoned",
                self.session_id
            ));
        }
    }

    /// Fire the stop-current-turn signal. Returns the signal's fire epoch so
    /// callers that schedule a deferred [`reset_cancel_if_epoch`](Self::reset_cancel_if_epoch)
    /// can avoid erasing a newer cancel that fired in the meantime (issue #428).
    ///
    /// Also fires every cancel signal registered for currently running turns
    /// of this session. The handle's own signal can be a stale instance that
    /// the streaming turn never observes (reattach after reload/disconnect,
    /// server-initiated turns, headless recovery), which used to make Esc show
    /// "Interrupting..." while the model kept generating for minutes
    /// (issue #428).
    /// Apply a bounded control request under its durable receipt.
    ///
    /// This is the receipt-bearing path: the same `operation_id` with the same
    /// action replays the recorded answer instead of running the action twice,
    /// and an `abort_run` naming a run the session is no longer executing
    /// answers `stale_run` without cancelling the newer one. Callers that have
    /// no operation id keep calling [`request_cancel`](Self::request_cancel) and
    /// [`cancel_soft_interrupts_by_id`](Self::cancel_soft_interrupts_by_id)
    /// directly, which is unchanged.
    pub async fn apply_control(
        &self,
        operation_id: &str,
        action: &super::control_receipts::ControlAction,
    ) -> super::control_receipts::ControlOutcome {
        let effect: Arc<dyn super::control_receipts::ControlEffect> =
            Arc::new(HandleControlEffect { handle: self.clone() });
        super::control_receipts::apply_control(&self.session_id, operation_id, action, effect).await
    }

    pub fn request_cancel(&self) -> u64 {
        crate::logging::info(&format!("SESSION_CANCEL_SIGNAL_FIRE session={}", self.session_id));
        self.stop_current_turn_signal.fire();
        let active_turn_signals = crate::turn_cancel_registry::active_turn_signals(&self.session_id);
        let mut fired_active = 0usize;
        for signal in &active_turn_signals {
            if signal.same_instance(&self.stop_current_turn_signal) {
                continue;
            }
            signal.fire();
            fired_active += 1;
        }
        if fired_active > 0 {
            crate::logging::info(&format!(
                "SESSION_CANCEL_ACTIVE_TURN_SIGNALS_FIRED session={} fired={} registered={}",
                self.session_id,
                fired_active,
                active_turn_signals.len()
            ));
        }
        self.stop_current_turn_signal.epoch()
    }

    pub fn reset_cancel(&self) {
        crate::logging::info(&format!("SESSION_CANCEL_SIGNAL_RESET session={}", self.session_id));
        self.stop_current_turn_signal.reset();
    }

    /// Reset the cancel signal only if no newer cancel fired since `epoch`
    /// was captured from [`request_cancel`](Self::request_cancel). Timed
    /// resets (used when the running turn is not owned by this connection)
    /// must use this instead of [`reset_cancel`](Self::reset_cancel):
    /// an unconditional deferred reset can erase a newer, not-yet-observed
    /// cancel, making repeated Esc presses appear to be ignored (issue #428).
    pub fn reset_cancel_if_epoch(&self, epoch: u64) -> bool {
        let reset = self.stop_current_turn_signal.reset_if_epoch(epoch);
        crate::logging::info(&format!(
            "SESSION_CANCEL_SIGNAL_RESET session={} epoch={} applied={}",
            self.session_id, epoch, reset
        ));
        reset
    }

    pub fn request_background_current_tool(&self) -> bool {
        // Prefer the directly-held signal; fall back to the process-global
        // registry for lock-free (`cancel_only`) handles built while the agent
        // mutex was busy. This is what makes Alt+B/Ctrl+B work during a busy
        // turn such as `await_members`.
        let signal = self
            .background_tool_signal
            .clone()
            .or_else(|| background_tool_signal_for_session(&self.session_id));
        if let Some(signal) = signal {
            signal.fire();
            crate::logging::info(&format!(
                "BACKGROUND_TOOL_SIGNAL_FIRE session={} result=sent",
                self.session_id
            ));
            true
        } else {
            crate::logging::warn(&format!(
                "BACKGROUND_TOOL_SIGNAL_FIRE session={} result=no_signal_handle",
                self.session_id
            ));
            false
        }
    }

    pub fn stop_current_turn_signal(&self) -> InterruptSignal {
        self.stop_current_turn_signal.clone()
    }
}

/// The session-side work a durable control receipt applies.
///
/// Both actions are the ones the plain cancel path already exposes; this only
/// gives them a receipt, so an action here can never do more than a cancel can.
struct HandleControlEffect {
    handle: SessionControlHandle,
}

impl super::control_receipts::ControlEffect for HandleControlEffect {
    fn abort_run(&self, run_id: Option<&str>) -> Result<serde_json::Value, String> {
        let epoch = self.handle.request_cancel();
        Ok(serde_json::json!({ "cancelled_epoch": epoch, "run_id": run_id }))
    }

    fn cancel_inputs(&self, input_ids: &[String]) -> Result<serde_json::Value, String> {
        let (cancelled, not_queued) = self.handle.cancel_soft_interrupts_by_id(input_ids);
        Ok(serde_json::json!({ "cancelled": cancelled, "not_queued": not_queued }))
    }
}

pub(super) async fn register_session_interrupt_queue(
    queues: &SessionInterruptQueues,
    session_id: &str,
    queue: SoftInterruptQueue,
) {
    let mut guard = queues.write().await;
    guard.insert(session_id.to_string(), queue);
}

pub(super) async fn rename_session_interrupt_queue(
    queues: &SessionInterruptQueues,
    old_session_id: &str,
    new_session_id: &str,
) {
    let mut guard = queues.write().await;
    if let Some(queue) = guard.remove(old_session_id) {
        guard.insert(new_session_id.to_string(), queue);
    }
}

pub(super) async fn remove_session_interrupt_queue(queues: &SessionInterruptQueues, session_id: &str) {
    let mut guard = queues.write().await;
    guard.remove(session_id);
}

pub(super) async fn queue_soft_interrupt_for_session(
    session_id: &str,
    content: String,
    urgent: bool,
    source: SoftInterruptSource,
    queues: &SessionInterruptQueues,
    sessions: &super::SessionAgents,
) -> bool {
    queue_soft_interrupt_for_session_with_id(session_id, content, urgent, source, None, queues, sessions).await
}

pub(super) async fn queue_soft_interrupt_for_session_with_id(
    session_id: &str,
    content: String,
    urgent: bool,
    source: SoftInterruptSource,
    client_id: Option<String>,
    queues: &SessionInterruptQueues,
    sessions: &super::SessionAgents,
) -> bool {
    if let Some(queue) = queues.read().await.get(session_id).cloned() {
        return accept_and_enqueue(session_id, &queue, content, Vec::new(), urgent, source, client_id).await;
    }

    let queue = {
        let guard = sessions.read().await;
        guard.get(session_id).and_then(|agent| {
            agent
                .try_lock()
                .ok()
                .map(|agent_guard| agent_guard.soft_interrupt_queue())
        })
    };

    if let Some(queue) = queue {
        register_session_interrupt_queue(queues, session_id, queue.clone()).await;
        return accept_and_enqueue(session_id, &queue, content, Vec::new(), urgent, source, client_id).await;
    }

    let session_exists = {
        let guard = sessions.read().await;
        guard.contains_key(session_id)
    } || crate::session::session_exists(session_id);

    if !session_exists {
        return false;
    }

    let message = SoftInterruptMessage {
        content,
        images: Vec::new(),
        urgent,
        source,
        client_id,
        input_id: None,
    };
    let accepted = match crate::durable_input_runtime::accept_message(session_id, message.clone()).await {
        Ok(accepted) => accepted,
        Err(err) => {
            crate::logging::warn(&format!(
                "Failed to persist deferred soft interrupt for session {}: {}",
                session_id, err
            ));
            return false;
        }
    };
    let _ = accepted;
    true
}

/// Accept a message durably, then push it into the live queue.
///
/// The journal row is synced first: a caller that is told "queued" holds a
/// durable accept, and a journal failure queues nothing. The memory push keeps
/// the same `client_id` and now carries the `input_id` the journal assigned.
async fn accept_and_enqueue(
    session_id: &str,
    queue: &SoftInterruptQueue,
    content: String,
    images: Vec<(String, String)>,
    urgent: bool,
    source: SoftInterruptSource,
    client_id: Option<String>,
) -> bool {
    let message = SoftInterruptMessage {
        content,
        images,
        urgent,
        source,
        client_id,
        input_id: None,
    };
    let accepted = match crate::durable_input_runtime::accept_message(session_id, message).await {
        Ok(accepted) => accepted,
        Err(err) => {
            crate::logging::warn(&format!(
                "SOFT_INTERRUPT_QUEUE_ACCEPT_FAILED session={} source={:?} error={}",
                session_id, source, err
            ));
            return false;
        }
    };
    enqueue_message(queue, accepted)
}

/// How many inputs one session may hold pending at once.
///
/// The queue is the last thing between a client and unbounded memory: a session
/// whose turns are slow can be handed inputs faster than it drains them, and
/// without a ceiling the process grows until it dies. At the ceiling a new
/// input is refused with `false`, which the callers already treat as "not
/// queued", so the client is told instead of the daemon quietly hoarding.
pub(super) const MAX_PENDING_INPUTS: usize = 64;

pub(super) fn enqueue_message(queue: &SoftInterruptQueue, message: SoftInterruptMessage) -> bool {
    let source = message.source;
    let urgent = message.urgent;
    let content_bytes = message.content.len();
    let content_chars = message.content.chars().count();
    let input_id = message.input_id.clone();
    if let Ok(mut pending) = queue.lock() {
        let pending_before = pending.len();
        if pending_before >= MAX_PENDING_INPUTS {
            crate::logging::warn(&format!(
                "SOFT_INTERRUPT_QUEUE_BUSY source={source:?} urgent={urgent} pending={pending_before} ceiling={MAX_PENDING_INPUTS} input_id={}",
                input_id.as_deref().unwrap_or("-")
            ));
            return false;
        }
        pending.push(message);
        crate::logging::info(&format!(
            "SOFT_INTERRUPT_QUEUE_PUSH source={:?} urgent={} content_bytes={} content_chars={} pending_before={} pending_after={} input_id={}",
            source,
            urgent,
            content_bytes,
            content_chars,
            pending_before,
            pending.len(),
            input_id.as_deref().unwrap_or("-")
        ));
        drop(pending);
        queue.observe();
        true
    } else {
        crate::logging::warn(&format!(
            "SOFT_INTERRUPT_QUEUE_PUSH_FAILED source={:?} urgent={} content_bytes={} content_chars={} reason=queue_lock_poisoned",
            source, urgent, content_bytes, content_chars
        ));
        false
    }
}

#[cfg(test)]
#[path = "../agent/turn_failover_durability_tests.rs"]
mod turn_failover_durability_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn approval_pending(request_id: &str) -> ServerEvent {
        ServerEvent::ApprovalPending {
            request_id: request_id.to_string(),
            action: "bash".to_string(),
            expires_at: None,
        }
    }

    #[test]
    fn emit_reaches_a_registered_session_exactly_once() {
        let session_id = "state-test-exactly-once";
        let (tx, mut rx) = mpsc::unbounded_channel();
        register_session_event_sink(session_id, tx);

        assert_eq!(
            emit_session_event(session_id, approval_pending("req-1")),
            EmitOutcome::Delivered
        );

        let delivered = rx.try_recv().expect("the registered sink receives the event");
        let ServerEvent::ApprovalPending { request_id, .. } = delivered else {
            panic!("wrong event type");
        };
        assert_eq!(request_id, "req-1");
        assert!(rx.try_recv().is_err(), "one emit must produce exactly one event");
    }

    #[test]
    fn emit_without_a_sink_reports_no_sink() {
        assert_eq!(
            emit_session_event("state-test-no-sink", approval_pending("req-2")),
            EmitOutcome::NoSink
        );
    }

    #[test]
    fn emit_to_a_dropped_sink_reports_closed_and_prunes_the_entry() {
        let session_id = "state-test-dropped-sink";
        let (tx, rx) = mpsc::unbounded_channel();
        register_session_event_sink(session_id, tx);
        drop(rx);

        assert_eq!(
            emit_session_event(session_id, approval_pending("req-3")),
            EmitOutcome::Closed
        );
        // A stale sink must not poison later emits: the entry was pruned, so the
        // next emit reports the honest state instead of a second Closed.
        assert_eq!(
            emit_session_event(session_id, approval_pending("req-4")),
            EmitOutcome::NoSink
        );
    }

    #[test]
    fn replacing_a_sink_retires_the_previous_channel() {
        let session_id = "state-test-replaced-sink";
        let (first_tx, mut first_rx) = mpsc::unbounded_channel();
        let (second_tx, mut second_rx) = mpsc::unbounded_channel();
        register_session_event_sink(session_id, first_tx);
        register_session_event_sink(session_id, second_tx);

        assert_eq!(
            emit_session_event(session_id, approval_pending("req-5")),
            EmitOutcome::Delivered
        );
        assert!(first_rx.try_recv().is_err(), "the retired channel gets nothing");
        assert!(second_rx.try_recv().is_ok(), "the newest channel receives the event");
    }

    /// Session teardown must drop the registry entry, not just let the turn
    /// end: the registry's own clone keeps the fanout task and its
    /// `swarm_members` handle alive for the life of the daemon, and
    /// [`deliver_session_event`] cannot prune an entry whose receiver is still
    /// open. `rx` stays alive here so a missing unregister is visible as a
    /// still-reachable sink rather than a `Closed` outcome.
    #[test]
    fn unregister_drops_the_sink_and_is_idempotent() {
        let session_id = "state-test-unregistered-sink";
        let (tx, _rx) = mpsc::unbounded_channel();
        register_session_event_sink(session_id, tx.clone());
        assert!(
            session_event_sink(session_id).is_some(),
            "the sink must be reachable before teardown, or this test proves nothing"
        );

        unregister_session_event_sink(session_id, &tx);
        assert!(
            session_event_sink(session_id).is_none(),
            "teardown must remove the registry entry instead of leaving it to keep the fanout alive"
        );

        // Teardown can run twice for one session (a client disconnect racing the
        // daemon's own cleanup), so removing an absent entry must be a no-op.
        unregister_session_event_sink(session_id, &tx);
        assert!(session_event_sink(session_id).is_none());

        assert_eq!(
            deliver_session_event(session_id, approval_pending("req-unregistered")),
            EmitOutcome::NoSink,
            "after teardown the session is unreachable, and the caller must keep the event"
        );
    }

    /// Teardown retires the channel it decided on, not whatever is registered
    /// when it runs. A client that attaches between the session removal and the
    /// unregister would otherwise lose its sink and never see an approval.
    #[test]
    fn unregister_spares_a_sink_registered_after_teardown_was_decided() {
        let session_id = "state-test-unregister-races-attach";
        let (retired_tx, _retired_rx) = mpsc::unbounded_channel();
        register_session_event_sink(session_id, retired_tx.clone());

        // The attachment that lands in the window, after the teardown decision.
        let (live_tx, mut live_rx) = mpsc::unbounded_channel();
        register_session_event_sink(session_id, live_tx);

        unregister_session_event_sink(session_id, &retired_tx);

        assert!(
            session_event_sink(session_id).is_some(),
            "the channel registered after the decision is a live attachment, not the retired one"
        );
        assert_eq!(
            emit_session_event(session_id, approval_pending("req-after-teardown")),
            EmitOutcome::Delivered,
            "the new attachment must still receive approvals"
        );
        assert!(live_rx.try_recv().is_ok(), "the live channel receives the event");

        // Retiring the channel that is actually on file still works.
        let (current_tx, _current_rx) = mpsc::unbounded_channel();
        register_session_event_sink(session_id, current_tx.clone());
        unregister_session_event_sink(session_id, &current_tx);
        assert!(session_event_sink(session_id).is_none());
    }

    fn approval_resolved(request_id: &str) -> ServerEvent {
        ServerEvent::ApprovalResolved {
            request_id: request_id.to_string(),
            approved: true,
            via: "test".to_string(),
            resolved_by: "test".to_string(),
        }
    }

    fn failover_switched(chain_id: &str) -> ServerEvent {
        ServerEvent::FailoverSwitched {
            chain_id: chain_id.to_string(),
            from: "model-a".to_string(),
            to: "model-b".to_string(),
            reason: "429 Too Many Requests".to_string(),
            attempt: 1,
            max: 3,
        }
    }

    /// The member a client attach leaves behind: the session's primary channel
    /// and its live attachment map both point at the connection.
    fn client_member(session_id: &str, tx: &mpsc::UnboundedSender<ServerEvent>) -> SwarmMember {
        SwarmMember {
            session_id: session_id.to_string(),
            event_tx: tx.clone(),
            event_txs: HashMap::from([("conn-1".to_string(), tx.clone())]),
            working_dir: None,
            swarm_id: None,
            swarm_enabled: false,
            status: "ready".to_string(),
            detail: None,
            task_label: None,
            friendly_name: None,
            report_back_to_session_id: None,
            latest_completion_report: None,
            role: "agent".to_string(),
            joined_at: Instant::now(),
            last_status_change: Instant::now(),
            is_headless: false,
            output_tail: None,
            todo_progress: None,
            todo_items: Vec::new(),
            runtime: crate::protocol::SwarmMemberRuntime::default(),
        }
    }

    async fn next_client_event(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> ServerEvent {
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("the attached client receives the event promptly")
            .expect("the client channel stays open")
    }

    /// A turn started by a connected client builds its ordered stream with the
    /// fallback factory, and that construction is the only route
    /// [`emit_session_event`] has to that client. Every test that looked like it
    /// covered this called `register_session_event_sink` itself first, which is
    /// the precondition the product failed to establish.
    ///
    /// In-process on purpose: the defect is reachability inside one process, and
    /// the socket surface is covered by the e2e suite. This test does not open a
    /// socket and claims nothing beyond this process.
    #[tokio::test]
    async fn the_client_turn_factory_makes_the_session_reachable() {
        let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().expect("isolated home");
        let session_id = "state-test-client-turn-sink".to_string();
        let (client_tx, mut client_rx) = mpsc::unbounded_channel();
        let members: Arc<RwLock<HashMap<String, SwarmMember>>> = Arc::new(RwLock::new(HashMap::from([(
            session_id.clone(),
            client_member(&session_id, &client_tx),
        )])));

        // Exactly what `start_processing_message` builds for a client-driven turn.
        let turn_tx =
            session_event_fanout_sender_with_fallback(session_id.clone(), Arc::clone(&members), client_tx.clone());

        // `ApprovalPending` is persisted nowhere, so a `NoSink` here is the
        // client never learning that a tool is waiting for it.
        assert_eq!(
            emit_session_event(&session_id, approval_pending("req-client-1")),
            EmitOutcome::Delivered
        );
        assert!(matches!(
            next_client_event(&mut client_rx).await,
            ServerEvent::ApprovalPending { ref request_id, .. } if request_id == "req-client-1"
        ));

        assert_eq!(
            emit_session_event(&session_id, approval_resolved("req-client-1")),
            EmitOutcome::Delivered
        );
        assert!(matches!(
            next_client_event(&mut client_rx).await,
            ServerEvent::ApprovalResolved {
                ref request_id,
                approved: true,
                ..
            } if request_id == "req-client-1"
        ));

        assert_eq!(
            emit_session_event(&session_id, failover_switched("chain-client-1")),
            EmitOutcome::Delivered
        );
        assert!(matches!(
            next_client_event(&mut client_rx).await,
            ServerEvent::FailoverSwitched { ref chain_id, .. } if chain_id == "chain-client-1"
        ));

        // A turn ends and the caller drops its sender. The registry still holds
        // its own clone, so the fanout task stays alive and the entry is not a
        // stale sink: an event emitted between two turns still reaches the
        // attached client instead of returning `Closed`.
        drop(turn_tx);
        assert_eq!(
            emit_session_event(&session_id, approval_pending("req-between-turns")),
            EmitOutcome::Delivered
        );
        assert!(matches!(
            next_client_event(&mut client_rx).await,
            ServerEvent::ApprovalPending { ref request_id, .. } if request_id == "req-between-turns"
        ));
    }

    #[tokio::test]
    async fn fanout_sender_registers_itself_as_the_session_sink() {
        let session_id = "state-test-fanout-registration".to_string();
        let members: Arc<RwLock<HashMap<String, SwarmMember>>> = Arc::new(RwLock::new(HashMap::new()));
        let _tx = session_event_fanout_sender(session_id.clone(), members);

        assert_eq!(
            emit_session_event(&session_id, approval_pending("req-6")),
            EmitOutcome::Delivered,
            "the fanout factory must make its session reachable without extra wiring"
        );
    }

    #[tokio::test]
    async fn attached_client_flag_follows_register_and_unregister() {
        let session_id = "state-test-attached-clients";
        let members: Arc<RwLock<HashMap<String, SwarmMember>>> = Arc::new(RwLock::new(HashMap::new()));
        let (tx, _rx) = mpsc::unbounded_channel();
        assert!(!session_has_attached_client(session_id));

        register_session_event_sender(&members, session_id, "conn-1", tx.clone()).await;
        assert!(session_has_attached_client(session_id));

        register_session_event_sender(&members, session_id, "conn-2", tx).await;
        unregister_session_event_sender(&members, session_id, "conn-1").await;
        assert!(
            session_has_attached_client(session_id),
            "a second live attachment keeps the session attached"
        );

        unregister_session_event_sender(&members, session_id, "conn-2").await;
        assert!(!session_has_attached_client(session_id));
    }
}
