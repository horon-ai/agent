use super::*;
use crate::provider::{EventStream, Provider};
use jobs::{
    CancelReason, CapturedPrefix, Cooldown, DiscardReason, FailureBreaker, JobIdentity, JobOutcomeKind, JobTrigger,
    SingleFlight, StartGate, StartRefusal, lead_tokens, prefix_digest,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::time::Instant;

const WINDOW: usize = 200_000;
const MESSAGE_CHARS: usize = 4_000;

fn text_message(role: Role, text: &str) -> Message {
    Message {
        role,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }
}

fn messages(count: usize) -> Vec<Message> {
    (0..count)
        .map(|index| {
            let role = if index % 2 == 0 { Role::User } else { Role::Assistant };
            text_message(role, &"x".repeat(MESSAGE_CHARS))
        })
        .collect()
}

fn artifact_json(goal: &str) -> String {
    serde_json::json!({
        "goal": goal,
        "done": ["summarized the covered range"],
        "incomplete": [],
        "cancelled": [],
        "files": [],
        "evidence": ["compaction_jobs test fixture"],
        "decisions": [],
        "next_action": "continue"
    })
    .to_string()
}

struct GatedProvider {
    permits: Arc<Semaphore>,
    calls: Arc<AtomicUsize>,
    failures_before_success: Arc<AtomicUsize>,
    error_text: &'static str,
    window: usize,
}

impl GatedProvider {
    fn new(failures_before_success: usize, error_text: &'static str) -> (Self, Arc<Semaphore>) {
        let permits = Arc::new(Semaphore::new(0));
        (
            Self {
                permits: Arc::clone(&permits),
                calls: Arc::new(AtomicUsize::new(0)),
                failures_before_success: Arc::new(AtomicUsize::new(failures_before_success)),
                error_text,
                window: WINDOW,
            },
            permits,
        )
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Provider for GatedProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }

    fn name(&self) -> &str {
        "scripted"
    }

    fn model(&self) -> String {
        "scripted-model".to_string()
    }

    fn context_window(&self) -> usize {
        self.window
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            permits: Arc::clone(&self.permits),
            calls: Arc::clone(&self.calls),
            failures_before_success: Arc::clone(&self.failures_before_success),
            error_text: self.error_text,
            window: self.window,
        })
    }

    async fn complete_simple(&self, prompt: &str, _system: &str) -> Result<String> {
        Ok(artifact_json(&format!("summarized {} chars", prompt.len())))
    }

    async fn complete_simple_with_usage(
        &self,
        prompt: &str,
        system: &str,
    ) -> Result<(String, horon_agent_provider_core::SimpleCompletionUsage)> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let remaining = self.failures_before_success.load(Ordering::SeqCst);
        if remaining > 0 {
            self.failures_before_success.fetch_sub(1, Ordering::SeqCst);
            return Err(anyhow::anyhow!(self.error_text));
        }
        let permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| anyhow::anyhow!("gate closed"))?;
        permit.forget();
        Ok((
            self.complete_simple(prompt, system).await?,
            horon_agent_provider_core::SimpleCompletionUsage::default(),
        ))
    }
}

fn identity(generation: u64) -> JobIdentity {
    JobIdentity {
        generation,
        provider: "scripted".to_string(),
        model: "scripted-model".to_string(),
        settings_digest: "settings".to_string(),
    }
}

fn proactive_gate() -> StartGate {
    StartGate {
        trigger: JobTrigger::Proactive,
        background_enabled: true,
        persistent_runtime: true,
        usage: 0.8,
        effective_tokens: 150_000,
        threshold_tokens: 140_000,
        cooldown_ready: true,
        breaker_allows: true,
    }
}

fn started_manager() -> (CompactionManager, Vec<Message>, Arc<GatedProvider>, Arc<Semaphore>) {
    let mut manager = CompactionManager::new().with_budget(WINDOW);
    let messages = messages(40);
    for _ in &messages {
        manager.notify_message_added();
    }
    let (provider, permits) = GatedProvider::new(0, "");
    let provider = Arc::new(provider);
    manager
        .force_compact_with(&messages, provider.clone())
        .expect("manual compaction starts");
    (manager, messages, provider, permits)
}

async fn finish_task(manager: &mut CompactionManager) -> Result<CompactionResult, JobFailure> {
    let handle = manager.pending_task.take().expect("pending task");
    handle.await.expect("task joins")
}

async fn reinsert_finished(manager: &mut CompactionManager, joined: Result<CompactionResult, JobFailure>) {
    let handle = tokio::spawn(async move { joined });
    while !handle.is_finished() {
        tokio::task::yield_now().await;
    }
    manager.pending_task = Some(handle);
}

