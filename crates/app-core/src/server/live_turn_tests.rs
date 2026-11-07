#![cfg_attr(test, allow(clippy::await_holding_lock))]

use super::{LiveTurnSwarmContext, run_live_turn_if_idle_with_completion};
use crate::agent::Agent;
use crate::message::{Message, StreamEvent, ToolDefinition};
use crate::protocol::ServerEvent;
use crate::provider::{EventStream, Provider};
use crate::server::SwarmMember;
use crate::tool::Registry;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tokio::time::timeout;

/// A private home for the duration of a test that persists turn history.
///
/// `Agent` resolves its session store under `HORON_AGENT_HOME`, so another
/// test swapping or deleting the home makes `history_messages` fail and the
/// completion report read back as `None`.
struct TestHome {
    previous: Option<std::ffi::OsString>,
    _dir: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl TestHome {
    fn new() -> Self {
        let lock = crate::storage::lock_test_env();
        let dir = tempfile::tempdir().expect("temp home");
        let previous = std::env::var_os("HORON_AGENT_HOME");
        crate::env::set_var("HORON_AGENT_HOME", dir.path());
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
            Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
            None => crate::env::remove_var("HORON_AGENT_HOME"),
        }
    }
}

enum ScriptedResponse {
    Text(String),
    Error(String),
}

struct ScriptedProvider {
    responses: Arc<StdMutex<VecDeque<ScriptedResponse>>>,
}

impl ScriptedProvider {
    fn new(responses: Vec<ScriptedResponse>) -> Self {
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
        let response = self
            .responses
            .lock()
            .expect("scripted response queue lock")
            .pop_front()
            .expect("scripted provider ran out of responses");
        match response {
            ScriptedResponse::Text(text) => Ok(Box::pin(tokio_stream::iter(vec![
                Ok(StreamEvent::TextDelta(text)),
                Ok(StreamEvent::MessageEnd { stop_reason: None }),
            ]))),
            ScriptedResponse::Error(message) => Err(anyhow::anyhow!(message)),
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

async fn test_agent(provider: Arc<dyn Provider>) -> Arc<Mutex<Agent>> {
    let registry = Registry::new(provider.clone()).await;
    Arc::new(Mutex::new(Agent::new(provider, registry)))
}

fn attached_member(session_id: &str, event_tx: mpsc::UnboundedSender<ServerEvent>) -> SwarmMember {
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
        friendly_name: Some("otter".to_string()),
        report_back_to_session_id: None,
        latest_completion_report: None,
        role: "agent".to_string(),
        joined_at: Instant::now(),
        last_status_change: Instant::now(),
        is_headless: false,
        output_tail: None,
        todo_progress: None,
        todo_items: Vec::new(),
        runtime: crate::protocol::SwarmMemberRuntime::default(),
    }
}

#[allow(clippy::type_complexity)]
fn test_swarm(
    session_id: &str,
    event_tx: mpsc::UnboundedSender<ServerEvent>,
) -> (
    Arc<RwLock<HashMap<String, SwarmMember>>>,
    Arc<RwLock<HashMap<String, std::collections::HashSet<String>>>>,
    Arc<RwLock<VecDeque<super::SwarmEvent>>>,
    Arc<std::sync::atomic::AtomicU64>,
    broadcast::Sender<super::SwarmEvent>,
) {
    let (swarm_event_tx, _) = broadcast::channel(16);
    (
        Arc::new(RwLock::new(HashMap::from([(
            session_id.to_string(),
            attached_member(session_id, event_tx),
        )]))),
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(VecDeque::new())),
        Arc::new(std::sync::atomic::AtomicU64::new(0)),
        swarm_event_tx,
    )
}

async fn wait_for_status(members: &Arc<RwLock<HashMap<String, SwarmMember>>>, session_id: &str, status: &str) {
    timeout(Duration::from_secs(5), async {
        loop {
            {
                let members = members.read().await;
                if members.get(session_id).is_some_and(|member| member.status == status) {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("member should reach the expected status");
}

#[tokio::test]
async fn completion_channel_receives_latest_assistant_text() {
    let _home = TestHome::new();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedResponse::Text(
        "Latest assistant answer.".to_string(),
    )]));
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let agent = test_agent(provider_dyn).await;
    let session_id = agent.lock().await.session_id().to_string();
    let sessions = Arc::new(RwLock::new(HashMap::from([(session_id.clone(), agent.clone())])));
    let (member_event_tx, _member_event_rx) = mpsc::unbounded_channel();
    let (swarm_members, swarms_by_id, event_history, event_counter, swarm_event_tx) =
        test_swarm(&session_id, member_event_tx);
    let ctx = LiveTurnSwarmContext::new(
        &swarm_members,
        &swarms_by_id,
        &event_history,
        &event_counter,
        &swarm_event_tx,
    );

    let receiver = run_live_turn_if_idle_with_completion(&session_id, "please reply", None, &sessions, ctx)
        .await
        .expect("idle live session should start the turn");

    let report = timeout(Duration::from_secs(5), receiver)
        .await
        .expect("completion channel should receive the turn outcome")
        .expect("completion sender should stay alive for the turn");
    assert!(
        report
            .as_ref()
            .is_some_and(|report| report.contains("Latest assistant answer.")),
        "completion channel should carry the turn's latest assistant text, got {report:?}"
    );
    wait_for_status(&swarm_members, &session_id, "ready").await;
}

#[tokio::test]
async fn completion_channel_receives_none_when_turn_fails() {
    let _home = TestHome::new();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedResponse::Error(
        "scripted provider failure".to_string(),
    )]));
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let agent = test_agent(provider_dyn).await;
    let session_id = agent.lock().await.session_id().to_string();
    let sessions = Arc::new(RwLock::new(HashMap::from([(session_id.clone(), agent.clone())])));
    let (member_event_tx, _member_event_rx) = mpsc::unbounded_channel();
    let (swarm_members, swarms_by_id, event_history, event_counter, swarm_event_tx) =
        test_swarm(&session_id, member_event_tx);
    let ctx = LiveTurnSwarmContext::new(
        &swarm_members,
        &swarms_by_id,
        &event_history,
        &event_counter,
        &swarm_event_tx,
    );

