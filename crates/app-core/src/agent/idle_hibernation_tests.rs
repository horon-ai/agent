//! Hibernation releases the derived caches and nothing else.
//!
//! Every test here has the same contract as the module: the provider
//! projection and the tool index go, the canonical transcript, frozen prompt
//! surface, undo state and the event log stay byte-identical.

use super::*;
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use std::ffi::OsString;
use std::sync::Arc;

/// A private home for the tests that persist a session, restored on drop.
struct TestHome {
    _lock: std::sync::MutexGuard<'static, ()>,
    _dir: tempfile::TempDir,
    previous_home: Option<OsString>,
}

impl TestHome {
    fn new() -> Self {
        let lock = crate::storage::lock_test_env();
        let dir = tempfile::tempdir().expect("temp home");
        let previous_home = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", dir.path());
        crate::storage::create_store_dir(&dir.path().join("sessions"), true).expect("create sessions dir");
        crate::config::Config::invalidate_cache();
        Self {
            _lock: lock,
            _dir: dir,
            previous_home,
        }
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        match self.previous_home.take() {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

struct NoopProvider;

#[async_trait::async_trait]
impl Provider for NoopProvider {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
        anyhow::bail!("hibernation must not run the provider")
    }

    fn name(&self) -> &str {
        "hibernation-test"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(NoopProvider)
    }
}

fn test_agent() -> Agent {
    let provider: Arc<dyn Provider> = Arc::new(NoopProvider);
    Agent::new_with_session(
        provider,
        Registry::empty(),
        Session::create_with_id(id::new_id("session_idle"), None, None),
        None,
    )
}

/// One assistant tool call plus its user-side result, the shape repair scans.
fn paired_tool_history(agent: &mut Agent, tool_id: &str, payload: String) {
    agent.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: tool_id.to_string(),
            name: "read".to_string(),
            input: serde_json::json!({"file_path": "test.txt"}),
            thought_signature: None,
        }],
    );
    agent.add_message(
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: tool_id.to_string(),
            content: payload,
            is_error: None,
        }],
    );
}

fn cache_count(agent: &mut Agent) -> usize {
    agent.session.memory_profile_snapshot().provider_cache_message_count
}

/// Test 1: a release drops both derived caches, leaves every resident input
/// alone, and the next repair rebuilds the index without duplicate results.
#[test]
fn idle_hibernation_releases_cache_and_rebuilds_exact_history_and_indexes() {
    let _home = TestHome::new();
    let mut agent = test_agent();
    paired_tool_history(&mut agent, "tool_old", "large result ".repeat(100_000));
    assert_eq!(agent.repair_missing_tool_outputs(), 0);
    // `Agent::new_with_session` seeds the session's own first message, so the
    // baselines are measured rather than assumed.
    let stored_before = agent.session.active_context().len();
    assert_eq!(stored_before, 3);
    let canonical_before = serde_json::to_value(agent.session.active_context()).unwrap();
    let provider_before = serde_json::to_value(agent.session.messages_for_provider()).unwrap();
    let projected_before = cache_count(&mut agent);
    assert!(projected_before >= 2, "both stored messages are in the projection");

    agent.locked_tools = Some(vec![ToolDefinition {
        name: "frozen".to_string(),
        description: "keep the original tool surface".to_string(),
        input_schema: serde_json::json!({"type": "object"}),
        defer_loading: false,
    }]);
    agent.agents_md_snapshot.0 = Some("Frozen instructions, not recaptured on wake".to_string());
    agent.provider_session_id = Some("upstream-live-session".to_string());
    agent.rewind_undo_snapshot = Some(RewindUndoSnapshot {
        head_event_id: "head".to_string(),
        provider_session_id: Some("undo-provider".to_string()),
        session_provider_session_id: None,
        visible_message_count: 2,
    });

    let released = agent
        .hibernate_idle_caches()
        .expect("an idle agent releases its caches");
    assert!(
        released.provider_cache_bytes > 1_000_000,
        "the large tool result must be what the projection was holding: {}",
        released.provider_cache_bytes
    );
    assert_eq!(released.tool_index_entries, 2);
    assert_eq!(cache_count(&mut agent), 0);
    assert_eq!(agent.tool_call_ids.capacity(), 0, "a fresh set releases the allocation");
    assert_eq!(agent.tool_result_ids.capacity(), 0);
    assert_eq!(agent.tool_output_scan_index, 0);
    assert_eq!(
        serde_json::to_value(agent.session.active_context()).unwrap(),
        canonical_before,
        "the canonical transcript is not part of a release"
    );
    assert_eq!(
        serde_json::to_value(agent.session.messages_for_provider()).unwrap(),
        provider_before,
        "the rebuilt projection is byte-identical to what was released"
    );
    assert_eq!(agent.provider_session_id.as_deref(), Some("upstream-live-session"));
    assert_eq!(agent.locked_tools.as_ref().unwrap()[0].name, "frozen");
    assert_eq!(
        agent.agents_md_snapshot.0.as_deref(),
        Some("Frozen instructions, not recaptured on wake")
    );
    assert_eq!(agent.rewind_undo_snapshot.as_ref().unwrap().visible_message_count, 2);

    // A later turn's repair starts from the retained history and must not add a
    // synthetic result for the already-paired call.
    paired_tool_history(&mut agent, "tool_new", "next result".to_string());
    assert_eq!(agent.repair_missing_tool_outputs(), 0);
    assert_eq!(agent.session.active_context().len(), stored_before + 2);
    assert!(agent.tool_result_ids.contains("tool_old"));
    assert!(agent.tool_result_ids.contains("tool_new"));
    assert_eq!(agent.tool_output_scan_index, stored_before + 2);
}

