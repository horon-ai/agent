//! The `handoff` strategy as one compaction job's boundary.
//!
//! Two things have to hold. A job planned under `handoff` writes a handoff
//! document and tags the boundary as one, so every later request frames it as the
//! successor's own earlier writing. A handoff that cannot be written falls back to
//! the structured summary for that same job, because a configured strategy must
//! not leave the turn uncompacted.

use super::*;
use crate::message::{Message, ToolDefinition};
use crate::provider::{EventStream, Provider};
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;

const WINDOW: usize = 200_000;
const MESSAGE_CHARS: usize = 4_000;

/// A document with the one heading the generator insists on.
const HANDOFF_DOCUMENT: &str = "## Goal\nfinish the handoff strategy branch\n\n## Next steps\nverify the projection\n";

/// The structured path's answer, shaped as the JSON artifact it expects.
fn artifact_json() -> String {
    serde_json::json!({
        "goal": "fall back to the structured summary",
        "done": ["the handoff attempt failed"],
        "incomplete": [],
        "cancelled": [],
        "files": [],
        "evidence": ["handoff strategy test fixture"],
        "decisions": [],
        "next_action": "continue"
    })
    .to_string()
}

/// One answer per `complete_simple_with_usage` call, in order.
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
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
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
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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

/// A manager whose next manual compaction runs under `strategy`.
fn manager_under(strategy: CompactionStrategy, messages: &[Message]) -> CompactionManager {
    let mut manager = CompactionManager::new().with_budget(WINDOW);
    manager.strategy = strategy;
    manager.configured_strategy = strategy;
    for _ in messages {
        manager.notify_message_added();
    }
    manager
}

async fn run_job(manager: &mut CompactionManager) -> Result<CompactionResult, JobFailure> {
    let handle = manager.pending_task.take().expect("pending task");
    handle.await.expect("task joins")
}

#[tokio::test(start_paused = true)]
async fn the_handoff_strategy_writes_a_handoff_boundary() {
    let _lock = crate::storage::lock_test_env();
    let messages = messages(40);
    let mut manager = manager_under(CompactionStrategy::Handoff, &messages);
    let provider = Arc::new(QueuedProvider::answering(vec![HANDOFF_DOCUMENT.to_string()]));

    manager
        .force_compact_with(&messages, provider.clone())
        .expect("the job starts");

    let result = run_job(&mut manager).await.expect("the handoff job succeeds");
    assert!(result.handoff, "the result is tagged as a handoff");
    assert_eq!(result.summary_text.trim(), HANDOFF_DOCUMENT.trim());
    assert!(
        result.openai_encrypted_content.is_none() && result.native_window.is_empty(),
        "a handoff replaces the provider's native state"
    );
    assert_eq!(provider.calls(), 1, "one provider call for the document");
}

#[tokio::test(start_paused = true)]
async fn a_handoff_boundary_is_projected_with_the_successor_framing() {
    let _lock = crate::storage::lock_test_env();
    let messages = messages(40);
    let mut manager = manager_under(CompactionStrategy::Handoff, &messages);
    let provider = Arc::new(QueuedProvider::answering(vec![HANDOFF_DOCUMENT.to_string()]));
    manager
        .force_compact_with(&messages, provider.clone())
        .expect("the job starts");

    let result = run_job(&mut manager).await.expect("the handoff job succeeds");
    let handle = tokio::spawn(async move { Ok(result) });
    while !handle.is_finished() {
        tokio::task::yield_now().await;
    }
    manager.pending_task = Some(handle);
    manager.check_and_apply_compaction_with(&messages);
    assert!(manager.active_handoff, "applying a handoff keeps the tag");

    let projected = manager.messages_for_api_with(&messages);
    let first = projected.first().expect("the boundary is first");
    let text = match &first.content[0] {
        ContentBlock::Text { text, .. } => text.clone(),
        other => panic!("expected a text boundary, got {other:?}"),
    };
    assert!(
        text.starts_with(horon_agent_compaction_core::handoff_frame::HANDOFF_FRAME),
        "the boundary is framed as the successor's own writing"
    );
    assert!(text.contains("finish the handoff strategy branch"));
    assert!(
        !text.contains("## Previous Conversation Summary"),
        "a handoff is not introduced as third-party notes"
    );
}

#[tokio::test(start_paused = true)]
async fn a_failed_handoff_falls_back_to_the_structured_summary() {
    let _lock = crate::storage::lock_test_env();
    let messages = messages(40);
    let mut manager = manager_under(CompactionStrategy::Handoff, &messages);
    // The first answer is not a handoff document (no Goal heading), so the
    // handoff attempt is refused; the structured path then gets the artifact.
    let provider = Arc::new(QueuedProvider::answering(vec![
        "I could not write a document this time.".to_string(),
        artifact_json(),
    ]));

    manager
        .force_compact_with(&messages, provider.clone())
        .expect("the job starts");

    let result = run_job(&mut manager)
        .await
        .expect("the fallback produces a boundary for the same job");
    assert!(!result.handoff, "the fallback is a structured summary");
    assert_eq!(provider.calls(), 2, "one refused handoff, one structured answer");
    assert!(
        result.covered_messages > 0,
        "the turn is still compacted by the fallback"
    );
}