    let receiver = run_live_turn_if_idle_with_completion(&session_id, "please fail", None, &sessions, ctx)
        .await
        .expect("idle live session should start the turn");

    let report = timeout(Duration::from_secs(5), receiver)
        .await
        .expect("completion channel should receive the turn outcome")
        .expect("completion sender should stay alive for the turn");
    assert_eq!(report, None, "a failed turn must report None");
    wait_for_status(&swarm_members, &session_id, "failed").await;
}

#[tokio::test]
async fn completion_channel_dropped_receiver_is_harmless() {
    let _home = TestHome::new();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedResponse::Text(
        "Nobody is listening.".to_string(),
    )]));
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let agent = test_agent(provider_dyn).await;
    let session_id = agent.lock().await.session_id().to_string();
    let sessions = Arc::new(RwLock::new(HashMap::from([(session_id.clone(), agent.clone())])));
    let (member_event_tx, _member_event_rx) = mpsc::unbounded_channel();
    let (swarm_members, swarms_by_id, event_history, event_counter, swarm_event_tx) =
        test_swarm(&session_id, member_event_tx);
    let ctx = LiveTurnSwarmContext::new(
        &swarm_members,
        &swarms_by_id,
        &event_history,
        &event_counter,
        &swarm_event_tx,
    );

    let receiver = run_live_turn_if_idle_with_completion(&session_id, "please reply to nobody", None, &sessions, ctx)
        .await
        .expect("idle live session should start the turn");
    drop(receiver);

    wait_for_status(&swarm_members, &session_id, "ready").await;
}
