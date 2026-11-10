//! Background compaction for conversation context management
//!
//! When context reaches 80% of the limit, kicks off background summarization.
//! User continues chatting while summary is generated. When ready, seamlessly
//! swaps in the compacted context.
//!
//! The CompactionManager does NOT store its own copy of messages. Instead,
//! callers pass `&[Message]` references when needed. The manager tracks how
//! many messages from the front have been compacted via `compacted_count`.
//!
//! ## Compaction Modes
//!
//! - **Reactive** (default): compact when context hits a fixed threshold (80%).
//! - **Proactive**: compact early based on predicted EWMA token growth rate.
//! - **Semantic**: compact based on embedding-detected topic shifts and
//!   relevance scoring. Falls back to proactive if embeddings are unavailable.

use crate::message::{ContentBlock, Message, Role};
use crate::provider::Provider;
use crate::provider::openai_request::{
    openai_encrypted_content_fallback_summary, openai_encrypted_content_is_sendable,
};
use anyhow::Result;
use horon_agent_compaction_core::NativeRouteBinding;
use source_projection::SourceProjection;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;
use summary::{PriorSummary, SummaryLimits};
use tokio::task::JoinHandle;

pub use horon_agent_compaction_core::pruning;
pub use horon_agent_compaction_core::{
    CHARS_PER_TOKEN_UPPER_BOUND, CRITICAL_THRESHOLD, CompactionAction, CompactionEvent, CompactionStats, ContextBudget,
    ContextBudgetError, DEFAULT_RECENT_TOKENS, DEFAULT_RESERVE_TOKENS, DEFAULT_TOKEN_BUDGET, EMBED_MAX_CHARS_PER_MSG,
    EMBEDDING_HISTORY_WINDOW, EMERGENCY_IMAGE_MAX_CHARS, EMERGENCY_TOOL_RESULT_MAX_CHARS, MANUAL_COMPACT_MIN_THRESHOLD,
    MIN_TURNS_TO_KEEP, PAYLOAD_IMAGE_CHAR_BUDGET, SEMANTIC_EMBED_CACHE_CAPACITY, SUMMARY_PROMPT,
    SYSTEM_OVERHEAD_TOKENS, Summary, TOKEN_HISTORY_WINDOW, base_context_ratio, build_compaction_prompt,
    build_emergency_summary_text, compacted_summary_text_block, compute_context_budget, conservative_context_tokens,
    content_char_count, effective_context_tokens_from_usage, effective_context_tokens_from_usage_with,
    emergency_strip_large_images, emergency_truncate_large_payloads, estimate_compaction_tokens_from_text,
    is_image_rejection_error, is_request_payload_too_large_error, mean_embedding, message_char_count,
    message_token_count, recent_cutoff_for_token_budget, recent_tokens_for_window, safe_compaction_cutoff,
    semantic_cache_key, semantic_goal_text, semantic_message_text, strip_large_images_in_contents,
    summary_payload_char_count,
};
use horon_agent_config_types::CompactionStrategy;

pub mod handoff;
pub mod jobs;
pub mod recovery;
pub mod source_projection;
pub mod summary;

const HARD_THRESHOLD_PENDING_WAIT_MS: u64 = 15_000;
const HARD_THRESHOLD_PENDING_POLL_MS: u64 = 50;

/// Requests estimated below this are not used as calibration samples.
const CALIBRATION_MIN_ESTIMATE_TOKENS: u64 = 4_096;
/// One sample's `observed / estimated` ratio is clamped to this range so a
/// single outlier (cache metadata, a tool-schema spike) cannot swing it.
const CALIBRATION_MIN_SAMPLE: f64 = 0.6;
const CALIBRATION_MAX_SAMPLE: f64 = 1.8;
/// EWMA weight of the newest sample.
const CALIBRATION_ALPHA: f64 = 0.3;
/// The calibrated estimate never drops below this share of the raw estimate.
const CALIBRATION_FLOOR: f64 = 0.75;

/// Result from background compaction task
struct CompactionResult {
    /// Job that produced this result; a result may only be applied by the job
    /// that owns the slot and only once.
    job_id: u64,
    summary_text: String,
    openai_encrypted_content: Option<String>,
    /// Ordered provider-native window retained whole; empty for structured
    /// summaries.
    native_window: Vec<serde_json::Value>,
    /// Route that produced `native_window`.
    native_route: Option<crate::session::StoredNativeRoute>,
    duration_ms: u64,
    summarized_messages: usize,
    /// Messages actually covered by the summary. Shrinking on overflow can make
    /// this smaller than the cutoff the job started with; the difference stays
    /// in the live tail.
    covered_messages: usize,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    restoration: Option<summary::RestorationManifest>,
}

/// Why a job's provider call did not produce a result.
#[derive(Debug)]
struct JobFailure {
    kind: jobs::JobOutcomeKind,
    detail: String,
}

impl std::fmt::Display for JobFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind.as_str(), self.detail)
    }
}

impl std::error::Error for JobFailure {}

struct CompactionOutcomeLog<'a> {
    trigger: &'a str,
    pre_tokens: u64,
    post_tokens: u64,
    messages_compacted: usize,
    messages_dropped: Option<usize>,
    duration_ms: u64,
    all_messages: &'a [Message],
}

struct HardThresholdWait {
    waited_ms: u64,
    applied: bool,
    timed_out: bool,
}

/// Rolling character-count estimate for the active (non-compacted) message
/// suffix.
///
/// Token estimation needs the size of the live message tail without rescanning
/// the entire history on every call, so this caches that sum next to a dirty
/// flag. The value and the flag must always move together: previously they were
/// two independent `CompactionManager` fields, and a code path that updated one
/// without the other silently corrupted token accounting. Keeping the raw
/// fields private and forcing every mutation through these named operations
/// makes that class of bug unrepresentable.
#[derive(Debug, Clone, Default)]
struct ActiveCharEstimate {
    chars: usize,
    dirty: bool,
}

impl ActiveCharEstimate {
    /// The currently cached character count. Only trustworthy when not dirty;
    /// readers must consult [`Self::is_dirty`] (and any external invariants)
    /// before relying on it.
    fn value(&self) -> usize {
        self.chars
    }

    /// Whether the cached value is stale and must be recomputed from history.
    fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Mark the cached value stale so the next read recomputes from history.
    fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// Record an exact, freshly computed count as the trusted value.
    fn set_exact(&mut self, chars: usize) {
        self.chars = chars;
        self.dirty = false;
    }

    /// Extend a trusted count by a newly appended message's characters.
    ///
    /// Mirrors the append-only fast path: the prior value is assumed accurate,
    /// so the running sum stays trusted (dirty cleared).
    fn append_exact(&mut self, chars: usize) {
        self.chars = self.chars.saturating_add(chars);
        self.dirty = false;
    }

    /// Reset to zero after a restore/clamp. Stays dirty when there may be active
    /// messages whose characters have not been measured yet.
    fn reset_pending(&mut self, maybe_has_active: bool) {
        self.chars = 0;
        self.dirty = maybe_has_active;
    }
}

/// Manages background compaction of conversation context.
///
/// Does NOT own message data. The caller owns the messages and passes
/// references into methods that need them. After compaction, the manager
/// records `compacted_count` — the number of leading messages that have
/// been summarized and should be skipped when building API payloads.
pub struct CompactionManager {
    /// Number of leading messages that have been compacted into the summary.
    /// When building API messages, skip the first `compacted_count` messages.
    compacted_count: usize,

    /// Active summary (if we've compacted before)
    active_summary: Option<Summary>,

    /// Rolling char estimate for the active (non-compacted) message suffix.
    ///
    /// In the common append-only case this is maintained incrementally, so token
    /// estimation does not need to rescan the entire active history every time.
    /// Bundled with its own dirty flag so the value and staleness can never
    /// drift apart (see [`ActiveCharEstimate`]).
    active_chars: ActiveCharEstimate,

    /// Background compaction task handle
    pending_task: Option<JoinHandle<Result<CompactionResult, JobFailure>>>,

    /// Cancellation signal for `pending_task`; sent before the task is aborted
    /// so a cooperative runner can stop without charging another attempt.
    pending_cancel: Option<tokio::sync::oneshot::Sender<()>>,

    /// Identity and captured prefix of the job behind `pending_task`.
    pending_job: Option<jobs::CompactionJob>,

    /// Monotonic compaction generation. Bumped when the captured prefix can no
    /// longer be trusted (rewind/clear/revision/import/model change) and after
    /// every applied boundary, so a late result from an older generation is
    /// discarded instead of applied.
    generation: u64,

    /// Single-flight job slot plus cooldown and failure-breaker state.
    job_slot: jobs::SingleFlight,

    /// Last job disposition, consumed by callers and tests.
    last_job_report: Option<jobs::JobReport>,

    /// `[compaction] background`: proactive and idle jobs. Reactive and manual
    /// compaction stay available when this is off.
    background_enabled: bool,

    /// Last observed provider route, used to detect a model change.
    route_identity: Option<(String, String)>,

    /// API method/runtime route (OAuth vs API key) of the last observed route.
    ///
    /// A native compaction window is bound to the exact route that produced it,
    /// so a credential-mode switch on the same model also invalidates replay.
    route_identity_method: Option<String>,

    /// User-facing trigger label for the currently running background compaction.
    pending_trigger: Option<String>,

    /// Turn index (relative to uncompacted messages) where pending compaction will cut off
    pending_cutoff: usize,

    /// Total turns seen (for tracking)
    total_turns: usize,

    /// When true, session restore/reseed has just loaded old history and
    /// compaction must stay disabled until a genuinely new message is added.
    suppress_compaction_until_new_message: bool,

    /// Token budget
    token_budget: usize,

    /// Model window before the operator cap, retained so removing/raising a cap
    /// can restore the budget without switching models.
    model_token_budget: usize,

    /// Provider-reported input token usage from the latest request.
    /// Used to trigger compaction with real token counts instead of only heuristics.
    observed_input_tokens: Option<u64>,

    /// Local estimate of the request most recently sent, consumed by the next
    /// provider usage report to produce one calibration sample.
    request_estimate_tokens: Option<u64>,

    /// Smoothed `observed / estimated` ratio for the current route. Corrects the
    /// script-aware estimator for the active tokenizer and request overhead;
    /// reset when the provider route changes.
    calibration_ratio: Option<f64>,

    /// True when the last API projection had to emergency-truncate a payload,
    /// so earlier tool output the model saw is no longer verbatim in context.
    projection_truncated: bool,

    /// Last compaction event (if any)
    last_compaction: Option<CompactionEvent>,

    /// Provider usage of the request that produced `active_summary`.
    active_summary_usage: (Option<u64>, Option<u64>),

    /// Bounded restoration manifest captured with `active_summary`.
    active_restoration: Option<crate::session::StoredRestorationManifest>,

    /// True when `active_summary` came from the deterministic fallback or an
    /// emergency hard compact instead of a provider summary. A resumed session
    /// must not present this as a complete summary.
    active_lossy: bool,

    /// Durable refusal of automatic context recovery for one exact source,
    /// provider, model and settings identity.
    recovery_blocked: Option<recovery::RecoveryBlocked>,

    /// Last recovery decision, consumed by callers that surface it.
    last_recovery: Option<recovery::RecoveryOutcome>,

    // ── Mode & strategy ────────────────────────────────────────────────────
    /// Active compaction mode (set from config at construction)
    mode: crate::config::CompactionMode,

    /// Config snapshot for mode-specific parameters
    compaction_config: crate::config::CompactionConfig,

    // ── Ownership & context budget (D6) ────────────────────────────────────
    /// Which engine owns compaction. `structured` runs horon's summarizer; the
    /// `openai_*` strategies leave compaction to the provider, so the
    /// structured trigger never fires for the same turn.
    strategy: CompactionStrategy,

    /// The strategy the configuration selected, before any provider override.
    ///
    /// Kept so a provider that stops owning compaction hands control back to the
    /// configured engine instead of freezing on the provider's last answer.
    configured_strategy: CompactionStrategy,

    /// Budget derived from the current window: reserve, trigger threshold, and
    /// the recent-tail token allowance.
    context_budget: Option<ContextBudget>,

    /// Why the current window cannot hold a budget, if it cannot. While this is
    /// set, no compaction plan can be made and no provider call is issued.
    context_budget_error: Option<ContextBudgetError>,

    /// Saving ratio (`tokens_saved / pre_tokens`) of the last compaction, used
    /// to adapt the trigger ratio up or down.
    prior_saving_ratio: Option<f64>,

    // ── Proactive mode state ───────────────────────────────────────────────
    /// Rolling window of observed token counts, one entry per turn snapshot.
    /// Used to compute EWMA growth rate for proactive compaction.
    token_history: VecDeque<u64>,

    /// Total turns elapsed since the last successful compaction.
    /// Used as a cooldown anti-signal.
    turns_since_last_compact: usize,

    // ── Semantic mode state ────────────────────────────────────────────────
    /// Per-turn embedding snapshots for topic-shift detection.
    /// Each entry is the L2-normalized embedding of the last assistant message
    /// of that turn (truncated to EMBED_MAX_CHARS_PER_MSG for speed).
    embedding_history: VecDeque<Vec<f32>>,

    /// Local cache for semantic compaction embeddings keyed by truncated-text hash.
    /// Stores both successful embeddings and failed lookups (`None`) so repeated
    /// semantic scans do not redo the same work.
    semantic_embed_cache: HashMap<u64, (Option<Vec<f32>>, u64)>,

    /// Monotonic recency counter for the semantic embedding cache LRU.
    semantic_embed_cache_counter: u64,
}

