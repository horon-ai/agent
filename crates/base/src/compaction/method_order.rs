//! `[compaction] method_order`: which compaction methods to try, and in what order.
//!
//! `strategy` names exactly one method. An order names a chain, so a method that
//! cannot produce a boundary this time does not leave the turn uncompacted: the
//! job moves on to the next method inside the same slot, over the same captured
//! prefix, under the same deadline. The trigger is never re-evaluated, because a
//! re-check could drop the job below threshold and lose the compaction entirely.
//!
//! Resolution is lenient on purpose. A malformed order is corrected when a job
//! starts, never at config load: a strict parse that dies during a reload is the
//! failure issue #349 already paid for.
//!
//! `tool_result_aging` is deliberately not a method here. It runs before any
//! summarization and fires on cache temperature and threshold proximity rather
//! than turn order, so "the history already changed" cannot be expressed as a
//! position in this chain.

use super::jobs::{self, JobOutcomeKind};
use super::{CompactionResult, JobFailure, Summary, SummaryLimits, generate_compaction_artifact};
use crate::message::Message;
use crate::provider::Provider;
use horon_agent_config_types::CompactionStrategy;
use std::sync::Arc;
use tokio::time::Instant;

/// How many consecutive failures retire one method from the chain.
///
/// Deliberately separate from [`jobs::BREAKER_THRESHOLD`]: the breaker suspends
/// every compaction attempt, while this only stops retrying one method that has
/// proven it cannot produce a boundary right now.
pub const METHOD_STRIKES: u32 = 2;

/// One compaction method a chain may name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Handoff,
    Structured,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Handoff => "handoff",
            Self::Structured => "structured",
        }
    }

    /// Parse one order entry. `None` covers both an unknown name and a
    /// provider-native strategy, which [`MethodChain::resolve`] treats apart.
    fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "handoff" => Some(Self::Handoff),
            "structured" => Some(Self::Structured),
            _ => None,
        }
    }

    /// The method for a configured strategy, or `None` when the provider owns
    /// compaction and no method of ours may run.
    pub fn from_strategy(strategy: CompactionStrategy) -> Option<Self> {
        match strategy {
            CompactionStrategy::Handoff => Some(Self::Handoff),
            CompactionStrategy::Structured => Some(Self::Structured),
            CompactionStrategy::OpenAiAuto | CompactionStrategy::OpenAiExplicit => None,
        }
    }
}

/// Why one method did not produce a boundary, and whether the chain may move on.
#[derive(Debug)]
pub struct MethodFailure {
    pub detail: String,
    /// The failure names the method, not the turn: another method would write a
    /// document over the same range, so this job can still be compacted.
    pub advance: bool,
}

impl MethodFailure {
    /// Retrying the same method is pointless; the next one may still work.
    pub fn advance(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            advance: true,
        }
    }
}

/// The methods one job may try, in order, plus the corrections it needed.
#[derive(Clone, Debug, Default)]
pub struct MethodChain {
    methods: Vec<Method>,
    notes: Vec<String>,
}

impl MethodChain {
    /// The chain a strategy names on its own: `handoff` keeps the structured
    /// summarizer as its one fallback, so a configured handoff whose document
    /// cannot be written still compacts that turn.
    pub fn single(strategy: CompactionStrategy) -> Self {
        let methods = match Method::from_strategy(strategy) {
            Some(Method::Handoff) => vec![Method::Handoff, Method::Structured],
            Some(method) => vec![method],
            None => Vec::new(),
        };
        Self {
            methods,
            notes: Vec::new(),
        }
    }

    /// Resolve `[compaction] method_order` against the configured strategy.
    ///
    /// An absent or empty order reproduces the single-strategy behaviour. A list
    /// that does not end in `structured` is discarded, because the last method is
    /// the only one with no further fallback. A provider-native name discards the
    /// list too: native compaction owns the transcript exclusively. An unknown
    /// name is only dropped.
    pub fn resolve(order: &[String], strategy: CompactionStrategy) -> Self {
        if order.is_empty() || strategy.is_native() {
            return Self::single(strategy);
        }
        let mut notes = Vec::new();
        let mut methods: Vec<Method> = Vec::new();
        for name in order {
            if let Some(method) = Method::parse(name) {
                if !methods.contains(&method) {
                    methods.push(method);
                }
                continue;
            }
            if CompactionStrategy::parse(name).is_some_and(CompactionStrategy::is_native) {
                return Self {
                    methods: Self::single(strategy).methods,
                    notes: vec![format!(
                        "[compaction] method_order mixes provider-native compaction ({name}) with other methods; \
                         native compaction owns the transcript exclusively, so the order is ignored"
                    )],
                };
            }
            notes.push(format!(
                "[compaction] method_order entry {name:?} is not a compaction method; ignoring it"
            ));
        }
        if methods.last() != Some(&Method::Structured) {
            notes.push(
                "[compaction] method_order must end in `structured`; the order is ignored and `strategy` is used"
                    .to_string(),
            );
            return Self {
                methods: Self::single(strategy).methods,
                notes,
            };
        }
        Self { methods, notes }
    }

    pub fn methods(&self) -> &[Method] {
        &self.methods
    }

