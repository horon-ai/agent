//! Single-flight background compaction jobs (plan D7).
//!
//! A session may run at most one summarization job at a time. The job captures
//! the active prefix it is summarizing, the route identity (provider/model) and
//! the settings digest it was planned under, plus the compaction generation at
//! that moment. When the provider answers, the result is applied only after the
//! live state is revalidated under the lock:
//!
//! * the generation and settings identity must be unchanged,
//! * `compacted_count` must be unchanged,
//! * the captured prefix must still be a prefix of the active slice, so only
//!   tail appends happened since the job started,
//! * the prior summary must be the same one the job built on,
//! * the job must not have exceeded its deadline or been applied already.
//!
//! A rewind, clear, revision, import, model change or another compaction that
//! moves the boundary bumps the generation and cancels the job, so a late
//! result is discarded and the originals are preserved.
//!
//! This module owns the policy and the state machine only; the provider call
//! and the transcript live in [`super::CompactionManager`].

use crate::message::{Message, extend_stable_hash, stable_message_hash};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::time::Instant;

pub const JOB_DEADLINE: Duration = Duration::from_secs(900);
pub const STREAM_IDLE_DEADLINE: Duration = Duration::from_secs(300);
pub const PROACTIVE_COOLDOWN: Duration = Duration::from_secs(30);
pub const LEAD_MIN_TOKENS: usize = 8_192;
pub const LEAD_MAX_TOKENS: usize = 32_768;
pub const LEAD_RATIO: f64 = 0.125;
pub const IDLE_MIN_USAGE: f32 = 0.5;
pub const MAX_TRANSIENT_RETRIES: u32 = 3;
pub const MAX_SHRINK_ATTEMPTS: u32 = 3;
pub const SHRINK_DEADLINE: Duration = Duration::from_secs(240);
pub const BREAKER_THRESHOLD: u32 = 3;
pub const BREAKER_SUSPENSION: Duration = Duration::from_secs(60);
pub const IDLE_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// How far ahead of `threshold_tokens` a background job starts.
pub fn lead_tokens(threshold_tokens: usize) -> usize {
    let scaled = (threshold_tokens as f64 * LEAD_RATIO).round() as usize;
    scaled.clamp(LEAD_MIN_TOKENS, LEAD_MAX_TOKENS)
}

/// SHA-256 of a text, hex encoded. Used for settings and prior-summary identity.
pub fn text_digest(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

/// Stable digest of a message prefix.
///
/// Each message contributes its [`stable_message_hash`]; the chain is hashed so
/// reordering, insertion or a revision anywhere inside the prefix changes the
/// digest. Timestamps participate: a rewritten message is a different prefix
/// even when the body text is identical.
pub fn prefix_digest(messages: &[Message]) -> String {
    let mut hasher = Sha256::new();
    let mut chain = 0u64;
    for message in messages {
        chain = extend_stable_hash(chain, stable_message_hash(message));
        hasher.update(chain.to_le_bytes());
    }
    hex::encode(hasher.finalize())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobTrigger {
    Reactive,
    Proactive,
    /// Only starts in a persistent serve/api-bridge runtime.
    Idle,
    /// Explicit `/compact`; bypasses the cooldown and breaker.
    Manual,
}

impl JobTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reactive => "reactive",
            Self::Proactive => "proactive",
            Self::Idle => "idle",
            Self::Manual => "manual",
        }
    }

    /// Whether the config `background` switch governs this trigger.
    pub fn is_background(self) -> bool {
        matches!(self, Self::Proactive | Self::Idle)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelReason {
    Rewind,
    Clear,
    Revision,
    ImportDivergence,
    ModelChange,
    Superseded,
    Shutdown,
    Manual,
    Deadline,
}

impl CancelReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rewind => "rewind",
            Self::Clear => "clear",
            Self::Revision => "revision",
            Self::ImportDivergence => "import_divergence",
            Self::ModelChange => "model_change",
            Self::Superseded => "superseded",
            Self::Shutdown => "shutdown",
            Self::Manual => "manual",
            Self::Deadline => "deadline",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscardReason {
    GenerationChanged,
    CompactedCountChanged,
    PrefixChanged,
    PriorSummaryChanged,
    JobExpired,
    AlreadyApplied,
    TailBudgetExceeded,
}

impl DiscardReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GenerationChanged => "generation_changed",
            Self::CompactedCountChanged => "compacted_count_changed",
            Self::PrefixChanged => "prefix_changed",
            Self::PriorSummaryChanged => "prior_summary_changed",
            Self::JobExpired => "job_expired",
            Self::AlreadyApplied => "already_applied",
            Self::TailBudgetExceeded => "tail_budget_exceeded",
        }
    }
}