impl CompactionManager {
    pub fn new() -> Self {
        let cfg = crate::config::config().compaction.clone();
        let mode = cfg.mode.clone();
        let strategy = cfg.resolved_strategy(&crate::config::config().provider);
        let mut manager = Self {
            compacted_count: 0,
            active_summary: None,
            active_chars: ActiveCharEstimate::default(),
            pending_task: None,
            pending_cancel: None,
            pending_job: None,
            generation: 0,
            job_slot: jobs::SingleFlight::new(),
            last_job_report: None,
            background_enabled: cfg.background,
            route_identity: None,
            route_identity_method: None,
            pending_trigger: None,
            pending_cutoff: 0,
            total_turns: 0,
            suppress_compaction_until_new_message: false,
            token_budget: Self::capped_budget(&cfg, DEFAULT_TOKEN_BUDGET),
            model_token_budget: DEFAULT_TOKEN_BUDGET,
            observed_input_tokens: None,
            request_estimate_tokens: None,
            calibration_ratio: None,
            projection_truncated: false,
            last_compaction: None,
            active_summary_usage: (None, None),
            active_restoration: None,
            active_lossy: false,
            recovery_blocked: None,
            last_recovery: None,
            mode,
            compaction_config: cfg,
            strategy,
            configured_strategy: strategy,
            context_budget: None,
            context_budget_error: None,
            prior_saving_ratio: None,
            token_history: VecDeque::with_capacity(TOKEN_HISTORY_WINDOW + 1),
            turns_since_last_compact: 0,
            embedding_history: VecDeque::new(),
            semantic_embed_cache: HashMap::new(),
            semantic_embed_cache_counter: 0,
        };
        manager.refresh_context_cap();
        manager
    }

    /// Reset all compaction state
    pub fn reset(&mut self) {
        // Dropping the JoinHandle alone would detach the provider call: it keeps
        // running, still charges tokens, and its result is unreachable. Cancel
        // and abort it, then start over with the next generation.
        self.cancel_pending(jobs::CancelReason::Superseded);
        let generation = self.generation.wrapping_add(1);
        *self = Self::new();
        self.generation = generation;
    }

    /// Invalidate every in-flight and future result built on the current
    /// prefix, and cancel the running job. Used for rewind/clear/revision/
    /// import divergence/model changes.
    pub fn invalidate_generation(&mut self, reason: jobs::CancelReason) {
        self.cancel_pending(reason);
        self.generation = self.generation.wrapping_add(1);
    }

    /// Cancel the running job (if any) without touching the generation.
    pub fn cancel_pending(&mut self, reason: jobs::CancelReason) -> bool {
        if let Some(cancel) = self.pending_cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(task) = self.pending_task.take() {
            task.abort();
        }
        let cancelled = self.pending_job.take();
        if let Some(job) = self.job_slot.cancel(reason) {
            self.last_job_report = Some(jobs::JobReport {
                job_id: job.job.id,
                trigger: job.job.trigger,
                generation: job.job.identity.generation,
                outcome: jobs::JobOutcomeKind::Cancelled(reason),
                covered: job.job.prefix.covered,
            });
        }
        self.pending_cutoff = 0;
        self.pending_trigger = None;
        cancelled.is_some()
    }

    /// Current compaction generation.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Identity a job started now would capture.
    pub fn current_job_identity(&self) -> jobs::JobIdentity {
        let (provider, model) = self
            .route_identity
            .clone()
            .unwrap_or_else(|| (String::new(), String::new()));
        jobs::JobIdentity {
            generation: self.generation,
            provider,
            model,
            settings_digest: self.settings_digest(),
        }
    }

    /// Digest of the knobs a summary was planned under. A change makes an
    /// in-flight result stale because its payload would claim settings that no
    /// longer hold.
    pub fn settings_digest(&self) -> String {
        let mut text = String::new();
        text.push_str(self.strategy.as_str());
        text.push('|');
        text.push_str(self.mode.as_str());
        text.push('|');
        text.push_str(&self.token_budget.to_string());
        text.push('|');
        text.push_str(&self.compaction_config.reserve_tokens.to_string());
        text.push('|');
        text.push_str(&self.compaction_config.recent_tokens.to_string());
        text.push('|');
        text.push_str(&self.compaction_config.max_context_tokens.to_string());
        jobs::text_digest(&text)
    }

    /// Observe the active provider route. A model or provider change bumps the
    /// generation and cancels any job planned under the old identity.
    pub fn observe_route_identity(&mut self, provider: &str, model: &str) -> bool {
        self.observe_route_identity_with_route(provider, model, None)
    }

    /// Observe the active provider route including its API method.
    ///
    /// The API method is part of the identity because an OAuth token and a
    /// platform API key serve the same model through different endpoints; a
    /// provider-native window produced on one cannot be replayed on the other.
    pub fn observe_route_identity_with_route(&mut self, provider: &str, model: &str, route: Option<&str>) -> bool {
        let next = (provider.to_string(), model.to_string());
        let next_route = route.map(str::to_owned);
        let route_changed = self.route_identity_method != next_route;
        match &self.route_identity {
            Some(current) if current == &next && !route_changed => false,
            Some(_) => {
                self.route_identity = Some(next);
                self.route_identity_method = next_route;
                self.invalidate_generation(jobs::CancelReason::ModelChange);
                self.calibration_ratio = None;
                self.request_estimate_tokens = None;
                true
            }
            None => {
                self.route_identity = Some(next);
                self.route_identity_method = next_route;
                false
            }
        }
    }

    /// The route a native window produced now would be bound to.
    pub fn current_native_route(&self) -> Option<crate::session::StoredNativeRoute> {
        let (provider, model) = self.route_identity.clone()?;
        Some(crate::session::StoredNativeRoute {
            provider,
            model,
            route: self.route_identity_method.clone(),
        })
    }

    /// Drop a native window the current route can no longer replay.
    ///
    /// A native window is opaque and bound to the model and credential route
    /// that produced it. Replaying it elsewhere would send the provider a
    /// payload it never created, and keeping the boundary would hide the
    /// originals from the summarizer that has to rebuild them. So the window is
    /// discarded and the boundary resets to zero: the originals become the
    /// active context again, and no compacted count is advanced.
    ///
    /// Returns true when the persisted view changed.
    pub fn discard_foreign_openai_native_compaction(&mut self) -> bool {
        let Some(summary) = self.active_summary.as_ref() else {
            return false;
        };
        if summary.native_window.is_empty() && summary.openai_encrypted_content.is_none() {
            return false;
        }
        let Some((provider, model)) = self.route_identity.clone() else {
            return false;
        };
        let Some(bound) = summary.native_route.as_ref() else {
            // A native state persisted before route binding is replayed only on
            // the route it was loaded on; nothing to compare, so keep it.
            return false;
        };
        if bound.matches(&provider, &model, self.route_identity_method.as_deref()) {
            return false;
        }
        crate::logging::warn(&format!(
            "[compaction] [OI] native window was produced on {}/{} ({}) but the active route is {}/{} ({:?}); discarding it and restoring the originals",
            bound.provider,
            bound.model,
            bound.route.as_deref().unwrap_or("unknown"),
            provider,
            model,
            self.route_identity_method,
        ));
        if let Some(summary) = self.active_summary.as_mut() {
            summary.native_window.clear();
            summary.openai_encrypted_content = None;
            summary.native_route = None;
            summary.text.clear();
        }
        self.compacted_count = 0;
        self.active_chars.reset_pending(true);
        self.observed_input_tokens = None;
        self.generation = self.generation.wrapping_add(1);
        true
    }

    /// Take the last job disposition (applied, discarded, cancelled, failed).
    pub fn take_job_report(&mut self) -> Option<jobs::JobReport> {
        self.last_job_report.take()
    }

    /// Whether a background (proactive/idle) job may start now, and why not.
    pub fn background_start_refusal(
        &self,
        trigger: jobs::JobTrigger,
        persistent_runtime: bool,
        all_messages: &[Message],
    ) -> Option<jobs::StartRefusal> {
        if self.pending_task.is_some() {
            return Some(jobs::StartRefusal::JobInFlight);
        }
        self.start_gate(trigger, persistent_runtime, all_messages).refusal()
    }

    fn start_gate(
        &self,
        trigger: jobs::JobTrigger,
        persistent_runtime: bool,
        all_messages: &[Message],
    ) -> jobs::StartGate {
        let now = tokio::time::Instant::now();
        jobs::StartGate {
            trigger,
            background_enabled: self.background_enabled,
            persistent_runtime,
            usage: self.context_usage_with(all_messages),
            effective_tokens: self.effective_token_count_with(all_messages),
            threshold_tokens: self.compaction_threshold_tokens(),
            cooldown_ready: self.job_slot.cooldown().ready(now),
            breaker_allows: self.job_slot.breaker().allows(now),
        }
    }

    pub fn with_budget(mut self, budget: usize) -> Self {
        self.set_budget(budget);
        self
    }

    /// Update the token budget (e.g., when model changes)
    pub fn set_budget(&mut self, budget: usize) {
        let previous = self.model_token_budget;
        self.model_token_budget = budget;
        self.refresh_context_cap();
        if previous != budget {
            self.invalidate_generation(jobs::CancelReason::ModelChange);
        }
    }

    fn refresh_context_cap(&mut self) {
        self.compaction_config.max_context_tokens = crate::config::config().compaction.max_context_tokens;
        self.compaction_config.reserve_tokens = crate::config::config().compaction.reserve_tokens;
        self.compaction_config.recent_tokens = crate::config::config().compaction.recent_tokens;
        self.background_enabled = crate::config::config().compaction.background;
        self.strategy = self
            .compaction_config
            .resolved_strategy(&crate::config::config().provider);
        self.token_budget = Self::capped_budget(&self.compaction_config, self.model_token_budget);

        let budget = compute_context_budget(
            self.token_budget,
            self.compaction_config.reserve_tokens,
            self.compaction_config.recent_tokens,
            self.prior_saving_ratio,
        );
        match budget {
            Ok(budget) => {
                self.context_budget = Some(budget);
                self.context_budget_error = None;
            }
            Err(error) => {
                self.context_budget = None;
                self.context_budget_error = Some(error);
            }
        }
    }

    /// Apply `[compaction] max_context_tokens` so a large-window model cannot
    /// push per-turn context past the operator's chosen ceiling.
    fn capped_budget(cfg: &crate::config::CompactionConfig, budget: usize) -> usize {
        if cfg.max_context_tokens > 0 {
            budget.min(cfg.max_context_tokens)
        } else {
            budget
        }
    }

    /// Get current token budget
    pub fn token_budget(&self) -> usize {
        self.token_budget
    }

    /// The resolved context budget for the current window.
    pub fn context_budget(&self) -> Option<ContextBudget> {
        self.context_budget
    }

    /// Why the current window cannot hold a compaction budget, if it cannot.
    pub fn context_budget_error(&self) -> Option<ContextBudgetError> {
        self.context_budget_error
    }

    /// Token count at which compaction triggers. 0 when the window cannot hold
    /// a budget (see [`Self::context_budget_error`]).
    pub fn compaction_threshold_tokens(&self) -> usize {
        self.context_budget.map(|budget| budget.threshold_tokens).unwrap_or(0)
    }

    /// Token allowance for the verbatim recent tail.
    fn recent_token_allowance(&self) -> usize {
        self.context_budget
            .map(|budget| budget.recent_tokens)
            .unwrap_or(self.compaction_config.recent_tokens)
    }

    /// Index of the first message kept verbatim in `all_messages`.
    ///
    /// Retention is measured in tokens, not messages: the newest messages that
    /// fit the recent-tail allowance stay verbatim. At least
    /// [`MIN_TURNS_TO_KEEP`] messages are kept and the newest user message is
    /// never summarized away.
    pub fn recent_cutoff(&self, all_messages: &[Message]) -> usize {
        self.recent_cutoff_from(self.active_messages(all_messages))
    }

    fn recent_cutoff_from(&self, active: &[Message]) -> usize {
        let allowance = self.recent_token_allowance();
        let cutoff = recent_cutoff_for_token_budget(active, allowance, MIN_TURNS_TO_KEEP);
        // Keep the retained tail opening on a user instruction when the slack
        // costs less than half the tail allowance.
        horon_agent_compaction_core::align_cutoff_to_turn_start(active, cutoff, allowance / 2).0
    }

    /// Strategy that owns compaction for this manager.
    pub fn strategy(&self) -> CompactionStrategy {
        self.strategy
    }

    /// Point the manager at the engine the active provider reports as owner.
    ///
    /// Exactly one engine may compact a turn. The provider knows whether it
    /// owns compaction natively (`auto`/`explicit`) or leaves it to horon; the
    /// configured strategy only decides the case where the provider says
    /// nothing. Reconciling here keeps a stale config from starting a structured
    /// summary while the provider is already compacting the same turn (duplicate
    /// compaction and double billing).
    ///
    /// Returns true when ownership changed.
    pub fn reconcile_strategy(&mut self, provider_owned: Option<CompactionStrategy>) -> bool {
        let next = provider_owned.unwrap_or(self.configured_strategy);
        if next == self.strategy {
            return false;
        }
        crate::logging::info(&format!(
            "[compaction] ownership moves {} -> {}",
            self.strategy.as_str(),
            next.as_str()
        ));
        self.strategy = next;
        // A job planned under the old owner must not land on the new one.
        self.cancel_pending(jobs::CancelReason::Superseded);
        self.generation = self.generation.wrapping_add(1);
        true
    }

    /// Feed the last compaction's saving ratio into the next budget so the
    /// trigger adapts: a cheap summary lets compaction wait longer, an
    /// expensive one makes it start earlier.
    fn record_compaction_saving(&mut self, pre_tokens: u64, post_tokens: u64) {
        if pre_tokens == 0 {
            return;
        }
        let saved = pre_tokens.saturating_sub(post_tokens) as f64 / pre_tokens as f64;
        self.prior_saving_ratio = Some(saved);
        self.refresh_context_cap();
    }

    /// Notify the manager that a message was added.
    ///
    /// Legacy callers that do not provide the message content keep turn counts
    /// correct, but mark the rolling char estimate dirty so the next token
    /// estimate will resync from the provided history slice.
    pub fn notify_message_added(&mut self) {
        self.total_turns += 1;
        self.suppress_compaction_until_new_message = false;
        self.active_chars.invalidate();
    }

    /// Notify the manager that existing active messages were shortened in
    /// place (tool-result aging). The rolling char estimate resyncs, and the
    /// last observed usage, which measured the longer request, stops counting.
    pub fn note_history_condensed(&mut self) {
        self.active_chars.invalidate();
        self.observed_input_tokens = None;
        self.request_estimate_tokens = None;
    }

    /// Notify the manager that a message was added and update the rolling char
    /// estimate incrementally.
    pub fn notify_message_added_with(&mut self, message: &Message) {
        self.notify_message_added_blocks(&message.content);
    }

