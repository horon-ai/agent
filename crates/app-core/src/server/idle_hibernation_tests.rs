//! The hibernation sweep: when it fires, when it refuses, and that it releases
//! exactly the projection an idle compaction left materialized.

use super::*;
use crate::agent::Agent;
use crate::message::{ContentBlock, Role};
use crate::message::{Message, ToolDefinition};
use crate::protocol::ServerEvent;
use crate::provider::{EventStream, Provider};
use crate::session::Session;
use crate::tool::Registry;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, mpsc};

struct IdleProvider;

#[async_trait::async_trait]
impl Provider for IdleProvider {
    async fn complete(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        anyhow::bail!("the hibernation sweep must not run the provider")
    }

    fn name(&self) -> &str {
        "idle-test"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(IdleProvider)
    }
}

/// A member record for a session that is neither running nor streaming.
fn member(session_id: &str, last_status_change: Instant) -> SwarmMember {
    let (event_tx, _rx) = mpsc::unbounded_channel::<ServerEvent>();
    SwarmMember {
        session_id: session_id.to_string(),
        event_tx,
        event_txs: HashMap::new(),
        working_dir: None,
        swarm_id: None,
        swarm_enabled: false,
        status: "ready".to_string(),
        detail: None,
        task_label: None,
        friendly_name: None,
        report_back_to_session_id: None,
        latest_completion_report: None,
        role: "agent".to_string(),
        joined_at: last_status_change,
        last_status_change,
        is_headless: true,
        output_tail: None,
        todo_progress: None,
        todo_items: Vec::new(),
        runtime: crate::protocol::SwarmMemberRuntime::default(),
    }
}

/// A session holding one paired tool call, with its provider projection
/// materialized, so a release has something measurable to drop.
async fn session_with_projection() -> (Arc<Mutex<Agent>>, String) {
    let provider: Arc<dyn Provider> = Arc::new(IdleProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut session = Session::create_with_id(crate::id::new_id("session_idle_sweep"), None, None);
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "tool_sweep".to_string(),
            name: "read".to_string(),
            input: serde_json::json!({"file_path": "sweep.txt"}),
            thought_signature: None,
        }],
    );
    session.add_message(
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: "tool_sweep".to_string(),
            content: "result ".repeat(10_000),
            is_error: None,
        }],
    );
    let mut agent = Agent::new_with_session(provider, registry, session, None);
    // The turn-start scan is what fills the index, and the idle compaction
    // check is what leaves the projection resident.
    assert_eq!(agent.seed_idle_tool_index(), 0);
    assert_eq!(agent.idle_index_snapshot(), (1, 1, 2));
    let projected = agent.materialize_idle_provider_cache();
    assert!(projected >= 2);
    assert!(agent.idle_provider_cache_snapshot().0 >= 2);
    let id = agent.session_id().to_string();
    (Arc::new(Mutex::new(agent)), id)
}

/// Test 5: the disable flag, a short idle window and recent activity each keep
/// the sweep from firing; only all four conditions together allow it.
#[test]
fn idle_sweep_due_respects_disable_idle_and_recent_activity() {
    let start = Instant::now();
    let threshold = Duration::from_secs(30);
    let state = IdleState::new(start);
    let later = start + threshold;

    assert!(
        !state.due(later, Duration::ZERO, threshold, None),
        "0 disables the sweep"
    );
    assert!(
        !state.due(start, threshold, threshold, None),
        "the window has not elapsed"
    );
    assert!(
        !state.due(later, threshold, Duration::ZERO, None),
        "the status just changed"
    );
    assert!(
        !state.due(later, threshold, threshold, Some(Duration::ZERO)),
        "a recent request resets activity"
    );
    assert!(state.due(later, threshold, threshold, None), "an idle session is due");
    assert!(
        IdleState::new(start).due(later, threshold, threshold, None),
        "a session first seen past the threshold is due on that tick"
    );
}

/// Test 6: a whole turn between two ticks re-arms the window by generation, not
/// by the age of the timestamp.
#[test]
fn idle_sweep_rearms_for_a_turn_entirely_between_ticks() {
    let start = Instant::now();
    let threshold = Duration::from_secs(1);
    let mut state = IdleState::new(start);
    state.released = true;
    let now = start + Duration::from_secs(10);
    assert!(!state.due(now, threshold, Duration::from_secs(10), None));

    let turn_finished = start + Duration::from_secs(2);
    state.observe_status(turn_finished);
    assert!(state.due(now, threshold, Duration::from_secs(8), Some(Duration::from_secs(8))));

    state.released = true;
    state.observe_status(turn_finished);
    assert!(
        !state.due(now, threshold, Duration::from_secs(8), None),
        "observing the same generation does not re-arm a released window"
    );
}

