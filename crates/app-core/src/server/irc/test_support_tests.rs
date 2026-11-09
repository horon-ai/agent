#![cfg_attr(test, allow(clippy::await_holding_lock))]

//! Shared fixtures for the server-side IRC tests: an isolated home/config, a
//! scripted provider, swarm member builders, and a bus factory.

use super::{delivery::SessionDeliver, directory::SwarmDirectory};
use crate::agent::Agent;
use crate::message::{Message, StreamEvent, ToolDefinition};
use crate::protocol::ServerEvent;
use crate::provider::{EventStream, Provider};
use crate::server::{SessionAgents, SessionInterruptQueues, SwarmEvent, SwarmMember};
use crate::tool::Registry;
use anyhow::Result;
use async_trait::async_trait;
use horon_agent_irc_core::{Deliver, DeliverResult, Directory, Endpoint, IrcBus, IrcLimits, IrcMessage, MessageId};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};

/// A private home for the duration of a test, serialized against every other
/// test that swaps `HORON_HOME` or installs the global bus.
pub(super) struct TestEnv {
    previous: Option<std::ffi::OsString>,
    _dir: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl TestEnv {
    pub(super) fn new(config: &str) -> Self {
        let lock = crate::storage::lock_test_env();
        let dir = tempfile::tempdir().expect("temp home");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", dir.path());
        if !config.is_empty() {
            let path = crate::config::Config::path().expect("config path");
            std::fs::create_dir_all(path.parent().expect("config parent")).expect("create config dir");
            std::fs::write(&path, config).expect("write config");
        }
        crate::config::Config::invalidate_cache();
        Self {
            previous,
            _dir: dir,
            _lock: lock,
        }
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        horon_agent_irc_core::global::clear();
        match self.previous.take() {
            Some(home) => crate::env::set_var("HORON_HOME", home),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

/// A provider that hands out one queued answer per turn.
pub(super) struct ScriptedProvider {
    responses: Arc<StdMutex<VecDeque<String>>>,
}

impl ScriptedProvider {
    pub(super) fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Arc::new(StdMutex::new(responses.into())),
        }
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let response = match self.responses.lock() {
            Ok(mut responses) => responses.pop_front(),
            Err(poisoned) => poisoned.into_inner().pop_front(),
        };
        match response {
            Some(text) => Ok(Box::pin(tokio_stream::iter(vec![
                Ok(StreamEvent::TextDelta(text)),
                Ok(StreamEvent::MessageEnd { stop_reason: None }),
            ]))),
            None => Err(anyhow::anyhow!("scripted provider ran out of responses")),
        }
    }

    fn name(&self) -> &str {
        "scripted"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            responses: Arc::clone(&self.responses),
        })
    }
}

pub(super) async fn scripted_agent(responses: Vec<String>) -> Arc<Mutex<Agent>> {
    let provider: Arc<dyn Provider> = Arc::new(ScriptedProvider::new(responses));
    let registry = Registry::new(provider.clone()).await;
    Arc::new(Mutex::new(Agent::new(provider, registry)))
}

/// An agent that never runs a turn successfully.
pub(super) async fn quiet_agent() -> Arc<Mutex<Agent>> {
    scripted_agent(Vec::new()).await
}

pub(super) fn member(
    session_id: &str,
    swarm_id: Option<&str>,
    status: &str,
    handle: Option<&str>,
    report_back: Option<&str>,
) -> (SwarmMember, mpsc::UnboundedReceiver<ServerEvent>) {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    (
        SwarmMember {
            session_id: session_id.to_string(),
            event_tx,
            event_txs: HashMap::new(),
            working_dir: None,
            swarm_id: swarm_id.map(str::to_string),
            swarm_enabled: swarm_id.is_some(),
            status: status.to_string(),
            detail: None,
            task_label: None,
            friendly_name: handle.map(str::to_string),
            report_back_to_session_id: report_back.map(str::to_string),
            latest_completion_report: None,
            role: "agent".to_string(),
            joined_at: Instant::now(),
            last_status_change: Instant::now(),
            is_headless: false,
            output_tail: None,
            todo_progress: None,
            todo_items: Vec::new(),
            runtime: crate::protocol::SwarmMemberRuntime::default(),
        },
        event_rx,
    )
}

