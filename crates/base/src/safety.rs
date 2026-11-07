use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use crate::storage;

/// How often a pending approval re-reads `history.json` while it waits.
///
/// Another process (an approval tool, an operator's script, the CLI) records
/// its decision in the file, so a waiter that only listened for in-process
/// notifications would miss it forever.
pub const DEFAULT_DECISION_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Wait window used by [`SafetySystem::request_permission`] when the caller does
/// not choose one explicitly. Matches the 30 minute ambient default.
pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(1800);

/// Recorded when the configured timeout default could not be applied because
/// the decision history was corrupt.
const CORRUPT_HISTORY_VIA: &str = "corrupt_history_deny";

/// Recorded when the wait elapsed and the request's own default was applied.
///
/// Public so the approval tool can tell a timeout default apart from a human
/// decision when it words the tool result.
pub const TIMEOUT_DEFAULT_VIA: &str = "timeout_default";

/// Bounded retry budget for the cross-process decision lock. Exhausting it is
/// an error: the caller keeps the request queued instead of approving it.
const DECISION_LOCK_MAX_ATTEMPTS: u32 = 20;
const DECISION_LOCK_BASE_DELAY: Duration = Duration::from_millis(2);

/// Fingerprints of corrupt history contents already copied aside in this
/// process, so a polling waiter does not mint a new copy every tick while the
/// file stays corrupt.
static QUARANTINED_HISTORY_PATHS: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// Hook invoked to deliver a permission-request notification.
///
/// Args: `(action, description, request_id)`.
type PermissionNotifier = fn(&str, &str, &str);

static PERMISSION_NOTIFIER: OnceLock<PermissionNotifier> = OnceLock::new();

/// Register the permission-request notification dispatcher.
///
/// This inverts the historical `safety -> notifications` dependency: the
/// `notifications` layer (which already depends on `safety` types like
/// [`AmbientTranscript`]) registers its dispatcher here at startup, so
/// `safety` no longer needs to construct a `NotificationDispatcher`.
pub fn register_permission_notifier(notifier: PermissionNotifier) {
    let _ = PERMISSION_NOTIFIER.set(notifier);
}

/// Deliver a queued permission request to the registered notifier.
///
/// The caller owns the ordering: this must run only once the request's durable
/// wait record exists. An approver who answers the alert immediately resolves
/// the record it names, so an alert that goes out first makes that answer fail
/// with "no blocked approval record". `safety` cannot write the record itself
/// (the blocked-approval store lives in the app layer), which is why
/// [`SafetySystem::request_permission_with_timeout`] queues the request without
/// alerting and leaves the dispatch to the caller that records the wait.
///
/// Delivery stays best-effort: a missing notifier is not an error.
pub fn dispatch_permission_notification(action: &str, description: &str, request_id: &str) {
    if let Some(notifier) = PERMISSION_NOTIFIER.get() {
        notifier(action, description, request_id);
    }
}

/// Callback invoked once per recorded permission decision.
///
/// Unlike [`register_permission_notifier`]'s single `OnceLock` slot, any number
/// of these may be registered; every one of them observes the same decision.
pub type DecisionNotifier = Arc<dyn Fn(&Decision) + Send + Sync>;

static DECISION_NOTIFIERS: RwLock<Vec<DecisionNotifier>> = RwLock::new(Vec::new());

/// Register a decision callback. Registering twice keeps both.
pub fn register_decision_notifier(notifier: DecisionNotifier) {
    let mut notifiers = DECISION_NOTIFIERS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    notifiers.push(notifier);
}

fn dispatch_decision_notification(decision: &Decision) {
    // Clone the handles and release the registry lock before invoking anything:
    // a callback may query the safety system, and a panicking callback must not
    // poison the registry or lose the decision record.
    let callbacks = match DECISION_NOTIFIERS.read() {
        Ok(notifiers) => notifiers.clone(),
        Err(poisoned) => (*poisoned.into_inner()).clone(),
    };
    for callback in callbacks {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(decision)));
    }
}

#[cfg(test)]
fn reset_decision_notifiers_for_tests() {
    DECISION_NOTIFIERS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

// ---------------------------------------------------------------------------
// Action classification
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionTier {
    AutoAllowed,
    RequiresPermission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    Low,
    Normal,
    High,
}

// ---------------------------------------------------------------------------
// Permission request / result / decision
// ---------------------------------------------------------------------------

/// What to do with a permission request when its wait elapses with no decision.
///
/// The default is [`ApprovalDefault::Deny`]: an unanswered request in an
/// unattended run must fail closed, and only an explicit `approve` may run the
/// gated action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDefault {
    Approve,
    #[default]
    Deny,
}