/// Test 2: every gate refuses on its own, and clearing all of them allows one
/// release.
#[tokio::test]
async fn idle_hibernation_refuses_pending_work_and_resumes_after_it_settles() {
    let _home = TestHome::new();
    let mut agent = test_agent();
    paired_tool_history(&mut agent, "tool_hibernation_inflight", "result".to_string());
    agent.session.messages_for_provider();

    agent.push_alert("pending delivery".to_string());
    assert!(agent.hibernate_idle_caches().is_none(), "a pending alert refuses");
    agent.pending_alerts.clear();

    let queue = agent.soft_interrupt_queue();
    queue.lock().unwrap().push(SoftInterruptMessage {
        content: "scheduled wake".to_string(),
        images: Vec::new(),
        urgent: false,
        source: SoftInterruptSource::User,
        client_id: None,
        input_id: None,
    });
    assert!(agent.hibernate_idle_caches().is_none(), "a queued interrupt refuses");
    queue.lock().unwrap().clear();

    let compaction = agent.registry.compaction();
    let compaction_guard = compaction.try_write().unwrap();
    assert!(agent.hibernate_idle_caches().is_none(), "busy compaction refuses");
    drop(compaction_guard);

    let tool = crate::tool::inflight::mark_tool_in_flight("tool_hibernation_inflight").expect("in-flight guard");
    assert!(agent.hibernate_idle_caches().is_none(), "a live tool refuses");
    assert!(cache_count(&mut agent) >= 2, "a refused release keeps the projection");
    drop(tool);

    let turn = crate::turn_cancel_registry::register_active_turn(agent.session_id(), InterruptSignal::new());
    assert!(agent.hibernate_idle_caches().is_none(), "an active turn refuses");
    drop(turn);

    assert!(
        agent.hibernate_idle_caches().is_some(),
        "with every gate clear the agent releases"
    );
}

/// Test 2 (continued): a running tool recorded by the running-tool registry and
/// a migration lease each refuse on their own.
#[test]
fn idle_hibernation_refuses_a_running_tool_and_a_migration_lease() {
    let _home = TestHome::new();
    let mut agent = test_agent();
    paired_tool_history(&mut agent, "tool_running", "result".to_string());
    agent.session.messages_for_provider();

    let running = crate::running_tool_registry::register(agent.session_id(), "tool_running", "read");
    assert!(
        agent.hibernate_idle_caches().is_none(),
        "a tool the session is awaiting refuses"
    );
    drop(running);

    // A restored attachment still owes its commit; its index must survive.
    agent.begin_pending_resume();
    assert!(agent.hibernate_idle_caches().is_none(), "a pending resume refuses");
    agent.clear_pending_resume();

    crate::storage::write_session_lease(&crate::storage::SessionLease {
        session_id: agent.session_id().to_string(),
        epoch: 1,
        away_host: Some("elsewhere".to_string()),
        repo_root: None,
        last_sync_snapshot: None,
        last_sync_head: None,
        updated_at: "test".to_string(),
    })
    .expect("write a lease naming another host");
    assert!(
        agent.hibernate_idle_caches().is_none(),
        "a session that moved away refuses"
    );
    crate::storage::remove_session_lease(agent.session_id());

    assert!(agent.hibernate_idle_caches().is_some());
}

