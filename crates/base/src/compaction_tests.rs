use super::*;
use crate::provider::{EventStream, Provider};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct MockSummaryProvider;

#[async_trait::async_trait]
impl Provider for MockSummaryProvider {
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
        "mock-summary"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(MockSummaryProvider)
    }

    async fn complete_simple(&self, prompt: &str, _system: &str) -> Result<String> {
        Ok(serde_json::json!({
            "goal": "continue the work",
            "done": [format!("summarized {} chars", prompt.len())],
            "next_action": "continue"
        })
        .to_string())
    }

    async fn complete_simple_with_usage(
        &self,
        prompt: &str,
        system: &str,
    ) -> Result<(String, horon_agent_provider_core::SimpleCompletionUsage)> {
        Ok((
            self.complete_simple(prompt, system).await?,
            horon_agent_provider_core::SimpleCompletionUsage::default(),
        ))
    }
}

/// Window used by the compaction-behaviour tests that used to pass a tiny
/// budget.
///
/// D6 rejects any window that cannot hold the reserve plus the safety headroom
/// (`ContextBudgetTooSmall`), so a 1_000-token budget no longer describes a
/// valid configuration. These tests use a realistic 200k window instead: the
/// trigger is 140_000 tokens and the verbatim recent tail is 20_000 tokens.
const POLICY_TEST_WINDOW: usize = 200_000;
/// 4_000 chars is ~1_001 tokens, so 19 messages fill the 20_000-token recent tail.
const POLICY_MESSAGE_CHARS: usize = 4_000;

fn make_text_message(role: Role, text: &str) -> Message {
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

#[test]
fn test_new_manager() {
    let manager = CompactionManager::new();
    assert_eq!(manager.compacted_count, 0);
    assert!(manager.active_summary.is_none());
    assert!(!manager.is_compacting());
}

#[test]
fn test_notify_message_added() {
    let mut manager = CompactionManager::new();
    manager.notify_message_added();
    manager.notify_message_added();
    assert_eq!(manager.total_turns, 2);
}

#[test]
fn test_restored_messages_do_not_trigger_compaction_immediately() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(POLICY_TEST_WINDOW);
    let messages = policy_messages(40, POLICY_MESSAGE_CHARS);
    manager.seed_restored_messages(messages.len());
    // Even a report past the trigger must stay quiet until a new message lands.
    manager.update_observed_input_tokens(190_000);

    assert!(
        !manager.should_compact_with(&messages),
        "restored history should not compact until a new message is added"
    );
}

#[test]
fn test_new_message_after_restore_reenables_compaction() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(POLICY_TEST_WINDOW);
    let mut messages = policy_messages(40, POLICY_MESSAGE_CHARS);
    manager.seed_restored_messages(messages.len());
    manager.update_observed_input_tokens(140_000);
    assert!(!manager.should_compact_with(&messages));

    messages.push(make_text_message(Role::User, "new turn after restore"));
    manager.notify_message_added();

    assert!(
        manager.should_compact_with(&messages),
        "compaction should resume once a genuinely new message is added"
    );
}

#[test]
fn test_token_estimate() {
    let _lock = crate::storage::lock_test_env();
    let manager = CompactionManager::new();
    // 100 chars = ~25 tokens (plus 18k overhead for full budget)
    let messages = vec![make_text_message(Role::User, &"x".repeat(100))];
    let estimate = manager.token_estimate_with(&messages);
    // With DEFAULT_TOKEN_BUDGET and 18k overhead: 25 + 18000 = 18025
    assert!((18_000..19_000).contains(&estimate));
}

#[test]
fn test_should_compact() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(POLICY_TEST_WINDOW);

    // No observed tokens here: the transcript alone must cross the trigger, as
    // it did when this test ran against a tiny budget. ASCII is priced at about
    // 4.87 characters per token, so 50 x 14_000 chars is roughly 143_800 message
    // tokens, which clears the 140_000 trigger together with the 18_000
    // system/tool overhead.
    let messages = policy_messages(50, 14_000);
    for _ in &messages {
        manager.notify_message_added();
    }

    assert!(manager.should_compact_with(&messages));
}

#[test]
fn test_context_usage_prefers_observed_tokens() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let messages = vec![make_text_message(Role::User, "short message")];
    manager.notify_message_added();
    manager.update_observed_input_tokens(900);

    assert!(manager.context_usage_with(&messages) >= 0.90);
    assert!(manager.effective_token_count_with(&messages) >= 900);
}

#[test]
fn test_should_compact_uses_observed_tokens() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(POLICY_TEST_WINDOW);

    let messages = policy_messages(40, POLICY_MESSAGE_CHARS);
    for _ in &messages {
        manager.notify_message_added();
    }
    // The local estimate (40_000 message tokens plus 18_000 overhead) is far
    // below the 140_000 trigger; the provider's report crosses it.
    manager.update_observed_input_tokens(140_000);

    assert!(manager.should_compact_with(&messages));
}

#[test]
fn test_messages_for_api_no_summary() {
    let mut manager = CompactionManager::new();
    let messages = vec![
        make_text_message(Role::User, "Hello"),
        make_text_message(Role::Assistant, "Hi!"),
    ];
    manager.notify_message_added();
    manager.notify_message_added();

    let msgs = manager.messages_for_api_with(&messages);
    assert_eq!(msgs.len(), 2);
}

#[tokio::test]
async fn test_force_compact_applies_summary() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(POLICY_TEST_WINDOW);
    let messages = policy_messages(40, POLICY_MESSAGE_CHARS);
    for _ in &messages {
        manager.notify_message_added();
    }

    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    manager
        .force_compact_with(&messages, provider)
        .expect("manual compaction should start");

    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        manager.check_and_apply_compaction();
        if manager.stats().has_summary {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(
        manager.stats().has_summary,
        "summary should be applied after compaction task completes"
    );

    // After compaction, compacted_count should be > 0
    assert!(manager.compacted_count > 0);

    let msgs = manager.messages_for_api_with(&messages);
    assert!(msgs.len() < messages.len());
    let first = msgs.first().expect("summary message missing");
    assert_eq!(first.role, Role::User);
    match &first.content[0] {
        ContentBlock::Text { text, .. } => {
            assert!(text.contains("Previous Conversation Summary"));
        }
        _ => panic!("expected text summary block"),
    }
}

// ── ensure_context_fits tests ──────────────────────────────

