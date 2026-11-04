use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::paths::{lock_path, state_path};
use super::{AmbientCycleResult, AmbientState, AmbientStatus, CycleStatus, ScheduledItem};
use crate::storage;

// ---------------------------------------------------------------------------
// AmbientState persistence
// ---------------------------------------------------------------------------

impl AmbientState {
    pub fn load() -> Result<Self> {
        let path = state_path()?;
        if path.exists() {
            storage::read_json(&path)
        } else {
            Ok(Self::default())
        }
    }

    pub fn save(&self) -> Result<()> {
        storage::write_json(&state_path()?, self)
    }

    pub fn record_cycle(&mut self, result: &AmbientCycleResult) {
        self.last_run = Some(result.ended_at);
        self.last_summary = Some(result.summary.clone());
        self.last_compactions = Some(result.compactions);
        self.last_memories_modified = Some(result.memories_modified);
        self.total_cycles += 1;

        match result.status {
            CycleStatus::Complete => {
                if let Some(ref req) = result.next_schedule {
                    let next = req.wake_at.unwrap_or_else(|| {
                        Utc::now() + chrono::Duration::minutes(req.wake_in_minutes.unwrap_or(30) as i64)
                    });
                    self.status = AmbientStatus::Scheduled { next_wake: next };
                } else {
                    self.status = AmbientStatus::Idle;
                }
            }
            CycleStatus::Interrupted | CycleStatus::Incomplete => {
                self.status = AmbientStatus::Idle;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ScheduledQueue
// ---------------------------------------------------------------------------

pub struct ScheduledQueue {
    items: Vec<ScheduledItem>,
    path: PathBuf,
}

impl ScheduledQueue {
    pub fn load(path: PathBuf) -> Self {
        let items: Vec<ScheduledItem> = if path.exists() {
            storage::read_json(&path).unwrap_or_default()
        } else {
            Vec::new()
        };
        Self { items, path }
    }

    pub fn save(&self) -> Result<()> {
        storage::write_json(&self.path, &self.items)
    }

    pub fn push(&mut self, item: ScheduledItem) {
        self.items.push(item);
        let _ = self.save();
    }

    /// Remove a scheduled item by ID, persisting the queue when found.
    pub fn remove_by_id(&mut self, id: &str) -> Result<Option<ScheduledItem>> {
        let Some(index) = self.items.iter().position(|item| item.id == id) else {
            return Ok(None);
        };

        let item = self.items.remove(index);
        self.save()?;
        Ok(Some(item))
    }

    /// Pop items whose `scheduled_for` is in the past, sorted by priority
    /// (highest first) then by time (earliest first).
    pub fn pop_ready(&mut self) -> Vec<ScheduledItem> {
        let now = Utc::now();
        let (ready, remaining): (Vec<_>, Vec<_>) = self.items.drain(..).partition(|i| i.scheduled_for <= now);

        self.items = remaining;

        let mut ready = ready;
        // Sort: highest priority first, then earliest scheduled_for
        ready.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.scheduled_for.cmp(&b.scheduled_for))
        });

        if !ready.is_empty() {
            let _ = self.save();
        }

        ready
    }

    /// Remove and return ready items targeted at a specific direct-delivery session,
    /// leaving ambient-targeted queue items intact for the ambient agent to process.
    pub fn take_ready_direct_items(&mut self) -> Vec<ScheduledItem> {
        let now = Utc::now();
        let mut ready_direct = Vec::new();
        let mut remaining = Vec::with_capacity(self.items.len());

        for item in self.items.drain(..) {
            let is_ready = item.scheduled_for <= now;
            let is_direct_target = item.target.is_direct_delivery();
            if is_ready && is_direct_target {
                ready_direct.push(item);
            } else {
                remaining.push(item);
            }
        }

        self.items = remaining;

        if !ready_direct.is_empty() {
            let _ = self.save();
        }

        ready_direct.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.scheduled_for.cmp(&b.scheduled_for))
        });

        ready_direct
    }

    pub fn peek_next(&self) -> Option<&ScheduledItem> {
        self.items.iter().min_by_key(|i| i.scheduled_for)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn items(&self) -> &[ScheduledItem] {
        &self.items
    }
}

// ---------------------------------------------------------------------------
// AmbientLock  (single-instance guard)
// ---------------------------------------------------------------------------

pub struct AmbientLock {
    pub(crate) lock_path: PathBuf,
}

impl AmbientLock {
    /// Try to acquire the ambient lock.
    /// Returns `Ok(Some(lock))` if acquired, `Ok(None)` if another instance
    /// already holds it, or `Err` on I/O failure.
    pub fn try_acquire() -> Result<Option<Self>> {
        let path = lock_path()?;

        // Check existing lock
        if path.exists() {
            if let Ok(contents) = std::fs::read_to_string(&path)
                && let Ok(pid) = contents.trim().parse::<u32>()
                && is_pid_alive(pid)
            {
                return Ok(None); // Another instance is running
            }
            let _ = std::fs::remove_file(&path);
        }

        // Write our PID
        let pid = std::process::id();
        if let Some(parent) = path.parent() {
            storage::ensure_dir(parent)?;
        }
        std::fs::write(&path, pid.to_string())?;

        Ok(Some(Self { lock_path: path }))
    }