    pub fn notify_message_added_blocks(&mut self, content: &[ContentBlock]) {
        self.total_turns += 1;
        self.suppress_compaction_until_new_message = false;
        self.active_chars.append_exact(content_char_count(content));
    }

    /// Backward-compatible alias for `notify_message_added`.
    /// Accepts (and ignores) the message — callers that haven't been
    /// updated yet can still call `add_message(msg)`.
    pub fn add_message(&mut self, message: Message) {
        self.notify_message_added_with(&message);
    }

    /// Seed the manager from already-existing history that was restored from
    /// disk or otherwise replayed into memory.
    ///
    /// This updates turn counts but deliberately suppresses compaction until a
    /// genuinely new message is added after the restore. Restoring history must
    /// not itself trigger compaction.
    pub fn seed_restored_messages(&mut self, count: usize) {
        self.total_turns = count;
        self.suppress_compaction_until_new_message = count > 0;
        self.active_chars.reset_pending(count > 0);
    }

    /// Seed the manager from already-existing history with an exact rolling char
    /// estimate for the active suffix.
    pub fn seed_restored_messages_with(&mut self, all_messages: &[Message]) {
        self.total_turns = all_messages.len();
        self.suppress_compaction_until_new_message = !all_messages.is_empty();
        self.active_chars
            .set_exact(all_messages.iter().map(message_char_count).sum());
    }

    pub fn seed_restored_stored_messages_with(&mut self, all_messages: &[crate::session::StoredMessage]) {
        self.total_turns = all_messages.len();
        self.suppress_compaction_until_new_message = !all_messages.is_empty();
        self.active_chars.set_exact(
            all_messages
                .iter()
                .map(|message| content_char_count(&message.content))
                .sum(),
        );
    }

    /// Restore a previously persisted compacted view.
    pub fn restore_persisted_state(&mut self, state: &crate::session::StoredCompactionState, total_messages: usize) {
        // A restore replaces the transcript boundary out from under any live
        // job (import divergence, attach, rewind). Cancel it and move to the
        // next generation so a late result cannot be applied to the restored
        // state.
        self.cancel_pending(jobs::CancelReason::ImportDivergence);
        self.generation = self.generation.wrapping_add(1);
        self.observed_input_tokens = None;
        self.last_compaction = None;
        self.token_history.clear();
        self.turns_since_last_compact = 0;
        self.embedding_history.clear();
        self.semantic_embed_cache.clear();
        self.semantic_embed_cache_counter = 0;
        self.total_turns = total_messages;
        let has_summary = state.has_summary();
        self.compacted_count = if has_summary {
            state.compacted_count.min(total_messages)
        } else {
            0
        };
        self.active_chars.reset_pending(total_messages > self.compacted_count);
        self.active_summary = has_summary.then(|| Summary {
            text: state.summary_text.clone(),
            openai_encrypted_content: state.openai_encrypted_content.clone(),
            native_window: state.native_window.clone(),
            native_route: state.native_route.as_ref().map(|route| NativeRouteBinding {
                provider: route.provider.clone(),
                model: route.model.clone(),
                route: route.route.clone(),
            }),
            covers_up_to_turn: state.covers_up_to_turn,
            original_turn_count: state.original_turn_count,
        });
        let structured = state.structured.as_ref();
        self.active_summary_usage = (
            structured.and_then(|meta| meta.input_tokens),
            structured.and_then(|meta| meta.output_tokens),
        );
        self.active_restoration = structured.and_then(|meta| meta.restoration.clone());
        self.active_lossy = structured.is_some_and(|meta| meta.lossy_emergency);
        self.recovery_blocked = structured
            .and_then(|meta| meta.recovery_blocked.clone())
            .map(|blocked| recovery::RecoveryBlocked {
                fingerprint: recovery::RecoveryFingerprint {
                    source_digest: blocked.source_digest,
                    provider: blocked.provider,
                    model: blocked.model,
                    settings_digest: blocked.settings_digest,
                },
                reason: recovery::RecoveryBlockReason::parse(&blocked.reason)
                    .unwrap_or(recovery::RecoveryBlockReason::NoPriorSummary),
                detail: blocked.detail.unwrap_or_default(),
            });
        // A restored refusal carries the route it was recorded under. Adopting it
        // keeps the fingerprint comparable before the next job observes a route.
        if let Some(blocked) = &self.recovery_blocked
            && self.route_identity.is_none()
            && !blocked.fingerprint.provider.is_empty()
        {
            self.route_identity = Some((blocked.fingerprint.provider.clone(), blocked.fingerprint.model.clone()));
        }
        self.suppress_compaction_until_new_message = total_messages > 0;
    }

    /// Restore persisted compaction state and compute the active-suffix char
    /// estimate from the provided full message list.
    pub fn restore_persisted_state_with(
        &mut self,
        state: &crate::session::StoredCompactionState,
        all_messages: &[Message],
    ) {
        self.restore_persisted_state(state, all_messages.len());
        self.active_chars
            .set_exact(self.active_messages(all_messages).iter().map(message_char_count).sum());
    }

    pub fn restore_persisted_stored_state_with(
        &mut self,
        state: &crate::session::StoredCompactionState,
        all_messages: &[crate::session::StoredMessage],
    ) {
        self.restore_persisted_state(state, all_messages.len());
        let start = self.compacted_count.min(all_messages.len());
        self.active_chars.set_exact(
            all_messages[start..]
                .iter()
                .map(|message| content_char_count(&message.content))
                .sum(),
        );
    }

    /// Export the currently active compacted view for persistence.
    pub fn persisted_state(&self) -> Option<crate::session::StoredCompactionState> {
        self.active_summary
            .as_ref()
            .map(|summary| crate::session::StoredCompactionState {
                summary_text: summary.text.clone(),
                openai_encrypted_content: summary.openai_encrypted_content.clone(),
                native_window: summary.native_window.clone(),
                native_route: summary
                    .native_route
                    .as_ref()
                    .map(|route| crate::session::StoredNativeRoute {
                        provider: route.provider.clone(),
                        model: route.model.clone(),
                        route: route.route.clone(),
                    }),
                covers_up_to_turn: summary.covers_up_to_turn,
                original_turn_count: summary.original_turn_count,
                compacted_count: self.compacted_count,
                structured: Some(crate::session::StoredSummaryMetadata {
                    input_tokens: self.active_summary_usage.0,
                    output_tokens: self.active_summary_usage.1,
                    restoration: self.active_restoration.clone(),
                    lossy_emergency: self.active_lossy,
                    recovery_blocked: self.stored_recovery_blocked(),
                })
                .filter(|meta| {
                    meta.input_tokens.is_some()
                        || meta.output_tokens.is_some()
                        || meta.restoration.is_some()
                        || meta.lossy_emergency
                        || meta.recovery_blocked.is_some()
                }),
            })
    }

    fn stored_recovery_blocked(&self) -> Option<crate::session::StoredRecoveryBlocked> {
        self.recovery_blocked
            .as_ref()
            .map(|blocked| crate::session::StoredRecoveryBlocked {
                source_digest: blocked.fingerprint.source_digest.clone(),
                provider: blocked.fingerprint.provider.clone(),
                model: blocked.fingerprint.model.clone(),
                settings_digest: blocked.fingerprint.settings_digest.clone(),
                reason: blocked.reason.as_str().to_string(),
                detail: Some(blocked.detail.clone()).filter(|detail| !detail.is_empty()),
            })
    }

    /// Persisted state that also carries a refusal recorded before any summary
    /// existed. The refusal has to survive a restart, so it is written next to
    /// the summary (or on its own when there is none).
    pub fn persisted_state_with_recovery(&self) -> Option<crate::session::StoredCompactionState> {
        if let Some(state) = self.persisted_state() {
            return Some(state);
        }
        self.stored_recovery_blocked()
            .map(|recovery_blocked| crate::session::StoredCompactionState {
                structured: Some(crate::session::StoredSummaryMetadata {
                    recovery_blocked: Some(recovery_blocked),
                    ..Default::default()
                }),
                ..Default::default()
            })
    }

    /// Whether the active summary came from the deterministic lossy fallback
    /// or an emergency hard compact rather than a provider summary.
    pub fn is_lossy_summary(&self) -> bool {
        self.active_lossy
    }

    /// The restoration manifest captured with the active summary, if any.
    pub fn active_restoration(&self) -> Option<&crate::session::StoredRestorationManifest> {
        self.active_restoration.as_ref()
    }

    /// The refusal that currently suppresses automatic recovery, if any.
    pub fn recovery_blocked(&self) -> Option<&recovery::RecoveryBlocked> {
        self.recovery_blocked.as_ref()
    }

    /// Take the last recovery decision (applied, blocked, or refused).
    pub fn take_recovery_outcome(&mut self) -> Option<recovery::RecoveryOutcome> {
        self.last_recovery.take()
    }

    /// Offset at which the provider projection starts. Everything before it is
    /// summarized; the past is read from the session by offset instead of
    /// keeping the covered messages in memory.
    pub fn provider_projection_offset(&self) -> usize {
        self.compacted_count
    }

    /// Drop the covered prefix from a scratch provider projection, leaving the
    /// summary plus the live tail. The caller's owned copy is released; the
    /// session's originals are never touched.
    pub fn release_projection_prefix(&self, projection: &mut Vec<Message>) -> usize {
        let covered = self.compacted_count.min(projection.len());
        if covered == 0 {
            return 0;
        }
        projection.drain(..covered);
        covered
    }

    /// Build a reduced provider projection without mutating the input.
    ///
    /// Only oversized payloads inside the live tail are shortened; the caller's
    /// messages (and therefore the stored originals they came from) keep their
    /// exact bytes.
    pub fn shrink_projection_with(&self, all_messages: &[Message]) -> (Vec<Message>, usize) {
        let mut projection = self.active_messages(all_messages).to_vec();
        let truncated = emergency_truncate_large_payloads(
            &mut projection,
            EMERGENCY_TOOL_RESULT_MAX_CHARS,
            EMERGENCY_IMAGE_MAX_CHARS,
        );
        (projection, truncated)
    }

    /// Identity a refused recovery is bound to.
    ///
    /// The digest covers the live slice after the compacted boundary, so it
    /// stays comparable across a restart, where the manager is re-seeded from
    /// the session's active context rather than the full transcript.
    pub fn recovery_fingerprint(&self, all_messages: &[Message]) -> recovery::RecoveryFingerprint {
        let identity = self.current_job_identity();
        recovery::RecoveryFingerprint {
            source_digest: jobs::prefix_digest(self.active_messages(all_messages)),
            provider: identity.provider,
            model: identity.model,
            settings_digest: identity.settings_digest,
        }
    }

    /// Whether automatic recovery is still refused for this exact input.
    pub fn recovery_refusal_active(&self, all_messages: &[Message]) -> bool {
        self.recovery_blocked
            .as_ref()
            .is_some_and(|blocked| blocked.fingerprint == self.recovery_fingerprint(all_messages))
    }

    /// Recover from a failed summary with the deterministic fallback.
    ///
    /// The fallback runs only when the previous valid summary, the newest user
    /// instruction and a complete recent tool chain fit the tail budget. When
    /// they do not, the refusal is recorded against this exact source, provider,
    /// model and settings so automatic repetition stops until something really
    /// changes.
    pub fn recover_failed_summary_with(
        &mut self,
        all_messages: &[Message],
        failure: &str,
    ) -> recovery::RecoveryOutcome {
        let active = self.active_messages(all_messages);
        let fingerprint = self.recovery_fingerprint(all_messages);
        if let Some(blocked) = &self.recovery_blocked
            && blocked.fingerprint == fingerprint
        {
            let outcome = recovery::RecoveryOutcome::Refused(blocked.clone());
            self.last_recovery = Some(outcome.clone());
            return outcome;
        }

        let prior = self.active_summary.as_ref().map(|summary| summary.text.as_str());
        let budget = self.recent_token_allowance();
        let window = self.context_budget.map(|budget| budget.window).unwrap_or(0);
        let plan = match recovery::plan_lossy_fallback(active, prior, failure, budget, window) {
            Ok(plan) => plan,
            Err((reason, detail)) => {
                crate::logging::warn(&format!(
                    "[compaction/recovery] blocked reason={} detail={} automatic recovery refused for this source/model/settings",
                    reason.as_str(),
                    detail
                ));
                let blocked = recovery::RecoveryBlocked {
                    fingerprint,
                    reason,
                    detail,
                };
                self.recovery_blocked = Some(blocked.clone());
                let outcome = recovery::RecoveryOutcome::Blocked(blocked);
                self.last_recovery = Some(outcome.clone());
                return outcome;
            }
        };

        let pre_tokens = self.effective_token_count_with(all_messages) as u64;
        let summary = Summary {
            text: plan.summary_text.clone(),
            openai_encrypted_content: None,
            native_window: Vec::new(),
            native_route: None,
            covers_up_to_turn: plan.keep_from,
            original_turn_count: plan.keep_from,
        };
        let tail_chars: usize = active[plan.keep_from..].iter().map(message_char_count).sum();

        self.compacted_count = self
            .compacted_count
            .saturating_add(plan.keep_from)
            .min(all_messages.len());
        self.active_chars.set_exact(tail_chars);
        self.active_summary = Some(summary);
        self.active_summary_usage = (None, None);
        self.active_restoration = Some(stored_restoration(&plan.restoration));
        self.active_lossy = true;
        self.recovery_blocked = None;
        self.observed_input_tokens = None;
        self.generation = self.generation.wrapping_add(1);
        self.turns_since_last_compact = 0;

        let post_tokens = self.effective_token_count_with(all_messages) as u64;
        self.last_compaction = Some(CompactionEvent {
            trigger: "lossy_emergency".to_string(),
            pre_tokens: Some(pre_tokens),
            post_tokens: Some(post_tokens),
            tokens_saved: Some(pre_tokens.saturating_sub(post_tokens)),
            duration_ms: Some(0),
            messages_dropped: None,
            messages_compacted: Some(plan.keep_from),
            summary_chars: self.active_summary.as_ref().map(|summary| summary.text.len()),
            active_messages: Some(self.active_messages_count()),
        });
        self.record_compaction_saving(pre_tokens, post_tokens);

        let report = recovery::LossyEmergencyReport {
            covered_messages: plan.keep_from,
            kept_messages: active.len().saturating_sub(plan.keep_from),
            tail_tokens: plan.tail_tokens,
            budget_tokens: plan.budget_tokens,
            summary_chars: plan.summary_text.len(),
            restoration_entries: plan.restoration.entries.len(),
        };
        crate::logging::warn(&format!(
            "[compaction/recovery] lossy_emergency covered={} kept={} tail_tokens={} budget={} restoration_entries={}",
            report.covered_messages,
            report.kept_messages,
            report.tail_tokens,
            report.budget_tokens,
            report.restoration_entries
        ));
        let outcome = recovery::RecoveryOutcome::Applied(report);
        self.last_recovery = Some(outcome.clone());
        outcome
    }

