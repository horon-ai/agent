//! `[compaction] method_order` as the fallback chain one job runs.
//!
//! Three things have to hold. A malformed order is corrected when a job starts
//! instead of at load, so a reload can never die on it. A method that cannot
//! produce a boundary hands the same job to the next method rather than leaving
//! the turn uncompacted. And the boundary that lands is the one the method that
//! actually ran produced, so a structured fallback is never presented as a
//! handoff.

use super::*;
use crate::message::{Message, ToolDefinition};
use crate::provider::{EventStream, Provider};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

const WINDOW: usize = 200_000;
const MESSAGE_CHARS: usize = 4_000;

const HANDOFF_DOCUMENT: &str = "## Goal\nfinish the method order branch\n\n## Next steps\nverify the chain\n";

fn artifact_json() -> String {
    serde_json::json!({
        "goal": "summarize through the structured path",
        "done": ["the chain reached the structured method"],
        "incomplete": [],
        "cancelled": [],
        "files": [],
        "evidence": ["method order test fixture"],
        "decisions": [],
        "next_action": "continue"
    })
    .to_string()
}

struct QueuedProvider {
    answers: Mutex<Vec<String>>,
    calls: AtomicUsize,
}

impl QueuedProvider {
    fn answering(answers: Vec<String>) -> Self {
        Self {
            answers: Mutex::new(answers),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Provider for QueuedProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
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
        WINDOW
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self::answering(self.answers.lock().expect("answers lock").clone()))
    }

    async fn complete_simple(&self, _prompt: &str, _system: &str) -> Result<String> {
        Ok(artifact_json())
    }

    async fn complete_simple_with_usage(
        &self,
        _prompt: &str,
        _system: &str,
    ) -> Result<(String, horon_agent_provider_core::SimpleCompletionUsage)> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let answer = {
            let mut answers = self.answers.lock().expect("answers lock");
            if answers.is_empty() {
                artifact_json()
            } else {
                answers.remove(0)
            }
        };
        Ok((answer, horon_agent_provider_core::SimpleCompletionUsage::default()))
    }
}

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

fn manager_under(strategy: CompactionStrategy, order: &[&str], messages: &[Message]) -> CompactionManager {
    let mut manager = CompactionManager::new().with_budget(WINDOW);
    manager.strategy = strategy;
    manager.configured_strategy = strategy;
    manager.compaction_config.method_order = order.iter().map(|name| name.to_string()).collect();
    for _ in messages {
        manager.notify_message_added();
    }
    manager
}

async fn run_job(manager: &mut CompactionManager) -> Result<CompactionResult, JobFailure> {
    let handle = manager.pending_task.take().expect("pending task");
    handle.await.expect("task joins")
}

fn resolved(order: &[&str], strategy: CompactionStrategy) -> MethodChain {
    let order: Vec<String> = order.iter().map(|name| name.to_string()).collect();
    MethodChain::resolve(&order, strategy)
}

// ── Resolution ─────────────────────────────────────────────────────────────

#[test]
fn an_absent_order_reproduces_the_single_strategy() {
    assert_eq!(
        resolved(&[], CompactionStrategy::Structured).methods(),
        &[Method::Structured]
    );
    assert_eq!(
        resolved(&[], CompactionStrategy::Handoff).methods(),
        &[Method::Handoff, Method::Structured],
        "a configured handoff keeps its one structured fallback"
    );
    assert!(resolved(&[], CompactionStrategy::OpenAiAuto).methods().is_empty());
}

#[test]
fn a_listed_order_is_taken_as_written() {
    let chain = resolved(&["handoff", "structured"], CompactionStrategy::Structured);
    assert_eq!(chain.methods(), &[Method::Handoff, Method::Structured]);
    assert!(chain.notes().is_empty(), "a valid order needs no correction");
    assert_eq!(chain.digest(), "handoff>structured");
}

#[test]
fn an_order_that_does_not_end_in_structured_is_discarded() {
    let chain = resolved(&["structured", "handoff"], CompactionStrategy::Handoff);
    assert_eq!(
        chain.methods(),
        &[Method::Handoff, Method::Structured],
        "the strategy's own chain replaces the order"
    );
    assert_eq!(chain.notes().len(), 1);
    assert!(chain.notes()[0].contains("must end in `structured`"));
}

#[test]
fn provider_native_compaction_in_an_order_discards_the_whole_order() {
    let chain = resolved(&["openai_auto", "structured"], CompactionStrategy::Structured);
    assert_eq!(
        chain.methods(),
        &[Method::Structured],
        "native compaction owns the transcript exclusively"
    );
    assert_eq!(chain.notes().len(), 1);
    assert!(chain.notes()[0].contains("openai_auto"));
}

#[test]
fn an_unknown_name_is_dropped_and_the_rest_kept() {
    let chain = resolved(&["handoff", "shrink", "structured"], CompactionStrategy::Structured);
    assert_eq!(chain.methods(), &[Method::Handoff, Method::Structured]);
    assert_eq!(chain.notes().len(), 1, "the unknown entry is reported once");
    assert!(chain.notes()[0].contains("shrink"));
}

#[test]
fn a_native_strategy_ignores_the_order() {
    let chain = resolved(&["handoff", "structured"], CompactionStrategy::OpenAiExplicit);
    assert!(
        chain.methods().is_empty(),
        "the provider owns compaction, so no method of ours may run"
    );
}

