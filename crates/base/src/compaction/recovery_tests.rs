use super::recovery::{self, LOSSY_EMERGENCY_MARKER, RecoveryBlockReason, RecoveryOutcome};
use super::*;
use crate::provider::{EventStream, Provider};
use horon_agent_provider_core::{NativeCompactionResult, SimpleCompletionUsage};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const WINDOW: usize = 200_000;
const MESSAGE_CHARS: usize = 4_000;

enum Scripted {
    Text(String),
    Empty,
}

struct RecoveryProvider {
    responses: Mutex<VecDeque<Scripted>>,
    native: Option<NativeCompactionResult>,
}

impl RecoveryProvider {
    fn structured(responses: Vec<Scripted>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            native: None,
        }
    }

    fn with_native(responses: Vec<Scripted>, native: NativeCompactionResult) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            native: Some(native),
        }
    }
}

#[async_trait::async_trait]
impl Provider for RecoveryProvider {
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
        "recovery-scripted"
    }

    fn model(&self) -> String {
        "recovery-model".to_string()
    }

    fn context_window(&self) -> usize {
        WINDOW
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            responses: Mutex::new(VecDeque::new()),
            native: self.native.clone(),
        })
    }

    async fn native_compact(
        &self,
        _messages: &[Message],
        _existing_summary_text: Option<&str>,
        _existing_openai_encrypted_content: Option<&str>,
    ) -> Result<NativeCompactionResult> {
        match &self.native {
            Some(native) => Ok(native.clone()),
            None => Err(anyhow::anyhow!("no native compaction")),
        }
    }

    async fn complete_simple(&self, _prompt: &str, _system: &str) -> Result<String> {
        match self.responses.lock().expect("responses lock").pop_front() {
            Some(Scripted::Text(text)) => Ok(text),
            Some(Scripted::Empty) => Ok(String::new()),
            None => Err(anyhow::anyhow!("scripted provider exhausted")),
        }
    }

    async fn complete_simple_with_usage(&self, prompt: &str, system: &str) -> Result<(String, SimpleCompletionUsage)> {
        Ok((
            self.complete_simple(prompt, system).await?,
            SimpleCompletionUsage::default(),
        ))
    }
}

fn artifact(goal: &str) -> String {
    json!({
        "goal": goal,
        "done": ["older turns covered"],
        "incomplete": [],
        "cancelled": [],
        "files": [{"path": "crates/base/src/compaction/mod.rs", "note": "compaction manager"}],
        "evidence": ["compaction_recovery fixture"],
        "decisions": [],
        "next_action": "continue the task"
    })
    .to_string()
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

fn tool_call(id: &str, path: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: id.to_string(),
            name: "read".to_string(),
            input: json!({"path": path}),
            thought_signature: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }
}

fn tool_result(id: &str, body: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: id.to_string(),
            content: body.to_string(),
            is_error: None,
            verdict: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }
}

fn padded_history(count: usize) -> Vec<Message> {
    (0..count)
        .map(|index| {
            let role = if index % 2 == 0 { Role::User } else { Role::Assistant };
            text_message(role, &format!("turn {index} {}", "x".repeat(MESSAGE_CHARS)))
        })
        .collect()
}

fn seeded_manager(messages: &[Message]) -> CompactionManager {
    let mut manager = CompactionManager::new().with_budget(WINDOW);
    for _ in messages {
        manager.notify_message_added();
    }
    manager
}

async fn run_job_to_completion(manager: &mut CompactionManager, messages: &[Message]) -> Result<(), String> {
    let handle = manager.pending_task.take().expect("pending task");
    let joined = handle.await.expect("join");
    match joined {
        Ok(result) => {
            manager.apply_compaction_result(result, "manual".to_string(), messages);
            Ok(())
        }
        Err(failure) => {
            let detail = failure.to_string();
            let handle = tokio::spawn(async move { Err::<CompactionResult, JobFailure>(failure) });
            while !handle.is_finished() {
                tokio::task::yield_now().await;
            }
            manager.pending_task = Some(handle);
            manager.check_and_apply_compaction_with(messages);
            Err(detail)
        }
    }
}

fn summary_state(summary_text: &str, compacted_count: usize) -> crate::session::StoredCompactionState {
    crate::session::StoredCompactionState {
        summary_text: summary_text.to_string(),
        openai_encrypted_content: None,
        native_route: None,
        native_window: Vec::new(),
        covers_up_to_turn: compacted_count,
        original_turn_count: compacted_count,
        compacted_count,
        structured: None,
    }
}