/// Test 3: the release writes nothing. The event log keeps its bytes and its
/// mtime even though the projection was materialized right before.
#[test]
fn idle_hibernation_does_not_write_the_session() {
    let _home = TestHome::new();
    let mut agent = test_agent();
    paired_tool_history(&mut agent, "tool_saved", "saved result".to_string());
    agent.session.save().expect("save the session");
    let log = agent.session.event_log_path().expect("event log path");
    let before = std::fs::read(&log).expect("read the log");
    let modified = std::fs::metadata(&log).unwrap().modified().unwrap();

    agent.session.messages_for_provider();
    assert!(agent.hibernate_idle_caches().is_some());

    assert_eq!(std::fs::read(&log).expect("read the log"), before);
    assert_eq!(std::fs::metadata(&log).unwrap().modified().unwrap(), modified);
    assert!(
        agent.session.pending_event_ids().is_empty(),
        "a release must not leave an unsaved event behind"
    );
}

/// Test 4: the next turn sends the original tool pair exactly once, with the
/// retained result text and no placeholder.
#[tokio::test]
async fn idle_hibernation_next_turn_sends_original_tool_pairs_without_duplicates() {
    #[derive(Clone)]
    struct CaptureProvider(Arc<std::sync::Mutex<Vec<Vec<Message>>>>);

    #[async_trait::async_trait]
    impl Provider for CaptureProvider {
        async fn complete(
            &self,
            messages: &[Message],
            _: &[ToolDefinition],
            _: &str,
            _: Option<&str>,
        ) -> Result<EventStream> {
            self.0.lock().unwrap().push(messages.to_vec());
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(StreamEvent::TextDelta("awake".to_string())),
                Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }),
            ])))
        }

        fn name(&self) -> &str {
            "capture-idle-test"
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(self.clone())
        }
    }

    let _home = TestHome::new();
    let calls: Arc<std::sync::Mutex<Vec<Vec<Message>>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(CaptureProvider(Arc::clone(&calls)));
    let mut agent = Agent::new_with_session(
        provider,
        Registry::empty(),
        Session::create_with_id(id::new_id("session_idle_turn"), None, None),
        None,
    );
    agent.memory_enabled = false;
    paired_tool_history(&mut agent, "pre_hibernation_tool", "retained exact result".to_string());
    assert_eq!(agent.repair_missing_tool_outputs(), 0);
    agent.session.messages_for_provider();
    assert!(agent.hibernate_idle_caches().is_some());

    assert_eq!(agent.run_once_capture("wake now").await.unwrap(), "awake");
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let blocks: Vec<_> = calls[0].iter().flat_map(|message| &message.content).collect();
    assert_eq!(
        blocks
            .iter()
            .filter(|block| matches!(block, ContentBlock::ToolUse { id, .. } if id == "pre_hibernation_tool"))
            .count(),
        1,
        "the original tool call appears once"
    );
    assert_eq!(
        blocks
            .iter()
            .filter(|block| matches!(block, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "pre_hibernation_tool"))
            .count(),
        1,
        "the original tool result appears once, not twice"
    );
    assert!(blocks.iter().any(
        |block| matches!(block, ContentBlock::ToolResult { tool_use_id, content, .. } if tool_use_id == "pre_hibernation_tool" && content.contains("retained exact result"))
    ));
    assert!(
        !blocks.iter().any(
            |block| matches!(block, ContentBlock::ToolResult { content, .. } if content == TOOL_OUTPUT_MISSING_TEXT)
        )
    );
}