#[tokio::test]
async fn test_guard_below_80_does_nothing() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(POLICY_TEST_WINDOW);
    let messages = policy_messages(40, POLICY_MESSAGE_CHARS);
    for _ in &messages {
        manager.notify_message_added();
    }
    // Half the window: well below the 70% trigger for a 200k window.
    manager.update_observed_input_tokens(100_000);

    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    let action = manager.ensure_context_fits(&messages, provider);
    assert_eq!(action, CompactionAction::None, "should do nothing below 80%");
    assert!(
        !manager.is_compacting(),
        "should NOT start background compaction below 80%"
    );
    assert_eq!(manager.compacted_count, 0);
}

#[tokio::test]
async fn test_guard_between_80_and_95_starts_background_only() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(POLICY_TEST_WINDOW);
    let messages = policy_messages(40, POLICY_MESSAGE_CHARS);
    for _ in &messages {
        manager.notify_message_added();
    }
    // 85% usage — above the 70% trigger but below the 95% critical threshold.
    manager.update_observed_input_tokens(170_000);

    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    let action = manager.ensure_context_fits(&messages, provider);
    assert_eq!(
        action,
        CompactionAction::BackgroundStarted {
            trigger: "reactive".to_string()
        },
        "should start background compaction at 85%"
    );
    assert!(manager.is_compacting(), "SHOULD start background compaction at 85%");
    assert_eq!(
        manager.compacted_count, 0,
        "compacted_count should stay 0 (no hard compact)"
    );
}

/// Regression: a hard compact that runs while a background (reactive)
/// compaction is in flight must abort the background task and discard its
/// stale `pending_cutoff`. Otherwise, when the background task completes,
/// `check_and_apply_compaction_with` adds the stale cutoff on top of the
/// already-advanced `compacted_count`, double-compacting and wiping out all
/// live messages (observed as "kept 0 recent messages").
#[tokio::test]
async fn test_hard_compact_aborts_inflight_background_compaction() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(POLICY_TEST_WINDOW);
    let messages = policy_messages(30, POLICY_MESSAGE_CHARS);
    for _ in &messages {
        manager.notify_message_added();
    }

    // Start a background reactive compaction (85% usage, below critical).
    manager.update_observed_input_tokens(170_000);
    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    manager.maybe_start_compaction_with(&messages, provider);
    assert!(manager.is_compacting(), "background compaction should be in flight");
    let inflight_cutoff = manager.pending_cutoff;
    assert!(inflight_cutoff > 0, "background task should have a cutoff");

    // Now pressure spikes to critical and we hard-compact synchronously while
    // the background task is still pending.
    let dropped = manager
        .hard_compact_with(&messages)
        .expect("hard compact should succeed");
    assert!(dropped > 0);

    // The in-flight background compaction must have been aborted/discarded.
    assert!(
        !manager.is_compacting(),
        "hard compact must abort the in-flight background compaction"
    );
    assert_eq!(manager.pending_cutoff, 0, "stale pending_cutoff must be reset");

    let compacted_after_hard = manager.compacted_count;

    // Simulate the (now-aborted) background task completion path. With the fix
    // there is no pending task, so this is a no-op and must NOT advance
    // compacted_count again.
    manager.check_and_apply_compaction_with(&messages);
    assert_eq!(
        manager.compacted_count, compacted_after_hard,
        "completing after abort must not double-advance compacted_count"
    );

    // Live messages must survive: active_messages_count stays positive.
    assert!(
        manager.active_messages_count() > 0,
        "must keep recent messages live, not wipe everything to 0"
    );
    let active = manager.active_messages(&messages);
    assert!(
        !active.is_empty(),
        "active message slice must not be empty after hard compact"
    );
}

/// Defense-in-depth: if `compacted_count` advances while a background
/// compaction is in flight (so its `pending_cutoff` becomes stale), applying
/// the completed result must NOT over-advance `compacted_count` and wipe the
/// live tail. The stale result should be discarded instead.
#[tokio::test]
async fn test_stale_background_result_discarded_when_context_shrinks() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(POLICY_TEST_WINDOW);
    let messages = policy_messages(30, POLICY_MESSAGE_CHARS);
    for _ in &messages {
        manager.notify_message_added();
    }

    manager.update_observed_input_tokens(170_000);
    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    manager.maybe_start_compaction_with(&messages, provider);
    assert!(manager.is_compacting());
    let pending = manager.pending_cutoff;
    assert!(pending > 0);

    // Simulate an interleaving mutation that advances compacted_count out from
    // under the in-flight task (e.g. a hard compact via a different path),
    // leaving only a small active tail.
    manager.compacted_count = messages.len() - 3;

    // Drain the background task to completion, then apply.
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        manager.check_and_apply_compaction_with(&messages);
        if !manager.is_compacting() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!manager.is_compacting(), "task should have been drained");

    // The stale result must have been discarded: compacted_count stays where
    // the interleaving mutation left it, and the live tail survives.
    assert_eq!(
        manager.compacted_count,
        messages.len() - 3,
        "stale pending_cutoff must not advance compacted_count further"
    );
    assert!(
        manager.active_messages(&messages).len() >= 3,
        "live tail must survive a discarded stale compaction"
    );
    assert_eq!(manager.pending_cutoff, 0, "pending_cutoff must be reset");
}

#[tokio::test]
async fn test_guard_at_95_triggers_hard_compact() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(POLICY_TEST_WINDOW);
    let messages = policy_messages(30, POLICY_MESSAGE_CHARS);
    for _ in &messages {
        manager.notify_message_added();
    }
    // 96% usage — above critical threshold
    manager.update_observed_input_tokens(192_000);

    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    let action = manager.ensure_context_fits(&messages, provider);
    assert!(
        matches!(action, CompactionAction::HardCompacted(_)),
        "SHOULD hard-compact at 96%"
    );
    assert!(
        manager.compacted_count > 0,
        "compacted_count should increase after hard compact"
    );
    assert!(manager.active_summary.is_some(), "should have an emergency summary");
}

#[tokio::test]
async fn test_guard_at_100_percent_drops_messages() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(POLICY_TEST_WINDOW);
    let messages = policy_messages(30, POLICY_MESSAGE_CHARS);
    for _ in &messages {
        manager.notify_message_added();
    }
    // Over 100% — simulates the exact bug scenario
    manager.update_observed_input_tokens(210_000);

    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    let action = manager.ensure_context_fits(&messages, provider);
    assert!(
        matches!(action, CompactionAction::HardCompacted(_)),
        "MUST hard-compact when over 100%"
    );

    let api_messages = manager.messages_for_api_with(&messages);
    assert!(
        api_messages.len() < messages.len(),
        "API messages should be fewer after hard compact"
    );
    // First message should be the emergency summary
    match &api_messages[0].content[0] {
        ContentBlock::Text { text, .. } => {
            assert!(text.contains("Previous Conversation Summary"));
            assert!(text.contains("Emergency compaction"));
        }
        _ => panic!("expected text summary block"),
    }
}