    /// Drop provider-native OpenAI compaction state when it can no longer be
    /// replayed within OpenAI's per-string request limit. The compacted prefix
    /// remains compacted, but future requests use a small text fallback instead
    /// of bricking the session with an oversized `encrypted_content` field.
    pub fn discard_oversized_openai_native_compaction(&mut self) -> bool {
        let Some(summary) = self.active_summary.as_mut() else {
            return false;
        };
        // A retained window is discarded as a unit: replaying it with any item
        // removed would present the provider with a window it never produced.
        let window_is_unreplayable = !summary.native_window.is_empty()
            && summary
                .native_window
                .iter()
                .find_map(|item| {
                    (item.get("type").and_then(|value| value.as_str()) == Some("compaction"))
                        .then(|| item.get("encrypted_content").and_then(|value| value.as_str()))
                        .flatten()
                })
                .is_none_or(|encrypted_content| !openai_encrypted_content_is_sendable(encrypted_content));
        let Some(encrypted_content) = summary.openai_encrypted_content.as_ref() else {
            if !window_is_unreplayable {
                return false;
            }
            let window_len: usize = summary.native_window.iter().map(|item| item.to_string().len()).sum();
            summary.native_window.clear();
            let fallback = openai_encrypted_content_fallback_summary(window_len);
            if summary.text.trim().is_empty() {
                summary.text = fallback;
            } else if !summary.text.contains("[OI] native compaction state was discarded") {
                summary.text.push_str("\n\n");
                summary.text.push_str(&fallback);
            }
            self.observed_input_tokens = None;
            return true;
        };
        if openai_encrypted_content_is_sendable(encrypted_content) && !window_is_unreplayable {
            return false;
        }

        let encrypted_content_len = encrypted_content.len();
        crate::logging::warn(&format!(
            "[compaction] Discarding oversized [OI] native compaction payload ({} chars)",
            encrypted_content_len,
        ));
        summary.openai_encrypted_content = None;
        summary.native_window.clear();
        let fallback = openai_encrypted_content_fallback_summary(encrypted_content_len);
        if summary.text.trim().is_empty() {
            summary.text = fallback;
        } else if !summary.text.contains("[OI] native compaction state was discarded") {
            summary.text.push_str("\n\n");
            summary.text.push_str(&fallback);
        }
        self.observed_input_tokens = None;
        true
    }

    // ── Token snapshot (proactive mode) ────────────────────────────────────

    /// Record the observed token count after a completed turn.
    ///
    /// Called by the agent after `update_compaction_usage_from_stream`.
    /// Pushes the value into the rolling history window used by the proactive
    /// and semantic modes. Also increments the cooldown counter.
    pub fn push_token_snapshot(&mut self, tokens: u64) {
        self.token_history.push_back(tokens);
        if self.token_history.len() > TOKEN_HISTORY_WINDOW {
            self.token_history.pop_front();
        }
        self.turns_since_last_compact += 1;
    }

    /// Record an embedding snapshot for the current turn (semantic mode).
    ///
    /// `text` should be a short representation of the turn's assistant output
    /// (first EMBED_MAX_CHARS_PER_MSG chars). Silently skipped if the
    /// embedding model is unavailable.
    pub fn push_embedding_snapshot(&mut self, text: &str) {
        let snippet: String = text.chars().take(EMBED_MAX_CHARS_PER_MSG).collect();
        if let Some(emb) = self.cached_semantic_embedding(&snippet) {
            self.embedding_history.push_back(emb);
            if self.embedding_history.len() > EMBEDDING_HISTORY_WINDOW {
                self.embedding_history.pop_front();
            }
        }
    }

    // ── Anti-signal guard (shared by proactive + semantic) ──────────────────

    /// Returns `true` when any anti-signal fires and we should NOT compact
    /// proactively right now.
    ///
    /// Anti-signals are universal guards applied before the mode-specific
    /// trigger logic. They prevent wasted work and respect user intent.
    fn anti_signals_block(&self, all_messages: &[Message]) -> bool {
        let cfg = &self.compaction_config;

        // 1. Already compacting — never double-trigger.
        if self.pending_task.is_some() {
            return true;
        }

        // 2. Context below the proactive floor — too early regardless of trend.
        let usage = self.context_usage_with(all_messages);
        if usage < cfg.proactive_floor {
            return true;
        }

        // 3. Not enough token history to project from.
        if self.token_history.len() < cfg.min_samples {
            return true;
        }

        // 4. Growth has stalled: last stall_window snapshots show no increase.
        //    If tokens haven't grown, there's no urgency.
        if self.token_history.len() >= cfg.stall_window {
            let recent: Vec<u64> = self
                .token_history
                .iter()
                .rev()
                .take(cfg.stall_window)
                .cloned()
                .collect();
            let oldest = recent[recent.len() - 1];
            let newest = recent[0];
            if newest <= oldest {
                return true;
            }
        }

        // 5. Cooldown: too soon after the last compaction.
        self.turns_since_last_compact < cfg.min_turns_between_compactions
    }

    // ── Proactive mode trigger ──────────────────────────────────────────────

    /// Returns `true` if the proactive strategy thinks we should compact now.
    ///
    /// Uses an EWMA over the token history to project forward `lookahead_turns`
    /// turns. If the projected token count would exceed the 80% threshold,
    /// it's time to compact before we get there.
    fn should_compact_proactively(&self, all_messages: &[Message]) -> bool {
        if self.anti_signals_block(all_messages) {
            return false;
        }

        let cfg = &self.compaction_config;
        let threshold = self.compaction_threshold_tokens() as f64;

        // Compute EWMA of per-turn token deltas.
        // We need at least 2 snapshots to get a delta.
        let snapshots: Vec<u64> = self.token_history.iter().cloned().collect();
        if snapshots.len() < 2 {
            return false;
        }

        let alpha = cfg.ewma_alpha as f64;
        let mut ewma_delta: f64 = (snapshots[1] as f64) - (snapshots[0] as f64);
        ewma_delta = ewma_delta.max(0.0);
        for i in 2..snapshots.len() {
            let delta = ((snapshots[i] as f64) - (snapshots[i - 1] as f64)).max(0.0);
            ewma_delta = alpha * delta + (1.0 - alpha) * ewma_delta;
        }
        let Some(current) = snapshots.last().copied().map(|value| value as f64) else {
            return false;
        };
        let projected = current + ewma_delta * cfg.lookahead_turns as f64;

        crate::logging::info(&format!(
            "[compaction/proactive] current={:.0} ewma_delta={:.1}/turn projected@{}turns={:.0} threshold={:.0}",
            current, ewma_delta, cfg.lookahead_turns, projected, threshold
        ));

        projected >= threshold
    }

    // ── Semantic mode trigger ───────────────────────────────────────────────

    /// Returns `true` if the semantic strategy detects a topic shift or
    /// predicts we should compact now.
    ///
    /// Topic-shift detection: compares the mean embedding of the oldest half
    /// of the history window against the newest half. A low cosine similarity
    /// between the two clusters indicates a topic boundary was crossed —
    /// the previous topic is complete and safe to summarize.
    ///
    /// Falls back to proactive logic if embeddings are unavailable.
    fn should_compact_semantic(&self, all_messages: &[Message]) -> bool {
        if self.anti_signals_block(all_messages) {
            return false;
        }

        // Need enough embedding history to split into two halves.
        let history_len = self.embedding_history.len();
        if history_len < 4 {
            // Fall back to proactive trigger.
            return self.should_compact_proactively(all_messages);
        }

        let cfg = &self.compaction_config;
        let half = history_len / 2;

        let old_embeddings: Vec<&Vec<f32>> = self.embedding_history.iter().take(half).collect();
        let new_embeddings: Vec<&Vec<f32>> = self.embedding_history.iter().skip(half).collect();

        let dim = old_embeddings[0].len();

        // Compute mean embedding for each half.
        let mean_old = mean_embedding(&old_embeddings, dim);
        let mean_new = mean_embedding(&new_embeddings, dim);

        let similarity = crate::embedding::cosine_similarity(&mean_old, &mean_new);

        crate::logging::info(&format!(
            "[compaction/semantic] topic similarity (old vs new half) = {:.3} (threshold={:.2})",
            similarity, cfg.topic_shift_threshold
        ));

        if similarity < cfg.topic_shift_threshold {
            crate::logging::info("[compaction/semantic] Topic shift detected — triggering proactive compaction");
            return true;
        }

        // No topic shift — still fall back to proactive growth check.
        self.should_compact_proactively(all_messages)
    }

    /// Build a relevance-scored keep set for semantic compaction.
    ///
    /// Embeds the last `goal_window_turns` messages to represent the current
    /// goal, then scores all active messages by cosine similarity. Returns the
    /// cutoff index: messages before the cutoff will be summarized, messages at
    /// or after are kept verbatim.
    ///
    /// Messages above `relevance_keep_threshold` anywhere in the history are
    /// pulled out of the summarize set. Falls back to the standard recency
    /// cutoff if embeddings fail.
    fn semantic_cutoff(&mut self, active: &[Message]) -> usize {
        let goal_window_turns = self.compaction_config.goal_window_turns;
        let relevance_keep_threshold = self.compaction_config.relevance_keep_threshold;
        let standard_cutoff = self.recent_cutoff_from(active);
        if standard_cutoff == 0 {
            return 0;
        }

        // Build goal text from recent turns.
        let goal_turns = goal_window_turns.min(active.len());
        let goal_text = semantic_goal_text(&active[active.len() - goal_turns..]);

        if goal_text.is_empty() {
            return standard_cutoff;
        }

        let goal_emb = match self.cached_semantic_embedding(&goal_text) {
            Some(embedding) => embedding,
            None => return standard_cutoff,
        };

        // Score each candidate message (those before standard_cutoff).
        let mut high_relevance_count = 0usize;
        let mut earliest_high_relevance = standard_cutoff;

        for (idx, msg) in active[..standard_cutoff].iter().enumerate() {
            let text = semantic_message_text(msg);

            if text.is_empty() {
                continue;
            }

            if let Some(embedding) = self.cached_semantic_embedding(&text) {
                let sim = crate::embedding::cosine_similarity(&goal_emb, &embedding);
                if sim >= relevance_keep_threshold {
                    high_relevance_count += 1;
                    earliest_high_relevance = earliest_high_relevance.min(idx);
                }
            }
        }

        if high_relevance_count == 0 {
            return standard_cutoff;
        }

        // Find the latest high-relevance message before standard_cutoff.
        // We can't have gaps in the summarized range (tool call integrity),
        // so we move the cutoff up to just before the earliest high-relevance
        // message in the tail of the compaction range.
        let adjusted_cutoff = earliest_high_relevance;

        // Ensure we actually compact something meaningful.
        if adjusted_cutoff < 2 {
            return standard_cutoff;
        }

        crate::logging::info(&format!(
            "[compaction/semantic] relevance scoring: {} high-relevance msgs kept, cutoff {} -> {}",
            high_relevance_count, standard_cutoff, adjusted_cutoff
        ));

        adjusted_cutoff
    }