#[tokio::test(start_paused = true)]
async fn compaction_jobs_happy_tail_append_applies_exactly_once() {
    let _lock = crate::storage::lock_test_env();
    let (mut manager, messages, provider, permits) = started_manager();
    let job = manager.pending_job.clone().expect("job");
    let job_id = job.id;
    let cutoff = manager.pending_cutoff;
    assert!(cutoff > 0, "the job must plan a covered range");

    let mut extended = messages.clone();
    extended.push(text_message(Role::User, "appended while summarizing"));
    manager.notify_message_added();

    permits.add_permits(1);
    let result = finish_task(&mut manager).await.expect("job succeeds");
    assert_eq!(result.job_id, job_id);
    reinsert_finished(&mut manager, Ok(result)).await;

    manager.check_and_apply_compaction_with(&extended);

    let report = manager.take_job_report().expect("job report");
    assert_eq!(report.outcome, JobOutcomeKind::Applied);
    assert_eq!(report.covered, cutoff);
    assert_eq!(report.job_id, job_id);
    assert_eq!(manager.compacted_count(), cutoff);
    assert_eq!(
        manager.active_messages(&extended).len(),
        extended.len() - cutoff,
        "the message appended mid-flight must stay live"
    );
    assert!(
        matches!(
            manager.active_messages(&extended).last(),
            Some(last) if std::ptr::eq(last, extended.last().expect("appended message"))
        ),
        "the live tail ends with the mid-flight append"
    );
    assert!(
        !manager.is_compacting(),
        "the slot is released after a single application"
    );

    let before = manager.compacted_count();
    manager.check_and_apply_compaction_with(&extended);
    assert_eq!(
        manager.compacted_count(),
        before,
        "a completed job must not be charged or applied twice"
    );
    assert!(manager.take_job_report().is_none(), "no second disposition for one job");
    assert_eq!(provider.calls(), 1, "one provider call per job");

    let replay = CompactionResult {
        job_id,
        summary_text: "replayed summary".to_string(),
        openai_encrypted_content: None,
        native_window: Vec::new(),
        native_route: None,
        duration_ms: 1,
        summarized_messages: cutoff,
        covered_messages: cutoff,
        input_tokens: None,
        output_tokens: None,
        restoration: None,
        handoff: false,
    };
    manager.pending_job = Some(job);
    manager.pending_cutoff = cutoff;
    reinsert_finished(&mut manager, Ok(replay)).await;
    manager.check_and_apply_compaction_with(&extended);
    let report = manager.take_job_report().expect("replay disposition");
    assert_eq!(report.outcome, JobOutcomeKind::Discarded(DiscardReason::AlreadyApplied));
    assert_eq!(
        manager.compacted_count(),
        before,
        "a replayed result for an applied job changes nothing"
    );
    assert_ne!(
        manager.active_summary.as_ref().map(|summary| summary.text.as_str()),
        Some("replayed summary"),
        "the applied summary is not overwritten"
    );
}

#[tokio::test(start_paused = true)]
async fn compaction_jobs_second_start_is_refused_while_one_is_in_flight() {
    let _lock = crate::storage::lock_test_env();
    let (mut manager, messages, provider, _permits) = started_manager();
    tokio::task::yield_now().await;

    let second = manager.force_compact_with(&messages, provider.clone());
    assert_eq!(
        second,
        Err("Compaction already in progress".to_string()),
        "single-flight: a second job cannot claim the slot"
    );
    assert_eq!(provider.calls(), 1, "the refused start issues no provider call");
}

#[tokio::test(start_paused = true)]
async fn compaction_jobs_rewind_cancels_and_preserves_originals() {
    let _lock = crate::storage::lock_test_env();
    let (mut manager, messages, _provider, permits) = started_manager();
    let generation = manager.generation();

    manager.invalidate_generation(CancelReason::Rewind);
    assert_eq!(manager.generation(), generation + 1);
    assert!(!manager.is_compacting(), "the job is cancelled");
    let report = manager.take_job_report().expect("cancellation report");
    assert_eq!(report.outcome, JobOutcomeKind::Cancelled(CancelReason::Rewind));

    permits.add_permits(1);
    manager.check_and_apply_compaction_with(&messages);
    assert_eq!(manager.compacted_count(), 0);
    assert!(manager.active_summary.is_none());
    assert_eq!(
        manager.messages_for_api_with(&messages).len(),
        messages.len(),
        "every original message survives a cancelled job"
    );
}