/// Test 7: a session with an active turn is skipped while an idle session is
/// released once, and a zero threshold releases nothing.
#[tokio::test]
async fn idle_sweep_skips_busy_then_releases_once_and_never_with_zero() {
    let (busy, busy_id) = session_with_projection().await;
    let (idle_agent, idle_id) = session_with_projection().await;
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([
        (busy_id.clone(), Arc::clone(&busy)),
        (idle_id.clone(), Arc::clone(&idle_agent)),
    ])));
    let old = Instant::now() - Duration::from_secs(60);
    let members = Arc::new(RwLock::new(HashMap::from([
        (busy_id.clone(), member(&busy_id, old)),
        (idle_id.clone(), member(&idle_id, old)),
    ])));
    let threshold = Duration::from_secs(1);

    // Hold the busy session's turn registration for the first sweep only.
    let busy_turn =
        crate::turn_cancel_registry::register_active_turn(&busy_id, horon_agent_agent_runtime::InterruptSignal::new());
    let mut idle: HashMap<String, IdleState> = HashMap::new();
    for id in [&busy_id, &idle_id] {
        idle.insert(id.clone(), IdleState::new(old));
    }
    assert_eq!(
        reclaim_idle_caches(&sessions, &members, &mut idle, threshold).await,
        1,
        "only the idle session releases"
    );
    assert!(idle[&idle_id].released);
    drop(busy_turn);

    assert_eq!(
        idle_agent.lock().await.idle_provider_cache_snapshot().0,
        0,
        "the idle session dropped its projection"
    );
    assert!(
        busy.lock().await.idle_provider_cache_snapshot().0 >= 2,
        "the busy session kept its projection"
    );

    assert_eq!(
        reclaim_idle_caches(&sessions, &members, &mut idle, threshold).await,
        0,
        "a released window does not fire twice"
    );
    assert_eq!(
        reclaim_idle_caches(&sessions, &members, &mut idle, Duration::ZERO).await,
        0,
        "0 disables the whole sweep"
    );
    assert!(idle.is_empty(), "disabling clears the windows");
}

/// Test 8: a contended Agent lock is skipped, never awaited, and the session is
/// released on the sweep after the lock frees.
#[tokio::test]
async fn idle_sweep_skips_a_contended_agent_lock() {
    let (agent, id) = session_with_projection().await;
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([(id.clone(), Arc::clone(&agent))])));
    let old = Instant::now() - Duration::from_secs(60);
    let members = Arc::new(RwLock::new(HashMap::from([(id.clone(), member(&id, old))])));
    let threshold = Duration::from_secs(1);
    let mut idle: HashMap<String, IdleState> = HashMap::from([(id.clone(), IdleState::new(old))]);

    let held = agent.lock().await;
    assert_eq!(
        reclaim_idle_caches(&sessions, &members, &mut idle, threshold).await,
        0,
        "a held Agent lock is skipped"
    );
    assert!(idle.is_empty(), "the window is dropped, not queued");
    drop(held);

    assert_eq!(
        reclaim_idle_caches(&sessions, &members, &mut idle, threshold).await,
        1,
        "the session is released as soon as its lock is free"
    );
}

/// Test 9: the idle compaction monitor materializes the projection to judge a
/// job, so the sweep is the only thing that can take those bytes back. This is
/// the scenario the feature exists for, and it is horon-specific.
#[tokio::test]
async fn idle_sweep_reclaims_the_projection_idle_compaction_materialized() {
    let (agent, id) = session_with_projection().await;
    {
        let mut guard = agent.lock().await;
        // `maybe_start_idle_compaction` reads the borrowed projection before it
        // decides anything (agent/compaction.rs), which leaves it resident.
        assert!(guard.materialize_idle_provider_cache() >= 2);
        let (count, bytes) = guard.idle_provider_cache_snapshot();
        assert!(count >= 2, "the projection is resident before the sweep");
        assert!(bytes > 50_000, "the tool result must be the bulk of it: {bytes}");
    }

    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([(id.clone(), Arc::clone(&agent))])));
    let old = Instant::now() - Duration::from_secs(60);
    let members = Arc::new(RwLock::new(HashMap::from([(id.clone(), member(&id, old))])));
    let mut idle: HashMap<String, IdleState> = HashMap::from([(id.clone(), IdleState::new(old))]);

    assert_eq!(
        reclaim_idle_caches(&sessions, &members, &mut idle, Duration::from_secs(1)).await,
        1
    );
    let mut guard = agent.lock().await;
    assert_eq!(guard.idle_provider_cache_snapshot(), (0, 0));
    assert_eq!(guard.message_count(), 2, "the transcript stays");
    assert_eq!(guard.idle_index_snapshot(), (0, 0, 0));
}