// ── hard_compact_with edge cases ────────────────────────────────

#[test]
fn test_hard_compact_too_few_messages() {
    let mut manager = CompactionManager::new().with_budget(100);
    let messages = vec![
        make_text_message(Role::User, "hello"),
        make_text_message(Role::Assistant, "hi"),
    ];
    manager.notify_message_added();
    manager.notify_message_added();

    let result = manager.hard_compact_with(&messages);
    assert!(result.is_err(), "should fail with only 2 messages (MIN_TURNS_TO_KEEP)");
}

#[test]
fn test_hard_compact_preserves_recent_turns() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(POLICY_TEST_WINDOW);
    let messages = policy_messages(30, POLICY_MESSAGE_CHARS);
    for _ in &messages {
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(190_000);

    let dropped = manager.hard_compact_with(&messages).expect("should compact");
    // The 20_000-token recent tail holds the 24 newest messages (822 tokens
    // each: 821 measured for the ASCII run plus 1 of framing), so the oldest 6
    // are dropped. Retention is measured in tokens, not messages.
    assert_eq!(dropped, 6, "should drop everything outside the recent tail");

    let api_messages = manager.messages_for_api_with(&messages);
    assert_eq!(api_messages.len(), 25, "summary plus the 24 verbatim recent messages");
    assert!(
        manager.active_messages(&messages).len() > MIN_TURNS_TO_KEEP,
        "must keep more than the minimum turns"
    );
    // The surviving tail is the newest 24 messages, byte for byte: dropping is
    // a suffix removal, never a re-selection.
    let kept_texts: Vec<&str> = manager
        .active_messages(&messages)
        .iter()
        .map(|message| match &message.content[0] {
            ContentBlock::Text { text, .. } => text.as_str(),
            _ => panic!("expected text message"),
        })
        .collect();
    let expected: Vec<&str> = messages[dropped..]
        .iter()
        .map(|message| match &message.content[0] {
            ContentBlock::Text { text, .. } => text.as_str(),
            _ => panic!("expected text message"),
        })
        .collect();
    assert_eq!(kept_texts, expected);
}

// ── safe_compaction_cutoff: tool call/result pair integrity ─────────

#[test]
fn test_safe_cutoff_preserves_tool_pairs() {
    // Messages: [user, assistant(tool_use), user(tool_result), assistant, user]
    // If cutoff tries to split between tool_use and tool_result, it should back up
    let messages = vec![
        make_text_message(Role::User, "do something"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "tool_1".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "ls"}),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::tool_result("tool_1", "file1.txt\nfile2.txt", Some(false))],
            timestamp: None,
            tool_duration_ms: None,
        },
        make_text_message(Role::Assistant, "I see the files"),
        make_text_message(Role::User, "thanks"),
    ];

    // Try to cut between tool_use (index 1) and tool_result (index 2)
    let cutoff = safe_compaction_cutoff(&messages, 2);
    // Should move back to include the tool_use at index 1
    assert!(
        cutoff <= 1,
        "cutoff should back up to include tool_use (got {})",
        cutoff
    );
}

#[test]
fn test_safe_cutoff_no_tool_pairs() {
    let messages = vec![
        make_text_message(Role::User, "hello"),
        make_text_message(Role::Assistant, "hi"),
        make_text_message(Role::User, "how are you"),
        make_text_message(Role::Assistant, "fine"),
    ];

    let cutoff = safe_compaction_cutoff(&messages, 2);
    assert_eq!(cutoff, 2, "no tool pairs, cutoff should stay unchanged");
}

#[test]
fn test_safe_cutoff_handles_chained_tool_dependencies_without_rescan() {
    let messages = vec![
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "tool_a".to_string(),
                name: "read".to_string(),
                input: serde_json::json!({"file": "a.txt"}),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        make_text_message(Role::User, "intermediate"),
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::tool_result("tool_a", "a contents", Some(false)),
                ContentBlock::ToolUse {
                    id: "tool_b".to_string(),
                    name: "grep".to_string(),
                    input: serde_json::json!({"pattern": "foo"}),
                    thought_signature: None,
                },
            ],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::tool_result("tool_b", "foo", Some(false))],
            timestamp: None,
            tool_duration_ms: None,
        },
        make_text_message(Role::Assistant, "done"),
    ];

    let cutoff = safe_compaction_cutoff(&messages, 3);
    assert_eq!(
        cutoff, 0,
        "cutoff should walk back through nested tool dependencies until the kept suffix is self-contained"
    );
}

// ── emergency_truncate_with ─────────────────────────────────────

#[test]
fn test_emergency_truncate_large_tool_results() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let big_result = "x".repeat(10_000); // Way over EMERGENCY_TOOL_RESULT_MAX_CHARS (4000)
    let mut messages = vec![
        make_text_message(Role::User, "run something"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "tool_1".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "cat bigfile"}),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::tool_result("tool_1", &big_result, Some(false))],
            timestamp: None,
            tool_duration_ms: None,
        },
        make_text_message(Role::Assistant, "that's a big file"),
    ];
    for _ in &messages {
        manager.notify_message_added();
    }

    let truncated = manager.emergency_truncate_with(&mut messages);
    assert_eq!(truncated, 1, "should truncate exactly 1 tool result");

    // Check the truncated content
    if let ContentBlock::ToolResult { content, .. } = &messages[2].content[0] {
        assert!(content.len() < big_result.len(), "content should be shorter");
        assert!(
            content.contains("truncated for context recovery"),
            "should have truncation marker"
        );
    } else {
        panic!("expected tool result");
    }
}

#[test]
fn test_emergency_truncate_skips_small_results() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::tool_result("tool_1", "small output", Some(false))],
        timestamp: None,
        tool_duration_ms: None,
    }];
    manager.notify_message_added();

    let truncated = manager.emergency_truncate_with(&mut messages);
    assert_eq!(truncated, 0, "should not truncate small results");
}

// ── Double compaction ───────────────────────────────────────────