#[tokio::test(start_paused = true)]
async fn compaction_jobs_model_change_cancels_and_preserves_originals() {
    let _lock = crate::storage::lock_test_env();
    let (mut manager, messages, _provider, permits) = started_manager();

    assert!(manager.observe_route_identity("scripted", "other-model"));
    assert!(!manager.is_compacting());
    let report = manager.take_job_report().expect("cancellation report");
    assert_eq!(report.outcome, JobOutcomeKind::Cancelled(CancelReason::ModelChange));

    permits.add_permits(1);
    manager.check_and_apply_compaction_with(&messages);
    assert_eq!(manager.compacted_count(), 0);
    assert_eq!(manager.messages_for_api_with(&messages).len(), messages.len());
}

#[tokio::test(start_paused = true)]
async fn compaction_jobs_generation_bump_discards_a_finished_result() {
    let _lock = crate::storage::lock_test_env();
    let (mut manager, messages, _provider, permits) = started_manager();

    permits.add_permits(1);
    let result = finish_task(&mut manager).await.expect("job succeeds");
    assert!(result.covered_messages > 0);

    manager.generation = manager.generation() + 1;
    reinsert_finished(&mut manager, Ok(result)).await;
    manager.check_and_apply_compaction_with(&messages);

    let report = manager.take_job_report().expect("discard report");
    assert_eq!(
        report.outcome,
        JobOutcomeKind::Discarded(DiscardReason::GenerationChanged)
    );
    assert_eq!(manager.compacted_count(), 0);
    assert!(manager.active_summary.is_none());
    assert_eq!(
        manager.messages_for_api_with(&messages).len(),
        messages.len(),
        "a stale result never edits the transcript"
    );
}

#[tokio::test(start_paused = true)]
async fn compaction_jobs_prefix_rewrite_discards_a_finished_result() {
    let _lock = crate::storage::lock_test_env();
    let (mut manager, messages, _provider, permits) = started_manager();

    permits.add_permits(1);
    let result = finish_task(&mut manager).await.expect("job succeeds");

    let mut rewritten = messages.clone();
    rewritten[0] = text_message(Role::User, "edited original");
    reinsert_finished(&mut manager, Ok(result)).await;
    manager.check_and_apply_compaction_with(&rewritten);

    let report = manager.take_job_report().expect("discard report");
    assert_eq!(report.outcome, JobOutcomeKind::Discarded(DiscardReason::PrefixChanged));
    assert_eq!(manager.compacted_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn compaction_jobs_stream_idle_deadline_times_out_inside_one_job() {
    let _lock = crate::storage::lock_test_env();
    let (mut manager, messages, provider, _permits) = started_manager();
    tokio::task::yield_now().await;

    let handle = manager.pending_task.take().expect("pending task");
    tokio::time::advance(jobs::STREAM_IDLE_DEADLINE + Duration::from_secs(1)).await;
    let joined = handle.await.expect("task joins");
    assert!(joined.is_err(), "an idle provider call must not succeed");

    reinsert_finished(&mut manager, joined).await;
    manager.check_and_apply_compaction_with(&messages);

    let report = manager.take_job_report().expect("timeout report");
    assert_eq!(report.outcome, JobOutcomeKind::TimedOut);
    assert_eq!(manager.compacted_count(), 0);
    assert_eq!(provider.calls(), 1, "a timeout does not re-charge the provider");
    assert!(!manager.is_compacting(), "the slot is free after a timeout");
}

#[tokio::test(start_paused = true)]
async fn compaction_jobs_transient_errors_retry_at_most_three_times() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(WINDOW);
    let messages = messages(40);
    for _ in &messages {
        manager.notify_message_added();
    }
    let (provider, permits) = GatedProvider::new(4, "503 service unavailable");
    let provider = Arc::new(provider);
    manager
        .force_compact_with(&messages, provider.clone())
        .expect("manual compaction starts");
    permits.add_permits(4);

    let result = finish_task(&mut manager).await;
    assert!(result.is_err(), "four transient failures exhaust the budget");
    assert_eq!(
        provider.calls(),
        1 + jobs::MAX_TRANSIENT_RETRIES as usize,
        "first attempt plus three retries, all inside one job"
    );
}

#[tokio::test(start_paused = true)]
async fn compaction_jobs_transient_error_then_success_applies() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(WINDOW);
    let messages = messages(40);
    for _ in &messages {
        manager.notify_message_added();
    }
    let (provider, permits) = GatedProvider::new(2, "connection reset");
    let provider = Arc::new(provider);
    manager
        .force_compact_with(&messages, provider.clone())
        .expect("manual compaction starts");
    permits.add_permits(4);

    let result = finish_task(&mut manager).await.expect("retry succeeds");
    assert_eq!(provider.calls(), 3, "two failures then one success");
    reinsert_finished(&mut manager, Ok(result)).await;
    manager.check_and_apply_compaction_with(&messages);
    assert_eq!(
        manager.take_job_report().expect("report").outcome,
        JobOutcomeKind::Applied
    );
}