// ── Per-method failure budget ──────────────────────────────────────────────

#[test]
fn two_consecutive_failures_retire_one_method() {
    let mut failures = MethodFailures::default();
    assert!(!failures.exhausted(Method::Handoff));
    failures.record(Method::Handoff);
    assert!(
        !failures.exhausted(Method::Handoff),
        "one failure is a strike, not a retirement"
    );
    failures.record(Method::Handoff);
    assert!(failures.exhausted(Method::Handoff));
    assert!(!failures.exhausted(Method::Structured), "the counter is per method");

    let chain = resolved(&["handoff", "structured"], CompactionStrategy::Structured);
    assert_eq!(
        chain.without_exhausted(&failures).methods(),
        &[Method::Structured],
        "the retired method is skipped for the next job"
    );

    failures.reset();
    assert_eq!(
        chain.without_exhausted(&failures).methods(),
        chain.methods(),
        "a boundary resets every strike"
    );
}

#[test]
fn an_exhausted_order_keeps_its_last_method() {
    let mut failures = MethodFailures::default();
    failures.record(Method::Handoff);
    failures.record(Method::Handoff);
    failures.record(Method::Structured);
    failures.record(Method::Structured);

    let chain = resolved(&["handoff", "structured"], CompactionStrategy::Structured);
    assert_eq!(
        chain.without_exhausted(&failures).methods(),
        chain.methods(),
        "a job with no method could never compact; the breaker owns that decision"
    );
}

// ── Execution ──────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn the_configured_order_runs_even_when_strategy_names_one_method() {
    let _lock = crate::storage::lock_test_env();
    let messages = messages(40);
    let mut manager = manager_under(CompactionStrategy::Structured, &["handoff", "structured"], &messages);
    // The first answer has no Goal heading, so the handoff method is refused and
    // the same job must continue with the structured summarizer.
    let provider = Arc::new(QueuedProvider::answering(vec![
        "I could not write a document this time.".to_string(),
        artifact_json(),
    ]));

    manager
        .force_compact_with(&messages, provider.clone())
        .expect("the job starts");

    let result = run_job(&mut manager)
        .await
        .expect("the chain produces a boundary for the same job");
    assert!(!result.handoff, "the boundary is the structured method's product");
    assert_eq!(provider.calls(), 2, "one refused handoff, one structured answer");
    assert!(result.covered_messages > 0, "the turn is still compacted");
}

#[tokio::test(start_paused = true)]
async fn a_structured_fallback_is_not_projected_as_a_handoff() {
    let _lock = crate::storage::lock_test_env();
    let messages = messages(40);
    let mut manager = manager_under(CompactionStrategy::Handoff, &[], &messages);
    let provider = Arc::new(QueuedProvider::answering(vec![
        "I could not write a document this time.".to_string(),
        artifact_json(),
    ]));
    manager
        .force_compact_with(&messages, provider.clone())
        .expect("the job starts");

    let result = run_job(&mut manager).await.expect("the fallback succeeds");
    let handle = tokio::spawn(async move { Ok(result) });
    while !handle.is_finished() {
        tokio::task::yield_now().await;
    }
    manager.pending_task = Some(handle);
    manager.check_and_apply_compaction_with(&messages);

    assert!(
        !manager.active_handoff,
        "the applied boundary carries the tag of the method that wrote it"
    );
    let projected = manager.messages_for_api_with(&messages);
    let text = match &projected.first().expect("the boundary is first").content[0] {
        ContentBlock::Text { text, .. } => text.clone(),
        other => panic!("expected a text boundary, got {other:?}"),
    };
    assert!(
        !text.starts_with(horon_agent_compaction_core::handoff_frame::HANDOFF_FRAME),
        "a structured summary is never framed as the successor's own writing"
    );
}

#[tokio::test(start_paused = true)]
async fn a_retired_method_is_skipped_by_the_next_job() {
    let _lock = crate::storage::lock_test_env();
    let messages = messages(40);
    let mut manager = manager_under(CompactionStrategy::Structured, &["handoff", "structured"], &messages);
    manager.method_failures.record(Method::Handoff);
    manager.method_failures.record(Method::Handoff);

    let provider = Arc::new(QueuedProvider::answering(vec![artifact_json()]));
    manager
        .force_compact_with(&messages, provider.clone())
        .expect("the job starts");

    let result = run_job(&mut manager).await.expect("the structured method succeeds");
    assert!(!result.handoff);
    assert_eq!(provider.calls(), 1, "the retired handoff method is not attempted again");
}

#[tokio::test(start_paused = true)]
async fn a_successful_chain_resets_the_strikes() {
    let _lock = crate::storage::lock_test_env();
    let messages = messages(40);
    let mut manager = manager_under(CompactionStrategy::Handoff, &[], &messages);
    manager.method_failures.record(Method::Handoff);

    let provider = Arc::new(QueuedProvider::answering(vec![HANDOFF_DOCUMENT.to_string()]));
    manager
        .force_compact_with(&messages, provider.clone())
        .expect("the job starts");
    let result = run_job(&mut manager).await.expect("the handoff succeeds");
    let handle = tokio::spawn(async move { Ok(result) });
    while !handle.is_finished() {
        tokio::task::yield_now().await;
    }
    manager.pending_task = Some(handle);
    manager.check_and_apply_compaction_with(&messages);

    assert!(
        !manager.method_failures.exhausted(Method::Handoff),
        "a boundary proves the chain works again"
    );
}