    /// Get the active (uncompacted) messages from a full message list.
    /// Skips the first `compacted_count` messages.
    fn active_messages<'a>(&self, all_messages: &'a [Message]) -> &'a [Message] {
        // If session restore/replay leaves the manager with bookkeeping from a
        // longer message vector, never fall back to the full transcript. That
        // makes already-compacted messages active again and can drive repeated
        // emergency compaction loops. Clamp to the end instead: all available
        // messages are covered by the summary until new turns arrive.
        let start = self.compacted_count.min(all_messages.len());
        &all_messages[start..]
    }

    fn clamp_compacted_count_to_messages(&mut self, all_messages: &[Message], reason: &str) -> bool {
        // Some backward-compatible call paths intentionally poll/apply without
        // caller-owned message history. An empty slice there means "unknown",
        // not necessarily an empty transcript, so do not treat it as an
        // authoritative upper bound.
        if all_messages.is_empty() {
            return false;
        }
        if self.compacted_count <= all_messages.len() {
            return false;
        }

        crate::logging::warn(&format!(
            "[compaction/invariant] compacted_count_exceeded_messages reason={} compacted_count={} messages_len={} total_turns={} has_summary={} summary_chars={} observed_input_tokens={:?}",
            reason,
            self.compacted_count,
            all_messages.len(),
            self.total_turns,
            self.active_summary.is_some(),
            self.summary_chars(),
            self.observed_input_tokens,
        ));
        self.compacted_count = all_messages.len();
        self.active_chars.set_exact(0);
        true
    }

    fn log_compaction_state(&self, phase: &str, trigger: &str, all_messages: &[Message]) {
        let active_len = self.active_messages(all_messages).len();
        crate::logging::info(&format!(
            "[compaction/state] phase={} trigger={} messages_len={} active_messages={} compacted_count={} total_turns={} token_budget={} token_estimate={} effective_tokens={} observed_input_tokens={:?} calibration_ratio={:?} has_summary={} summary_chars={} pending_cutoff={} is_compacting={}",
            phase,
            trigger,
            all_messages.len(),
            active_len,
            self.compacted_count,
            self.total_turns,
            self.token_budget,
            self.token_estimate_with(all_messages),
            self.effective_token_count_with(all_messages),
            self.observed_input_tokens,
            self.calibration_ratio,
            self.active_summary.is_some(),
            self.summary_chars(),
            self.pending_cutoff,
            self.pending_task.is_some(),
        ));
    }

    fn log_compaction_outcome(&self, outcome: CompactionOutcomeLog<'_>) {
        let tokens_saved = outcome.pre_tokens.saturating_sub(outcome.post_tokens);
        let grew = outcome.post_tokens > outcome.pre_tokens;
        let level = if grew { "warn" } else { "info" };
        let line = format!(
            "[compaction/outcome] level={} trigger={} duration_ms={} pre_tokens={} post_tokens={} tokens_saved={} grew={} messages_len={} active_messages={} compacted_count={} total_turns={} messages_compacted={} messages_dropped={} summary_chars={} observed_input_tokens={:?}",
            level,
            outcome.trigger,
            outcome.duration_ms,
            outcome.pre_tokens,
            outcome.post_tokens,
            tokens_saved,
            grew,
            outcome.all_messages.len(),
            self.active_messages(outcome.all_messages).len(),
            self.compacted_count,
            self.total_turns,
            outcome.messages_compacted,
            outcome.messages_dropped.unwrap_or(0),
            self.summary_chars(),
            self.observed_input_tokens,
        );
        if grew {
            crate::logging::warn(&line);
        } else {
            crate::logging::info(&line);
        }
    }

    fn active_message_chars_with(&self, all_messages: &[Message]) -> usize {
        // Recompute from history when the cache is stale, or when the
        // display-side turn estimate disagrees with the real active slice
        // length (the two can diverge across restore/clamp/compaction paths,
        // and trusting a mismatched cache is exactly what corrupts token
        // accounting).
        if self.active_chars.is_dirty() || self.active_messages_count() != self.active_messages(all_messages).len() {
            self.active_messages(all_messages).iter().map(message_char_count).sum()
        } else {
            self.active_chars.value()
        }
    }

    /// Get current token estimate using the caller's message list
    pub fn token_estimate_with(&self, all_messages: &[Message]) -> usize {
        estimate_compaction_tokens_from_text(
            self.active_summary.as_ref(),
            self.active_messages(all_messages),
            self.token_budget,
        )
    }

    /// Get current token estimate (backward compat — uses 0 messages, only summary + observed)
    pub fn token_estimate(&self) -> usize {
        estimate_compaction_tokens_from_text(self.active_summary.as_ref(), &[], self.token_budget)
    }

    /// Store provider-reported input token usage for compaction decisions.
    ///
    /// When the matching request's local estimate was noted, the pair becomes
    /// one calibration sample. Tiny requests are skipped: fixed overhead
    /// dominates them and would skew the ratio.
    pub fn update_observed_input_tokens(&mut self, tokens: u64) {
        self.observed_input_tokens = Some(tokens);
        if let Some(estimate) = self.request_estimate_tokens.take()
            && estimate >= CALIBRATION_MIN_ESTIMATE_TOKENS
        {
            let sample = (tokens as f64 / estimate as f64).clamp(CALIBRATION_MIN_SAMPLE, CALIBRATION_MAX_SAMPLE);
            self.calibration_ratio = Some(match self.calibration_ratio {
                None => sample,
                Some(ratio) => (1.0 - CALIBRATION_ALPHA) * ratio + CALIBRATION_ALPHA * sample,
            });
        }
    }

    /// Record the local estimate of the request about to be sent.
    pub fn note_request_estimate(&mut self, tokens: u64) {
        self.request_estimate_tokens = Some(tokens);
    }

    /// Smoothed observed/estimated ratio for the current route, if learned.
    pub fn calibration_ratio(&self) -> Option<f64> {
        self.calibration_ratio
    }

    /// Whether the last API projection emergency-truncated a payload; reading
    /// it clears the flag.
    pub fn take_projection_truncated(&mut self) -> bool {
        std::mem::take(&mut self.projection_truncated)
    }

    fn calibrated_estimate(&self, estimate: usize) -> usize {
        let ratio = self.calibration_ratio.unwrap_or(1.0).max(CALIBRATION_FLOOR);
        (estimate as f64 * ratio).ceil() as usize
    }

    /// Best-effort current token count using the caller's messages.
    ///
    /// Context accounting is conservative: the larger of the calibrated local
    /// estimate (message text plus system/tool overhead) and the
    /// provider-reported usage wins. Cache-hit tokens are part of the prompt and
    /// are never subtracted.
    pub fn effective_token_count_with(&self, all_messages: &[Message]) -> usize {
        conservative_context_tokens(
            self.calibrated_estimate(self.token_estimate_with(all_messages)),
            self.observed_input_tokens,
        )
    }

    /// Best-effort token count without message data (uses only observed tokens)
    pub fn effective_token_count(&self) -> usize {
        conservative_context_tokens(
            self.calibrated_estimate(self.token_estimate()),
            self.observed_input_tokens,
        )
    }

    /// Get current context usage as percentage (using caller's messages)
    pub fn context_usage_with(&self, all_messages: &[Message]) -> f32 {
        self.effective_token_count_with(all_messages) as f32 / self.token_budget as f32
    }

    /// Get current context usage (without messages, uses observed tokens only)
    pub fn context_usage(&self) -> f32 {
        self.effective_token_count() as f32 / self.token_budget as f32
    }

    /// Check if we should start compaction
    pub fn should_compact_with(&self, all_messages: &[Message]) -> bool {
        use crate::config::CompactionMode;
        if self.suppress_compaction_until_new_message {
            return false;
        }
        // Exactly one owner: when the provider owns compaction, the structured
        // trigger stays silent so both never fire for the same turn.
        if self.strategy.is_native() {
            return false;
        }
        // A window too small to hold a budget cannot be planned at all.
        if self.context_budget.is_none() {
            return false;
        }
        let active = self.active_messages(all_messages);
        if active.len() <= MIN_TURNS_TO_KEEP || self.recent_cutoff_from(active) == 0 {
            return false;
        }
        if self.recovery_refusal_active(all_messages) {
            return false;
        }
        match self.mode {
            CompactionMode::Reactive => {
                self.pending_task.is_none()
                    && self.effective_token_count_with(all_messages) >= self.compaction_threshold_tokens()
            }
            CompactionMode::Proactive => self.should_compact_proactively(all_messages),
            CompactionMode::Semantic => self.should_compact_semantic(all_messages),
        }
    }

    /// Start background compaction if needed
    pub fn maybe_start_compaction_with(&mut self, all_messages: &[Message], provider: Arc<dyn Provider>) {
        if !self.should_compact_with(all_messages) {
            return;
        }
        if let Some(error) = self.context_budget_error {
            crate::logging::error(&format!("[compaction] {error} — no compaction plan can be made"));
            return;
        }

        let active = self.active_messages(all_messages);

        // Calculate cutoff within active messages.
        // Semantic mode uses relevance scoring; other modes keep the recent
        // tail whose size is the token allowance, not a fixed message count.
        let mut cutoff = match self.mode {
            crate::config::CompactionMode::Semantic => self.semantic_cutoff(active),
            _ => self.recent_cutoff_from(active),
        };
        if cutoff == 0 {
            return;
        }

        // Adjust cutoff to not split tool call/result pairs
        cutoff = safe_compaction_cutoff(active, cutoff);
        if cutoff == 0 {
            return;
        }

        // Snapshot the active slice (prefix plus kept tail) for the async task.
        // The tail travels with the job so shrinking can leave the excluded
        // range in place instead of guessing what followed the cutoff.
        let msg_count = cutoff;
        let existing_summary = self.active_summary.clone();
        let mode_label = self.mode_trigger_label().to_string();
        let reserve = self.context_budget.map(|budget| budget.reserve).unwrap_or(4_000);
        let limits = SummaryLimits::for_window_with_reserve(provider.context_window(), reserve);
        let estimated_tokens = self.effective_token_count_with(all_messages);
        crate::logging::info(&format!(
            "[TIMING] compaction_start: trigger={}, active_messages={}, cutoff={}, estimated_tokens={}, has_existing_summary={}",
            mode_label,
            active.len(),
            cutoff,
            estimated_tokens,
            existing_summary.is_some(),
        ));

        let trigger = match self.mode {
            crate::config::CompactionMode::Reactive => jobs::JobTrigger::Reactive,
            crate::config::CompactionMode::Proactive | crate::config::CompactionMode::Semantic => {
                jobs::JobTrigger::Proactive
            }
        };
        let started = self.start_job(
            all_messages,
            active,
            msg_count,
            trigger,
            false,
            provider,
            existing_summary,
            limits,
        );
        match started {
            Ok(job_id) => crate::logging::info(&format!(
                "[compaction/job] started id={job_id} trigger={}",
                trigger.as_str()
            )),
            Err(refusal) => crate::logging::info(&format!(
                "[compaction/job] not started trigger={} reason={}",
                trigger.as_str(),
                refusal.as_str()
            )),
        }
    }

    /// Claim the single job slot and spawn the summarization task.
    ///
    /// The job captures the active prefix, the route identity and the settings
    /// digest it was planned under; the task reports through its `JoinHandle`
    /// and is cancelled through `pending_cancel`.
    #[allow(clippy::too_many_arguments)]
    fn start_job(
        &mut self,
        all_messages: &[Message],
        active: &[Message],
        cutoff: usize,
        trigger: jobs::JobTrigger,
        persistent_runtime: bool,
        provider: Arc<dyn Provider>,
        existing_summary: Option<Summary>,
        mut limits: SummaryLimits,
    ) -> Result<u64, jobs::StartRefusal> {
        // The kept tail starts mid-turn when the cutoff is not a turn start;
        // the summary then has to carry the original request forward.
        limits.split_turn =
            cutoff < active.len() && !horon_agent_compaction_core::pruning::is_turn_start(&active[cutoff]);
        let now = tokio::time::Instant::now();
        let identity = jobs::JobIdentity {
            generation: self.generation,
            provider: provider.name().to_string(),
            model: provider.model(),
            settings_digest: self.settings_digest(),
        };
        self.route_identity = Some((identity.provider.clone(), identity.model.clone()));
        let prefix = jobs::CapturedPrefix::capture(
            active,
            self.compacted_count,
            cutoff,
            existing_summary.as_ref().map(|summary| summary.text.as_str()),
        );
        let gate = self.start_gate(trigger, persistent_runtime, all_messages);
        let job_id = self.job_slot.begin(&gate, identity, prefix, now)?;
        let Some(job) = self.job_slot.current().cloned() else {
            return Err(jobs::StartRefusal::NoPlan);
        };
        let deadline = job.job_deadline();
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();

        let active_snapshot = active.to_vec();
        let native_route = self.current_native_route();
        let msg_count = cutoff;
        let label = trigger.as_str().to_string();
        self.pending_job = Some(job);
        self.pending_cancel = Some(cancel_tx);
        self.pending_cutoff = cutoff;
        self.pending_trigger = Some(label.clone());

        self.pending_task = Some(tokio::spawn(async move {
            let start = std::time::Instant::now();
            let result = run_compaction_job(
                provider,
                &active_snapshot,
                cutoff,
                existing_summary.as_ref(),
                limits,
                native_route,
                deadline,
                cancel_rx,
            )
            .await;
            let duration_ms = start.elapsed().as_millis() as u64;
            crate::logging::info(&format!(
                "Compaction ({}) finished in {:.2}s ({} messages summarized)",
                label,
                duration_ms as f64 / 1000.0,
                msg_count,
            ));
            crate::bus::Bus::global().publish(crate::bus::BusEvent::CompactionFinished);
            result.map(|mut result| {
                result.job_id = job_id;
                result.duration_ms = duration_ms;
                result.summarized_messages = msg_count;
                result
            })
        }));

        Ok(job_id)
    }

    /// Start an idle background compaction job.
    ///
    /// Idle jobs run only in the persistent serve/api-bridge runtime, only once
    /// the context is at least half full, only with no job in flight, and only
    /// past the lead point (`threshold - clamp(threshold*0.125, 8192, 32768)`).
    /// A one-shot `run` never starts one.
    pub fn maybe_start_idle_compaction_with(
        &mut self,
        all_messages: &[Message],
        provider: Arc<dyn Provider>,
        persistent_runtime: bool,
    ) -> bool {
        if self.strategy.is_native()
            || self.context_budget.is_none()
            || self.suppress_compaction_until_new_message
            || self.pending_task.is_some()
        {
            return false;
        }
        let active = self.active_messages(all_messages);
        if active.len() <= MIN_TURNS_TO_KEEP {
            return false;
        }
        let cutoff = safe_compaction_cutoff(active, self.recent_cutoff_from(active));
        if cutoff == 0 {
            return false;
        }
        let reserve = self.context_budget.map(|budget| budget.reserve).unwrap_or(4_000);
        let limits = SummaryLimits::for_window_with_reserve(provider.context_window(), reserve);
        let existing_summary = self.active_summary.clone();
        match self.start_job(
            all_messages,
            active,
            cutoff,
            jobs::JobTrigger::Idle,
            persistent_runtime,
            provider,
            existing_summary,
            limits,
        ) {
            Ok(job_id) => {
                crate::logging::info(&format!(
                    "[compaction/job] started id={job_id} trigger=idle cutoff={cutoff}"
                ));
                true
            }
            Err(refusal) => {
                crate::logging::info(&format!(
                    "[compaction/job] not started trigger=idle reason={}",
                    refusal.as_str()
                ));
                false
            }
        }
    }

    /// Ensure context fits before an API call.
    ///
    /// Starts background compaction if above 80%. If context is critically full
    /// (>=95%), also performs an immediate hard-compact (drops old messages) so
    /// the next API call doesn't fail with "prompt too long".
    pub fn ensure_context_fits(&mut self, all_messages: &[Message], provider: Arc<dyn Provider>) -> CompactionAction {
        self.refresh_context_cap();
        // Exactly one owner. When the provider owns compaction, horon's own
        // summarizer and its emergency hard compact both stay out of the way:
        // running them here would compact the transcript behind the provider's
        // native state and leave two compaction records for the same turn.
        if self.strategy.is_native() {
            return CompactionAction::None;
        }
        // A window that cannot hold a budget has no valid trigger point. Report
        // it and return before any provider call is made: compacting against an
        // impossible threshold would either never fire or fire on every turn.
        if let Some(error) = self.context_budget_error {
            crate::logging::error(&format!(
                "[compaction] {error} — skipping compaction until the context window can hold a budget"
            ));
            return CompactionAction::None;
        }
        // If we're already critically full, hard-compact synchronously *before*
        // kicking off any background compaction. Starting a background task here
        // would only get aborted by the hard compact (its summary is computed
        // against the pre-hard-compact offsets), so skip the wasted work and the
        // risk of a stale `pending_cutoff` being applied later.
        let usage = self.context_usage_with(all_messages);
        if usage >= CRITICAL_THRESHOLD {
            if self.pending_task.is_some() {
                crate::logging::warn(&format!(
                    "[compaction] Context at {:.1}% with background compaction in flight — waiting up to {}ms before hard compact",
                    usage * 100.0,
                    HARD_THRESHOLD_PENDING_WAIT_MS,
                ));
                let waited = self.wait_for_pending_compaction_at_hard_threshold(all_messages);
                let post_wait_usage = self.context_usage_with(all_messages);
                crate::logging::info(&format!(
                    "[compaction] Hard-threshold wait complete: waited_ms={}, applied={}, timed_out={}, usage_now={:.1}%",
                    waited.waited_ms,
                    waited.applied,
                    waited.timed_out,
                    post_wait_usage * 100.0,
                ));
                if post_wait_usage < CRITICAL_THRESHOLD {
                    // We may still be above the soft threshold. Let the normal
                    // path below decide whether another async compaction should
                    // start, but avoid dropping context now that the hard
                    // threshold has been cleared.
                } else {
                    crate::logging::warn(&format!(
                        "[compaction] Context still at {:.1}% after waiting for in-flight compaction; escalating to hard compact",
                        post_wait_usage * 100.0,
                    ));
                    match self.hard_compact_with(all_messages) {
                        Ok(dropped) => {
                            let post_usage = self.context_usage_with(all_messages);
                            crate::logging::info(&format!(
                                "[compaction] Hard compact dropped {} messages, context now at {:.1}%",
                                dropped,
                                post_usage * 100.0,
                            ));
                            return CompactionAction::HardCompacted(dropped);
                        }
                        Err(reason) => {
                            crate::logging::error(&format!(
                                "[compaction] Hard compact failed at critical threshold: {}",
                                reason
                            ));
                        }
                    }
                }
            } else {
                crate::logging::warn(&format!(
                    "[compaction] Context at {:.1}% (critical threshold {:.0}%) — performing synchronous hard compact",
                    usage * 100.0,
                    CRITICAL_THRESHOLD * 100.0,
                ));
                match self.hard_compact_with(all_messages) {
                    Ok(dropped) => {
                        let post_usage = self.context_usage_with(all_messages);
                        crate::logging::info(&format!(
                            "[compaction] Hard compact dropped {} messages, context now at {:.1}%",
                            dropped,
                            post_usage * 100.0,
                        ));
                        return CompactionAction::HardCompacted(dropped);
                    }
                    Err(reason) => {
                        crate::logging::error(&format!(
                            "[compaction] Hard compact failed at critical threshold: {}",
                            reason
                        ));
                    }
                }
            }
        }

        let was_compacting = self.is_compacting();
        self.maybe_start_compaction_with(all_messages, provider);
        let bg_started = !was_compacting && self.is_compacting();

        if bg_started {
            CompactionAction::BackgroundStarted {
                trigger: self
                    .pending_trigger
                    .clone()
                    .unwrap_or_else(|| self.mode_trigger_label().to_string()),
            }
        } else {
            CompactionAction::None
        }
    }

    fn wait_for_pending_compaction_at_hard_threshold(&mut self, all_messages: &[Message]) -> HardThresholdWait {
        let start = Instant::now();
        let timeout = std::time::Duration::from_millis(HARD_THRESHOLD_PENDING_WAIT_MS);
        let poll = std::time::Duration::from_millis(HARD_THRESHOLD_PENDING_POLL_MS);

        while start.elapsed() < timeout {
            if self
                .pending_task
                .as_ref()
                .map(|task| task.is_finished())
                .unwrap_or(false)
            {
                self.check_and_apply_compaction_with(all_messages);
                return HardThresholdWait {
                    waited_ms: start.elapsed().as_millis() as u64,
                    applied: self.last_compaction.is_some(),
                    timed_out: false,
                };
            }
            std::thread::sleep(poll);
        }

        if self
            .pending_task
            .as_ref()
            .map(|task| task.is_finished())
            .unwrap_or(false)
        {
            self.check_and_apply_compaction_with(all_messages);
            return HardThresholdWait {
                waited_ms: start.elapsed().as_millis() as u64,
                applied: self.last_compaction.is_some(),
                timed_out: false,
            };
        }

        HardThresholdWait {
            waited_ms: start.elapsed().as_millis() as u64,
            applied: false,
            timed_out: true,
        }
    }

    /// Force immediate compaction (for manual /compact command).
    pub fn force_compact_with(&mut self, all_messages: &[Message], provider: Arc<dyn Provider>) -> Result<(), String> {
        if self.pending_task.is_some() {
            return Err("Compaction already in progress".to_string());
        }
        if self.strategy.is_native() {
            return Err(format!(
                "Compaction is owned by the provider ({})",
                self.strategy.as_str()
            ));
        }
        if let Some(error) = self.context_budget_error {
            return Err(error.to_string());
        }

        let active = self.active_messages(all_messages);

        let mut cutoff = self.recent_cutoff_from(active);
        if cutoff == 0 {
            return Err(format!(
                "Not enough messages to compact (the newest {} messages already fill the recent tail, have {})",
                MIN_TURNS_TO_KEEP,
                active.len()
            ));
        }

        if self.context_usage_with(all_messages) < MANUAL_COMPACT_MIN_THRESHOLD {
            return Err(format!(
                "Context usage too low ({:.1}%) - nothing to compact",
                self.context_usage_with(all_messages) * 100.0
            ));
        }

        cutoff = safe_compaction_cutoff(active, cutoff);
        if cutoff == 0 {
            return Err("Cannot compact - would split tool call/result pairs".to_string());
        }

        let msg_count = cutoff;
        let existing_summary = self.active_summary.clone();
        let reserve = self.context_budget.map(|budget| budget.reserve).unwrap_or(4_000);
        let limits = SummaryLimits::for_window_with_reserve(provider.context_window(), reserve);

        self.start_job(
            all_messages,
            active,
            msg_count,
            jobs::JobTrigger::Manual,
            false,
            provider,
            existing_summary,
            limits,
        )
        .map(|_| ())
        .map_err(|refusal| format!("Cannot compact: {}", refusal.as_str()))
    }

    /// Check if background compaction is done and apply it, updating rolling
    /// token-estimate state from the provided full message list.
    pub fn check_and_apply_compaction_with(&mut self, all_messages: &[Message]) {
        self.clamp_compacted_count_to_messages(all_messages, "check_and_apply_start");
        let task = match self.pending_task.take() {
            Some(task) => task,
            None => return,
        };

        // Check if done without blocking
        if !task.is_finished() {
            // Not done yet, put it back
            self.pending_task = Some(task);
            return;
        }

        // Get result
        let now = tokio::time::Instant::now();
        match futures::executor::block_on(task) {
            Ok(Ok(result)) => {
                let trigger = self
                    .pending_trigger
                    .clone()
                    .unwrap_or_else(|| self.mode_trigger_label().to_string());
                self.apply_compaction_result(result, trigger, all_messages);
            }
            Ok(Err(failure)) => {
                crate::logging::error(&format!("[compaction] Failed to generate summary: {}", failure));
                self.finish_failed_job(failure.kind, now);
                if matches!(
                    failure.kind,
                    jobs::JobOutcomeKind::Failed | jobs::JobOutcomeKind::TimedOut
                ) {
                    self.recover_failed_summary_with(all_messages, &failure.detail);
                }
            }
            Err(e) => {
                crate::logging::error(&format!("[compaction] Task panicked: {}", e));
                self.finish_failed_job(jobs::JobOutcomeKind::Failed, now);
            }
        }
    }

    /// Release the slot for a job that never produced a result and record the
    /// failure in the breaker.
    fn finish_failed_job(&mut self, kind: jobs::JobOutcomeKind, now: tokio::time::Instant) {
        let Some(job) = self.pending_job.take() else {
            self.pending_cancel = None;
            self.pending_cutoff = 0;
            self.pending_trigger = None;
            return;
        };
        self.job_slot.finish(job.id);
        if self.job_slot.record_failure(now) {
            crate::logging::warn(&format!(
                "[compaction/job] automatic attempts suspended for {}s after {} consecutive failures",
                jobs::BREAKER_SUSPENSION.as_secs(),
                jobs::BREAKER_THRESHOLD
            ));
        }
        self.last_job_report = Some(jobs::JobReport {
            job_id: job.id,
            trigger: job.trigger,
            generation: job.identity.generation,
            outcome: kind,
            covered: job.prefix.covered,
        });
        self.pending_cancel = None;
        self.pending_cutoff = 0;
        self.pending_trigger = None;
    }

    /// Discard a finished result that no longer matches the live state.
    fn discard_job(&mut self, job: jobs::CompactionJob, reason: jobs::DiscardReason, now: tokio::time::Instant) {
        crate::logging::warn(&format!(
            "[compaction/job] discarding stale result job={} trigger={} reason={} captured_prefix_len={} covered={} — originals preserved",
            job.id,
            job.trigger.as_str(),
            reason.as_str(),
            job.prefix.active_len,
            job.prefix.covered
        ));
        self.job_slot.finish(job.id);
        if self.job_slot.record_failure(now) {
            crate::logging::warn(&format!(
                "[compaction/job] automatic attempts suspended for {}s after {} consecutive failures",
                jobs::BREAKER_SUSPENSION.as_secs(),
                jobs::BREAKER_THRESHOLD
            ));
        }
        self.last_job_report = Some(jobs::JobReport {
            job_id: job.id,
            trigger: job.trigger,
            generation: job.identity.generation,
            outcome: jobs::JobOutcomeKind::Discarded(reason),
            covered: job.prefix.covered,
        });
        self.pending_job = None;
        self.pending_cancel = None;
        self.pending_cutoff = 0;
        self.pending_trigger = None;
    }

    /// Apply one finished compaction result to the live state.
    ///
    /// The result may cover less than the job's cutoff after an overflow
    /// shrink; the messages between the two stay in the live tail in their
    /// original order.
    fn apply_compaction_result(&mut self, result: CompactionResult, trigger: String, all_messages: &[Message]) -> bool {
        self.log_compaction_state("apply_start", &trigger, all_messages);
        let now = tokio::time::Instant::now();
        let Some(job) = self.pending_job.clone() else {
            crate::logging::warn(&format!(
                "[compaction/job] discarding result for job {} — no job owns the slot",
                result.job_id
            ));
            self.pending_cutoff = 0;
            self.pending_trigger = None;
            return false;
        };
        if job.id != result.job_id {
            crate::logging::warn(&format!(
                "[compaction/job] discarding result from job {} while job {} owns the slot",
                result.job_id, job.id
            ));
            return false;
        }

        let history = (!all_messages.is_empty()).then_some(all_messages);
        if !recovery::result_is_applicable(
            &result.summary_text,
            result.openai_encrypted_content.as_deref(),
            result
                .openai_encrypted_content
                .as_deref()
                .is_some_and(openai_encrypted_content_is_sendable),
        ) {
            crate::logging::warn(
                "[compaction/job] discarding result with no usable summary text and no replayable native window",
            );
            self.discard_job(job, jobs::DiscardReason::PrefixChanged, now);
            return false;
        }
        let prior_text = self.active_summary.as_ref().map(|summary| summary.text.as_str());
        let identity = self.current_job_identity();
        let covered = match self
            .job_slot
            .revalidate(&job, history, self.compacted_count, prior_text, &identity, now)
        {
            Ok(covered) => covered,
            Err(reason) => {
                self.discard_job(job, reason, now);
                return false;
            }
        };

        let active_len = self.active_messages(all_messages).len();
        let applied_cutoff = result.covered_messages.min(covered);
        let leaves_no_healthy_tail = applied_cutoff > active_len.saturating_sub(MIN_TURNS_TO_KEEP);
        if !all_messages.is_empty() && leaves_no_healthy_tail {
            self.discard_job(job, jobs::DiscardReason::PrefixChanged, now);
            return false;
        }

        let summary = Summary {
            text: result.summary_text,
            openai_encrypted_content: result.openai_encrypted_content,
            native_window: result.native_window,
            native_route: result.native_route.as_ref().map(|route| NativeRouteBinding {
                provider: route.provider.clone(),
                model: route.model.clone(),
                route: route.route.clone(),
            }),
            covers_up_to_turn: applied_cutoff,
            original_turn_count: applied_cutoff,
        };
        let active = self.active_messages(all_messages);
        let tail = &active[applied_cutoff.min(active.len())..];
        let post_tokens = estimate_compaction_tokens_from_text(Some(&summary), tail, self.token_budget);
        if post_tokens > self.token_budget {
            self.discard_job(job, jobs::DiscardReason::TailBudgetExceeded, now);
            return false;
        }

        let pre_tokens = self.effective_token_count_with(all_messages) as u64;
        let compacted_chars: usize = self
            .active_messages(all_messages)
            .iter()
            .take(applied_cutoff)
            .map(message_char_count)
            .sum();

        self.compacted_count = self.compacted_count.saturating_add(applied_cutoff);
        if !all_messages.is_empty() {
            self.compacted_count = self.compacted_count.min(all_messages.len());
        }
        self.active_chars.set_exact(
            self.active_message_chars_with(all_messages)
                .saturating_sub(compacted_chars),
        );

        self.active_summary = Some(summary);
        self.active_summary_usage = (result.input_tokens, result.output_tokens);
        self.active_restoration = result.restoration.as_ref().map(stored_restoration);
        self.active_lossy = false;
        self.recovery_blocked = None;
        self.discard_oversized_openai_native_compaction();
        self.observed_input_tokens = None;
        let post_tokens = self.effective_token_count_with(all_messages) as u64;
        self.last_compaction = Some(CompactionEvent {
            trigger: trigger.clone(),
            pre_tokens: Some(pre_tokens),
            post_tokens: Some(post_tokens),
            tokens_saved: Some(pre_tokens.saturating_sub(post_tokens)),
            duration_ms: Some(result.duration_ms),
            messages_dropped: None,
            messages_compacted: Some(applied_cutoff),
            summary_chars: self.active_summary.as_ref().map(|summary| summary.text.len()),
            active_messages: Some(self.active_messages_count()),
        });
        self.record_compaction_saving(pre_tokens, post_tokens);
        crate::logging::info(&format!(
            "[TIMING] compaction_complete: trigger={}, duration={}ms, pre_tokens={}, post_tokens={}, tokens_saved={}, messages_compacted={}, summary_chars={}, active_messages={}",
            self.last_compaction
                .as_ref()
                .map(|event| event.trigger.as_str())
                .unwrap_or("unknown"),
            result.duration_ms,
            pre_tokens,
            post_tokens,
            pre_tokens.saturating_sub(post_tokens),
            applied_cutoff,
            self.active_summary
                .as_ref()
                .map(|summary| summary.text.len())
                .unwrap_or(0),
            self.active_messages_count(),
        ));
        self.log_compaction_outcome(CompactionOutcomeLog {
            trigger: &trigger,
            pre_tokens,
            post_tokens,
            messages_compacted: applied_cutoff,
            messages_dropped: None,
            duration_ms: result.duration_ms,
            all_messages,
        });

        // Reset cooldown counter so proactive/semantic modes don't
        // fire again immediately after a successful compaction.
        self.turns_since_last_compact = 0;

        self.job_slot.mark_applied(job.id);
        self.job_slot.finish(job.id);
        self.job_slot.record_success();
        self.last_job_report = Some(jobs::JobReport {
            job_id: job.id,
            trigger: job.trigger,
            generation: job.identity.generation,
            outcome: jobs::JobOutcomeKind::Applied,
            covered: applied_cutoff,
        });
        self.pending_job = None;
        self.pending_cancel = None;
        self.pending_cutoff = 0;
        self.pending_trigger = None;
        // The boundary moved: anything planned against the previous prefix is
        // now stale even though only a tail append happened since capture.
        self.generation = self.generation.wrapping_add(1);
        true
    }

    /// Backward-compatible completion check without caller history.
    pub fn check_and_apply_compaction(&mut self) {
        self.check_and_apply_compaction_with(&[]);
        self.active_chars.invalidate();
    }

    /// Take the last compaction event (if any)
    pub fn take_compaction_event(&mut self) -> Option<CompactionEvent> {
        self.last_compaction.take()
    }

    /// Get messages for API call (with summary if compacted).
    /// Takes the full message list from the caller.
    pub fn messages_for_api_with(&mut self, all_messages: &[Message]) -> Vec<Message> {
        self.check_and_apply_compaction_with(all_messages);
        self.discard_oversized_openai_native_compaction();

        let active = self.active_messages(all_messages);

        let mut result = match &self.active_summary {
            Some(summary) => {
                // A native state replays its retained window whole, in order. A
                // window-less native state (persisted before the window was
                // kept) falls back to the single compaction item, and a
                // structured summary becomes text.
                let summary_block = match summary
                    .openai_encrypted_content
                    .as_ref()
                    .map(|encrypted_content| (encrypted_content, summary.native_window.clone()))
                {
                    Some((encrypted_content, window)) => ContentBlock::OpenAICompaction {
                        encrypted_content: encrypted_content.clone(),
                        window,
                    },
                    None => ContentBlock::Text {
                        text: compacted_summary_text_block(&summary.text),
                        cache_control: None,
                    },
                };

                let mut result = Vec::with_capacity(active.len() + 1);

                result.push(Message {
                    role: Role::User,
                    content: vec![summary_block],
                    timestamp: None,
                    tool_duration_ms: None,
                });

                // Clone only the active (non-compacted) messages
                result.extend(active.iter().cloned());

                result
            }
            None => active.to_vec(),
        };

        // Reducing an oversized projection happens on this owned copy only. The
        // caller's messages are the stored originals and keep their exact bytes.
        if estimate_compaction_tokens_from_text(None, &result, self.token_budget) > self.token_budget {
            let truncated = emergency_truncate_large_payloads(
                &mut result,
                EMERGENCY_TOOL_RESULT_MAX_CHARS,
                EMERGENCY_IMAGE_MAX_CHARS,
            );
            if truncated > 0 {
                self.projection_truncated = true;
                crate::logging::warn(&format!(
                    "[compaction/projection] shortened {truncated} oversized payload(s) in the provider projection; originals unchanged"
                ));
            }
        }

        result
    }

    /// Check if compaction is in progress
    pub fn is_compacting(&self) -> bool {
        self.pending_task.is_some()
    }

    /// Get the active compaction mode
    pub fn mode(&self) -> crate::config::CompactionMode {
        self.mode.clone()
    }

    /// Change the active compaction mode for this session at runtime.
    pub fn set_mode(&mut self, mode: crate::config::CompactionMode) {
        self.mode = mode.clone();
        self.compaction_config.mode = mode;
    }

    fn mode_trigger_label(&self) -> &'static str {
        self.mode.as_str()
    }

    /// Get the number of compacted (summarized) messages
    pub fn compacted_count(&self) -> usize {
        self.compacted_count
    }

    /// Get the character count of the active summary (0 if none)
    pub fn summary_chars(&self) -> usize {
        self.active_summary
            .as_ref()
            .map(summary_payload_char_count)
            .unwrap_or(0)
    }

    /// Get the current number of active, un-compacted messages.
    pub fn active_messages_count(&self) -> usize {
        self.total_turns.saturating_sub(self.compacted_count)
    }

    /// Get stats about current state (without message data)
    pub fn stats(&self) -> CompactionStats {
        CompactionStats {
            total_turns: self.total_turns,
            active_messages: 0, // unknown without messages
            has_summary: self.active_summary.is_some(),
            is_compacting: self.is_compacting(),
            token_estimate: self.token_estimate(),
            effective_tokens: self.effective_token_count(),
            observed_input_tokens: self.observed_input_tokens,
            context_usage: self.context_usage(),
        }
    }

    /// Get stats with full message data
    pub fn stats_with(&self, all_messages: &[Message]) -> CompactionStats {
        let active = self.active_messages(all_messages);
        CompactionStats {
            total_turns: self.total_turns,
            active_messages: active.len(),
            has_summary: self.active_summary.is_some(),
            is_compacting: self.is_compacting(),
            token_estimate: self.token_estimate_with(all_messages),
            effective_tokens: self.effective_token_count_with(all_messages),
            observed_input_tokens: self.observed_input_tokens,
            context_usage: self.context_usage_with(all_messages),
        }
    }

    fn cached_semantic_embedding(&mut self, text: &str) -> Option<Vec<f32>> {
        let key = semantic_cache_key(text);

        if let Some((cached, recency)) = self.semantic_embed_cache.get_mut(&key) {
            let counter = self.semantic_embed_cache_counter;
            self.semantic_embed_cache_counter = counter.wrapping_add(1);
            *recency = counter;
            return cached.clone();
        }

        let embedding = crate::embedding::embed(text).ok();
        self.insert_semantic_embedding_cache(key, embedding.clone());
        embedding
    }

    fn insert_semantic_embedding_cache(&mut self, key: u64, embedding: Option<Vec<f32>>) {
        if self.semantic_embed_cache.len() >= SEMANTIC_EMBED_CACHE_CAPACITY {
            let oldest_key = self
                .semantic_embed_cache
                .iter()
                .min_by_key(|(_, (_, recency))| *recency)
                .map(|(&key, _)| key);
            if let Some(oldest_key) = oldest_key {
                self.semantic_embed_cache.remove(&oldest_key);
            }
        }

        let counter = self.semantic_embed_cache_counter;
        self.semantic_embed_cache_counter = counter.wrapping_add(1);
        self.semantic_embed_cache.insert(key, (embedding, counter));
    }

    /// Poll for compaction completion and return an event if one was applied.
    pub fn poll_compaction_event_with(&mut self, all_messages: &[Message]) -> Option<CompactionEvent> {
        self.check_and_apply_compaction_with(all_messages);
        self.take_compaction_event()
    }

    /// Emergency hard compaction: drop old messages without summarizing.
    /// Takes the caller's full message list to inspect content.
    ///
    /// When the remaining turns (after keeping the recent-tail token allowance)
    /// still exceed the token budget, progressively keeps fewer turns down to
    /// `MIN_TURNS_TO_KEEP`.
    pub fn hard_compact_with(&mut self, all_messages: &[Message]) -> Result<usize, String> {
        if self.clamp_compacted_count_to_messages(all_messages, "hard_compact_start") {
            self.log_compaction_state("hard_compact_clamped", "hard_compact", all_messages);
        }

        let active = self.active_messages(all_messages);

        if active.len() <= MIN_TURNS_TO_KEEP {
            return Err(format!(
                "Not enough messages to compact (have {}, need more than {})",
                active.len(),
                MIN_TURNS_TO_KEEP
            ));
        }

        let pre_tokens = self.effective_token_count_with(all_messages) as u64;
        self.log_compaction_state("hard_compact_start", "hard_compact", all_messages);
        let active_char_counts: Vec<usize> = active.iter().map(message_char_count).collect();
        let mut remaining_suffix_chars = vec![0usize; active_char_counts.len() + 1];
        for idx in (0..active_char_counts.len()).rev() {
            remaining_suffix_chars[idx] = remaining_suffix_chars[idx + 1].saturating_add(active_char_counts[idx]);
        }
        // The budget is in tokens, so the suffix has to be summed in tokens too:
        // a Korean message costs about one token per syllable, not one per four
        // bytes, and dividing the character sum would drop it too late.
        let active_token_estimates: Vec<usize> = active.iter().map(message_token_count).collect();
        let mut remaining_suffix_tokens = vec![0usize; active_token_estimates.len() + 1];
        for idx in (0..active_token_estimates.len()).rev() {
            remaining_suffix_tokens[idx] = remaining_suffix_tokens[idx + 1].saturating_add(active_token_estimates[idx]);
        }

        let mut turns_to_keep = active
            .len()
            .saturating_sub(self.recent_cutoff_from(active))
            .min(active.len().saturating_sub(1));
        let mut cutoff;
        loop {
            cutoff = active.len().saturating_sub(turns_to_keep);
            cutoff = safe_compaction_cutoff(active, cutoff);

            if cutoff > 0 {
                let remaining_tokens = remaining_suffix_tokens[cutoff];
                if remaining_tokens <= self.token_budget {
                    break;
                }
            }

            if turns_to_keep <= MIN_TURNS_TO_KEEP {
                cutoff = active.len().saturating_sub(MIN_TURNS_TO_KEEP);
                cutoff = safe_compaction_cutoff(active, cutoff);
                break;
            }
            turns_to_keep = (turns_to_keep / 2).max(MIN_TURNS_TO_KEEP);
        }

        if cutoff == 0 {
            return Err("Cannot compact — would split tool call/result pairs".to_string());
        }

        // This hard compact will advance `compacted_count` and supersede any
        // in-flight background (reactive/proactive/semantic) compaction. That
        // background task summarized messages relative to the *old*
        // `compacted_count`; if it completed afterwards, `check_and_apply_*`
        // would add its stale cutoff on top of the already-advanced
        // `compacted_count`, double-compacting and wiping out all live messages
        // (observed as "kept 0 recent messages"). Cancel it now that we're
        // committed to the hard compact, and bump the generation so any result
        // that still slips through is discarded.
        if self.pending_task.is_some() {
            crate::logging::warn(&format!(
                "[compaction] Aborting in-flight background compaction (pending_cutoff={}, trigger={:?}) — superseded by hard compact",
                self.pending_cutoff, self.pending_trigger,
            ));
        }
        self.cancel_pending(jobs::CancelReason::Superseded);
        self.generation = self.generation.wrapping_add(1);

        let dropped_count = cutoff;
        let summary_text = build_emergency_summary_text(
            self.active_summary.as_ref().map(|summary| summary.text.as_str()),
            dropped_count,
            pre_tokens,
            self.token_budget,
            &active[..cutoff],
        );

        let summary = Summary {
            text: summary_text,
            openai_encrypted_content: None,
            native_window: Vec::new(),
            native_route: None,
            covers_up_to_turn: cutoff,
            original_turn_count: cutoff,
        };

        self.compacted_count = self.compacted_count.saturating_add(cutoff).min(all_messages.len());
        self.active_chars.set_exact(remaining_suffix_chars[cutoff]);
        self.active_summary = Some(summary);
        self.active_summary_usage = (None, None);
        self.active_lossy = true;
        self.observed_input_tokens = None;
        let post_tokens = self.effective_token_count_with(all_messages) as u64;
        self.last_compaction = Some(CompactionEvent {
            trigger: "hard_compact".to_string(),
            pre_tokens: Some(pre_tokens),
            post_tokens: Some(post_tokens),
            tokens_saved: Some(pre_tokens.saturating_sub(post_tokens)),
            duration_ms: Some(0),
            messages_dropped: Some(dropped_count),
            messages_compacted: Some(dropped_count),
            summary_chars: self.active_summary.as_ref().map(|summary| summary.text.len()),
            active_messages: Some(self.active_messages_count()),
        });
        self.record_compaction_saving(pre_tokens, post_tokens);
        self.log_compaction_outcome(CompactionOutcomeLog {
            trigger: "hard_compact",
            pre_tokens,
            post_tokens,
            messages_compacted: dropped_count,
            messages_dropped: Some(dropped_count),
            duration_ms: 0,
            all_messages,
        });

        Ok(dropped_count)
    }

    /// Emergency truncation: shorten large tool results in active messages.
    ///
    /// When hard compaction isn't sufficient (the remaining few turns are
    /// individually too large), this truncates tool result content so the
    /// conversation can fit within the token budget.
    ///
    /// Returns the number of tool results that were truncated.
    pub fn emergency_truncate_with(&mut self, all_messages: &mut [Message]) -> usize {
        let start = self.compacted_count.min(all_messages.len());
        let active = &mut all_messages[start..];
        let truncated =
            emergency_truncate_large_payloads(active, EMERGENCY_TOOL_RESULT_MAX_CHARS, EMERGENCY_IMAGE_MAX_CHARS);

        if truncated > 0 {
            self.observed_input_tokens = None;
            self.active_chars.invalidate();
        }
        truncated
    }

    /// Synchronously force the context back under budget without waiting for a
    /// background summary.
    ///
    /// This is the shared escalation policy used by every emergency-recovery
    /// caller: drop old turns via [`hard_compact_with`], then — only if the
    /// context is *still* over budget — shorten oversized tool results via
    /// [`emergency_truncate_with`]. Previously each caller open-coded this
    /// sequence with subtly different escalation (one retried after a hard
    /// compact without re-checking the budget), so centralizing it both removes
    /// the duplication and guarantees consistent behavior.
    ///
    /// Returns a structured outcome so callers can render their own
    /// user-facing message. `pre_usage` is the context usage fraction observed
    /// before recovery (captured here so the report matches what triggered it).
    pub fn recover_within_budget(&mut self, all_messages: &mut [Message]) -> EmergencyRecovery {
        let pre_usage = self.context_usage_with(all_messages);

        let dropped = match self.hard_compact_with(all_messages) {
            Ok(dropped) => Some(dropped),
            Err(reason) => {
                crate::logging::warn(&format!(
                    "[compaction] recover_within_budget: hard compact failed ({reason})"
                ));
                None
            }
        };

        // Only escalate to truncation when dropping turns did not get us under
        // budget (or could not run at all).
        let still_over_budget = self.context_usage_with(all_messages) > 1.0 || dropped.is_none();
        let truncated = if still_over_budget {
            self.emergency_truncate_with(all_messages)
        } else {
            0
        };

        EmergencyRecovery {
            pre_usage,
            dropped,
            truncated,
        }
    }
}