    pub fn release(self) -> Result<()> {
        let _ = std::fs::remove_file(&self.lock_path);
        // Drop runs, but we already cleaned up
        std::mem::forget(self);
        Ok(())
    }
}

impl Drop for AmbientLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

fn is_pid_alive(pid: u32) -> bool {
    crate::platform::is_process_running(pid)
}

// ---------------------------------------------------------------------------
// BlockedApproval records (the durable side of an approval wait)
// ---------------------------------------------------------------------------

/// One approval that stopped an unattended run, plus the ownership and resume
/// bookkeeping needed to pick the run up again after a restart.
///
/// This type is the single definition of the wait-record contract: the approval
/// tool, the ambient runner and the notification/CLI paths all read and write it
/// through [`BlockedApprovalStore`]. Every field defaults, so a file written by
/// an older or partial shape still loads instead of taking the whole store down.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BlockedApproval {
    /// Session whose turn is parked on this approval.
    pub session_id: String,
    /// Id of the permission request this record tracks.
    pub request_id: String,
    /// When the record was created.
    pub created_at: DateTime<Utc>,
    /// When a decision (approval, denial, timeout default) was recorded.
    pub decided_at: Option<DateTime<Utc>>,
    /// When the run was resumed after the decision.
    pub resumed_at: Option<DateTime<Utc>>,
    /// How many times a resume was attempted (the resume budget is 2).
    pub resume_attempts: u8,
    /// PID of the process currently waiting on the decision, if any.
    pub waiter_pid: Option<u32>,
    /// When the current waiter took ownership.
    pub waiter_started_at: Option<DateTime<Utc>>,
    /// Lease deadline; while this is in the future the runner must not step in.
    pub waiter_lease_expires_at: Option<DateTime<Utc>>,
    /// Ownership generation, bumped when the lease is taken over.
    pub owner_epoch: u64,
    /// When the decision was applied inline, in the waiting turn.
    pub inline_at: Option<DateTime<Utc>>,
    /// Outcome of that inline application (`Some(true)` approved).
    pub inline_approved: Option<bool>,
    /// Terminal outcome (`inline`, `resumed`, `timeout_denied`, ...).
    pub outcome: Option<String>,
    /// Per-tool-call human verdicts: `tool_call_id` -> `human_retry` | `human_skip`.
    ///
    /// One request can cover several calls, so a human verdict names the exact
    /// call it applies to; the other calls of the same request stay unaffected.
    #[serde(default)]
    pub tool_verdicts: std::collections::BTreeMap<String, String>,
    /// Event that still owes the client an ack; deleted only once acked.
    pub pending_event: Option<serde_json::Value>,
}

impl Default for BlockedApproval {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            request_id: String::new(),
            created_at: Utc::now(),
            decided_at: None,
            resumed_at: None,
            resume_attempts: 0,
            waiter_pid: None,
            waiter_started_at: None,
            waiter_lease_expires_at: None,
            owner_epoch: 0,
            inline_at: None,
            inline_approved: None,
            outcome: None,
            tool_verdicts: std::collections::BTreeMap::new(),
            pending_event: None,
        }
    }
}

/// Directory holding the wait records and their lock files.
pub fn records_dir() -> Result<PathBuf> {
    let dir = storage::horon_agent_dir()?.join("approvals");
    storage::ensure_dir(&dir)?;
    Ok(dir)
}

/// `~/.horon-agent/approvals/blocked_approvals.json`
pub fn blocked_approvals_path() -> Result<PathBuf> {
    Ok(records_dir()?.join("blocked_approvals.json"))
}

/// `~/.horon-agent/approvals/.records.lock`.
///
/// A dedicated lock file for approval records. [`AmbientLock`] is the ambient
/// runner's process-wide single-instance guard and is held for a whole run, so
/// it must never be used to serialize record updates.
pub fn records_lock_path() -> Result<PathBuf> {
    Ok(records_dir()?.join(".records.lock"))
}

/// Attempts to take an already-held record lock before giving up.
const RECORDS_LOCK_ATTEMPTS: u32 = 100;
/// Delay between lock attempts (20ms x 100 = about 2 seconds of patience).
const RECORDS_LOCK_RETRY_DELAY: Duration = Duration::from_millis(20);

/// Run `f` holding the exclusive approval-record lock.
///
/// Every read-modify-write of the wait records goes through here, so two
/// processes (an approval tool and the ambient runner) cannot interleave and
/// lose an update. Contention is retried for a bounded time; if the lock is
/// still held the call returns an error instead of waiting forever.
pub fn with_records_lock<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    with_records_lock_retrying(RECORDS_LOCK_ATTEMPTS, RECORDS_LOCK_RETRY_DELAY, f)
}