/// Identity a job was planned under. Any change invalidates the result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobIdentity {
    pub generation: u64,
    pub provider: String,
    pub model: String,
    pub settings_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedPrefix {
    /// Offset of the captured active slice.
    pub compacted_count: usize,
    pub active_len: usize,
    pub covered: usize,
    pub digest: String,
    pub prior_digest: Option<String>,
}

impl CapturedPrefix {
    pub fn capture(active: &[Message], compacted_count: usize, covered: usize, prior_summary: Option<&str>) -> Self {
        Self {
            compacted_count,
            active_len: active.len(),
            covered: covered.min(active.len()),
            digest: prefix_digest(active),
            prior_digest: prior_summary.map(text_digest),
        }
    }

    /// True when `active` still starts with the captured slice, so only tail
    /// appends happened since capture.
    pub fn tail_append_only(&self, active: &[Message]) -> bool {
        active.len() >= self.active_len && prefix_digest(&active[..self.active_len]) == self.digest
    }
}

/// One running (or finished but unapplied) compaction job.
#[derive(Clone, Debug)]
pub struct CompactionJob {
    pub id: u64,
    pub identity: JobIdentity,
    pub prefix: CapturedPrefix,
    pub trigger: JobTrigger,
    started_at: Instant,
    last_progress_at: Instant,
    transient_retries: u32,
}

impl CompactionJob {
    fn start(id: u64, identity: JobIdentity, prefix: CapturedPrefix, trigger: JobTrigger, now: Instant) -> Self {
        Self {
            id,
            identity,
            prefix,
            trigger,
            started_at: now,
            last_progress_at: now,
            transient_retries: 0,
        }
    }

    pub fn started_at(&self) -> Instant {
        self.started_at
    }

    pub fn job_deadline(&self) -> Instant {
        self.started_at + JOB_DEADLINE
    }

    /// The deadline that actually applies: the earlier of the job ceiling and
    /// the stream-idle cap measured from the last observed progress.
    pub fn effective_deadline(&self) -> Instant {
        let idle = self.last_progress_at + STREAM_IDLE_DEADLINE;
        let job = self.job_deadline();
        if idle < job { idle } else { job }
    }

    pub fn record_progress(&mut self, now: Instant) {
        self.last_progress_at = now;
    }

    pub fn transient_retries(&self) -> u32 {
        self.transient_retries
    }

    /// Consume one transient retry. Returns false once the budget is spent.
    pub fn record_transient_retry(&mut self) -> bool {
        if self.transient_retries >= MAX_TRANSIENT_RETRIES {
            return false;
        }
        self.transient_retries += 1;
        true
    }