#[tokio::test]
async fn compaction_recovery_empty_summary_applies_lossy_fallback_and_keeps_originals() {
    let _lock = crate::storage::lock_test_env();
    let mut messages = padded_history(40);
    let mut manager = seeded_manager(&messages);

    let first: Arc<dyn Provider> = Arc::new(RecoveryProvider::structured(vec![Scripted::Text(artifact(
        "first summary",
    ))]));
    manager
        .force_compact_with(&messages, first)
        .expect("first compaction starts");
    run_job_to_completion(&mut manager, &messages)
        .await
        .expect("first summary applies");
    assert!(!manager.is_lossy_summary());
    let first_boundary = manager.compacted_count();
    assert!(first_boundary > 0);

    messages.extend(padded_history(20));
    for _ in 0..20 {
        manager.notify_message_added();
    }
    let before = jobs::prefix_digest(&messages);

    let failing: Arc<dyn Provider> = Arc::new(RecoveryProvider::structured(vec![Scripted::Empty]));
    manager
        .force_compact_with(&messages, failing)
        .expect("second compaction starts");
    let failure = run_job_to_completion(&mut manager, &messages)
        .await
        .expect_err("an empty response is not a summary");
    assert!(failure.contains("empty"), "unexpected failure: {failure}");

    assert!(
        manager.is_lossy_summary(),
        "the deterministic fallback must be labelled lossy"
    );
    let summary = manager.active_summary.as_ref().expect("fallback summary");
    assert!(
        summary.text.contains(LOSSY_EMERGENCY_MARKER),
        "the lossy marker must be visible in the summary"
    );
    assert!(manager.compacted_count() > first_boundary);
    assert_eq!(
        manager.take_compaction_event().expect("lossy event").trigger,
        "lossy_emergency"
    );
    assert_eq!(
        jobs::prefix_digest(&messages),
        before,
        "originals keep their exact hashes through a lossy recovery"
    );
    let state = manager.persisted_state().expect("persisted state");
    assert!(
        state.structured.as_ref().is_some_and(|meta| meta.lossy_emergency),
        "the lossy flag is persisted with the summary"
    );
}

#[tokio::test]
async fn compaction_recovery_length_notice_native_fallback_is_not_success() {
    let _lock = crate::storage::lock_test_env();
    let messages = padded_history(40);
    let mut manager = seeded_manager(&messages);
    let before = jobs::prefix_digest(&messages);

    let notice = "This conversation exceeded the maximum context length. Retry with less history.";
    let provider: Arc<dyn Provider> = Arc::new(RecoveryProvider::with_native(
        vec![Scripted::Text(notice.to_string())],
        NativeCompactionResult {
            summary_text: Some(notice.to_string()),
            openai_encrypted_content: None,
            window: Vec::new(),
        },
    ));
    manager
        .force_compact_with(&messages, provider)
        .expect("compaction starts");
    let failure = run_job_to_completion(&mut manager, &messages)
        .await
        .expect_err("a length notice is not a summary");
    assert!(
        failure.contains("context length") || failure.contains("no JSON"),
        "unexpected failure: {failure}"
    );

    assert!(
        manager.active_summary.is_none(),
        "neither the native notice nor the notice-only text became a summary"
    );
    assert_eq!(manager.compacted_count(), 0);
    assert_eq!(jobs::prefix_digest(&messages), before);
    let outcome = manager
        .take_recovery_outcome()
        .expect("the failure produced a recovery decision");
    assert_eq!(
        outcome.blocked().map(|blocked| blocked.reason),
        Some(RecoveryBlockReason::NoPriorSummary)
    );
}

#[test]
fn compaction_recovery_notice_and_window_usability() {
    let notice = NativeCompactionResult {
        summary_text: Some("prompt is too long for the model".to_string()),
        openai_encrypted_content: None,
        window: Vec::new(),
    };
    assert!(!recovery::native_fallback_is_usable(&notice, false));
    assert!(recovery::native_fallback_is_usable(&notice, true));
    let empty = NativeCompactionResult {
        summary_text: None,
        openai_encrypted_content: None,
        window: Vec::new(),
    };
    assert!(!recovery::native_fallback_is_usable(&empty, false));
    assert!(!recovery::result_is_applicable("", None, false));
    assert!(!recovery::result_is_applicable(
        "maximum context length exceeded",
        None,
        false
    ));
    assert!(recovery::result_is_applicable("## Goal\nfinish", None, false));
}