#[test]
fn test_hard_compact_twice() {
    let mut manager = CompactionManager::new().with_budget(500);
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(Role::User, &format!("turn {} {}", i, "z".repeat(40))));
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(480);

    // First hard compact
    let dropped1 = manager.hard_compact_with(&messages).expect("first compact should work");
    assert!(dropped1 > 0);
    let count_after_first = manager.compacted_count;

    // Simulate more messages arriving after first compact
    for i in 30..45 {
        messages.push(make_text_message(Role::User, &format!("turn {} {}", i, "z".repeat(40))));
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(490);

    // Second hard compact
    let dropped2 = manager
        .hard_compact_with(&messages)
        .expect("second compact should work");
    assert!(dropped2 > 0);
    assert!(
        manager.compacted_count > count_after_first,
        "compacted_count should increase"
    );

    // Summary should mention both compactions
    let api_messages = manager.messages_for_api_with(&messages);
    assert!(api_messages.len() < messages.len());
    match &api_messages[0].content[0] {
        ContentBlock::Text { text, .. } => {
            assert!(text.contains("Emergency compaction"));
        }
        _ => panic!("expected summary"),
    }
}

#[test]
fn test_hard_compact_clamps_pathological_compacted_count() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("turn {} content {}", i, "x".repeat(200)),
        ));
        manager.notify_message_added();
    }

    // Reproduce the #175 bad state: bookkeeping says more messages were
    // compacted than exist in the current message vector. Before the fix,
    // active_messages() returned the full transcript in this state, so each
    // hard compaction appended another emergency marker and increased
    // compacted_count even further past messages.len().
    manager.compacted_count = 100;
    manager.active_summary = Some(Summary {
        native_window: Vec::new(),
        native_route: None,
        text: "# Existing summary".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 100,
        original_turn_count: 100,
    });
    manager.active_chars.invalidate();

    for _ in 0..3 {
        let _ = manager.hard_compact_with(&messages);
    }

    assert_eq!(
        manager.compacted_count,
        messages.len(),
        "hard compaction must clamp compacted_count to the available messages"
    );
    let summary_markers = manager
        .active_summary
        .as_ref()
        .map(|summary| summary.text.matches("[Emergency compaction]").count())
        .unwrap_or(0);
    assert_eq!(
        summary_markers, 0,
        "pathological state should not append repeated emergency markers"
    );

    let api_messages = manager.messages_for_api_with(&messages);
    assert_eq!(
        api_messages.len(),
        1,
        "all current messages should remain covered by the existing summary until new turns arrive"
    );
}

#[test]
fn test_hard_compact_reduces_api_payload_and_reports_saved_tokens() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..40 {
        messages.push(make_text_message(
            Role::User,
            &format!("turn {} {}", i, "payload ".repeat(80)),
        ));
        manager.notify_message_added();
    }

    let pre_api_messages = manager.messages_for_api_with(&messages);
    let pre_chars: usize = pre_api_messages.iter().map(message_char_count).sum();
    let pre_tokens = manager.effective_token_count_with(&messages);

    manager
        .hard_compact_with(&messages)
        .expect("hard compaction should recover oversized context");

    let post_api_messages = manager.messages_for_api_with(&messages);
    let post_chars: usize = post_api_messages.iter().map(message_char_count).sum();
    let post_tokens = manager.effective_token_count_with(&messages);
    let event = manager
        .take_compaction_event()
        .expect("hard compaction should publish an event");

    assert!(
        post_api_messages.len() < pre_api_messages.len(),
        "hard compaction should send fewer messages"
    );
    assert!(
        post_chars < pre_chars,
        "hard compaction should reduce outgoing payload chars: pre={pre_chars}, post={post_chars}"
    );
    assert!(
        post_tokens <= pre_tokens,
        "hard compaction must not increase effective tokens: pre={pre_tokens}, post={post_tokens}"
    );
    assert!(
        event.tokens_saved.unwrap_or(0) > 0,
        "event should attribute positive token savings: {event:?}"
    );
}

#[test]
fn test_invalid_compacted_count_does_not_resurrect_full_transcript_after_new_turn() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("old turn {} {}", i, "x".repeat(120)),
        ));
        manager.notify_message_added();
    }

    manager.compacted_count = 500;
    manager.active_summary = Some(Summary {
        native_window: Vec::new(),
        native_route: None,
        text: "# Existing summary".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 500,
        original_turn_count: 500,
    });
    manager.active_chars.invalidate();

    let before_new_turn = manager.messages_for_api_with(&messages);
    assert_eq!(before_new_turn.len(), 1);
    assert_eq!(manager.compacted_count(), messages.len());

    messages.push(make_text_message(Role::User, "new turn after restore"));
    manager.notify_message_added();

    let after_new_turn = manager.messages_for_api_with(&messages);
    assert_eq!(
        after_new_turn.len(),
        2,
        "request should contain summary plus only the new active turn"
    );
    match &after_new_turn[1].content[0] {
        ContentBlock::Text { text, .. } => assert_eq!(text, "new turn after restore"),
        _ => panic!("expected new active text turn"),
    }
}

// ── messages_for_api_with after compaction ──────────────────────

#[test]
fn test_messages_for_api_with_summary_prepended() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(POLICY_TEST_WINDOW);
    let messages = policy_messages(40, POLICY_MESSAGE_CHARS);
    for _ in &messages {
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(190_000);

    manager.hard_compact_with(&messages).expect("should compact");

    let api_msgs = manager.messages_for_api_with(&messages);
    // First message should be the summary
    assert_eq!(api_msgs[0].role, Role::User);
    match &api_msgs[0].content[0] {
        ContentBlock::Text { text, .. } => {
            assert!(text.starts_with("## Previous Conversation Summary"));
        }
        _ => panic!("expected text"),
    }
    // Remaining should be recent turns from original messages
    assert!(api_msgs.len() < messages.len());
}

#[test]
fn test_persisted_state_round_trip_preserves_compacted_view() {
    let mut manager = CompactionManager::new().with_budget(500);
    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(make_text_message(Role::User, &format!("turn {} {}", i, "x".repeat(40))));
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(490);
    manager
        .hard_compact_with(&messages)
        .expect("should compact before persisting");

    let persisted = manager
        .persisted_state()
        .expect("compaction state should be exportable");
    let expected = manager.messages_for_api_with(&messages);

    let mut restored = CompactionManager::new().with_budget(500);
    restored.restore_persisted_state(&persisted, messages.len());
    let restored_msgs = restored.messages_for_api_with(&messages);

    assert_eq!(restored.compacted_count, persisted.compacted_count);
    assert_eq!(restored_msgs.len(), expected.len());
    match &restored_msgs[0].content[0] {
        ContentBlock::Text { text, .. } => {
            assert!(text.contains("Previous Conversation Summary"));
            assert!(text.contains("Emergency compaction"));
        }
        _ => panic!("expected restored summary block"),
    }
}