impl ApprovalDefault {
    /// The serde form of this value, which is also the accepted spelling of the
    /// `ambient.approval_default_on_timeout` config string.
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalDefault::Approve => "approve",
            ApprovalDefault::Deny => "deny",
        }
    }

    /// Parse the config string. Anything that is not an explicit approval
    /// (`approve` or `allow`, case-insensitive) falls back to `Deny`, so a typo
    /// can never turn into an automatic approval.
    pub fn from_config_str(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "approve" | "allow" => ApprovalDefault::Approve,
            _ => ApprovalDefault::Deny,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: String,
    pub action: String,
    pub description: String,
    pub rationale: String,
    pub urgency: Urgency,
    pub wait: bool,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
    /// Ambient run this request belongs to. `None` for interactive sessions,
    /// and for every request queued before this field existed.
    #[serde(default)]
    pub run_id: Option<String>,
    /// Absolute instant after which the request must not be granted. `None`
    /// means "no deadline", which keeps pre-existing queue entries valid.
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    /// Result to use when the wait elapses with no decision.
    #[serde(default)]
    pub default_on_timeout: ApprovalDefault,
}

impl PermissionRequest {
    /// Whether this request's wait window has already elapsed at `now`.
    ///
    /// A request without a deadline never expires this way; the owner-session
    /// check in [`SafetySystem::expire_dead_session_requests`] still applies.
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|expires_at| expires_at <= now)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PermissionResult {
    Approved { message: Option<String> },
    Denied { reason: Option<String> },
    Queued { request_id: String },
    Timeout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub request_id: String,
    pub approved: bool,
    pub decided_at: DateTime<Utc>,
    pub decided_via: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Result of asking the store to record a decision.
///
/// `AlreadyDecided` is not an error: two writers (the live process and an
/// external approver, or an approval and a timeout) can reach the same request
/// at the same moment, and the first recorded decision is the only one that
/// counts. Callers must follow the returned decision rather than the one they
/// asked for.
#[derive(Debug, Clone)]
pub enum RecordDecisionOutcome {
    /// This call wrote the decision.
    Recorded(Decision),
    /// A decision already existed; nothing was appended.
    AlreadyDecided { existing: Decision },
}

impl RecordDecisionOutcome {
    pub fn decision(&self) -> &Decision {
        match self {
            RecordDecisionOutcome::Recorded(decision) => decision,
            RecordDecisionOutcome::AlreadyDecided { existing } => existing,
        }
    }
}

fn decision_to_result(decision: Decision) -> PermissionResult {
    if decision.approved {
        PermissionResult::Approved {
            message: decision.message,
        }
    } else {
        PermissionResult::Denied {
            reason: decision.message,
        }
    }
}

// ---------------------------------------------------------------------------
// Action log / transcript
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionLog {
    pub action_type: String,
    pub description: String,
    pub tier: ActionTier,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptStatus {
    Complete,
    Interrupted,
    Incomplete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmbientTranscript {
    pub session_id: String,
    pub started_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    pub status: TranscriptStatus,
    pub provider: String,
    pub model: String,
    pub actions: Vec<ActionLog>,
    pub pending_permissions: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub compactions: u32,
    pub memories_modified: u32,
    /// Full conversation transcript (markdown) appended to the ambient notification body
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
}

// ---------------------------------------------------------------------------
// Tier-1 (auto-allowed) action names
// ---------------------------------------------------------------------------

const AUTO_ALLOWED: &[&str] = &[
    "read",
    "glob",
    "grep",
    "ls",
    "memory",
    "todo",
    "todowrite",
    "todoread",
    "conversation_search",
    "session_search",
    "codesearch",
];

// ---------------------------------------------------------------------------
// SafetySystem
// ---------------------------------------------------------------------------

pub struct SafetySystem {
    queue: Mutex<Vec<PermissionRequest>>,
    history: Mutex<Vec<Decision>>,
    actions: Mutex<Vec<ActionLog>>,
    /// Wakes in-process waiters the moment a decision is recorded here.
    decision_notify: tokio::sync::Notify,
    /// Request ids whose decision has already reached the registered callbacks.
    notified_request_ids: Mutex<HashSet<String>>,
    /// Reflects the most recent `history.json` read: true when that read failed
    /// to parse. Approvals recorded against such a history are downgraded to a
    /// denial inside the decision lock.
    history_corrupt: AtomicBool,
}

impl SafetySystem {
    /// Create a new SafetySystem, loading persisted queue/history from disk.
    pub fn new() -> Self {
        let queue: Vec<PermissionRequest> = queue_path()
            .ok()
            .and_then(|p| storage::read_json(&p).ok())
            .unwrap_or_default();

        let (history, history_corrupt) = match load_history_lenient() {
            Ok(loaded) => (loaded.decisions, loaded.corrupt),
            // An unreadable history is treated like a corrupt one: decisions
            // from before cannot be audited, so approvals must fail closed.
            Err(_) => (Vec::new(), true),
        };

        SafetySystem {
            queue: Mutex::new(queue),
            history: Mutex::new(history),
            actions: Mutex::new(Vec::new()),
            decision_notify: tokio::sync::Notify::new(),
            notified_request_ids: Mutex::new(HashSet::new()),
            history_corrupt: AtomicBool::new(history_corrupt),
        }
    }

    /// Classify an action name into a tier.
    pub fn classify(&self, action: &str) -> ActionTier {
        let lower = action.to_lowercase();
        if AUTO_ALLOWED.iter().any(|&a| a == lower) {
            ActionTier::AutoAllowed
        } else {
            ActionTier::RequiresPermission
        }
    }

    /// Submit a permission request. Returns `Queued` with the request id.
    ///
    /// The request's wait deadline is filled from [`DEFAULT_APPROVAL_TIMEOUT`]
    /// unless the caller already set one.
    pub fn request_permission(&self, request: PermissionRequest) -> PermissionResult {
        self.request_permission_with_timeout(request, DEFAULT_APPROVAL_TIMEOUT)
    }

    /// Submit a permission request with an explicit wait window.
    ///
    /// `expires_at` is filled as `now + timeout` when the request does not
    /// carry a deadline of its own, which is what lets the sweeper close a
    /// request whose waiter died.
    ///
    /// This queues and persists the request but does not notify anyone: the
    /// caller must call [`dispatch_permission_notification`] after it has
    /// created the durable wait record, so an approver who answers the alert
    /// at once already finds the record they are answering.
    pub fn request_permission_with_timeout(
        &self,
        mut request: PermissionRequest,
        timeout: Duration,
    ) -> PermissionResult {
        if request.expires_at.is_none() {
            let window = chrono::Duration::from_std(timeout)
                .unwrap_or_else(|_| chrono::Duration::seconds(DEFAULT_APPROVAL_TIMEOUT.as_secs() as i64));
            request.expires_at = Some(Utc::now() + window);
        }

        let request_id = request.id.clone();
        if let Ok(mut q) = self.queue.lock() {
            q.push(request);
            let _ = persist_queue(&q);
        }
        // No notification here: the caller records the durable wait first and
        // then calls `dispatch_permission_notification`, so the alert never
        // outruns the record an approver answers.
        PermissionResult::Queued { request_id }
    }

    /// Wait for a decision on `request_id` for at most `timeout`.
    ///
    /// Two sources are watched together: the in-process notification fired by
    /// [`Self::record_decision`], and a re-read of `history.json` every
    /// [`DEFAULT_DECISION_POLL_INTERVAL`] (another process may have written the
    /// decision). When the wait elapses, the request's own
    /// [`PermissionRequest::default_on_timeout`] is applied through
    /// [`Self::record_decision`] first, so an approval that was recorded at the
    /// same moment always wins over the timeout.
    ///
    /// A decision that could not be persisted is never turned into an approval:
    /// the result is [`PermissionResult::Queued`] and the caller reports that
    /// the request is still awaiting a decision.
    pub async fn await_decision(&self, request_id: &str, timeout: Duration) -> PermissionResult {
        self.await_decision_with_poll_interval(request_id, timeout, DEFAULT_DECISION_POLL_INTERVAL)
            .await
    }

    /// [`Self::await_decision`] with an injectable poll interval for tests.
    pub async fn await_decision_with_poll_interval(
        &self,
        request_id: &str,
        timeout: Duration,
        poll_interval: Duration,
    ) -> PermissionResult {
        match tokio::time::timeout(timeout, self.wait_for_decision(request_id, poll_interval)).await {
            Ok(decision) => decision_to_result(decision),
            Err(_) => self.apply_timeout_default(request_id),
        }
    }

    async fn wait_for_decision(&self, request_id: &str, poll_interval: Duration) -> Decision {
        loop {
            if let Some(decision) = self.decision_for(request_id) {
                return decision;
            }
            // Register the waiter before the second check so a decision
            // recorded between the check and the await cannot be lost.
            let notified = self.decision_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(decision) = self.decision_for(request_id) {
                return decision;
            }
            tokio::select! {
                _ = &mut notified => {}
                _ = tokio::time::sleep(poll_interval) => {}
            }
        }
    }

    fn apply_timeout_default(&self, request_id: &str) -> PermissionResult {
        let (approved, via, message) = if self.history_is_corrupt() {
            (
                false,
                CORRUPT_HISTORY_VIA,
                "Denied because the decision history was corrupt and the approval could not be audited.".to_string(),
            )
        } else {
            let default = self.pending_default_on_timeout(request_id);
            let message = format!(
                "No decision arrived before the wait elapsed; applied the request default ({}).",
                default.as_str()
            );
            (default == ApprovalDefault::Approve, TIMEOUT_DEFAULT_VIA, message)
        };

        // The corrupt-history guard also runs inside the decision lock, so a
        // history that breaks between this check and the write still cannot
        // turn an approval into a recorded one.
        match self.record_decision(request_id, approved, via, Some(message)) {
            Ok(outcome) => decision_to_result(outcome.decision().clone()),
            // The lock or the history write failed: the decision is not on
            // disk, so this must not become an approval (or a denial) the
            // caller would act on.
            Err(_) => PermissionResult::Queued {
                request_id: request_id.to_string(),
            },
        }
    }

    fn pending_default_on_timeout(&self, request_id: &str) -> ApprovalDefault {
        self.queue
            .lock()
            .ok()
            .and_then(|q| q.iter().find(|r| r.id == request_id).map(|r| r.default_on_timeout))
            .unwrap_or(ApprovalDefault::Deny)
    }

    /// Whether the most recent `history.json` read failed to parse. While true,
    /// an approval recorded against that history is downgraded to a denial.
    pub fn history_is_corrupt(&self) -> bool {
        self.history_corrupt.load(Ordering::SeqCst)
    }

    /// Look up the decision for `request_id`, falling back to `history.json`.
    ///
    /// The in-memory copy is authoritative only for decisions recorded by this
    /// instance. Another process (the approval tool, an operator's script) writes
    /// the file directly, so a miss in memory must re-read it, cache what it
    /// finds, and notify the registered callbacks exactly once per request.
    pub fn decision_for(&self, request_id: &str) -> Option<Decision> {
        let in_memory = self
            .history
            .lock()
            .ok()
            .and_then(|h| h.iter().rev().find(|d| d.request_id == request_id).cloned());
        if let Some(decision) = in_memory {
            self.notify_decision_once(&decision);
            return Some(decision);
        }

        let loaded = load_history_lenient().ok()?;
        self.history_corrupt.store(loaded.corrupt, Ordering::SeqCst);
        let found = loaded
            .decisions
            .iter()
            .rev()
            .find(|d| d.request_id == request_id)
            .cloned()?;
        self.cache_decision(&found);
        self.notify_decision_once(&found);
        Some(found)
    }

    fn cache_decision(&self, decision: &Decision) {
        if let Ok(mut history) = self.history.lock()
            && !history.iter().any(|d| d.request_id == decision.request_id)
        {
            history.push(decision.clone());
        }
    }

    fn notify_decision_once(&self, decision: &Decision) {
        let first_notification = self
            .notified_request_ids
            .lock()
            .map(|mut notified| notified.insert(decision.request_id.clone()))
            .unwrap_or(false);
        if first_notification {
            dispatch_decision_notification(decision);
        }
    }

    /// Expire pending permission requests that can no longer be serviced
    /// because their originating session is no longer active, or because their
    /// own wait window has elapsed.
    pub fn expire_dead_session_requests(&self, via: &str) -> Result<Vec<String>> {
        let now = Utc::now();
        let snapshot = self.queue.lock().map(|q| q.clone()).unwrap_or_default();

        let mut expired: Vec<(PermissionRequest, String, bool)> = Vec::new();
        for request in snapshot {
            if let Some(reason) = stale_request_reason(&request) {
                expired.push((request, reason, true));
            } else if request.is_expired_at(now) {
                let deadline = request.expires_at.map(|at| at.to_rfc3339()).unwrap_or_default();
                expired.push((request, format!("its wait window elapsed at {}", deadline), false));
            }
        }

        let mut expired_ids = Vec::with_capacity(expired.len());
        for (request, reason, session_expired) in expired {
            let message = if session_expired {
                format!("Expired automatically: {}. Original agent is no longer active.", reason)
            } else {
                format!("Expired automatically: {}.", reason)
            };
            // A failed write leaves the request in the queue: it is dropped
            // only once its decision is on disk.
            self.record_decision(&request.id, false, via, Some(message))?;
            expired_ids.push(request.id);
        }

        Ok(expired_ids)
    }

    /// Record a user decision (approve / deny) for a pending request.
    ///
    /// Recording is serialized across processes through
    /// `~/.horon-agent/approvals/.decisions.lock`, so a request carries exactly
    /// one decision no matter how many writers race for it. The first recorded
    /// decision wins and is returned as [`RecordDecisionOutcome::AlreadyDecided`]
    /// to every later caller.
    pub fn record_decision(
        &self,
        request_id: &str,
        approved: bool,
        via: &str,
        message: Option<String>,
    ) -> Result<RecordDecisionOutcome> {
        let outcome = record_decision_on_disk(request_id, approved, via, message)?;
        // A successful record leaves a valid history on disk.
        self.history_corrupt.store(false, Ordering::SeqCst);

        let decision = outcome.decision().clone();
        self.cache_decision(&decision);
        self.notify_decision_once(&decision);
        self.decision_notify.notify_waiters();

        if let Ok(mut q) = self.queue.lock() {
            q.retain(|r| r.id != request_id);
            let _ = persist_queue(&q);
        }

        Ok(outcome)
    }

    /// Return all pending permission requests.
    pub fn pending_requests(&self) -> Vec<PermissionRequest> {
        self.queue.lock().map(|q| q.clone()).unwrap_or_default()
    }

    /// Append an action to the in-memory log.
    pub fn log_action(&self, log: ActionLog) {
        if let Ok(mut actions) = self.actions.lock() {
            actions.push(log);
        }
    }

    /// Generate a human-readable summary of logged actions.
    pub fn generate_summary(&self) -> String {
        let actions = self.actions.lock().map(|a| a.clone()).unwrap_or_default();
        let pending = self.pending_requests();

        let mut lines: Vec<String> = Vec::new();

        if actions.is_empty() && pending.is_empty() {
            return "No actions recorded.".to_string();
        }

        // Separate auto vs permission-required
        let auto: Vec<&ActionLog> = actions.iter().filter(|a| a.tier == ActionTier::AutoAllowed).collect();
        let perm: Vec<&ActionLog> = actions
            .iter()
            .filter(|a| a.tier == ActionTier::RequiresPermission)
            .collect();

        if !auto.is_empty() {
            lines.push("Done (auto-allowed):".to_string());
            for a in &auto {
                lines.push(format!("- {} — {}", a.action_type, a.description));
            }
        }

        if !perm.is_empty() {
            lines.push(String::new());
            lines.push("Done (with permission):".to_string());
            for a in &perm {
                lines.push(format!("- {} — {}", a.action_type, a.description));
            }
        }

        if !pending.is_empty() {
            lines.push(String::new());
            lines.push("Needs your review:".to_string());
            for r in &pending {
                lines.push(format!("- [{:?}] {} — {}", r.urgency, r.action, r.description));
            }
        }

        lines.join("\n")
    }

    /// Persist a transcript to ~/.horon-agent/ambient/transcripts/{timestamp}.json
    pub fn save_transcript(&self, transcript: &AmbientTranscript) -> Result<()> {
        let dir = storage::horon_agent_dir()?.join("ambient").join("transcripts");
        storage::ensure_dir(&dir)?;

        let filename = transcript.started_at.format("%Y-%m-%d-%H%M%S").to_string();
        let path = dir.join(format!("{}.json", filename));
        storage::write_json(&path, transcript)
    }
}

impl Default for SafetySystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Persistence helpers
// ---------------------------------------------------------------------------

fn queue_path() -> Result<std::path::PathBuf> {
    Ok(storage::horon_agent_dir()?.join("safety").join("queue.json"))
}

fn history_path() -> Result<std::path::PathBuf> {
    Ok(storage::horon_agent_dir()?.join("safety").join("history.json"))
}

fn persist_queue(queue: &[PermissionRequest]) -> Result<()> {
    let path = queue_path()?;
    storage::write_json(&path, queue)
}

fn persist_history(history: &[Decision]) -> Result<()> {
    let path = history_path()?;
    storage::write_json(&path, history)
}

/// A history read that preserves a corrupt file instead of silently replacing
/// it. `corrupt` is true when the bytes on disk could not be parsed.
struct LoadedHistory {
    decisions: Vec<Decision>,
    corrupt: bool,
}

fn load_history_lenient() -> Result<LoadedHistory> {
    let path = history_path()?;
    if !path.exists() {
        return Ok(LoadedHistory {
            decisions: Vec::new(),
            corrupt: false,
        });
    }

    let raw = std::fs::read_to_string(&path)?;
    if raw.trim().is_empty() {
        return Ok(LoadedHistory {
            decisions: Vec::new(),
            corrupt: false,
        });
    }

    match serde_json::from_str::<Vec<Decision>>(&raw) {
        Ok(decisions) => Ok(LoadedHistory {
            decisions,
            corrupt: false,
        }),
        Err(error) => {
            quarantine_corrupt_history(&path, &raw, &error)?;
            Ok(LoadedHistory {
                decisions: Vec::new(),
                corrupt: true,
            })
        }
    }
}

/// Copy a corrupt `history.json` to `history.corrupt-<timestamp>.json` before
/// anything can replace it. Repeated reads of the same corrupt bytes reuse the
/// first copy.
fn quarantine_corrupt_history(path: &Path, raw: &str, error: &serde_json::Error) -> Result<()> {
    let fingerprint = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut hasher);
        raw.hash(&mut hasher);
        hasher.finish()
    };

    {
        let mut seen = QUARANTINED_HISTORY_PATHS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if seen.contains(&fingerprint) {
            return Ok(());
        }
        seen.push(fingerprint);
    }

    let stamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let target = path.with_file_name(format!("history.corrupt-{}.json", stamp));
    if let Err(copy_error) = std::fs::copy(path, &target) {
        // Let a later attempt retry the copy instead of believing the evidence
        // was preserved.
        if let Ok(mut seen) = QUARANTINED_HISTORY_PATHS.lock() {
            seen.retain(|value| *value != fingerprint);
        }
        return Err(copy_error.into());
    }

    crate::logging::warn(&format!(
        "Corrupt permission history at {} ({}); preserved a copy at {} and continuing with an empty history",
        path.display(),
        error,
        target.display()
    ));
    Ok(())
}

/// Exclusive cross-process guard around decision read-modify-write.
struct DecisionLock {
    file: std::fs::File,
}

impl Drop for DecisionLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn decisions_lock_path() -> Result<PathBuf> {
    Ok(storage::horon_agent_dir()?.join("approvals").join(".decisions.lock"))
}

fn acquire_decisions_lock() -> Result<DecisionLock> {
    let path = decisions_lock_path()?;
    if let Some(parent) = path.parent() {
        storage::ensure_dir(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;

    let mut delay = DECISION_LOCK_BASE_DELAY;
    for attempt in 0..DECISION_LOCK_MAX_ATTEMPTS {
        match file.try_lock() {
            Ok(()) => return Ok(DecisionLock { file }),
            Err(std::fs::TryLockError::WouldBlock) => {
                if attempt + 1 == DECISION_LOCK_MAX_ATTEMPTS {
                    break;
                }
                std::thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_millis(20));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }

    anyhow::bail!(
        "permission decision lock {} stayed busy for {} attempts",
        path.display(),
        DECISION_LOCK_MAX_ATTEMPTS
    )
}

/// Append a decision to an already-loaded history, keeping the first decision
/// recorded for a request id. The caller must hold the decision lock.
fn append_decision_to_history(
    loaded: &mut LoadedHistory,
    request_id: &str,
    approved: bool,
    via: &str,
    message: Option<String>,
) -> Result<RecordDecisionOutcome> {
    if let Some(existing) = loaded.decisions.iter().find(|d| d.request_id == request_id).cloned() {
        return Ok(RecordDecisionOutcome::AlreadyDecided { existing });
    }

    // A corrupt history means the decisions before it are unknown, so no
    // decision recorded against it can be audited: the corrupt state always
    // records a denial, whatever the requested outcome was.
    let (approved, via, message) = if loaded.corrupt {
        (
            false,
            CORRUPT_HISTORY_VIA.to_string(),
            Some("Denied because the decision history was corrupt and the approval could not be audited.".to_string()),
        )
    } else {
        (approved, via.to_string(), message)
    };

    let decision = Decision {
        request_id: request_id.to_string(),
        approved,
        decided_at: Utc::now(),
        decided_via: via,
        message,
    };
    loaded.decisions.push(decision.clone());
    persist_history(&loaded.decisions)?;
    Ok(RecordDecisionOutcome::Recorded(decision))
}

/// Acquire the shared lock, read the history, and record `request_id` only when
/// it has no decision yet.
fn record_decision_on_disk(
    request_id: &str,
    approved: bool,
    via: &str,
    message: Option<String>,
) -> Result<RecordDecisionOutcome> {
    let _lock = acquire_decisions_lock()?;
    let mut loaded = load_history_lenient()?;
    append_decision_to_history(&mut loaded, request_id, approved, via, message)
}

// ---------------------------------------------------------------------------
// File-based permission decision (for external callers)
// ---------------------------------------------------------------------------

/// Record a permission decision by directly manipulating the queue/history JSON files.
/// Used by callers that have no access to the live SafetySystem instance.
///
/// Shares `~/.horon-agent/approvals/.decisions.lock` with
/// [`SafetySystem::record_decision`], so a request keeps exactly one decision
/// even when both paths race for it.
pub fn record_permission_via_file(request_id: &str, approved: bool, via: &str, message: Option<String>) -> Result<()> {
    let _lock = acquire_decisions_lock()?;

    let qp = queue_path()?;
    if let Some(parent) = qp.parent() {
        storage::ensure_dir(parent)?;
    }
    let mut queue: Vec<PermissionRequest> = if qp.exists() {
        storage::read_json(&qp).unwrap_or_default()
    } else {
        Vec::new()
    };
    queue.retain(|r| r.id != request_id);
    persist_queue(&queue)?;

    let mut loaded = load_history_lenient()?;
    let _ = append_decision_to_history(&mut loaded, request_id, approved, via, message)?;
    Ok(())
}

/// Expire stale permission requests directly via queue/history files.
/// Used by processes that don't hold the live SafetySystem instance.
pub fn expire_stale_permissions_via_file(via: &str) -> Result<Vec<String>> {
    let _lock = acquire_decisions_lock()?;

    let qp = queue_path()?;
    if let Some(parent) = qp.parent() {
        storage::ensure_dir(parent)?;
    }
    let queue: Vec<PermissionRequest> = if qp.exists() {
        storage::read_json(&qp).unwrap_or_default()
    } else {
        Vec::new()
    };

    let now = Utc::now();
    let mut expired: Vec<(PermissionRequest, String, bool)> = Vec::new();
    let mut retained: Vec<PermissionRequest> = Vec::with_capacity(queue.len());
    for request in queue {
        if let Some(reason) = stale_request_reason(&request) {
            expired.push((request, reason, true));
        } else if request.is_expired_at(now) {
            let deadline = request.expires_at.map(|at| at.to_rfc3339()).unwrap_or_default();
            expired.push((request, format!("its wait window elapsed at {}", deadline), false));
        } else {
            retained.push(request);
        }
    }

    if expired.is_empty() {
        return Ok(Vec::new());
    }

    let mut loaded = load_history_lenient()?;
    let mut expired_ids = Vec::with_capacity(expired.len());
    for (request, reason, session_expired) in expired {
        let message = if session_expired {
            format!("Expired automatically: {}. Original agent is no longer active.", reason)
        } else {
            format!("Expired automatically: {}.", reason)
        };
        match append_decision_to_history(&mut loaded, &request.id, false, via, Some(message)) {
            Ok(_) => expired_ids.push(request.id),
            // Leave the request queued when its decision could not be written.
            Err(_) => retained.push(request),
        }
    }
    persist_queue(&retained)?;

    Ok(expired_ids)
}

fn stale_request_reason(request: &PermissionRequest) -> Option<String> {
    let session_id = request_session_id(request)?;
    let mut session = match crate::session::Session::load(&session_id) {
        Ok(s) => s,
        Err(_) => return Some(format!("owner session '{}' was not found", session_id)),
    };

    // Refresh crash status based on PID if needed.
    if session.detect_crash() {
        let _ = session.save();
    }

    if session.status == crate::session::SessionStatus::Active {
        None
    } else {
        Some(format!(
            "owner session '{}' is {}",
            session_id,
            session.status.display()
        ))
    }
}

fn request_session_id(request: &PermissionRequest) -> Option<String> {
    let context = request.context.as_ref()?;

    context
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            context
                .get("requester")
                .and_then(|r| r.get("session_id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
}

// ---------------------------------------------------------------------------
// ID generation helper
// ---------------------------------------------------------------------------

/// Generate a unique permission request id: `req_{timestamp}_{random}`
pub fn new_request_id() -> String {
    crate::id::new_id("req")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "safety_tests.rs"]
mod tests;

/// Reading a queue or history file written before the approval fields
/// existed.
///
/// Its own module so the documented filter (`cargo test -p horon-agent-base --lib
/// safety::legacy_queue`) selects exactly the tests in this module.
#[cfg(test)]
mod legacy_queue {
    use super::tests::with_temp_home;
    use super::*;

    /// A queue entry written before `run_id`, `expires_at` and
    /// `default_on_timeout` existed must keep parsing, with fail-closed
    /// defaults rather than empty values.
    const LEGACY_QUEUE_JSON: &str = r#"[
        {
            "id": "req_legacy_1",
            "action": "push",
            "description": "Push to origin",
            "rationale": "Ready for review",
            "urgency": "low",
            "wait": true,
            "created_at": "2026-01-02T03:04:05Z",
            "context": { "session_id": "sess_legacy" }
        }
    ]"#;

    #[test]
    fn legacy_queue_without_new_fields_parses() {
        // Shape 1: the whole queue file (an array) written by an older build.
        let queue: Vec<PermissionRequest> =
            serde_json::from_str(LEGACY_QUEUE_JSON).expect("legacy queue file must still parse");
        assert_eq!(queue.len(), 1);
        let legacy = &queue[0];
        assert_eq!(legacy.id, "req_legacy_1");
        assert_eq!(legacy.urgency, Urgency::Low);
        assert!(legacy.wait);
        assert_eq!(legacy.run_id, None);
        assert_eq!(legacy.expires_at, None);
        assert_eq!(legacy.default_on_timeout, ApprovalDefault::Deny);

        // Shape 2: one request object, missing every new field and carrying an
        // unknown extra key that a newer writer may have added.
        let one: PermissionRequest = serde_json::from_str(
            r#"{
                "id": "req_legacy_2",
                "action": "bash",
                "description": "rm -rf build",
                "rationale": "Clean the tree",
                "urgency": "normal",
                "wait": false,
                "created_at": "2025-12-31T23:59:59Z",
                "context": null,
                "some_future_field": { "nested": [1, 2, 3] }
            }"#,
        )
        .expect("legacy request with an unknown extra field must parse");
        assert_eq!(one.run_id, None);
        assert_eq!(one.expires_at, None);
        assert_eq!(one.default_on_timeout, ApprovalDefault::Deny);
        assert_eq!(one.default_on_timeout.as_str(), "deny");
    }

    /// A `history.json` written before the approval-record changes must keep
    /// loading. The file is a plain array of `Decision` records, and that
    /// record has the same five fields at base commit `62ae3f1`, so a history
    /// from that build is still a valid history today.
    #[test]
    fn legacy_history_without_new_fields_parses() {
        with_temp_home(|| {
            let path = history_path().expect("history path");
            let dir = path.parent().expect("history directory");
            std::fs::create_dir_all(dir).expect("create the safety directory");
            std::fs::write(
                &path,
                r#"[
                    {
                        "request_id": "req_legacy_1",
                        "approved": true,
                        "decided_at": "2026-01-02T03:04:05Z",
                        "decided_via": "imap",
                        "message": "Approved from mail"
                    },
                    {
                        "request_id": "req_legacy_2",
                        "approved": false,
                        "decided_at": "2026-01-02T03:05:06Z",
                        "decided_via": "cli"
                    }
                ]"#,
            )
            .expect("write a pre-change history.json");

            let loaded = load_history_lenient().expect("legacy history must load");
            assert!(!loaded.corrupt, "a legacy history is not corrupt");
            assert_eq!(loaded.decisions.len(), 2);

            let approved = &loaded.decisions[0];
            assert_eq!(approved.request_id, "req_legacy_1");
            assert!(approved.approved);
            assert_eq!(approved.decided_via, "imap");
            assert_eq!(approved.message.as_deref(), Some("Approved from mail"));
            assert_eq!(approved.decided_at.to_rfc3339(), "2026-01-02T03:04:05+00:00");

            let denied = &loaded.decisions[1];
            assert_eq!(denied.request_id, "req_legacy_2");
            assert!(!denied.approved);
            assert_eq!(denied.decided_via, "cli");
            assert_eq!(denied.message, None);
        });
    }
}