    pub fn expired(&self, now: Instant) -> bool {
        now >= self.effective_deadline()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartRefusal {
    JobInFlight,
    BackgroundDisabled,
    BreakerSuspended,
    Cooldown,
    NotPersistentRuntime,
    BelowIdleUsage,
    BelowLead,
    NoPlan,
}

impl StartRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::JobInFlight => "job_in_flight",
            Self::BackgroundDisabled => "background_disabled",
            Self::BreakerSuspended => "breaker_suspended",
            Self::Cooldown => "cooldown",
            Self::NotPersistentRuntime => "not_persistent_runtime",
            Self::BelowIdleUsage => "below_idle_usage",
            Self::BelowLead => "below_lead",
            Self::NoPlan => "no_plan",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StartGate {
    pub trigger: JobTrigger,
    /// `[compaction] background`; only governs proactive and idle triggers.
    pub background_enabled: bool,
    pub persistent_runtime: bool,
    pub usage: f32,
    pub effective_tokens: usize,
    pub threshold_tokens: usize,
    pub cooldown_ready: bool,
    pub breaker_allows: bool,
}

impl StartGate {
    /// Token count at which a background job starts: the trigger minus the lead.
    pub fn lead_start_tokens(&self) -> usize {
        self.threshold_tokens.saturating_sub(lead_tokens(self.threshold_tokens))
    }

    /// First condition that forbids the start, if any. Manual jobs bypass every
    /// policy gate; the single-flight slot is checked by the registry.
    pub fn refusal(&self) -> Option<StartRefusal> {
        if self.trigger == JobTrigger::Manual {
            return None;
        }
        if self.trigger.is_background() && !self.background_enabled {
            return Some(StartRefusal::BackgroundDisabled);
        }
        if !self.breaker_allows {
            return Some(StartRefusal::BreakerSuspended);
        }
        if !self.cooldown_ready {
            return Some(StartRefusal::Cooldown);
        }
        match self.trigger {
            JobTrigger::Idle => {
                if !self.persistent_runtime {
                    return Some(StartRefusal::NotPersistentRuntime);
                }
                if self.usage < IDLE_MIN_USAGE {
                    return Some(StartRefusal::BelowIdleUsage);
                }
                if self.effective_tokens < self.lead_start_tokens() {
                    return Some(StartRefusal::BelowLead);
                }
            }
            JobTrigger::Proactive => {
                if self.effective_tokens < self.lead_start_tokens() {
                    return Some(StartRefusal::BelowLead);
                }
            }
            _ => {}
        }
        None
    }
}

/// Consecutive-failure breaker: after [`BREAKER_THRESHOLD`] failures automatic
/// attempts pause for [`BREAKER_SUSPENSION`]; a manual `/compact` still runs.
#[derive(Clone, Debug, Default)]
pub struct FailureBreaker {
    consecutive: u32,
    suspended_until: Option<Instant>,
}

impl FailureBreaker {
    pub fn allows(&self, now: Instant) -> bool {
        self.suspended_until.is_none_or(|until| now >= until)
    }

    pub fn record_success(&mut self) {
        self.consecutive = 0;
        self.suspended_until = None;
    }

    /// Record a failed job. Returns true when this failure tripped the breaker.
    pub fn record_failure(&mut self, now: Instant) -> bool {
        self.consecutive = self.consecutive.saturating_add(1);
        if self.consecutive >= BREAKER_THRESHOLD {
            self.suspended_until = Some(now + BREAKER_SUSPENSION);
            true
        } else {
            false
        }
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive
    }

    pub fn suspended_until(&self) -> Option<Instant> {
        self.suspended_until
    }
}

#[derive(Clone, Debug, Default)]
pub struct Cooldown {
    last_started: Option<Instant>,
}

impl Cooldown {
    pub fn ready(&self, now: Instant) -> bool {
        self.last_started
            .is_none_or(|last| now.saturating_duration_since(last) >= PROACTIVE_COOLDOWN)
    }

    pub fn mark(&mut self, now: Instant) {
        self.last_started = Some(now);
    }

    pub fn last_started(&self) -> Option<Instant> {
        self.last_started
    }
}

#[derive(Clone, Debug)]
pub struct CancelledJob {
    pub job: CompactionJob,
    pub reason: CancelReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobOutcomeKind {
    Applied,
    Discarded(DiscardReason),
    Failed,
    Cancelled(CancelReason),
    TimedOut,
}

impl JobOutcomeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Discarded(reason) => reason.as_str(),
            Self::Failed => "failed",
            Self::Cancelled(reason) => reason.as_str(),
            Self::TimedOut => "timed_out",
        }
    }

