#![cfg_attr(test, allow(clippy::await_holding_lock))]

use super::{SessionInterruptQueues, queue_soft_interrupt_for_session, register_session_interrupt_queue};
use crate::agent::Agent;
use crate::message::{Message, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use anyhow::Result;
use async_trait::async_trait;
use horon_agent_agent_runtime::SoftInterruptSource;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

struct TestProvider;

#[async_trait]
impl Provider for TestProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Err(anyhow::anyhow!(
            "test provider complete should not be called in queue tests"
        ))
    }

    fn name(&self) -> &str {
        "test"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(TestProvider)
    }
}

async fn test_agent() -> Arc<Mutex<Agent>> {
    let provider: Arc<dyn Provider> = Arc::new(TestProvider);
    let registry = Registry::new(provider.clone()).await;
    Arc::new(Mutex::new(Agent::new(provider, registry)))
}

/// A private temporary home for a test whose queue path persists state.
///
/// `Agent::new` and the durable input journal resolve their directories under
/// `HORON_HOME`, so a test that accepts an interrupt writes into whatever
/// home is current. Without this guard another test's swapped or deleted home
/// makes the journal refuse the write ("unsafe or non-private path"), the accept
/// returns `false`, and the test fails only in the parallel suite.
struct TestHome {
    previous: Option<std::ffi::OsString>,
    _dir: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl TestHome {
    fn new() -> Self {
        let lock = crate::storage::lock_test_env();
        let dir = tempfile::tempdir().expect("temp home");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", dir.path());
        Self {
            previous,
            _dir: dir,
            _lock: lock,
        }
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
    }
}

#[tokio::test]
async fn queue_soft_interrupt_for_session_uses_registered_queue_when_agent_busy() {
    let _home = TestHome::new();
    let agent = test_agent().await;
    let session_id = {
        let guard = agent.lock().await;
        guard.session_id().to_string()
    };
    let queue = {
        let guard = agent.lock().await;
        guard.soft_interrupt_queue()
    };
    let queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    register_session_interrupt_queue(&queues, &session_id, queue.clone()).await;
    let sessions = Arc::new(RwLock::new(HashMap::from([(session_id.clone(), agent.clone())])));

    let _busy_guard = agent.lock().await;
    let queued = queue_soft_interrupt_for_session(
        &session_id,
        "queued while busy".to_string(),
        false,
        SoftInterruptSource::User,
        &queues,
        &sessions,
    )
    .await;

    assert!(queued, "interrupt should queue even while agent lock is held");
    let pending = queue.lock().expect("queue lock");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].content, "queued while busy");
    assert!(!pending[0].urgent);
    assert_eq!(pending[0].source, SoftInterruptSource::User);
}

#[tokio::test]
async fn queue_soft_interrupt_for_session_registers_queue_on_fallback_lookup() {
    let _home = TestHome::new();
    let agent = test_agent().await;
    let session_id = {
        let guard = agent.lock().await;
        guard.session_id().to_string()
    };
    let queue = {
        let guard = agent.lock().await;
        guard.soft_interrupt_queue()
    };
    let queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let sessions = Arc::new(RwLock::new(HashMap::from([(session_id.clone(), agent.clone())])));

    let queued = queue_soft_interrupt_for_session(
        &session_id,
        "fallback lookup".to_string(),
        true,
        SoftInterruptSource::System,
        &queues,
        &sessions,
    )
    .await;

    assert!(queued, "interrupt should queue via session fallback");
    assert!(
        queues.read().await.contains_key(&session_id),
        "fallback should cache the session queue for later busy sends"
    );
    let pending = queue.lock().expect("queue lock");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].content, "fallback lookup");
    assert!(pending[0].urgent);
    assert_eq!(pending[0].source, SoftInterruptSource::System);
}