/// Outcome of [`CompactionManager::recover_within_budget`].
#[derive(Debug, Clone, Copy)]
pub struct EmergencyRecovery {
    /// Context usage fraction (1.0 == full budget) observed before recovery.
    pub pre_usage: f32,
    /// Messages dropped by the hard compact, or `None` if it could not run.
    pub dropped: Option<usize>,
    /// Number of oversized tool results that were truncated as a fallback.
    pub truncated: usize,
}

impl EmergencyRecovery {
    /// Whether any space-reclaiming action actually happened.
    pub fn did_anything(&self) -> bool {
        self.dropped.unwrap_or(0) > 0 || self.truncated > 0
    }

    /// A user-facing description of what recovery did, without a trailing
    /// call to action (callers append their own, e.g. "Retrying..." or
    /// "You can continue."). `trigger_usage` is the usage fraction that
    /// triggered recovery (rendered as a percentage).
    pub fn summary_line(&self, trigger_usage: f32) -> String {
        let pct = trigger_usage * 100.0;
        match (self.dropped, self.truncated) {
            (Some(dropped), 0) => {
                format!("⚡ Emergency compaction: dropped {dropped} old messages (context was at {pct:.0}%).",)
            }
            (Some(dropped), truncated) => format!(
                "⚡ Emergency compaction: dropped {dropped} old messages and truncated {truncated} tool result(s) (context was at {pct:.0}%).",
            ),
            (None, truncated) => {
                format!("⚡ Emergency truncation: shortened {truncated} large tool result(s) to fit context.",)
            }
        }
    }
}