fn with_records_lock_retrying<T>(attempts: u32, delay: Duration, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let path = records_lock_path()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| anyhow::anyhow!("cannot open the approval record lock {}: {error}", path.display()))?;

    let mut attempt = 0;
    loop {
        attempt += 1;
        match file.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if attempt < attempts => std::thread::sleep(delay),
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(anyhow::anyhow!(
                    "approval record lock {} is still held after {attempts} attempts; another process is updating the wait records",
                    path.display()
                ));
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(anyhow::anyhow!("cannot lock {}: {error}", path.display()));
            }
        }
    }

    let result = f();
    let _ = file.unlock();
    result
}

/// File-backed store for [`BlockedApproval`] records.
///
/// The store holds only its path; every operation takes the exclusive record
/// lock, re-reads the file, applies the change and atomically rewrites it, so a
/// stale in-memory copy can never overwrite a concurrent writer's record.
#[derive(Debug, Clone)]
pub struct BlockedApprovalStore {
    path: PathBuf,
}

impl BlockedApprovalStore {
    /// The store at [`blocked_approvals_path`].
    pub fn open_default() -> Result<Self> {
        Ok(Self {
            path: blocked_approvals_path()?,
        })
    }

    /// A store at an explicit path.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Path of the backing JSON file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every record, in file order.
    pub fn list(&self) -> Result<Vec<BlockedApproval>> {
        with_records_lock(|| self.read())
    }

    /// The record for `request_id`, if there is one.
    pub fn get(&self, request_id: &str) -> Result<Option<BlockedApproval>> {
        with_records_lock(|| Ok(self.read()?.into_iter().find(|record| record.request_id == request_id)))
    }

    /// Insert `record` when no record with its `request_id` exists yet.
    ///
    /// Returns `false` when an existing record was left untouched, so a
    /// reconciling runner can call this repeatedly without duplicating records.
    pub fn add(&self, record: BlockedApproval) -> Result<bool> {
        with_records_lock(|| {
            let mut records = self.read()?;
            if records.iter().any(|existing| existing.request_id == record.request_id) {
                return Ok(false);
            }
            records.push(record);
            self.write(&records)?;
            Ok(true)
        })
    }

    /// Replace the record with the same `request_id` wholesale.
    ///
    /// Returns `false` when no such record exists. Prefer [`Self::update_with`]
    /// when the change depends on the record's current values.
    pub fn update(&self, record: BlockedApproval) -> Result<bool> {
        with_records_lock(|| {
            let mut records = self.read()?;
            let Some(existing) = records
                .iter_mut()
                .find(|existing| existing.request_id == record.request_id)
            else {
                return Ok(false);
            };
            *existing = record;
            self.write(&records)?;
            Ok(true)
        })
    }

    /// Read-modify-write one record while holding the lock.
    ///
    /// This is the safe way to bump `resume_attempts`, take the waiter lease or
    /// clear an acked `pending_event`: the record handed to `f` is the one on
    /// disk, not a snapshot the caller read earlier.
    pub fn update_with(&self, request_id: &str, f: impl FnOnce(&mut BlockedApproval)) -> Result<bool> {
        with_records_lock(|| {
            let mut records = self.read()?;
            let Some(record) = records.iter_mut().find(|record| record.request_id == request_id) else {
                return Ok(false);
            };
            f(record);
            self.write(&records)?;
            Ok(true)
        })
    }

    /// Mark the record closed with a terminal `outcome`.
    ///
    /// Returns `false` when there is no such record. `decided_at` and
    /// `resumed_at` are owned by the paths that record those facts, so they are
    /// left untouched here.
    pub fn close(&self, request_id: &str, outcome: &str) -> Result<bool> {
        let outcome = outcome.to_string();
        self.update_with(request_id, move |record| {
            record.outcome = Some(outcome);
        })
    }

    /// Read the backing file. A missing file is an empty store.
    fn read(&self) -> Result<Vec<BlockedApproval>> {
        if self.path.exists() {
            storage::read_json(&self.path)
        } else {
            Ok(Vec::new())
        }
    }

    /// Atomically rewrite the backing file (temp file + rename).
    fn write(&self, records: &[BlockedApproval]) -> Result<()> {
        storage::write_json(&self.path, records)
    }
}

// ---------------------------------------------------------------------------
// Orphan reconciliation
// ---------------------------------------------------------------------------

/// What a startup sweep changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Requests that had no record and now have one.
    pub created: Vec<String>,
    /// Requests the safety system expired because nobody could decide them.
    pub expired: Vec<String>,
}

impl ReconcileReport {
    /// Whether the sweep found nothing to reconcile.
    pub fn is_empty(&self) -> bool {
        self.created.is_empty() && self.expired.is_empty()
    }
}