// ── context_usage accuracy ──────────────────────────────────────

#[test]
fn test_context_usage_with_both_estimate_and_observed() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(200_000);
    // Build messages totalling ~50k chars = ~12.5k token estimate
    let mut messages = Vec::new();
    for i in 0..50 {
        messages.push(make_text_message(Role::User, &format!("{} {}", i, "a".repeat(1000))));
        manager.notify_message_added();
    }

    // Without observed tokens, usage should be based on char estimate
    let usage_no_observed = manager.context_usage_with(&messages);
    assert!(
        usage_no_observed < 0.2,
        "char estimate should be low: {}",
        usage_no_observed
    );

    // With observed tokens at 160k, should use observed (higher) value
    manager.update_observed_input_tokens(160_000);
    let usage_with_observed = manager.context_usage_with(&messages);
    assert!(
        usage_with_observed >= 0.79,
        "should use observed tokens: {}",
        usage_with_observed
    );
}

#[test]
fn test_context_usage_after_compaction_resets_observed() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(make_text_message(
            Role::User,
            &format!("msg {} pad {}", i, "x".repeat(50)),
        ));
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(960);

    // Hard compact should reset observed_input_tokens
    manager.hard_compact_with(&messages).expect("should compact");
    assert!(
        manager.observed_input_tokens.is_none(),
        "observed_input_tokens should be cleared after hard compact"
    );

    // After compaction, usage should be based on char estimate of remaining messages only
    let post_usage = manager.context_usage_with(&messages);
    // The remaining messages are small, so usage should be well below the critical threshold
    assert!(
        post_usage < CRITICAL_THRESHOLD,
        "post-compaction usage should be below critical: {}",
        post_usage
    );
}

#[test]
fn test_recover_within_budget_drops_messages_without_truncation() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("msg {} pad {}", i, "x".repeat(40)),
        ));
        manager.notify_message_added();
    }
    // Push well over budget so recovery triggers.
    manager.update_observed_input_tokens(2_000);

    let recovery = manager.recover_within_budget(&mut messages);
    assert!(recovery.dropped.unwrap_or(0) > 0, "should drop old messages");
    // Dropping turns alone should fit the small remaining tail, so no
    // truncation escalation is needed.
    assert_eq!(
        recovery.truncated, 0,
        "should not truncate when dropping turns fits the budget"
    );
    assert!(recovery.did_anything());
    assert!(
        manager.context_usage_with(&messages) <= 1.0,
        "context should be back under budget after recovery"
    );
}

#[test]
fn test_recover_within_budget_truncates_when_tail_still_too_large() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = Vec::new();
    // Build tool-use/tool-result pairs whose results are each individually
    // larger than the whole budget. After hard compaction drops down to the
    // minimum kept tail, the surviving tool result is still far over budget, so
    // recovery must escalate to truncation (which only acts on tool results).
    for i in 0..10 {
        let id = format!("tool_{i}");
        messages.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.clone(),
                name: "bash".to_string(),
                input: serde_json::json!({ "command": "cat big.log" }),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        });
        manager.notify_message_added();
        messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::tool_result(
                &id,
                &format!("huge {} {}", i, "y".repeat(20_000)),
                Some(false),
            )],
            timestamp: None,
            tool_duration_ms: None,
        });
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(50_000);

    let recovery = manager.recover_within_budget(&mut messages);
    assert!(recovery.did_anything());
    assert!(
        recovery.truncated > 0,
        "should escalate to truncation when the remaining tail is still too large"
    );
}

#[test]
fn test_recover_within_budget_summary_line_variants() {
    let dropped_only = EmergencyRecovery {
        pre_usage: 1.6,
        dropped: Some(7),
        truncated: 0,
    };
    let line = dropped_only.summary_line(dropped_only.pre_usage);
    assert!(line.contains("dropped 7 old messages"));
    assert!(line.contains("160%"));
    assert!(!line.contains("truncated"));

    let dropped_and_truncated = EmergencyRecovery {
        pre_usage: 2.0,
        dropped: Some(3),
        truncated: 2,
    };
    let line = dropped_and_truncated.summary_line(dropped_and_truncated.pre_usage);
    assert!(line.contains("dropped 3 old messages"));
    assert!(line.contains("truncated 2 tool result(s)"));

    let truncate_only = EmergencyRecovery {
        pre_usage: 1.2,
        dropped: None,
        truncated: 5,
    };
    let line = truncate_only.summary_line(truncate_only.pre_usage);
    assert!(line.contains("shortened 5 large tool result(s)"));
    assert!(!line.contains("dropped"));
}

#[test]
fn max_context_tokens_caps_budget_from_large_window_models() {
    // A 1M-window model with the default 0.80 trigger lets a session reach
    // ~800k tokens per request before anything folds. The operator cap bounds
    // that regardless of what the provider advertises.
    let cfg = crate::config::CompactionConfig {
        max_context_tokens: 200_000,
        ..Default::default()
    };
    assert_eq!(CompactionManager::capped_budget(&cfg, 1_000_000), 200_000);
    // A model smaller than the cap keeps its own window.
    assert_eq!(CompactionManager::capped_budget(&cfg, 128_000), 128_000);
}

#[test]
fn max_context_tokens_zero_means_no_cap() {
    let cfg = crate::config::CompactionConfig::default();
    assert_eq!(CompactionManager::capped_budget(&cfg, 1_000_000), 1_000_000);
}

#[test]
fn max_context_tokens_applies_at_construction_and_reloads_before_requests() {
    let _lock = crate::storage::lock_test_env();
    struct RestoreHome(Option<std::ffi::OsString>);
    impl Drop for RestoreHome {
        fn drop(&mut self) {
            if let Some(home) = &self.0 {
                crate::env::set_var("HORON_HOME", home);
            } else {
                crate::env::remove_var("HORON_HOME");
            }
            crate::config::Config::invalidate_cache();
        }
    }
    let home = tempfile::tempdir().unwrap();
    let _restore = RestoreHome(std::env::var_os("HORON_HOME"));
    crate::env::set_var("HORON_HOME", home.path());
    let mut cfg = crate::config::Config::default();
    cfg.compaction.max_context_tokens = 50_000;
    cfg.save().unwrap();
    let mut manager = CompactionManager::new();
    assert_eq!(manager.token_budget(), 50_000);
    manager.set_budget(1_000_000);
    assert_eq!(manager.token_budget(), 50_000);

    cfg.compaction.max_context_tokens = 10_000;
    cfg.save().unwrap();
    manager.ensure_context_fits(&[], Arc::new(MockSummaryProvider));
    assert_eq!(manager.token_budget(), 10_000);

    cfg.compaction.max_context_tokens = 80_000;
    cfg.save().unwrap();
    manager.set_budget(128_000);
    assert_eq!(manager.token_budget(), 80_000);

    cfg.compaction.max_context_tokens = 0;
    cfg.save().unwrap();
    manager.ensure_context_fits(&[], Arc::new(MockSummaryProvider));
    assert_eq!(manager.token_budget(), 128_000);
}