impl Default for CompactionManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether a provider error is worth retrying inside the same job deadline.
///
/// Overflow errors are not transient: they are handled by shrinking the
/// covered range inside [`summary::generate_structured_summary`].
fn is_transient_provider_error(detail: &str) -> bool {
    let lowered = detail.to_ascii_lowercase();
    const MARKERS: &[&str] = &[
        "rate limit",
        "429",
        "500",
        "502",
        "503",
        "504",
        "timeout",
        "timed out",
        "connection reset",
        "connection refused",
        "connection closed",
        "temporarily unavailable",
        "service unavailable",
        "overloaded",
    ];
    MARKERS.iter().any(|marker| lowered.contains(marker))
}

/// Run one job's provider call under its deadline and cancellation signal.
///
/// Transient failures are retried at most [`jobs::MAX_TRANSIENT_RETRIES`] times
/// after the first attempt, all inside the same job deadline. Each attempt is
/// additionally capped by [`jobs::STREAM_IDLE_DEADLINE`] measured from the
/// moment the attempt started, so a provider that stops responding cannot hold
/// the slot forever while a slow but progressing attempt keeps its budget.
#[allow(clippy::too_many_arguments)]
async fn run_compaction_job(
    provider: Arc<dyn Provider>,
    active: &[Message],
    candidate_end: usize,
    existing_summary: Option<&Summary>,
    limits: SummaryLimits,
    native_route: Option<crate::session::StoredNativeRoute>,
    job_deadline: tokio::time::Instant,
    mut cancel: tokio::sync::oneshot::Receiver<()>,
) -> Result<CompactionResult, JobFailure> {
    let mut retries = 0u32;
    loop {
        let idle_deadline = tokio::time::Instant::now() + jobs::STREAM_IDLE_DEADLINE;
        let deadline = job_deadline.min(idle_deadline);
        let attempt = tokio::select! {
            biased;
            _ = &mut cancel => {
                return Err(JobFailure {
                    kind: jobs::JobOutcomeKind::Cancelled(jobs::CancelReason::Manual),
                    detail: "compaction job cancelled".to_string(),
                });
            }
            attempt = tokio::time::timeout_at(
                deadline,
                generate_compaction_artifact(
                    provider.clone(),
                    active,
                    candidate_end,
                    existing_summary,
                    limits,
                    native_route.clone(),
                ),
            ) => attempt,
        };
        match attempt {
            Err(_elapsed) => {
                return Err(JobFailure {
                    kind: jobs::JobOutcomeKind::TimedOut,
                    detail: format!(
                        "compaction job exceeded its deadline ({})",
                        jobs::JOB_DEADLINE.as_secs()
                    ),
                });
            }
            Ok(Ok(result)) => {
                return Ok(result);
            }
            Ok(Err(error)) => {
                let detail = error.to_string();
                if is_transient_provider_error(&detail) && retries < jobs::MAX_TRANSIENT_RETRIES {
                    retries += 1;
                    crate::logging::warn(&format!(
                        "[compaction/job] transient provider error (retry {retries}/{}): {detail}",
                        jobs::MAX_TRANSIENT_RETRIES
                    ));
                    continue;
                }
                return Err(JobFailure {
                    kind: jobs::JobOutcomeKind::Failed,
                    detail,
                });
            }
        }
    }
}