#[test]
fn compaction_recovery_orphan_tool_pair_blocks_and_refuses_repeat() {
    let _lock = crate::storage::lock_test_env();
    let messages = vec![
        text_message(Role::User, "start the task"),
        text_message(Role::Assistant, "started"),
        text_message(Role::User, "continue"),
        tool_call("call-a", "src/lib.rs"),
        tool_result("call-b", "result without a matching call"),
    ];
    let mut manager = seeded_manager(&messages);
    manager.restore_persisted_state_with(&summary_state("## Goal\nkeep going", 0), &messages);
    let before = jobs::prefix_digest(&messages);

    let outcome = manager.recover_failed_summary_with(&messages, "summary provider failed");
    match &outcome {
        RecoveryOutcome::Blocked(blocked) => {
            assert_eq!(blocked.reason, RecoveryBlockReason::OrphanToolPair);
        }
        other => panic!("expected a blocked recovery, got {other:?}"),
    }
    assert!(manager.active_summary.is_none() || !manager.is_lossy_summary());
    assert_eq!(
        jobs::prefix_digest(&messages),
        before,
        "a blocked recovery never rewrites originals"
    );
    assert!(manager.recovery_refusal_active(&messages));
    assert!(
        !manager.should_compact_with(&messages),
        "automatic compaction stays refused for the same source"
    );

    let repeated = manager.recover_failed_summary_with(&messages, "summary provider failed");
    assert!(matches!(repeated, RecoveryOutcome::Refused(_)));
    assert_eq!(jobs::prefix_digest(&messages), before);

    let mut grown = messages.clone();
    grown.push(text_message(Role::Assistant, "a genuinely new turn"));
    assert!(
        !manager.recovery_refusal_active(&grown),
        "a new message changes the source digest and lifts the refusal"
    );
}

#[test]
fn compaction_recovery_over_budget_tail_blocks_and_refuses_repeat() {
    let _lock = crate::storage::lock_test_env();
    let huge = "y".repeat(WINDOW * 2);
    let messages = vec![
        text_message(Role::User, "begin"),
        text_message(Role::Assistant, "begun"),
        text_message(Role::User, &huge),
    ];
    let mut manager = seeded_manager(&messages);
    manager.restore_persisted_state_with(&summary_state("## Goal\nkeep going", 0), &messages);
    let before = jobs::prefix_digest(&messages);

    let outcome = manager.recover_failed_summary_with(&messages, "summary provider failed");
    match &outcome {
        RecoveryOutcome::Blocked(blocked) => {
            assert_eq!(blocked.reason, RecoveryBlockReason::TailExceedsBudget);
        }
        other => panic!("expected an over-budget refusal, got {other:?}"),
    }
    assert_eq!(jobs::prefix_digest(&messages), before);
    assert!(!manager.should_compact_with(&messages));
    assert!(matches!(
        manager.recover_failed_summary_with(&messages, "summary provider failed"),
        RecoveryOutcome::Refused(_)
    ));
}

#[test]
fn compaction_recovery_empty_user_instruction_blocks() {
    let _lock = crate::storage::lock_test_env();
    let messages = vec![
        text_message(Role::Assistant, "working"),
        tool_call("call-a", "src/lib.rs"),
        tool_result("call-a", "file body"),
    ];
    let mut manager = seeded_manager(&messages);
    manager.restore_persisted_state_with(&summary_state("## Goal\nkeep going", 0), &messages);
    let before = jobs::prefix_digest(&messages);

    let outcome = manager.recover_failed_summary_with(&messages, "summary provider failed");
    match &outcome {
        RecoveryOutcome::Blocked(blocked) => {
            assert_eq!(blocked.reason, RecoveryBlockReason::NoNewestUser);
        }
        other => panic!("expected an empty-user refusal, got {other:?}"),
    }
    assert_eq!(jobs::prefix_digest(&messages), before);
    assert!(!manager.should_compact_with(&messages));
}