/// Rebuild wait records for queued requests that lost theirs.
///
/// The record store recovers from a one-write-old backup when its primary file
/// is corrupt, which silently adopts an older list and drops every record added
/// since. Losing a record means losing the only pointer to a parked run, so the
/// runner rebuilds the missing ones from `queue.json` at startup: a request that
/// is still queued and whose session still exists gets a record again, and a
/// request whose session is gone is expired instead of waiting forever.
///
/// Safe to call repeatedly: [`BlockedApprovalStore::add`] never duplicates.
pub fn reconcile_orphan_requests(
    store: &BlockedApprovalStore,
    safety: &crate::safety::SafetySystem,
) -> Result<ReconcileReport> {
    let mut report = ReconcileReport::default();
    let mut orphaned = false;

    for request in safety.pending_requests() {
        if store.get(&request.id)?.is_some() {
            continue;
        }
        let session_id = request_session_id(&request);
        let session_alive = session_id.as_deref().is_some_and(crate::session::session_exists);
        if !session_alive {
            // The safety sweep below records the denial that closes it.
            orphaned = true;
            continue;
        }

        let record = BlockedApproval {
            session_id: session_id.unwrap_or_default(),
            request_id: request.id.clone(),
            created_at: request.created_at,
            // No tool is waiting on it any more: the run that asked is gone.
            waiter_pid: None,
            ..BlockedApproval::default()
        };
        if store.add(record)? {
            report.created.push(request.id.clone());
        }
    }

    if orphaned {
        // This also closes the queue entries, so the sweep runs once for the
        // whole batch instead of once per request.
        report.expired = safety.expire_dead_session_requests("ambient_runner_orphan")?;
    }

    Ok(report)
}

/// The session a permission request belongs to, mirroring how safety resolves it.
fn request_session_id(request: &crate::safety::PermissionRequest) -> Option<String> {
    let context = request.context.as_ref()?;
    context
        .get("session_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            context
                .get("requester")
                .and_then(|requester| requester.get("session_id"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
}

// ---------------------------------------------------------------------------
// Resume claims (attempt accounting only)
// ---------------------------------------------------------------------------

/// Where one resume attempt stands.
///
/// A claim never answers "was this request resumed" — the session journal does.
/// It only bounds how many times this process may try, so a resume that keeps
/// crashing cannot loop forever.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResumeClaimState {
    /// The record was claimed; nothing has been attempted yet.
    #[default]
    Claimed,
    /// A resume turn is being created or executed.
    Resuming,
    /// The resume turn exists and ran.
    Done,
    /// The attempt budget is spent; a person has to step in.
    NeedsHuman,
}

/// Attempt accounting for one blocked approval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ResumeClaim {
    pub request_id: String,
    pub state: ResumeClaimState,
    /// How many times a resume turn was attempted (cap 2).
    pub attempts: u32,
    pub claimed_by_pid: Option<u32>,
    pub claimed_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// When a needs-human stop was last reported, so the notice goes out once.
    pub notified_at: Option<DateTime<Utc>>,
}

impl Default for ResumeClaim {
    fn default() -> Self {
        Self {
            request_id: String::new(),
            state: ResumeClaimState::Claimed,
            attempts: 0,
            claimed_by_pid: None,
            claimed_at: Utc::now(),
            updated_at: Utc::now(),
            notified_at: None,
        }
    }
}

/// Executions of a resume turn that was created but never ran.
///
/// Separate from [`ResumeClaim`] because the two failures are different: the
/// first is "the turn could not be created", this one is "the turn exists and
/// nothing has run it yet".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResumeExecState {
    #[default]
    Idle,
    Running,
    Done,
    NeedsHuman,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ResumeExecClaim {
    pub request_id: String,
    pub state: ResumeExecState,
    pub exec_owner_pid: Option<u32>,
    pub exec_lease_expires_at: Option<DateTime<Utc>>,
    /// How many times the created turn was executed (cap 2).
    pub exec_attempts: u32,
    pub updated_at: DateTime<Utc>,
}

impl Default for ResumeExecClaim {
    fn default() -> Self {
        Self {
            request_id: String::new(),
            state: ResumeExecState::Idle,
            exec_owner_pid: None,
            exec_lease_expires_at: None,
            exec_attempts: 0,
            updated_at: Utc::now(),
        }
    }
}

/// Result of trying to own the execution of a created resume turn.
#[derive(Debug, Clone, PartialEq)]
pub enum ExecClaimOutcome {
    /// This process owns the execution and must run the turn.
    Acquired(ResumeExecClaim),
    /// Another live process owns it.
    Busy(ResumeExecClaim),
    /// The turn already ran.
    Done(ResumeExecClaim),
    /// The execution budget is spent and the turn still has not run.
    Exhausted(ResumeExecClaim),
}

/// Directory holding the per-request resume claims.
pub fn resume_claims_dir() -> Result<PathBuf> {
    let dir = records_dir()?.join("resume-claims");
    storage::ensure_dir(&dir)?;
    Ok(dir)
}

/// Directory holding the per-request execution claims.
pub fn resume_exec_claims_dir() -> Result<PathBuf> {
    let dir = records_dir()?.join("resume-exec-claims");
    storage::ensure_dir(&dir)?;
    Ok(dir)
}

/// Turn a request id into a file name that cannot escape its directory.
fn safe_request_id(request_id: &str) -> String {
    let mut safe: String = request_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() {
        safe.push_str("unnamed");
    }
    safe
}

/// File-backed attempt accounting for resume turns.
#[derive(Debug, Clone)]
pub struct ResumeClaimStore {
    dir: PathBuf,
}

impl ResumeClaimStore {
    /// The store under `approvals/resume-claims` in the agent home.
    pub fn open_default() -> Result<Self> {
        Ok(Self {
            dir: resume_claims_dir()?,
        })
    }

    fn path_for(&self, request_id: &str) -> PathBuf {
        self.dir.join(format!("{}.json", safe_request_id(request_id)))
    }

    /// Take the claim for `request_id`, creating it when it does not exist yet.
    pub fn acquire(&self, request_id: &str) -> Result<ResumeClaim> {
        with_records_lock(|| {
            let path = self.path_for(request_id);
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let claim = ResumeClaim {
                        request_id: request_id.to_string(),
                        ..ResumeClaim::default()
                    };
                    let body = serde_json::to_vec_pretty(&claim)?;
                    file.write_all(&body)?;
                    file.sync_all()?;
                    Ok(claim)
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => storage::read_json(&path),
                Err(error) => Err(error.into()),
            }
        })
    }

    /// Read the claim for `request_id`, or `None` when it was never written.
    #[cfg(test)]
    pub fn get(&self, request_id: &str) -> Result<Option<ResumeClaim>> {
        with_records_lock(|| {
            let path = self.path_for(request_id);
            if !path.exists() {
                return Ok(None);
            }
            storage::read_json(&path).map(Some)
        })
    }

    /// Persist `claim`, replacing the stored copy for its request id.
    pub fn save(&self, claim: &ResumeClaim) -> Result<()> {
        with_records_lock(|| storage::write_json(&self.path_for(&claim.request_id), claim))
    }
}