/// Generate summary using the provider
async fn generate_compaction_artifact(
    provider: Arc<dyn Provider>,
    active: &[Message],
    candidate_end: usize,
    existing_summary: Option<&Summary>,
    limits: SummaryLimits,
    native_route: Option<crate::session::StoredNativeRoute>,
) -> Result<CompactionResult> {
    let start = Instant::now();
    let mut existing_summary = existing_summary.cloned();
    if let Some(summary) = existing_summary.as_mut()
        && let Some(encrypted_content) = summary.openai_encrypted_content.as_ref()
        && !openai_encrypted_content_is_sendable(encrypted_content)
    {
        let encrypted_content_len = encrypted_content.len();
        crate::logging::warn(&format!(
            "[compaction] Existing OpenAI native compaction payload is oversized ({} chars); falling back to text summary",
            encrypted_content_len,
        ));
        summary.openai_encrypted_content = None;
        let fallback = openai_encrypted_content_fallback_summary(encrypted_content_len);
        if summary.text.trim().is_empty() {
            summary.text = fallback;
        } else if !summary.text.contains("OpenAI native compaction state was discarded") {
            summary.text.push_str("\n\n");
            summary.text.push_str(&fallback);
        }
    }

    let candidate_end = candidate_end.min(active.len());
    if let Ok(native) = provider
        .native_compact(
            &active[..candidate_end],
            existing_summary.as_ref().map(|summary| summary.text.as_str()),
            existing_summary
                .as_ref()
                .and_then(|summary| summary.openai_encrypted_content.as_deref()),
        )
        .await
    {
        // A native result only counts as compaction when its window can be
        // replayed to [OI] as-is. An oversized or malformed window would let the
        // compacted boundary advance while the next request replays a
        // substitute, so it falls through to a structured summary built from
        // the originals instead.
        match native_replayable_window(&native) {
            Ok(window) => {
                return Ok(CompactionResult {
                    job_id: 0,
                    summary_text: native.summary_text.unwrap_or_default(),
                    openai_encrypted_content: native.openai_encrypted_content,
                    native_window: window,
                    native_route,
                    duration_ms: start.elapsed().as_millis() as u64,
                    summarized_messages: candidate_end,
                    covered_messages: candidate_end,
                    input_tokens: None,
                    output_tokens: None,
                    restoration: None,
                });
            }
            Err(reason) => crate::logging::warn(&format!(
                "[compaction] [OI] native compaction window is not replayable ({reason}); falling back to a structured summary from the originals",
            )),
        }
    }
    let projection = SourceProjection::from_messages(active);
    let prior = existing_summary.as_ref().map(|summary| PriorSummary {
        text: summary.text.clone(),
        compaction_id: None,
        content_hash: None,
    });
    // The summary model is a structured-path choice only; a native route keeps
    // the session model so its replayable window stays on one model.
    let configured_summary_model = if native_route.is_none() {
        crate::config::config().compaction.summary_model.clone()
    } else {
        None
    };
    let (bound, _summary_provider) = summary::generate_structured_summary_with_model(
        &provider,
        configured_summary_model.as_deref(),
        &projection,
        candidate_end,
        prior.as_ref(),
        &limits,
    )
    .await
    .map_err(|error| anyhow::anyhow!(error))?;

    Ok(CompactionResult {
        job_id: 0,
        summary_text: bound.summary_text,
        openai_encrypted_content: None,
        native_window: Vec::new(),
        native_route: None,
        duration_ms: start.elapsed().as_millis() as u64,
        summarized_messages: bound.covered.len(),
        covered_messages: bound.covered.end(),
        input_tokens: bound.input_tokens,
        output_tokens: bound.output_tokens,
        restoration: (!bound.restoration.entries.is_empty()).then_some(bound.restoration),
    })
}

/// The ordered window to replay for a native compaction result, or why it
/// cannot be replayed.
///
/// A result that carries only `encrypted_content` is promoted to a
/// single-item window, so every native state has one representation. A window
/// is replayable only when it contains the compaction item, that payload is
/// within [OI]'s per-string limit, and every retained item is an object with a
/// string `type`.
pub fn native_replayable_window(
    native: &horon_agent_provider_core::NativeCompactionResult,
) -> std::result::Result<Vec<serde_json::Value>, String> {
    let window = if native.window.is_empty() {
        native
            .openai_encrypted_content
            .as_ref()
            .map(|encrypted_content| {
                vec![serde_json::json!({
                    "type": "compaction",
                    "encrypted_content": encrypted_content,
                })]
            })
            .unwrap_or_default()
    } else {
        native.window.clone()
    };
    if window.is_empty() {
        return Err("the response carried no compaction item".to_string());
    }
    let encrypted_content = window
        .iter()
        .find_map(|item| {
            (item.get("type").and_then(|value| value.as_str()) == Some("compaction"))
                .then(|| item.get("encrypted_content").and_then(|value| value.as_str()))
                .flatten()
        })
        .ok_or_else(|| "the window has no compaction item".to_string())?;
    if !openai_encrypted_content_is_sendable(encrypted_content) {
        return Err(format!(
            "encrypted_content is {} chars, above the safe replay limit of {} chars",
            encrypted_content.len(),
            crate::provider::openai_request::OPENAI_ENCRYPTED_CONTENT_SAFE_MAX_CHARS,
        ));
    }
    for item in &window {
        if !item.is_object() {
            return Err("a retained window item is not an object".to_string());
        }
        if item.get("type").and_then(|value| value.as_str()).is_none() {
            return Err("a retained window item has no string type".to_string());
        }
    }
    Ok(window)
}

fn stored_restoration(manifest: &summary::RestorationManifest) -> crate::session::StoredRestorationManifest {
    crate::session::StoredRestorationManifest {
        entries: manifest
            .entries
            .iter()
            .map(|entry| crate::session::StoredRestorationEntry {
                path: entry.path.clone(),
                kind: entry.kind.clone(),
                note: entry.note.clone(),
            })
            .collect(),
        total_bytes: manifest.total_bytes,
    }
}

pub async fn build_transfer_compaction_state(
    provider: Arc<dyn Provider>,
    messages: Vec<Message>,
    existing_state: Option<crate::session::StoredCompactionState>,
) -> Result<Option<crate::session::StoredCompactionState>> {
    let existing_summary = existing_state.as_ref().map(|state| Summary {
        text: state.summary_text.clone(),
        openai_encrypted_content: state.openai_encrypted_content.clone(),
        native_window: state.native_window.clone(),
        native_route: state.native_route.as_ref().map(|route| NativeRouteBinding {
            provider: route.provider.clone(),
            model: route.model.clone(),
            route: route.route.clone(),
        }),
        covers_up_to_turn: state.covers_up_to_turn,
        original_turn_count: state.original_turn_count,
    });

    if messages.is_empty() {
        return Ok(existing_state.map(|mut state| {
            state.compacted_count = 0;
            state
        }));
    }

    let prior_turns = existing_state
        .as_ref()
        .map(|state| state.original_turn_count.max(state.covers_up_to_turn))
        .unwrap_or(0);
    let mut limits = SummaryLimits::for_window(provider.context_window());
    limits.keep_newest_user = false;
    let candidate_end = messages.len();
    let result = generate_compaction_artifact(
        provider,
        &messages,
        candidate_end,
        existing_summary.as_ref(),
        limits,
        None,
    )
    .await?;
    let total_turns = prior_turns + messages.len();

    Ok(Some(crate::session::StoredCompactionState {
        summary_text: result.summary_text,
        openai_encrypted_content: result.openai_encrypted_content,
        native_window: result.native_window,
        native_route: result
            .native_route
            .as_ref()
            .map(|route| crate::session::StoredNativeRoute {
                provider: route.provider.clone(),
                model: route.model.clone(),
                route: route.route.clone(),
            }),
        covers_up_to_turn: total_turns,
        original_turn_count: total_turns,
        compacted_count: 0,
        structured: Some(crate::session::StoredSummaryMetadata {
            input_tokens: result.input_tokens,
            output_tokens: result.output_tokens,
            restoration: result.restoration.as_ref().map(stored_restoration),
            lossy_emergency: false,
            recovery_blocked: None,
        }),
    }))
}

#[cfg(test)]
#[path = "../compaction_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "jobs_tests.rs"]
mod jobs_tests;

#[cfg(test)]
#[path = "structured_summary_tests.rs"]
mod structured_summary_tests;

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod recovery_tests;