/// Shared swarm bookkeeping handles for one test.
pub(super) struct Fixture {
    pub(super) members: Arc<RwLock<HashMap<String, SwarmMember>>>,
    pub(super) swarms_by_id: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    pub(super) event_history: Arc<RwLock<VecDeque<SwarmEvent>>>,
    pub(super) event_counter: Arc<AtomicU64>,
    pub(super) event_tx: broadcast::Sender<SwarmEvent>,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let (event_tx, _) = broadcast::channel(16);
        Self {
            members: Arc::new(RwLock::new(HashMap::new())),
            swarms_by_id: Arc::new(RwLock::new(HashMap::new())),
            event_history: Arc::new(RwLock::new(VecDeque::new())),
            event_counter: Arc::new(AtomicU64::new(0)),
            event_tx,
        }
    }

    pub(super) async fn add(&self, member: SwarmMember) {
        let session_id = member.session_id.clone();
        self.members.write().await.insert(session_id, member);
    }

    pub(super) fn deliver(&self, sessions: SessionAgents, queues: SessionInterruptQueues) -> SessionDeliver {
        SessionDeliver::new(
            Arc::clone(&self.members),
            sessions,
            queues,
            Arc::clone(&self.swarms_by_id),
            Arc::clone(&self.event_history),
            Arc::clone(&self.event_counter),
            self.event_tx.clone(),
        )
    }

    pub(super) fn directory(&self) -> SwarmDirectory {
        SwarmDirectory::new(Arc::clone(&self.members))
    }
}

pub(super) fn irc_message(id: u64, seq: u64, from: &str, to: &str) -> IrcMessage {
    IrcMessage {
        id: MessageId(id),
        seq,
        from: Arc::from(from),
        to: Arc::from(to),
        body: Arc::from("hello"),
        ts_ms: 0,
        reply_to: None,
        broadcast: false,
        progress: false,
        wake_relay: false,
    }
}

pub(super) fn endpoint_of(member: &SwarmMember) -> Endpoint {
    Endpoint {
        id: Arc::from(member.session_id.as_str()),
        handle: member.friendly_name.as_deref().map(Arc::from),
        parent: member.report_back_to_session_id.as_deref().map(Arc::from),
        status: Arc::from(member.status.as_str()),
    }
}

pub(super) fn sessions_of(agents: &[(&str, Arc<Mutex<Agent>>)]) -> SessionAgents {
    Arc::new(RwLock::new(
        agents
            .iter()
            .map(|(session_id, agent)| (session_id.to_string(), Arc::clone(agent)))
            .collect(),
    ))
}

/// A deliver target that is never live: every leg fails without buffering.
pub(super) struct NeverDeliver;

impl Deliver for NeverDeliver {
    fn deliver<'a>(
        &'a self,
        _target: &'a Endpoint,
        _message: &'a IrcMessage,
        _from_parent: bool,
    ) -> futures::future::BoxFuture<'a, DeliverResult> {
        Box::pin(async { DeliverResult::Errored("no live session".to_string()) })
    }

    fn has_pending_input<'a>(&'a self, _agent: &'a str) -> futures::future::BoxFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn can_respond<'a>(&'a self, _agent: &'a str) -> futures::future::BoxFuture<'a, bool> {
        Box::pin(async { false })
    }
}

/// Build a bus over `directory` and `deliver`, and publish it as the global bus.
pub(super) fn install_bus(directory: Arc<dyn Directory>, deliver: Arc<dyn Deliver>, limits: IrcLimits) -> Arc<IrcBus> {
    let bus = IrcBus::new(limits, directory, deliver);
    horon_agent_irc_core::global::install(Arc::clone(&bus));
    bus
}