#[test]
fn compaction_jobs_lead_is_clamped_around_the_threshold() {
    assert_eq!(lead_tokens(1_000), jobs::LEAD_MIN_TOKENS);
    assert_eq!(lead_tokens(140_000), 17_500);
    assert_eq!(lead_tokens(1_000_000), jobs::LEAD_MAX_TOKENS);
    let gate = proactive_gate();
    assert_eq!(gate.lead_start_tokens(), 122_500);
}

#[test]
fn compaction_jobs_cooldown_blocks_a_restart_for_thirty_seconds() {
    let start = Instant::now();
    let mut cooldown = Cooldown::default();
    assert!(cooldown.ready(start));
    cooldown.mark(start);
    assert!(!cooldown.ready(start + Duration::from_secs(29)));
    assert!(cooldown.ready(start + jobs::PROACTIVE_COOLDOWN));
}

#[test]
fn compaction_jobs_breaker_suspends_automatic_attempts_after_three_failures() {
    let start = Instant::now();
    let mut breaker = FailureBreaker::default();
    assert!(!breaker.record_failure(start));
    assert!(!breaker.record_failure(start));
    assert!(breaker.record_failure(start), "third failure trips the breaker");
    assert!(!breaker.allows(start + Duration::from_secs(30)));
    assert!(breaker.allows(start + jobs::BREAKER_SUSPENSION));
    breaker.record_success();
    assert_eq!(breaker.consecutive_failures(), 0);
}

#[test]
fn compaction_jobs_start_gate_enforces_background_switch_and_idle_rules() {
    let mut disabled = proactive_gate();
    disabled.background_enabled = false;
    assert_eq!(
        disabled.refusal(),
        Some(StartRefusal::BackgroundDisabled),
        "the config off switch disables proactive jobs"
    );

    let mut idle = proactive_gate();
    idle.trigger = JobTrigger::Idle;
    idle.persistent_runtime = false;
    assert_eq!(idle.refusal(), Some(StartRefusal::NotPersistentRuntime));

    let mut idle = proactive_gate();
    idle.trigger = JobTrigger::Idle;
    idle.usage = 0.4;
    assert_eq!(idle.refusal(), Some(StartRefusal::BelowIdleUsage));

    let mut idle = proactive_gate();
    idle.trigger = JobTrigger::Idle;
    idle.effective_tokens = 120_000;
    assert_eq!(idle.refusal(), Some(StartRefusal::BelowLead));

    let mut idle = proactive_gate();
    idle.trigger = JobTrigger::Idle;
    assert_eq!(idle.refusal(), None);

    let mut manual = proactive_gate();
    manual.trigger = JobTrigger::Manual;
    manual.background_enabled = false;
    manual.breaker_allows = false;
    manual.cooldown_ready = false;
    assert_eq!(
        manual.refusal(),
        None,
        "an explicit /compact is never blocked by policy"
    );
}

#[test]
fn compaction_jobs_breaker_blocks_automatic_but_not_manual_starts() {
    let now = Instant::now();
    let mut slot = SingleFlight::new();
    for _ in 0..jobs::BREAKER_THRESHOLD {
        slot.record_failure(now);
    }
    let gate = proactive_gate();
    let prefix = CapturedPrefix::capture(&messages(4), 0, 2, None);
    assert_eq!(
        slot.begin(&gate, identity(0), prefix.clone(), now),
        Err(StartRefusal::BreakerSuspended)
    );
    let mut manual = gate;
    manual.trigger = JobTrigger::Manual;
    assert!(slot.begin(&manual, identity(0), prefix, now).is_ok());
}