// ── Task 11: token-budget and compaction-ownership policy ─────────────────

fn policy_messages(count: usize, chars: usize) -> Vec<Message> {
    (0..count)
        .map(|_| make_text_message(Role::User, &"x".repeat(chars)))
        .collect()
}

fn policy_assistant_messages(count: usize, chars: usize) -> Vec<Message> {
    (0..count)
        .map(|_| make_text_message(Role::Assistant, &"x".repeat(chars)))
        .collect()
}

#[test]
fn compaction_policy_absent_native_mode_resolves_structured() {
    let cfg: crate::config::Config = toml::from_str("").expect("empty config loads");
    assert_eq!(cfg.provider.openai_native_compaction_mode, "off");
    assert!(cfg.provider.explicit_native_compaction_mode().is_none());
    assert_eq!(cfg.compaction.strategy, None);
    assert_eq!(
        cfg.compaction.resolved_strategy(&cfg.provider),
        crate::compaction::CompactionStrategy::Structured
    );
}

#[test]
fn compaction_policy_explicit_native_modes_select_the_legacy_path() {
    let auto: crate::config::Config =
        toml::from_str("[provider]\nopenai_native_compaction_mode = \"auto\"\n").expect("config");
    assert_eq!(auto.provider.explicit_native_compaction_mode(), Some("auto"));
    assert_eq!(
        auto.compaction.resolved_strategy(&auto.provider),
        crate::compaction::CompactionStrategy::OpenAiAuto
    );

    let explicit: crate::config::Config =
        toml::from_str("[provider]\nopenai_native_compaction_mode = \"explicit\"\n").expect("config");
    assert_eq!(
        explicit.compaction.resolved_strategy(&explicit.provider),
        crate::compaction::CompactionStrategy::OpenAiExplicit
    );
}

#[test]
fn compaction_policy_explicit_off_stays_structured() {
    let cfg: crate::config::Config =
        toml::from_str("[provider]\nopenai_native_compaction_mode = \"off\"\n").expect("config");
    assert_eq!(
        cfg.compaction.resolved_strategy(&cfg.provider),
        crate::compaction::CompactionStrategy::Structured
    );
}

#[test]
fn compaction_policy_explicit_strategy_wins_over_legacy_native_mode() {
    let cfg: crate::config::Config = toml::from_str(
        "[compaction]\nstrategy = \"openai_explicit\"\n[provider]\nopenai_native_compaction_mode = \"off\"\n",
    )
    .expect("config");
    assert_eq!(
        cfg.compaction.resolved_strategy(&cfg.provider),
        crate::compaction::CompactionStrategy::OpenAiExplicit
    );

    let cfg: crate::config::Config = toml::from_str(
        "[compaction]\nstrategy = \"structured\"\n[provider]\nopenai_native_compaction_mode = \"auto\"\n",
    )
    .expect("config");
    assert_eq!(
        cfg.compaction.resolved_strategy(&cfg.provider),
        crate::compaction::CompactionStrategy::Structured
    );
}

#[test]
fn compaction_policy_recent_retention_uses_token_budget() {
    let _lock = crate::storage::lock_test_env();
    let manager = CompactionManager::new().with_budget(32_000);
    let messages = policy_messages(30, 4_000);
    // 32k window -> reserve 16_384 (configured default) -> threshold ceiling
    // 32_000 - 16_384 - 1_024 = 14_592, below window*0.50 = 16_000.
    assert_eq!(manager.compaction_threshold_tokens(), 14_592);
    // Recent tail: the recovery band caps the window-based 14,400 tokens at
    // floor(14,592 * 0.8) - min(4,096, 14,592 / 4) = 8,025 tokens; at 822 tokens
    // per 4,000-char message, 9 messages stay and 21 are summarized. The old
    // fixed ten-message rule would have kept 10 and compacted 20.
    assert_eq!(manager.recent_cutoff(&messages), 21);
}

#[test]
fn compaction_policy_tool_pairs_survive_recent_cutoff() {
    let _lock = crate::storage::lock_test_env();
    let mut messages = vec![make_text_message(Role::User, "start")];
    for index in 1..61 {
        if index % 2 == 1 {
            messages.push(Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: format!("call_{index}"),
                    name: "read".to_string(),
                    input: serde_json::json!({"file": "src/lib.rs"}),
                    thought_signature: None,
                }],
                timestamp: None,
                tool_duration_ms: None,
            });
        } else {
            messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::tool_result(
                    &format!("call_{}", index - 1),
                    &"y".repeat(4_000),
                    None,
                )],
                timestamp: None,
                tool_duration_ms: None,
            });
        }
    }

    let manager = CompactionManager::new().with_budget(32_000);
    let raw = manager.recent_cutoff(&messages);
    assert!(raw > 0, "the transcript must have something to compact");
    let safe = safe_compaction_cutoff(&messages, raw);
    assert!(safe <= raw);

    let kept_tool_uses: std::collections::HashSet<&str> = messages[safe..]
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    for message in &messages[safe..] {
        for block in &message.content {
            if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                assert!(
                    kept_tool_uses.contains(tool_use_id.as_str()),
                    "kept tool result {tool_use_id} lost its tool use"
                );
            }
        }
    }
}

#[test]
fn compaction_policy_newest_user_intent_is_kept() {
    let _lock = crate::storage::lock_test_env();
    let mut messages = policy_assistant_messages(40, 4_000);
    messages[3] = make_text_message(Role::User, "the current request");
    let manager = CompactionManager::new().with_budget(200_000);
    let cutoff = manager.recent_cutoff(&messages);
    assert!(cutoff <= 3, "cutoff {cutoff} would summarize the newest user request");
}