    /// Stable text identity of the order. A boundary produced by a different
    /// method is a different product, so a reorder must invalidate an in-flight
    /// job.
    pub fn digest(&self) -> String {
        self.methods
            .iter()
            .map(|method| method.as_str())
            .collect::<Vec<_>>()
            .join(">")
    }

    /// Corrections this order needed, for one log line each.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// Drop methods that have failed [`METHOD_STRIKES`] times in a row.
    ///
    /// When every method is exhausted the full chain is returned: a job with no
    /// method at all could never compact, and the breaker owns that decision.
    pub fn without_exhausted(&self, failures: &MethodFailures) -> Self {
        let kept: Vec<Method> = self
            .methods
            .iter()
            .copied()
            .filter(|method| !failures.exhausted(*method))
            .collect();
        if kept.is_empty() || kept.len() == self.methods.len() {
            return self.clone();
        }
        Self {
            methods: kept,
            notes: Vec::new(),
        }
    }
}

/// Consecutive failures per method, held by the manager across jobs.
///
/// One job visits each method at most once, so a per-job counter could never
/// reach [`METHOD_STRIKES`]; the point of the rule is to stop paying for a
/// method that has already failed twice.
#[derive(Clone, Debug, Default)]
pub struct MethodFailures {
    consecutive: Vec<(Method, u32)>,
}

impl MethodFailures {
    /// Count one more consecutive failure of `method`.
    pub fn record(&mut self, method: Method) {
        match self.consecutive.iter_mut().find(|(recorded, _)| *recorded == method) {
            Some((_, count)) => *count += 1,
            None => self.consecutive.push((method, 1)),
        }
    }

    /// A job that produced a boundary proves the chain works again.
    pub fn reset(&mut self) {
        self.consecutive.clear();
    }

    pub fn exhausted(&self, method: Method) -> bool {
        self.consecutive
            .iter()
            .find(|(recorded, _)| *recorded == method)
            .is_some_and(|(_, count)| *count >= METHOD_STRIKES)
    }
}

/// Run one job through `chain`, in order, under one deadline.
///
/// Every method runs in the same slot over the same captured prefix. A method
/// that reports [`MethodFailure::advance`] hands the job to the next one; the
/// last method's failure is the job's failure. Transient provider errors are
/// retried on the same method first, because a network blip says nothing about
/// the method.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_chain(
    provider: Arc<dyn Provider>,
    active: &[Message],
    candidate_end: usize,
    existing_summary: Option<&Summary>,
    limits: SummaryLimits,
    native_route: Option<crate::session::StoredNativeRoute>,
    chain: &MethodChain,
    job_deadline: Instant,
    mut cancel: tokio::sync::oneshot::Receiver<()>,
) -> Result<CompactionResult, JobFailure> {
    for (index, method) in chain.methods().iter().copied().enumerate() {
        let next = chain.methods().get(index + 1).copied();
        let mut retries = 0u32;
        loop {
            let deadline = job_deadline.min(Instant::now() + jobs::STREAM_IDLE_DEADLINE);
            let attempt = tokio::select! {
                biased;
                _ = &mut cancel => {
                    return Err(JobFailure {
                        kind: JobOutcomeKind::Cancelled(jobs::CancelReason::Manual),
                        detail: "compaction job cancelled".to_string(),
                        method: Some(method),
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
                        method,
                    ),
                ) => attempt,
            };
            match attempt {
                Err(_elapsed) => {
                    return Err(JobFailure {
                        kind: JobOutcomeKind::TimedOut,
                        detail: format!(
                            "compaction job exceeded its deadline ({})",
                            jobs::JOB_DEADLINE.as_secs()
                        ),
                        method: Some(method),
                    });
                }
                Ok(Ok(result)) => return Ok(result),
                Ok(Err(failure)) => {
                    if is_transient_provider_error(&failure.detail) && retries < jobs::MAX_TRANSIENT_RETRIES {
                        retries += 1;
                        crate::logging::warn(&format!(
                            "[compaction/job] transient provider error (retry {retries}/{}): {}",
                            jobs::MAX_TRANSIENT_RETRIES,
                            failure.detail
                        ));
                        continue;
                    }
                    match (failure.advance, next) {
                        (true, Some(next)) => {
                            crate::logging::warn(&format!(
                                "[compaction] {} produced no boundary ({}); trying {} for this job",
                                method.as_str(),
                                failure.detail,
                                next.as_str()
                            ));
                            break;
                        }
                        _ => {
                            return Err(JobFailure {
                                kind: JobOutcomeKind::Failed,
                                detail: failure.detail,
                                method: Some(method),
                            });
                        }
                    }
                }
            }
        }
    }
    Err(JobFailure {
        kind: JobOutcomeKind::Failed,
        detail: "the method chain held no method".to_string(),
        method: None,
    })
}

/// Whether a provider failure is worth retrying inside the same job deadline.
///
/// Overflow errors are not transient: they are handled by shrinking the
/// covered range inside [`super::summary::generate_structured_summary`].
fn is_transient_provider_error(detail: &str) -> bool {
    let lowered = detail.to_ascii_lowercase();
    const MARKERS: [&str; 14] = [
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