/// File-backed execution claims for resume turns that were created but not run.
#[derive(Debug, Clone)]
pub struct ResumeExecClaimStore {
    dir: PathBuf,
}

impl ResumeExecClaimStore {
    /// The store under `approvals/resume-exec-claims` in the agent home.
    pub fn open_default() -> Result<Self> {
        Ok(Self {
            dir: resume_exec_claims_dir()?,
        })
    }

    fn path_for(&self, request_id: &str) -> PathBuf {
        self.dir.join(format!("{}.json", safe_request_id(request_id)))
    }

    /// Take ownership of running the created turn, or report who already owns it.
    ///
    /// A lease that is still live keeps a second process out; an expired lease is
    /// reclaimed as long as the execution budget allows, so a process that died
    /// mid-turn does not strand the run.
    pub fn try_acquire(
        &self,
        request_id: &str,
        now: DateTime<Utc>,
        lease: chrono::Duration,
        cap: u32,
    ) -> Result<ExecClaimOutcome> {
        with_records_lock(|| {
            let path = self.path_for(request_id);
            let mut claim: ResumeExecClaim = if path.exists() {
                storage::read_json(&path)?
            } else {
                ResumeExecClaim {
                    request_id: request_id.to_string(),
                    ..ResumeExecClaim::default()
                }
            };

            if claim.state == ResumeExecState::Done {
                return Ok(ExecClaimOutcome::Done(claim));
            }
            let lease_live = claim.state == ResumeExecState::Running
                && claim.exec_lease_expires_at.is_some_and(|expires_at| expires_at > now);
            if lease_live {
                return Ok(ExecClaimOutcome::Busy(claim));
            }
            if claim.exec_attempts >= cap {
                claim.state = ResumeExecState::NeedsHuman;
                claim.exec_owner_pid = None;
                claim.exec_lease_expires_at = None;
                claim.updated_at = now;
                storage::write_json(&path, &claim)?;
                return Ok(ExecClaimOutcome::Exhausted(claim));
            }

            claim.state = ResumeExecState::Running;
            claim.exec_attempts += 1;
            claim.exec_owner_pid = Some(std::process::id());
            claim.exec_lease_expires_at = Some(now + lease);
            claim.updated_at = now;
            storage::write_json(&path, &claim)?;
            Ok(ExecClaimOutcome::Acquired(claim))
        })
    }