#[test]
fn compaction_recovery_restoration_manifest_survives_restart() {
    let _lock = crate::storage::lock_test_env();
    let mut messages = vec![
        tool_call("call-1", "crates/base/src/compaction/mod.rs"),
        tool_result("call-1", "file body one"),
        tool_call("call-2", "crates/base/src/compaction/recovery.rs"),
        tool_result("call-2", "file body two"),
        text_message(Role::User, "resume the compaction work"),
    ];
    messages.push(text_message(Role::Assistant, "continuing"));
    let mut manager = seeded_manager(&messages);
    manager.restore_persisted_state_with(&summary_state("## Goal\nfinish task 14", 0), &messages);

    let outcome = manager.recover_failed_summary_with(&messages, "summary provider failed");
    let report = outcome.report().cloned().expect("the fallback fits");
    assert!(report.tail_tokens <= report.budget_tokens);
    let manifest = manager.active_restoration().cloned().expect("restoration captured");
    assert!(!manifest.entries.is_empty());
    assert!(manifest.entries.len() <= summary::MAX_RESTORATION_ENTRIES);
    let window_budget = (WINDOW as f64 * 0.15) as u64;
    assert!(manifest.total_bytes <= summary::MAX_RESTORATION_BYTES);
    assert!(manifest.total_bytes <= window_budget);
    for entry in &manifest.entries {
        assert!(
            entry.kind == "file" || entry.kind == "skill" || entry.kind == "task",
            "unexpected restoration kind {}",
            entry.kind
        );
        assert!(
            !entry.path.contains("file body"),
            "the manifest carries paths, never file bodies"
        );
    }
    assert!(
        manifest
            .entries
            .iter()
            .any(|entry| entry.kind == "task" && entry.path == "task:newest_user"),
        "the newest user instruction is persisted as a task reminder"
    );

    let state = manager.persisted_state().expect("persisted state");
    let encoded = serde_json::to_string(&state).expect("serialize");
    let decoded: crate::session::StoredCompactionState = serde_json::from_str(&encoded).expect("deserialize");

    let mut restarted = CompactionManager::new().with_budget(WINDOW);
    restarted.restore_persisted_state_with(&decoded, &messages);
    assert!(restarted.is_lossy_summary());
    assert_eq!(restarted.active_restoration(), manager.active_restoration());
    assert_eq!(
        restarted.compacted_count(),
        manager.compacted_count(),
        "the boundary survives the restart"
    );
    assert_eq!(
        restarted.messages_for_api_with(&messages).len(),
        manager.messages_for_api_with(&messages).len()
    );
}

#[test]
fn compaction_recovery_projection_reduction_never_touches_originals() {
    let _lock = crate::storage::lock_test_env();
    let big = "z".repeat(120_000);
    let messages = vec![
        text_message(Role::User, "read the large output"),
        tool_call("call-1", "build/output.log"),
        tool_result("call-1", &big),
        text_message(Role::User, "summarize it"),
    ];
    let mut manager = seeded_manager(&messages);
    manager.restore_persisted_state_with(&summary_state("## Goal\ncontinue", 2), &messages);
    manager.set_budget(1_000);
    let before = jobs::prefix_digest(&messages);

    let (projection, truncated) = manager.shrink_projection_with(&messages);
    assert!(truncated > 0, "the oversized tool result is shortened");
    assert_eq!(
        projection.len(),
        messages.len() - 2,
        "the reduction starts at the compacted boundary, not at the transcript head"
    );
    let projected_len: usize = projection.iter().map(message_char_count).sum();
    assert!(projected_len < big.len());

    let api = manager.messages_for_api_with(&messages);
    let api_tool_result = api
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|block| match block {
            ContentBlock::ToolResult { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .expect("the tool result stays in the projection");
    assert!(
        api_tool_result.len() < big.len(),
        "the provider projection is what gets shortened"
    );
    assert_eq!(
        jobs::prefix_digest(&messages),
        before,
        "only the provider projection is reduced"
    );
    assert_eq!(messages[2].content.len(), 1);
    match &messages[2].content[0] {
        ContentBlock::ToolResult { content, .. } => assert_eq!(content.len(), big.len()),
        other => panic!("expected the original tool result, got {other:?}"),
    }
}

#[test]
fn compaction_recovery_releases_covered_prefix_and_reads_past_by_offset() {
    let _lock = crate::storage::lock_test_env();
    let messages = padded_history(10);
    let mut manager = seeded_manager(&messages);
    manager.restore_persisted_state_with(&summary_state("## Goal\ncontinue", 4), &messages);

    assert_eq!(manager.provider_projection_offset(), 4);
    let mut projection = messages.clone();
    let released = manager.release_projection_prefix(&mut projection);
    assert_eq!(released, 4);
    assert_eq!(projection.len(), messages.len() - 4);
    assert_eq!(
        jobs::prefix_digest(&messages),
        jobs::prefix_digest(&padded_history(10)),
        "the caller's originals are untouched by the release"
    );
    let api = manager.messages_for_api_with(&messages);
    assert_eq!(api.len(), 1 + messages.len() - 4);
}