#[test]
fn compaction_policy_reactive_threshold_follows_window_tier() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(200_000);
    assert_eq!(manager.compaction_threshold_tokens(), 140_000);
    let messages = policy_messages(40, 4_000);

    manager.update_observed_input_tokens(139_999);
    assert!(!manager.should_compact_with(&messages));

    manager.update_observed_input_tokens(140_000);
    assert!(manager.should_compact_with(&messages));
}

#[test]
fn compaction_policy_adaptive_saving_ratio_moves_threshold() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(200_000);
    assert_eq!(manager.compaction_threshold_tokens(), 140_000);

    manager.prior_saving_ratio = Some(0.60);
    manager.refresh_context_cap();
    assert_eq!(manager.compaction_threshold_tokens(), 130_000);

    manager.prior_saving_ratio = Some(0.05);
    manager.refresh_context_cap();
    assert_eq!(manager.compaction_threshold_tokens(), 150_000);
}

struct PolicyHome(Option<std::ffi::OsString>);

impl Drop for PolicyHome {
    fn drop(&mut self) {
        match &self.0 {
            Some(home) => crate::env::set_var("HORON_HOME", home),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

/// Install a config file for `home` and point the process at it, so a manager
/// built afterwards resolves its strategy and budget the way production does.
fn install_policy_config(
    home: &std::path::Path,
    compaction: &crate::config::CompactionConfig,
    native_mode: Option<&str>,
) {
    crate::env::set_var("HORON_HOME", home);
    let mut cfg = crate::config::Config {
        compaction: compaction.clone(),
        ..crate::config::Config::default()
    };
    if let Some(mode) = native_mode {
        cfg.provider.openai_native_compaction_mode = mode.to_string();
    }
    cfg.save().expect("config saves");
    crate::config::Config::invalidate_cache();
}

#[tokio::test]
async fn compaction_policy_native_strategy_skips_structured_paths() {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _restore = PolicyHome(std::env::var_os("HORON_HOME"));
    install_policy_config(home.path(), &crate::config::CompactionConfig::default(), Some("auto"));

    let mut manager = CompactionManager::new().with_budget(200_000);
    assert_eq!(manager.strategy(), crate::compaction::CompactionStrategy::OpenAiAuto);
    manager.update_observed_input_tokens(199_000);
    let messages = policy_messages(40, 4_000);
    assert!(!manager.should_compact_with(&messages));
    // Even past the critical threshold the structured paths stay out of the way.
    assert_eq!(
        manager.ensure_context_fits(&messages, Arc::new(MockSummaryProvider)),
        CompactionAction::None
    );
    assert!(manager.last_compaction.is_none());
    assert!(
        manager
            .force_compact_with(&messages, Arc::new(MockSummaryProvider))
            .is_err()
    );
}

#[test]
fn compaction_policy_context_budget_too_small_blocks_compaction() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(16_000);
    assert!(manager.context_budget_error().is_some());
    assert!(manager.context_budget().is_none());
    assert_eq!(manager.compaction_threshold_tokens(), 0);

    manager.update_observed_input_tokens(15_500);
    let messages = policy_messages(40, 4_000);
    assert!(!manager.should_compact_with(&messages));
}

#[test]
fn compaction_policy_retention_is_not_a_fixed_message_count() {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _restore = PolicyHome(std::env::var_os("HORON_HOME"));
    // A reserve small enough that both windows can hold a budget, so the only
    // difference between the two managers is the window size.
    let compaction = crate::config::CompactionConfig {
        reserve_tokens: 1_024,
        ..Default::default()
    };
    install_policy_config(home.path(), &compaction, None);

    // 40 messages of 4_000 chars each: 1_001 tokens apiece.
    let messages = policy_messages(40, 4_000);

    // 16k window: ratio 0.45, the recovery band caps the tail at
    // floor(7,200 * 0.8) - min(4,096, 1,800) = 3,960 tokens, so 4 messages stay.
    let small = CompactionManager::new().with_budget(16_000);
    let small_kept = messages.len() - small.recent_cutoff(&messages);
    assert_eq!(small_kept, 4);
    assert!(
        small_kept < 10,
        "a small-window session must keep fewer than the old fixed ten messages, kept {small_kept}"
    );

    // 200k window: ratio 0.70, recent allowance 20_000 tokens -> 24 kept.
    let large = CompactionManager::new().with_budget(200_000);
    let large_kept = messages.len() - large.recent_cutoff(&messages);
    assert_eq!(large_kept, 24);
    assert!(
        large_kept > 10,
        "a large-window session must keep more than the old fixed ten messages, kept {large_kept}"
    );

    // Same transcript, same message count: only the window changed retention.
    assert!(small_kept < large_kept);
}

#[test]
fn compaction_policy_measures_korean_by_utf8_bytes() {
    let _lock = crate::storage::lock_test_env();
    let manager = CompactionManager::new().with_budget(POLICY_TEST_WINDOW);
    // The name records the assumption this test used to encode and now refutes:
    // that counting a Hangul syllable as its 3 UTF-8 bytes and dividing by four
    // is the conservative direction. It is not. On o200k Korean prose measures
    // about 0.56 tokens per character (44 tokens for the 79-character anchor),
    // so the byte heuristic charged a Korean character roughly 1.3x its real
    // cost and compacted early. cl100k is the family that undercounts Korean
    // (72 tokens for the same anchor), and `korean_messages_are_not_
    // underestimated_on_cl100k` in compaction-core holds that direction.
    let korean = "가".repeat(1_200); // 3_600 bytes; 1_200 syllables x 0.78 = 936 tokens
    let messages: Vec<Message> = (0..40).map(|_| make_text_message(Role::User, &korean)).collect();

    // 937 tokens per message (936 measured plus 1 of framing) keeps 21 messages
    // inside the 20_000-token allowance; the byte heuristic kept 22 because it
    // charged 901.
    let cutoff = manager.recent_cutoff(&messages);
    assert_eq!(messages.len() - cutoff, 21);
    assert_eq!(cutoff, 19);
}

#[tokio::test]
async fn compaction_policy_too_small_budget_never_spawns_a_provider_call() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(16_000);
    manager.update_observed_input_tokens(15_500);
    let messages = policy_messages(40, 4_000);
    manager.maybe_start_compaction_with(&messages, Arc::new(MockSummaryProvider));
    assert!(manager.pending_task.is_none());
}

// ── [OI] native compaction window compatibility (D9) ────────────────────────

fn native_window_state(
    route: crate::session::StoredNativeRoute,
    compacted_count: usize,
) -> crate::session::StoredCompactionState {
    let window = vec![
        serde_json::json!({
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": "retained" }],
        }),
        serde_json::json!({ "type": "compaction", "encrypted_content": "enc_window" }),
    ];
    crate::session::StoredCompactionState {
        summary_text: String::new(),
        openai_encrypted_content: Some("enc_window".to_string()),
        native_window: window,
        native_route: Some(route),
        covers_up_to_turn: compacted_count,
        original_turn_count: compacted_count,
        compacted_count,
        structured: None,
    }
}

#[test]
fn native_window_is_replayed_whole_from_the_manager() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(200_000);
    let messages: Vec<Message> = (0..4)
        .map(|index| make_text_message(Role::User, &format!("message {index}")))
        .collect();
    let state = native_window_state(
        crate::session::StoredNativeRoute {
            provider: "openai".to_string(),
            model: "gpt-window".to_string(),
            route: Some("openai-api".to_string()),
        },
        2,
    );
    manager.restore_persisted_state_with(&state, &messages);