#[test]
fn compaction_jobs_revalidate_rejects_expired_and_reapplied_jobs() {
    let now = Instant::now();
    let mut slot = SingleFlight::new();
    let captured = messages(6);
    let gate = proactive_gate();
    let prefix = CapturedPrefix::capture(&captured, 0, 3, None);
    let job_id = slot.begin(&gate, identity(0), prefix, now).expect("job starts");
    let job = slot.current().cloned().expect("job");

    assert_eq!(
        slot.revalidate(&job, Some(&captured), 0, None, &identity(0), now),
        Ok(3)
    );
    assert_eq!(
        slot.revalidate(&job, Some(&captured), 0, None, &identity(1), now),
        Err(DiscardReason::GenerationChanged)
    );
    assert_eq!(
        slot.revalidate(&job, Some(&captured), 1, None, &identity(0), now),
        Err(DiscardReason::CompactedCountChanged)
    );
    assert_eq!(
        slot.revalidate(
            &job,
            Some(&captured),
            0,
            Some("different prior summary"),
            &identity(0),
            now
        ),
        Err(DiscardReason::PriorSummaryChanged)
    );
    assert_eq!(
        slot.revalidate(
            &job,
            Some(&captured),
            0,
            None,
            &identity(0),
            now + jobs::STREAM_IDLE_DEADLINE
        ),
        Err(DiscardReason::JobExpired)
    );

    slot.mark_applied(job_id);
    assert_eq!(
        slot.revalidate(&job, Some(&captured), 0, None, &identity(0), now),
        Err(DiscardReason::AlreadyApplied)
    );
}

#[tokio::test(start_paused = true)]
async fn compaction_jobs_legacy_no_history_poll_applies_once_from_the_captured_prefix() {
    let _lock = crate::storage::lock_test_env();
    let (mut manager, messages, provider, permits) = started_manager();
    let cutoff = manager.pending_cutoff;

    permits.add_permits(1);
    let result = finish_task(&mut manager).await.expect("job succeeds");
    reinsert_finished(&mut manager, Ok(result)).await;

    manager.check_and_apply_compaction_with(&[]);
    assert_eq!(
        manager.take_job_report().expect("report").outcome,
        JobOutcomeKind::Applied,
        "a caller without history still applies the finished result once"
    );
    assert_eq!(
        manager.compacted_count(),
        cutoff,
        "coverage comes from the captured prefix, not from a re-derived cutoff"
    );

    manager.check_and_apply_compaction_with(&messages);
    assert!(
        manager.take_job_report().is_none(),
        "a later poll with history reuses the applied summary instead of re-applying"
    );
    assert_eq!(provider.calls(), 1, "reuse never re-charges the provider");
}

#[tokio::test(start_paused = true)]
async fn compaction_jobs_overflow_shrinks_are_not_multiplied_by_transient_retries() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(WINDOW);
    let messages = messages(40);
    for _ in &messages {
        manager.notify_message_added();
    }
    let (provider, permits) = GatedProvider::new(99, "maximum context length exceeded");
    let provider = Arc::new(provider);
    manager
        .force_compact_with(&messages, provider.clone())
        .expect("manual compaction starts");
    permits.add_permits(16);

    let result = finish_task(&mut manager).await;
    assert!(result.is_err(), "an unshrinkable overflow fails the job");
    assert_eq!(
        provider.calls(),
        1 + jobs::MAX_SHRINK_ATTEMPTS as usize,
        "overflow is handled by shrinking inside one attempt, never by transient retries"
    );
}

#[test]
fn compaction_jobs_deadline_is_the_earlier_of_job_ceiling_and_idle_cap() {
    let start = Instant::now();
    let gate = proactive_gate();
    let mut slot = SingleFlight::new();
    let prefix = CapturedPrefix::capture(&messages(4), 0, 2, None);
    slot.begin(&gate, identity(0), prefix, start).expect("job starts");
    let mut job = slot.current().cloned().expect("job");

    assert_eq!(job.job_deadline(), start + jobs::JOB_DEADLINE);
    assert_eq!(job.effective_deadline(), start + jobs::STREAM_IDLE_DEADLINE);

    job.record_progress(start + Duration::from_secs(400));
    assert_eq!(
        job.effective_deadline(),
        start + Duration::from_secs(700),
        "progress moves the idle cap without extending the job ceiling"
    );
    job.record_progress(start + Duration::from_secs(800));
    assert_eq!(
        job.effective_deadline(),
        start + jobs::JOB_DEADLINE,
        "the 900s job ceiling always wins"
    );
}

#[test]
fn compaction_jobs_prefix_digest_tracks_edits_and_tail_appends() {
    let base = messages(4);
    let digest = prefix_digest(&base);

    let mut appended = base.clone();
    appended.push(text_message(Role::User, "tail"));
    assert_eq!(prefix_digest(&appended[..base.len()]), digest);

    let mut edited = base.clone();
    edited[1] = text_message(Role::Assistant, "edited");
    assert_ne!(prefix_digest(&edited), digest);

    let captured = CapturedPrefix::capture(&base, 0, 2, None);
    assert!(captured.tail_append_only(&appended));
    assert!(!captured.tail_append_only(&edited));
}