#[tokio::test]
async fn queue_soft_interrupt_for_session_persists_when_live_queue_is_unavailable() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let prev_home = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", temp.path());

    let agent = test_agent().await;
    let session_id = {
        let guard = agent.lock().await;
        guard.session_id().to_string()
    };
    crate::session::Session::create_with_id(session_id.clone(), None, None)
        .save_prepared()
        .expect("save session snapshot");

    let queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let sessions = Arc::new(RwLock::new(HashMap::new()));

    let queued = queue_soft_interrupt_for_session(
        &session_id,
        "persist while reloading".to_string(),
        false,
        SoftInterruptSource::BackgroundTask,
        &queues,
        &sessions,
    )
    .await;

    assert!(queued, "interrupt should persist when live queue is unavailable");

    let persisted = crate::soft_interrupt_store::load(&session_id).expect("load persisted interrupts");
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].content, "persist while reloading");
    assert_eq!(persisted[0].source, SoftInterruptSource::BackgroundTask);

    let provider: Arc<dyn Provider> = Arc::new(TestProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut restored = Agent::new(provider, registry);
    restored
        .restore_session(&session_id)
        .expect("restore session should rehydrate interrupts");
    assert_eq!(restored.soft_interrupt_count(), 1);
    assert_eq!(
        crate::soft_interrupt_store::load(&session_id)
            .expect("load persisted interrupts after restore")
            .len(),
        1,
        "restore is non-destructive, so the disk copy survives until a turn commits"
    );
    assert_eq!(
        restored.soft_interrupt_count(),
        1,
        "a non-destructive restore must not duplicate the message in memory"
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("HORON_HOME", prev_home);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
}

#[tokio::test]
async fn a_soft_interrupt_burst_raises_the_queue_high_water_counter() {
    let _home = TestHome::new();
    // The counters are process-wide, so reset-and-read only means anything
    // while no other test is measuring them.
    let _counters = crate::perf::lock_perf_counters().await;
    horon_agent_perf_counters::reset();
    let queue = horon_agent_agent_runtime::SoftInterruptQueue::new();
    let session_id = "burst-session";
    let queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    register_session_interrupt_queue(&queues, session_id, queue.clone()).await;
    let sessions: Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>> = Arc::new(RwLock::new(HashMap::new()));

    let before = horon_agent_perf_counters::snapshot();
    assert_eq!(
        before.queue_depth_high_water, 0,
        "a fresh queue reports no depth before the burst"
    );

    for index in 0..16 {
        let queued = queue_soft_interrupt_for_session(
            session_id,
            format!("burst {index}"),
            false,
            SoftInterruptSource::User,
            &queues,
            &sessions,
        )
        .await;
        assert!(queued, "the registered queue accepts every interrupt in the burst");
    }

    let after = horon_agent_perf_counters::snapshot();
    assert!(
        after.queue_depth_high_water >= 16,
        "the burst must raise the high-water mark, got {}",
        after.queue_depth_high_water
    );
    assert!(
        after.queue_bytes_high_water > 0,
        "the burst must record buffered payload bytes"
    );
    // Read by scripts/perf/check_structural.py: the bridge path must raise the
    // same counter as the runtime path, and to no more than the bound.
    println!(
        "STRUCTURAL_PERF section=queue source=app-core burst=16 queue_depth_high_water={} queue_bytes_high_water={}",
        after.queue_depth_high_water, after.queue_bytes_high_water
    );
}

/// The pending queue has a hard ceiling, and reaching it refuses rather than
/// growing.
///
/// A session whose turns are slow can be handed inputs faster than it drains
/// them. Without a bound the process grows until it dies; with one, the client
/// is told the input was not queued.
#[tokio::test]
async fn durable_input_queue_refuses_past_the_ceiling() {
    let queue = horon_agent_agent_runtime::SoftInterruptQueue::default();
    for index in 0..super::state::MAX_PENDING_INPUTS {
        let accepted = super::state::enqueue_message(
            &queue,
            horon_agent_agent_runtime::SoftInterruptMessage {
                content: format!("input {index}"),
                images: Vec::new(),
                urgent: false,
                source: horon_agent_agent_runtime::SoftInterruptSource::User,
                client_id: None,
                input_id: None,
            },
        );
        assert!(accepted, "input {index} is under the ceiling and must be queued");
    }
    assert_eq!(
        queue.lock().expect("queue").len(),
        super::state::MAX_PENDING_INPUTS,
        "the queue holds exactly the ceiling"
    );

    let refused = super::state::enqueue_message(
        &queue,
        horon_agent_agent_runtime::SoftInterruptMessage {
            content: "one too many".to_string(),
            images: Vec::new(),
            urgent: false,
            source: horon_agent_agent_runtime::SoftInterruptSource::User,
            client_id: None,
            input_id: None,
        },
    );
    assert!(!refused, "past the ceiling the input is refused, not queued");
    assert_eq!(
        queue.lock().expect("queue").len(),
        super::state::MAX_PENDING_INPUTS,
        "a refused input does not grow the queue"
    );
}