    let for_api = manager.messages_for_api_with(&messages);
    assert_eq!(for_api.len(), 3, "summary block plus the two active messages");
    match &for_api[0].content[0] {
        ContentBlock::OpenAICompaction {
            encrypted_content,
            window,
        } => {
            assert_eq!(encrypted_content, "enc_window");
            assert_eq!(
                window, &state.native_window,
                "the retained window must be replayed whole and in order"
            );
        }
        other => panic!("expected a native compaction window, got {other:?}"),
    }
}

#[test]
fn native_window_from_another_route_does_not_advance_the_boundary() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(200_000);
    let messages: Vec<Message> = (0..4)
        .map(|index| make_text_message(Role::User, &format!("message {index}")))
        .collect();
    let state = native_window_state(
        crate::session::StoredNativeRoute {
            provider: "openai".to_string(),
            model: "gpt-old".to_string(),
            route: Some("openai-api".to_string()),
        },
        2,
    );
    manager.restore_persisted_state_with(&state, &messages);
    manager.observe_route_identity_with_route("openai", "gpt-new", Some("openai-api"));

    assert!(
        manager.discard_foreign_openai_native_compaction(),
        "a window produced on another model must be discarded"
    );
    assert_eq!(
        manager.compacted_count(),
        0,
        "the boundary must not stay advanced over an unreplayable window"
    );
    let for_api = manager.messages_for_api_with(&messages);
    assert!(
        for_api.iter().all(|message| message
            .content
            .iter()
            .all(|block| { !matches!(block, ContentBlock::OpenAICompaction { .. }) })),
        "a foreign window must never be replayed"
    );
    assert!(
        for_api
            .iter()
            .any(|message| message.content.iter().any(|block| matches!(
                block,
                ContentBlock::Text { text, .. } if text.contains("message 0")
            ))),
        "the originals must become active again"
    );
}

#[test]
fn native_window_on_the_same_route_survives_the_check() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(200_000);
    let messages: Vec<Message> = (0..4)
        .map(|index| make_text_message(Role::User, &format!("message {index}")))
        .collect();
    let state = native_window_state(
        crate::session::StoredNativeRoute {
            provider: "openai".to_string(),
            model: "gpt-window".to_string(),
            route: Some("openai-api".to_string()),
        },
        2,
    );
    manager.restore_persisted_state_with(&state, &messages);
    manager.observe_route_identity_with_route("openai", "gpt-window", Some("openai-api"));

    assert!(!manager.discard_foreign_openai_native_compaction());
    assert_eq!(manager.compacted_count(), 2);

    // The same model on a different credential route is still a different route.
    manager.observe_route_identity_with_route("openai", "gpt-window", Some("openai-oauth"));
    assert!(
        manager.discard_foreign_openai_native_compaction(),
        "an OAuth route cannot replay a window produced with an API key"
    );
}

#[test]
fn native_strategy_reconciliation_keeps_a_single_owner() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(200_000);
    assert_eq!(manager.strategy(), CompactionStrategy::Structured);

    assert!(manager.reconcile_strategy(Some(CompactionStrategy::OpenAiAuto)));
    assert_eq!(manager.strategy(), CompactionStrategy::OpenAiAuto);
    assert!(
        !manager.reconcile_strategy(Some(CompactionStrategy::OpenAiAuto)),
        "reconciling to the same owner must not churn the generation"
    );
    assert!(
        manager.reconcile_strategy(None),
        "a provider that leaves compaction to horon hands ownership back"
    );
    assert_eq!(manager.strategy(), CompactionStrategy::Structured);
}

#[test]
fn calibration_ratio_learns_observed_over_estimated_and_resets_on_route_change() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(200_000);
    manager.observe_route_identity_with_route("openai", "gpt-a", Some("openai-api"));
    assert_eq!(manager.calibration_ratio(), None);

    for _ in 0..3 {
        manager.note_request_estimate(10_000);
        manager.update_observed_input_tokens(13_000);
    }
    let ratio = manager.calibration_ratio().expect("ratio learned");
    assert!((ratio - 1.3).abs() < 1e-9, "ratio {ratio}");

    // A usage report without a noted estimate is not a sample.
    manager.update_observed_input_tokens(40_000);
    assert!((manager.calibration_ratio().unwrap() - 1.3).abs() < 1e-9);

    // An outlier is clamped and smoothed rather than adopted.
    manager.note_request_estimate(10_000);
    manager.update_observed_input_tokens(100_000);
    let ratio = manager.calibration_ratio().unwrap();
    assert!((ratio - (0.7 * 1.3 + 0.3 * 1.8)).abs() < 1e-9, "ratio {ratio}");

    // Small requests are dominated by fixed overhead and skipped.
    manager.note_request_estimate(1_000);
    manager.update_observed_input_tokens(5_000);
    assert!((manager.calibration_ratio().unwrap() - ratio).abs() < 1e-9);

    assert!(manager.observe_route_identity_with_route("openai", "gpt-b", Some("openai-api")));
    assert_eq!(manager.calibration_ratio(), None);
}

#[test]
fn calibration_scales_the_effective_estimate_with_a_floor() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(200_000);
    let raw = manager.effective_token_count();

    manager.note_request_estimate(10_000);
    manager.update_observed_input_tokens(6_000);
    // Observed usage still wins when it is larger; clear it to see the estimate.
    manager.observed_input_tokens = None;
    assert_eq!(
        manager.effective_token_count(),
        (raw as f64 * 0.75).ceil() as usize,
        "a 0.6 ratio is floored at 0.75"
    );
}