    pub fn applied(self) -> bool {
        matches!(self, Self::Applied)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobReport {
    pub job_id: u64,
    pub trigger: JobTrigger,
    pub generation: u64,
    pub outcome: JobOutcomeKind,
    pub covered: usize,
}

#[derive(Debug, Default)]
pub struct SingleFlight {
    current: Option<CompactionJob>,
    next_id: u64,
    last_applied: Option<u64>,
    cooldown: Cooldown,
    breaker: FailureBreaker,
}

impl SingleFlight {
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim the single slot. One job per session: a second start is refused
    /// while a job is live, so the same prefix is never charged twice.
    ///
    /// Cooldown and breaker are read from this slot rather than from the
    /// caller, so a stale gate snapshot can never bypass them.
    pub fn begin(
        &mut self,
        gate: &StartGate,
        identity: JobIdentity,
        prefix: CapturedPrefix,
        now: Instant,
    ) -> Result<u64, StartRefusal> {
        if self.current.is_some() {
            return Err(StartRefusal::JobInFlight);
        }
        if gate.trigger != JobTrigger::Manual {
            if !self.breaker.allows(now) {
                return Err(StartRefusal::BreakerSuspended);
            }
            if gate.trigger.is_background() && !gate.background_enabled {
                return Err(StartRefusal::BackgroundDisabled);
            }
            if gate.trigger.is_background() && !self.cooldown.ready(now) {
                return Err(StartRefusal::Cooldown);
            }
        }
        if let Some(refusal) = gate.refusal() {
            return Err(refusal);
        }
        self.next_id = self.next_id.saturating_add(1);
        let id = self.next_id;
        self.current = Some(CompactionJob::start(id, identity, prefix, gate.trigger, now));
        self.cooldown.mark(now);
        Ok(id)
    }

    pub fn current(&self) -> Option<&CompactionJob> {
        self.current.as_ref()
    }

    pub fn is_running(&self) -> bool {
        self.current.is_some()
    }

    pub fn last_applied(&self) -> Option<u64> {
        self.last_applied
    }

    pub fn finish(&mut self, job_id: u64) -> Option<CompactionJob> {
        if self.current.as_ref().is_some_and(|job| job.id == job_id) {
            self.current.take()
        } else {
            None
        }
    }

    pub fn cancel(&mut self, reason: CancelReason) -> Option<CancelledJob> {
        let job = self.current.take()?;
        Some(CancelledJob { job, reason })
    }

    pub fn record_success(&mut self) {
        self.breaker.record_success();
    }

    pub fn record_failure(&mut self, now: Instant) -> bool {
        self.breaker.record_failure(now)
    }

    pub fn mark_applied(&mut self, job_id: u64) {
        self.last_applied = Some(job_id);
    }

    /// Decide whether a finished result may still be applied.
    ///
    /// `history` is `None` when the caller has no message slice (legacy
    /// no-history poll); the prefix and prior-summary checks are then skipped
    /// and only the identity, boundary and once-only guards apply.
    pub fn revalidate(
        &self,
        job: &CompactionJob,
        history: Option<&[Message]>,
        compacted_count: usize,
        prior_summary: Option<&str>,
        current: &JobIdentity,
        now: Instant,
    ) -> Result<usize, DiscardReason> {
        if self.last_applied == Some(job.id) {
            return Err(DiscardReason::AlreadyApplied);
        }
        if &job.identity != current {
            return Err(DiscardReason::GenerationChanged);
        }
        if job.prefix.compacted_count != compacted_count {
            return Err(DiscardReason::CompactedCountChanged);
        }
        if let Some(history) = history
            && !job.prefix.tail_append_only(history)
        {
            return Err(DiscardReason::PrefixChanged);
        }
        if job.prefix.prior_digest.as_deref() != prior_summary.map(text_digest).as_deref() {
            return Err(DiscardReason::PriorSummaryChanged);
        }
        if job.expired(now) {
            return Err(DiscardReason::JobExpired);
        }
        Ok(job.prefix.covered)
    }

    pub fn cooldown(&self) -> &Cooldown {
        &self.cooldown
    }

    pub fn breaker(&self) -> &FailureBreaker {
        &self.breaker
    }
}