    /// Give up ownership after a failed execution so the next pass can retry.
    pub fn release(&self, request_id: &str) -> Result<()> {
        with_records_lock(|| {
            let path = self.path_for(request_id);
            if !path.exists() {
                return Ok(());
            }
            let mut claim: ResumeExecClaim = storage::read_json(&path)?;
            claim.state = ResumeExecState::Idle;
            claim.exec_owner_pid = None;
            claim.exec_lease_expires_at = None;
            claim.updated_at = Utc::now();
            storage::write_json(&path, &claim)
        })
    }

    /// Close the claim as done, releasing the execution lease.
    pub fn mark_done(&self, request_id: &str) -> Result<()> {
        self.close(request_id, ResumeExecState::Done)
    }

    /// Close the claim as needing a person, releasing the execution lease.
    pub fn mark_needs_human(&self, request_id: &str) -> Result<()> {
        self.close(request_id, ResumeExecState::NeedsHuman)
    }

    fn close(&self, request_id: &str, state: ResumeExecState) -> Result<()> {
        with_records_lock(|| {
            let path = self.path_for(request_id);
            if !path.exists() {
                return Ok(());
            }
            let mut claim: ResumeExecClaim = storage::read_json(&path)?;
            claim.state = state;
            claim.exec_owner_pid = None;
            claim.exec_lease_expires_at = None;
            claim.updated_at = Utc::now();
            storage::write_json(&path, &claim)
        })
    }
}

#[cfg(test)]
mod blocked_approval_tests {
    use super::*;
    use chrono::TimeDelta;
    use std::fs::OpenOptions;

    /// Point the process at a throwaway home for the duration of `f`. The
    /// process-wide env lock keeps this test from racing the crate's other
    /// `HORON_AGENT_HOME` tests.
    fn with_temp_home<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::storage::lock_test_env();
        let previous = std::env::var_os("HORON_AGENT_HOME");
        let temp = tempfile::TempDir::new().expect("temp dir");
        crate::env::set_var("HORON_AGENT_HOME", temp.path());

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));

        match previous {
            Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
            None => crate::env::remove_var("HORON_AGENT_HOME"),
        }

        result.unwrap_or_else(|payload| std::panic::resume_unwind(payload))
    }

    fn record(request_id: &str) -> BlockedApproval {
        BlockedApproval {
            session_id: "sess_records".to_string(),
            request_id: request_id.to_string(),
            created_at: Utc::now(),
            ..BlockedApproval::default()
        }
    }

    #[test]
    fn records_live_under_the_approvals_directory() {
        with_temp_home(|| {
            let home = std::env::var_os("HORON_AGENT_HOME").expect("temp home");
            let home = PathBuf::from(home);
            assert_eq!(
                blocked_approvals_path().unwrap(),
                home.join("approvals/blocked_approvals.json")
            );
            assert_eq!(records_lock_path().unwrap(), home.join("approvals/.records.lock"));
        });
    }

    #[test]
    fn a_missing_store_reads_as_empty() {
        with_temp_home(|| {
            let store = BlockedApprovalStore::open_default().unwrap();
            assert!(store.list().unwrap().is_empty());
            assert_eq!(store.get("req_absent").unwrap(), None);
        });
    }

    /// The whole point of the store is surviving restarts and older writers, so
    /// a shape this build did not write must load instead of panicking.
    #[test]
    fn an_older_or_partial_record_shape_still_loads() {
        with_temp_home(|| {
            let path = blocked_approvals_path().unwrap();
            std::fs::write(
                &path,
                r#"[{"session_id":"s-1","request_id":"req_old","unknown_future_key":7}]"#,
            )
            .expect("seed an older record file");

            let store = BlockedApprovalStore::open_default().unwrap();
            let records = store.list().expect("an older record shape must load");
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].session_id, "s-1");
            assert_eq!(records[0].request_id, "req_old");
            assert_eq!(records[0].resume_attempts, 0);
            assert_eq!(records[0].owner_epoch, 0);
            assert_eq!(records[0].outcome, None);
            assert_eq!(records[0].waiter_pid, None);
            assert!(records[0].tool_verdicts.is_empty());

            // Adding to the loaded store keeps the older record.
            assert!(store.add(record("req_new")).unwrap());
            assert_eq!(store.list().unwrap().len(), 2);
        });
    }

    #[test]
    fn add_is_idempotent_and_update_with_reads_the_stored_record() {
        with_temp_home(|| {
            let store = BlockedApprovalStore::open_default().unwrap();
            assert!(store.add(record("req_one")).unwrap(), "first add creates the record");
            assert!(
                !store.add(record("req_one")).unwrap(),
                "a reconciling add must not duplicate"
            );
            assert_eq!(store.list().unwrap().len(), 1);

            let now = Utc::now();
            let found = store
                .update_with("req_one", |stored| {
                    stored.resume_attempts += 1;
                    stored.waiter_pid = Some(4242);
                    stored.waiter_started_at = Some(now);
                    stored.waiter_lease_expires_at = Some(now + TimeDelta::seconds(30));
                    stored.owner_epoch += 1;
                })
                .unwrap();
            assert!(found);

            let stored = store.get("req_one").unwrap().expect("record present");
            assert_eq!(stored.resume_attempts, 1);
            assert_eq!(stored.waiter_pid, Some(4242));
            assert_eq!(stored.owner_epoch, 1);
            assert_eq!(stored.waiter_lease_expires_at, Some(now + TimeDelta::seconds(30)));

            assert!(!store.update_with("req_absent", |_| {}).unwrap());
            assert!(!store.close("req_absent", "resumed").unwrap());
            assert!(store.close("req_one", "inline").unwrap());
            assert_eq!(
                store.get("req_one").unwrap().unwrap().outcome.as_deref(),
                Some("inline")
            );

            // A record replaced wholesale keeps its identity.
            let mut replacement = record("req_one");
            replacement.decided_at = Some(now);
            assert!(store.update(replacement).unwrap());
            let replaced = store.get("req_one").unwrap().unwrap();
            assert_eq!(replaced.decided_at, Some(now));
            assert_eq!(replaced.outcome, None, "update replaces the whole record");
        });
    }

    /// One request can cover several tool calls, so a human verdict is stored
    /// per call: the request-level outcome marks the request as no longer
    /// waiting, and `tool_verdicts` names exactly which call was retried or
    /// skipped. Without the map the runner could only act on every call of the
    /// request at once.
    #[test]
    fn per_tool_call_verdicts_are_stored_alongside_the_request_outcome() {
        with_temp_home(|| {
            let store = BlockedApprovalStore::open_default().unwrap();
            assert!(store.add(record("req_multi_call")).unwrap());

            for (tool_call_id, verdict) in [("call_1", "human_retry"), ("call_2", "human_skip")] {
                let verdict = verdict.to_string();
                let found = store
                    .update_with("req_multi_call", move |stored| {
                        // The first verdict closes the request; later verdicts
                        // only add their own call entry.
                        if stored.outcome.is_none() {
                            stored.outcome = Some(verdict.clone());
                        }
                        stored.tool_verdicts.insert(tool_call_id.to_string(), verdict);
                    })
                    .unwrap();
                assert!(found);
            }

            let stored = store.get("req_multi_call").unwrap().expect("record present");
            assert_eq!(
                stored.outcome.as_deref(),
                Some("human_retry"),
                "the request-level outcome is set once, by the first verdict"
            );
            assert_eq!(stored.tool_verdicts.len(), 2);
            assert_eq!(
                stored.tool_verdicts.get("call_1").map(String::as_str),
                Some("human_retry")
            );
            assert_eq!(
                stored.tool_verdicts.get("call_2").map(String::as_str),
                Some("human_skip")
            );
        });
    }

    /// Concurrent read-modify-write must serialize on the lock. Without it the
    /// losing writer would overwrite the winner's increment and the total would
    /// come out short.
    #[test]
    fn concurrent_writers_lose_no_update_through_the_record_lock() {
        with_temp_home(|| {
            const WRITERS: u8 = 4;
            const PER_WRITER: u8 = 25;

            let store = BlockedApprovalStore::open_default().unwrap();
            assert!(store.add(record("req_race")).unwrap());

            std::thread::scope(|scope| {
                for _ in 0..WRITERS {
                    scope.spawn(|| {
                        for _ in 0..PER_WRITER {
                            let bumped = store
                                .update_with("req_race", |stored| stored.resume_attempts += 1)
                                .expect("update_with under contention");
                            assert!(bumped, "the record exists for every writer");
                        }
                    });
                }
            });

            let stored = store.get("req_race").unwrap().expect("record present");
            assert_eq!(
                stored.resume_attempts,
                WRITERS * PER_WRITER,
                "every increment must survive; a lost update means the lock is not serializing read-modify-write"
            );
        });
    }

    /// A lock held past the retry budget must be reported as an error, because
    /// panicking inside an unattended run would kill the whole cycle.
    #[test]
    fn a_lock_held_past_the_retry_cap_returns_an_error() {
        with_temp_home(|| {
            let lock_path = records_lock_path().unwrap();
            let holder = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&lock_path)
                .expect("open the lock file");
            holder.try_lock().expect("hold the record lock");

            let error = with_records_lock_retrying(3, Duration::from_millis(1), || Ok(()))
                .expect_err("a lock held past the cap must be reported, not waited on forever");
            assert!(
                error.to_string().contains(".records.lock"),
                "the error names the contended lock file, got: {error}"
            );

            holder.unlock().expect("release the record lock");
            with_records_lock_retrying(3, Duration::from_millis(1), || Ok(()))
                .expect("the lock is available again once released");
        });
    }

    /// Writes are atomic (temp file + rename), so a reader never sees a torn
    /// file and a crash cannot leave a half-written store behind.
    #[test]
    fn writes_leave_no_partial_file_behind() {
        with_temp_home(|| {
            let store = BlockedApprovalStore::open_default().unwrap();
            store.add(record("req_atomic")).unwrap();

            let raw = std::fs::read_to_string(store.path()).expect("read the store file");
            let parsed: Vec<BlockedApproval> =
                serde_json::from_str(&raw).expect("the store file is always complete JSON");
            assert_eq!(parsed.len(), 1);

            let siblings: Vec<_> = std::fs::read_dir(store.path().parent().unwrap())
                .expect("read the approvals directory")
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with("blocked_approvals.tmp."))
                .collect();
            assert!(siblings.is_empty(), "no temp file may survive a write: {siblings:?}");
        });
    }

    /// The ack path clears exactly one record's durable event. Clearing the
    /// wrong record would silently drop a notification another parked run is
    /// still waiting to have delivered.
    #[test]
    fn an_ack_clears_only_the_acked_records_pending_event() {
        with_temp_home(|| {
            let store = BlockedApprovalStore::open_default().unwrap();
            assert!(store.add(record("req_acked")).unwrap());
            assert!(store.add(record("req_other")).unwrap());

            let pending = serde_json::json!({"type": "approval_resolved", "request_id": "req_acked"});
            for request_id in ["req_acked", "req_other"] {
                let pending = pending.clone();
                assert!(
                    store
                        .update_with(request_id, move |stored| stored.pending_event = Some(pending))
                        .unwrap(),
                    "both records start owing a notification"
                );
            }

            let cleared = store
                .update_with("req_acked", |stored| stored.pending_event = None)
                .expect("the ack applies");
            assert!(cleared, "the record existed, so the ack cleared it");

            let records = store.list().unwrap();
            assert_eq!(records.len(), 2, "an ack never creates or removes a record");
            let acked = records
                .iter()
                .find(|stored| stored.request_id == "req_acked")
                .expect("the acked record is still there");
            assert_eq!(acked.pending_event, None, "the acked record owes nothing more");
            let other = records
                .iter()
                .find(|stored| stored.request_id == "req_other")
                .expect("the other record is untouched");
            assert_eq!(
                other.pending_event,
                Some(pending),
                "a different record keeps its pending event"
            );
        });
    }

    /// An ack for a record the runner already cleaned up must succeed and
    /// change nothing: the client cannot tell the difference, and failing it
    /// would only make it retry an ack that has nothing left to clear.
    #[test]
    fn an_ack_for_a_missing_request_changes_nothing() {
        with_temp_home(|| {
            let store = BlockedApprovalStore::open_default().unwrap();
            assert!(store.add(record("req_kept")).unwrap());
            let pending = serde_json::json!({"type": "approval_resolved", "request_id": "req_kept"});
            let seeded = pending.clone();
            assert!(
                store
                    .update_with("req_kept", move |stored| stored.pending_event = Some(seeded))
                    .unwrap()
            );

            let cleared = store
                .update_with("req_absent", |stored| stored.pending_event = None)
                .expect("a missing record is not an error");
            assert!(!cleared, "nothing was there to clear");

            let records = store.list().unwrap();
            assert_eq!(records.len(), 1, "an ack never creates a record");
            assert_eq!(
                records[0].pending_event,
                Some(pending),
                "the untouched record keeps its pending event"
            );
        });
    }

    /// Clearing `pending_event` says only that a client saw the notice. The
    /// resume decision reads the decision fields, so the verdict must be
    /// identical before and after the ack: an ack is not a decision.
    #[test]
    fn clearing_a_pending_event_does_not_change_the_resume_verdict() {
        use crate::ambient::resume::{ResumeOrigin, ResumeVerdict, classify_blocked_approval};
        use crate::safety::Decision;
        use crate::session::SessionStatus;

        let now = Utc::now();
        let decision = Decision {
            request_id: "req_verdict".to_string(),
            approved: true,
            decided_at: now,
            decided_via: "file".to_string(),
            message: None,
        };
        let mut record = BlockedApproval {
            session_id: "sess_verdict".to_string(),
            request_id: "req_verdict".to_string(),
            created_at: now,
            decided_at: Some(now),
            pending_event: Some(serde_json::json!({
                "type": "approval_resolved",
                "request_id": "req_verdict",
            })),
            ..BlockedApproval::default()
        };

        let status = SessionStatus::Crashed { message: None };
        let before = classify_blocked_approval(&record, Some(&decision), true, Some(&status), now);
        assert_eq!(
            before,
            ResumeVerdict::Resume {
                origin: ResumeOrigin::Approved
            },
            "precondition: the recorded approval resumes the run"
        );

        record.pending_event = None;
        let after = classify_blocked_approval(&record, Some(&decision), true, Some(&status), now);
        assert_eq!(
            after, before,
            "an ack only retires the notification; the resume verdict must not move"
        );
    }
}
