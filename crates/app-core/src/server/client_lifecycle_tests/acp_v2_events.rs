//! Daemon reports for ACP v2 connections: insertions, foreground state, tool exits, message ids.

use super::*;
use serde_json::{Value, json};
use std::collections::VecDeque;
use tokio::sync::Semaphore;

const DEADLINE: Duration = Duration::from_secs(30);

pub(super) struct Step {
    gate: Option<Arc<Semaphore>>,
    events: Vec<StreamEvent>,
}

/// Answers each provider call with the next scripted step, then plain text.
#[derive(Clone, Default)]
pub(super) struct V2Provider {
    script: Arc<std::sync::Mutex<VecDeque<Step>>>,
}

fn text_events(text: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::TextDelta(text.to_string()),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".into()),
        },
    ]
}

impl V2Provider {
    pub(super) fn push(&self, step: Step) {
        self.script
            .lock()
            .expect("the script is never poisoned")
            .push_back(step);
    }

    /// The next call answers `text` only once the returned gate gets a permit.
    pub(super) fn push_gated_text(&self, text: &str) -> Arc<Semaphore> {
        let gate = Arc::new(Semaphore::new(0));
        self.push(Step {
            gate: Some(Arc::clone(&gate)),
            events: text_events(text),
        });
        gate
    }

    pub(super) fn push_tool_call(&self, id: &str, name: &str, input: Value) {
        self.push(Step {
            gate: None,
            events: vec![
                StreamEvent::ToolUseStart {
                    id: id.to_string(),
                    name: name.to_string(),
                },
                StreamEvent::ToolInputDelta(input.to_string()),
                StreamEvent::ToolUseEnd,
                StreamEvent::MessageEnd {
                    stop_reason: Some("tool_use".into()),
                },
            ],
        });
        self.push(Step {
            gate: None,
            events: text_events("tool round finished"),
        });
    }
}

#[async_trait]
impl Provider for V2Provider {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
        let step = self.script.lock().expect("the script is never poisoned").pop_front();
        let events = match step {
            Some(Step { gate, events }) => {
                if let Some(gate) = gate {
                    let _permit = gate.acquire().await.expect("the gate stays open");
                }
                events
            }
            None => text_events("finished"),
        };
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }

    fn name(&self) -> &str {
        "acp-v2-events-test"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

pub(super) struct Conn {
    task: tokio::task::JoinHandle<Result<()>>,
    reader: BufReader<crate::transport::ReadHalf>,
    writer: crate::transport::WriteHalf,
}

impl Conn {
    pub(super) async fn send(&mut self, value: Value) {
        let mut line = value.to_string();
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .await
            .expect("the daemon reads the frame");
    }

    async fn next_raw(&mut self) -> (String, ServerEvent) {
        let mut line = String::new();
        let read = tokio::time::timeout(DEADLINE, self.reader.read_line(&mut line))
            .await
            .expect("the daemon answers within the deadline")
            .expect("the connection stays readable");
        assert!(read > 0, "the connection closed before the expected event");
        let event = serde_json::from_str(line.trim()).expect("the daemon frames a server event");
        (line, event)
    }

    pub(super) async fn until(&mut self, stop: impl Fn(&ServerEvent) -> bool) -> Vec<ServerEvent> {
        self.until_raw(stop).await.into_iter().map(|(_, event)| event).collect()
    }

    async fn until_raw(&mut self, stop: impl Fn(&ServerEvent) -> bool) -> Vec<(String, ServerEvent)> {
        let mut seen = Vec::new();
        loop {
            let (line, event) = self.next_raw().await;
            let done = stop(&event);
            seen.push((line, event));
            if done {
                return seen;
            }
        }
    }
}

pub(super) struct Harness {
    _env: IsolatedReloadRecoveryEnv,
    work: tempfile::TempDir,
    pub(super) provider: V2Provider,
    sessions: SessionAgents,
    global_session_id: Arc<RwLock<String>>,
    client_count: Arc<RwLock<usize>>,
    client_connections: Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    swarm_members: Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    shared_context: Arc<RwLock<HashMap<String, HashMap<String, SharedContext>>>>,
    plan_proposals: PlanProposalStore,
    swarm_plans: Arc<RwLock<HashMap<String, VersionedPlan>>>,
    swarm_coordinators: Arc<RwLock<HashMap<String, String>>>,
    file_touch: FileTouchService,
    channel_subscriptions: ChannelSubscriptions,
    channel_subscriptions_by_session: ChannelSubscriptions,
    event_history: Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    event_counter: Arc<std::sync::atomic::AtomicU64>,
    swarm_event_tx: broadcast::Sender<SwarmEvent>,
    global_event_tx: broadcast::Sender<ServerEvent>,
    global_is_processing: Arc<RwLock<bool>>,
    shutdown_signals: Arc<RwLock<HashMap<String, InterruptSignal>>>,
    soft_interrupt_queues: SessionInterruptQueues,
    mcp_pool: Arc<crate::mcp::SharedMcpPool>,
    // Last, so the environment above is restored before another test may run.
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Harness {
    pub(super) fn new() -> Self {
        let lock = crate::storage::lock_test_env();
        let env = IsolatedReloadRecoveryEnv::new();
        crate::config::invalidate_config_cache();
        Self {
            _lock: lock,
            _env: env,
            work: tempfile::TempDir::new().expect("work dir"),
            provider: V2Provider::default(),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            global_session_id: Arc::new(RwLock::new(String::new())),
            client_count: Arc::new(RwLock::new(0usize)),
            client_connections: Arc::new(RwLock::new(HashMap::new())),
            swarm_members: Arc::new(RwLock::new(HashMap::new())),
            swarms_by_id: Arc::new(RwLock::new(HashMap::new())),
            shared_context: Arc::new(RwLock::new(HashMap::new())),
            plan_proposals: Arc::new(RwLock::new(HashMap::new())),
            swarm_plans: Arc::new(RwLock::new(HashMap::new())),
            swarm_coordinators: Arc::new(RwLock::new(HashMap::new())),
            file_touch: FileTouchService::new(),
            channel_subscriptions: Arc::new(RwLock::new(HashMap::new())),
            channel_subscriptions_by_session: Arc::new(RwLock::new(HashMap::new())),
            event_history: Arc::new(RwLock::new(std::collections::VecDeque::new())),
            event_counter: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            swarm_event_tx: broadcast::channel(8).0,
            global_event_tx: broadcast::channel(8).0,
            global_is_processing: Arc::new(RwLock::new(false)),
            shutdown_signals: Arc::new(RwLock::new(HashMap::new())),
            soft_interrupt_queues: Arc::new(RwLock::new(HashMap::new())),
            mcp_pool: Arc::new(crate::mcp::SharedMcpPool::from_default_config()),
        }
    }

    fn connect(&self) -> Conn {
        let provider_template: Arc<dyn Provider> = Arc::new(self.provider.clone());
        let (server_stream, client_stream) = crate::transport::Stream::pair().expect("socket pair");
        let task = tokio::spawn(handle_client(
            server_stream,
            Arc::clone(&self.sessions),
            self.global_event_tx.clone(),
            provider_template,
            self.global_is_processing.clone(),
            self.global_session_id.clone(),
            self.client_count.clone(),
            Arc::clone(&self.client_connections),
            self.swarm_members.clone(),
            self.swarms_by_id.clone(),
            self.shared_context.clone(),
            self.plan_proposals.clone(),
            self.swarm_plans.clone(),
            self.swarm_coordinators.clone(),
            self.file_touch.clone(),
            self.channel_subscriptions.clone(),
            self.channel_subscriptions_by_session.clone(),
            self.event_history.clone(),
            self.event_counter.clone(),
            self.swarm_event_tx.clone(),
            "horon-agent-test".to_string(),
            "🧪".to_string(),
            self.mcp_pool.clone(),
            self.shutdown_signals.clone(),
            self.soft_interrupt_queues.clone(),
            AwaitMembersRuntime::default(),
            SwarmMutationRuntime::default(),
            std::time::Duration::ZERO,
        ));
        let (reader, writer) = client_stream.into_split();
        Conn {
            task,
            reader: BufReader::new(reader),
            writer,
        }
    }

    fn work_dir(&self) -> String {
        self.work.path().display().to_string()
    }

    /// A v2 editor connection, attached once its foreground snapshot arrived.
    pub(super) async fn connect_v2(&self, target: Option<&str>, permission_requests: bool) -> Conn {
        let mut conn = self.connect();
        let mut subscribe = json!({
            "type": "subscribe",
            "id": 1,
            "working_dir": self.work_dir(),
            "acp": {
                "client_capabilities": {},
                "permission_requests": permission_requests,
                "protocol_version": 2,
            },
        });
        if let Some(target) = target {
            subscribe["target_session_id"] = json!(target);
        }
        conn.send(subscribe).await;
        conn.until(|event| matches!(event, ServerEvent::ForegroundState { snapshot: true, .. }))
            .await;
        conn
    }

    /// A v1 editor (`acp` without a version) or, with `acp: false`, a plain
    /// client such as the desktop.
    pub(super) async fn connect_other(&self, target: Option<&str>, acp: bool) -> (Conn, Vec<String>) {
        let mut conn = self.connect();
        let mut subscribe = json!({"type": "subscribe", "id": 1, "working_dir": self.work_dir()});
        if acp {
            subscribe["acp"] = json!({"client_capabilities": {}, "permission_requests": false});
        }
        if let Some(target) = target {
            subscribe["target_session_id"] = json!(target);
        }
        conn.send(subscribe).await;
        let lines = conn
            .until_raw(|event| matches!(event, ServerEvent::Done { id: 1 }))
            .await
            .into_iter()
            .map(|(line, _)| line)
            .collect();
        (conn, lines)
    }

    pub(super) async fn only_session(&self) -> String {
        let sessions = self.sessions.read().await;
        assert_eq!(sessions.len(), 1, "exactly one session exists");
        sessions.keys().next().expect("a subscribed session").clone()
    }

    fn live_turn_context(&self) -> crate::server::live_turn::LiveTurnSwarmContext {
        crate::server::live_turn::LiveTurnSwarmContext::new(
            &self.swarm_members,
            &self.swarms_by_id,
            &self.event_history,
            &self.event_counter,
            &self.swarm_event_tx,
        )
    }

    pub(super) async fn close(self, conns: Vec<Conn>) {
        self.sessions.write().await.clear();
        for conn in conns {
            let Conn { task, reader, writer } = conn;
            drop(writer);
            drop(reader);
            tokio::time::timeout(Duration::from_secs(10), task)
                .await
                .expect("the connection ends")
                .expect("the connection task does not panic")
                .expect("the connection ends cleanly");
        }
    }
}

pub(super) fn is_idle(event: &ServerEvent) -> bool {
    matches!(
        event,
        ServerEvent::ForegroundState {
            running: false,
            snapshot: false,
            ..
        }
    )
}

pub(super) fn is_running(event: &ServerEvent) -> bool {
    matches!(
        event,
        ServerEvent::ForegroundState {
            running: true,
            snapshot: false,
            ..
        }
    )
}

pub(super) fn position(events: &[ServerEvent], what: &str, matches: impl Fn(&ServerEvent) -> bool) -> usize {
    events
        .iter()
        .position(matches)
        .unwrap_or_else(|| panic!("{what} is missing from {events:#?}"))
}

fn inserted_id(events: &[ServerEvent], request_id: u64) -> String {
    events
        .iter()
        .find_map(|event| match event {
            ServerEvent::UserMessageInserted { id, message_id } if *id == request_id => Some(message_id.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no insertion for request {request_id} in {events:#?}"))
}

/// (start_message_id, user_content, run_id) of the turn's running state.
pub(super) fn running_state(events: &[ServerEvent]) -> (Option<String>, Option<String>, String) {
    events
        .iter()
        .find_map(|event| match event {
            ServerEvent::ForegroundState {
                running: true,
                snapshot: false,
                start_message_id,
                user_content,
                run_id,
                ..
            } => Some((start_message_id.clone(), user_content.clone(), run_id.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no running state in {events:#?}"))
}

fn stop_reason(event: &ServerEvent) -> Option<String> {
    match event {
        ServerEvent::ForegroundState { stop_reason, .. } => stop_reason.clone(),
        other => panic!("not a foreground state: {other:?}"),
    }
}

/// A request whose reply travels this connection's event queue, so every
/// event queued for the connection before it has been written once it arrives.
async fn barrier(conn: &mut Conn, id: u64) -> Vec<(String, ServerEvent)> {
    conn.send(json!({"type": "withdraw_message", "id": id, "message_request_id": 9_999_999}))
        .await;
    conn.until_raw(|event| matches!(event, ServerEvent::Error { id: reply, .. } if *reply == id))
        .await
}

async fn history(conn: &mut Conn, id: u64) -> (String, Vec<crate::protocol::HistoryMessage>) {
    conn.send(json!({"type": "get_history", "id": id})).await;
    let lines = conn
        .until_raw(|event| matches!(event, ServerEvent::History { id: reply, .. } if *reply == id))
        .await;
    let (line, event) = lines.into_iter().last().expect("the history reply");
    let ServerEvent::History { messages, .. } = event else {
        unreachable!()
    };
    (line, messages)
}

/// IS-1: the History a v2 connection reads acknowledges the version it
/// subscribed with, so an adapter can tell a daemon that serves v2 from one
/// that ignores the Subscribe's `protocol_version`. A v1 or plain client's
/// History keeps its old bytes: the field is absent, not null.
#[tokio::test]
async fn history_acks_v2_protocol_only_for_v2_connections() {
    let h = Harness::new();
    // A new-session Subscribe creates the session without a History; the
    // adapter reads it with GetHistory.
    let mut v2 = h.connect_v2(None, false).await;
    let session_id = h.only_session().await;
    let (line, _) = history(&mut v2, 40).await;
    assert!(
        line.contains("\"acp_protocol_version\":2"),
        "the v2 connection's History is acknowledged: {line}"
    );

    // Attaching as v2 reads the Subscribe's History.
    let mut attach = h.connect();
    attach
        .send(json!({
            "type": "subscribe",
            "id": 1,
            "working_dir": h.work_dir(),
            "target_session_id": session_id.clone(),
            "acp": {"client_capabilities": {}, "permission_requests": false, "protocol_version": 2},
        }))
        .await;
    let attach_lines = attach
        .until_raw(|event| matches!(event, ServerEvent::Done { id: 1 }))
        .await;
    let (attach_history, _) = attach_lines
        .iter()
        .find(|(_, event)| matches!(event, ServerEvent::History { .. }))
        .expect("the attach reads a History");
    assert!(
        attach_history.contains("\"acp_protocol_version\":2"),
        "the v2 attach History is acknowledged: {attach_history}"
    );

    // v1 and plain clients keep their old bytes: the field is absent.
    let (v1, v1_lines) = h.connect_other(Some(&session_id), true).await;
    let (plain, plain_lines) = h.connect_other(Some(&session_id), false).await;
    for line in v1_lines.iter().chain(plain_lines.iter()) {
        assert!(
            !line.contains("acp_protocol_version"),
            "a v1 or plain History carries no ack: {line}"
        );
    }
    h.close(vec![v2, attach, v1, plain]).await;
}

#[tokio::test]
async fn v2_subscriber_gets_insert_before_first_delta() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    editor
        .send(json!({"type": "message", "id": 10, "content": "hello"}))
        .await;
    let events = editor.until(is_idle).await;

    let inserted = position(&events, "the insertion", |event| {
        matches!(event, ServerEvent::UserMessageInserted { id: 10, .. })
    });
    let running = position(&events, "the running state", is_running);
    let first_delta = position(&events, "the first delta", |event| {
        matches!(event, ServerEvent::TextDelta { .. })
    });
    assert!(inserted < running, "insertion precedes the turn state: {events:#?}");
    assert!(
        running < first_delta,
        "the turn state precedes the first delta: {events:#?}"
    );
    let (start_id, user_content, run_id) = running_state(&events);
    assert_eq!(start_id.as_deref(), Some(inserted_id(&events, 10).as_str()));
    assert_eq!(user_content.as_deref(), Some("hello"));
    assert!(!run_id.is_empty(), "a running turn names its run");
    assert_eq!(
        stop_reason(events.last().expect("the idle state")).as_deref(),
        Some("end_turn")
    );
    h.close(vec![editor]).await;
}

#[tokio::test]
async fn insert_id_equals_history_message_id() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    editor
        .send(json!({"type": "message", "id": 10, "content": "remember me"}))
        .await;
    let events = editor.until(is_idle).await;
    let message_id = inserted_id(&events, 10);

    let (_, messages) = history(&mut editor, 20).await;
    let user = messages
        .iter()
        .find(|message| message.role == "user" && message.content == "remember me")
        .unwrap_or_else(|| panic!("the prompt is in history: {messages:#?}"));
    assert_eq!(user.message_id.as_deref(), Some(message_id.as_str()));
    assert!(
        messages
            .iter()
            .filter(|message| message.role == "assistant")
            .all(|message| message.message_id.is_some()),
        "every stored row carries its id for a v2 connection: {messages:#?}"
    );
    h.close(vec![editor]).await;
}

/// The row a turn started with carries `turn_start`, and a user message the
/// running turn took in between its rounds (a soft interrupt) does not, so a
/// v2 replay keeps the turn's ids across it.
#[tokio::test]
async fn history_marks_turn_starts_but_not_soft_interrupts() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    let gate = Arc::new(Semaphore::new(0));
    h.provider.push(Step {
        gate: Some(Arc::clone(&gate)),
        events: vec![
            StreamEvent::ToolUseStart {
                id: "call-si".to_string(),
                name: "bash".to_string(),
            },
            StreamEvent::ToolInputDelta(json!({"command": "true", "intent": "qa"}).to_string()),
            StreamEvent::ToolUseEnd,
            StreamEvent::MessageEnd {
                stop_reason: Some("tool_use".into()),
            },
        ],
    });
    h.provider.push(Step {
        gate: None,
        events: text_events("both done"),
    });
    editor
        .send(json!({"type": "message", "id": 10, "content": "first ask"}))
        .await;
    let started = editor.until(is_running).await;
    let start_id = running_state(&started).0.expect("the turn's start id");
    editor
        .send(json!({"type": "soft_interrupt", "id": 11, "content": "also this"}))
        .await;
    editor
        .until(|event| matches!(event, ServerEvent::Ack { id: 11 } | ServerEvent::Done { id: 11 }))
        .await;
    gate.add_permits(1);
    let events = editor.until(is_idle).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ServerEvent::SoftInterruptInjected { .. })),
        "the interrupt was taken in by the running turn: {events:#?}"
    );

    let (line, messages) = history(&mut editor, 20).await;
    let row = |content: &str| {
        messages
            .iter()
            .find(|message| message.role == "user" && message.content == content)
            .unwrap_or_else(|| panic!("{content:?} is in history: {messages:#?}"))
    };
    let start = row("first ask");
    assert_eq!(start.message_id.as_deref(), Some(start_id.as_str()));
    assert!(start.turn_start, "{line}");
    let interrupt = row("also this");
    assert!(interrupt.message_id.is_some(), "{line}");
    assert!(!interrupt.turn_start, "{line}");
    assert_eq!(
        line.matches("\"turn_start\":true").count(),
        1,
        "only the start row is marked: {line}"
    );
    h.close(vec![editor]).await;
}

#[tokio::test]
async fn insert_waits_for_agent_lock_held_by_peer_turn() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    let session_id = h.only_session().await;
    let (mut desktop, _) = h.connect_other(Some(&session_id), false).await;

    let gate = h.provider.push_gated_text("peer reply");
    desktop
        .send(json!({"type": "message", "id": 50, "content": "peer prompt"}))
        .await;
    // The peer turn is running, so it holds the agent until the gate opens.
    let peer_start = editor.until(is_running).await;
    let (_, peer_content, peer_run) = running_state(&peer_start);
    assert_eq!(peer_content.as_deref(), Some("peer prompt"));

    editor
        .send(json!({"type": "message", "id": 10, "content": "mine"}))
        .await;
    let while_held = barrier(&mut editor, 11).await;
    assert!(
        !while_held
            .iter()
            .any(|(_, event)| matches!(event, ServerEvent::UserMessageInserted { id: 10, .. })),
        "nothing is inserted while the peer turn holds the agent: {while_held:#?}"
    );

    gate.add_permits(1);
    let mut events = editor
        .until(|event| matches!(event, ServerEvent::Done { id: 10 }))
        .await;
    let message_id = inserted_id(&events, 10);
    desktop
        .until(|event| matches!(event, ServerEvent::Done { id: 50 }))
        .await;
    let mine_run = events
        .iter()
        .find_map(|event| match event {
            ServerEvent::ForegroundState {
                running: true,
                snapshot: false,
                start_message_id: Some(start),
                run_id,
                ..
            } if *start == message_id => Some(run_id.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the editor turn reports running: {events:#?}"));
    events.extend(
        editor
            .until(|event| matches!(event, ServerEvent::ForegroundState { running: false, snapshot: false, run_id, .. } if *run_id == mine_run))
            .await,
    );

    // IS-5 across the two turns: the peer turn ends (Done, then idle) before
    // the editor turn that waited for the agent starts, and every update of
    // the editor turn comes between its running and its idle.
    fn state_of(run: &str, running: bool) -> impl Fn(&ServerEvent) -> bool + '_ {
        move |event: &ServerEvent| {
            matches!(event, ServerEvent::ForegroundState { running: state, snapshot: false, run_id, .. }
                if *state == running && run_id == run)
        }
    }
    let peer_done = position(&events, "the peer Done", |event| {
        matches!(event, ServerEvent::Done { id: 50 })
    });
    let peer_idle = position(&events, "the peer idle", state_of(&peer_run, false));
    let inserted = position(&events, "the editor insertion", |event| {
        matches!(event, ServerEvent::UserMessageInserted { id: 10, .. })
    });
    let mine_running = position(&events, "the editor running", state_of(&mine_run, true));
    let mine_text = position(
        &events,
        "the editor reply",
        |event| matches!(event, ServerEvent::TextDelta { text } if text == "finished"),
    );
    let mine_done = position(&events, "the editor Done", |event| {
        matches!(event, ServerEvent::Done { id: 10 })
    });
    let mine_idle = position(&events, "the editor idle", state_of(&mine_run, false));
    assert!(
        peer_done < peer_idle
            && peer_idle < inserted
            && inserted < mine_running
            && mine_running < mine_text
            && mine_text < mine_done
            && mine_done < mine_idle,
        "peer done {peer_done} < peer idle {peer_idle} < inserted {inserted} < running {mine_running} \
         < reply {mine_text} < done {mine_done} < idle {mine_idle}: {events:#?}"
    );
    for run in [&peer_run, &mine_run] {
        assert_eq!(
            events.iter().filter(|event| state_of(run, false)(event)).count(),
            1,
            "one idle for run {run}: {events:#?}"
        );
    }

    let (_, messages) = history(&mut editor, 20).await;
    let index_of = |what: &str, matches: &dyn Fn(&crate::protocol::HistoryMessage) -> bool| {
        messages
            .iter()
            .position(matches)
            .unwrap_or_else(|| panic!("{what} is missing from {messages:#?}"))
    };
    let mine = index_of("the editor prompt", &|message| {
        message.message_id.as_deref() == Some(message_id.as_str())
    });
    let peer_reply = index_of("the peer reply", &|message| {
        message.role == "assistant" && message.content.contains("peer reply")
    });
    assert!(
        peer_reply < mine,
        "the editor prompt is stored only after the peer turn released the agent: {messages:#?}"
    );
    h.close(vec![editor, desktop]).await;
}

#[tokio::test]
async fn v1_and_plain_subscribers_get_no_v2_events_and_no_message_id() {
    let h = Harness::new();
    let (mut v1, mut v1_lines) = h.connect_other(None, true).await;
    let session_id = h.only_session().await;
    let (mut desktop, mut desktop_lines) = h.connect_other(Some(&session_id), false).await;
    // Attaching last makes the v2 editor the session's ACP owner, so the shell
    // exit below is produced at all.
    let mut editor = h.connect_v2(Some(&session_id), false).await;

    h.provider
        .push_tool_call("call-exit", "bash", json!({"command": "exit 3", "intent": "qa"}));
    editor
        .send(json!({"type": "message", "id": 10, "content": "run it"}))
        .await;
    let editor_events = editor.until(is_idle).await;
    assert!(editor_events.iter().any(is_running), "{editor_events:#?}");
    assert!(
        editor_events
            .iter()
            .any(|event| matches!(event, ServerEvent::ToolExit { .. })),
        "the v2 editor gets the exit: {editor_events:#?}"
    );
    let (editor_history, _) = history(&mut editor, 20).await;
    assert!(editor_history.contains("\"message_id\""), "{editor_history}");

    for (conn, lines) in [(&mut v1, &mut v1_lines), (&mut desktop, &mut desktop_lines)] {
        lines.extend(barrier(conn, 30).await.into_iter().map(|(line, _)| line));
        let (history_line, _) = history(conn, 31).await;
        lines.push(history_line);
        let stream = lines.concat();
        assert!(
            stream.contains("\"type\":\"done\""),
            "the turn reached this client: {stream}"
        );
        for forbidden in [
            "\"user_message_inserted\"",
            "\"foreground_state\"",
            "\"tool_exit\"",
            "\"message_id\"",
            "\"turn_start\"",
        ] {
            assert!(!stream.contains(forbidden), "{forbidden} leaked: {stream}");
        }
    }
    h.close(vec![editor, v1, desktop]).await;
}

#[tokio::test]
async fn rejected_busy_start_emits_no_foreground_state() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    let gate = h.provider.push_gated_text("slow answer");
    editor
        .send(json!({"type": "message", "id": 10, "content": "first"}))
        .await;
    let mut events = editor.until(is_running).await;

    editor
        .send(json!({"type": "message", "id": 11, "content": "second"}))
        .await;
    let rejected = editor
        .until(|event| matches!(event, ServerEvent::Error { id: 11, .. }))
        .await;
    assert!(
        !rejected.iter().any(|event| matches!(
            event,
            ServerEvent::ForegroundState { .. } | ServerEvent::UserMessageInserted { id: 11, .. }
        )),
        "a busy rejection reports no turn: {rejected:#?}"
    );
    events.extend(rejected);
    gate.add_permits(1);
    events.extend(editor.until(is_idle).await);

    let first = inserted_id(&events, 10);
    let states: Vec<&ServerEvent> = events
        .iter()
        .filter(|event| matches!(event, ServerEvent::ForegroundState { .. }))
        .collect();
    assert_eq!(states.len(), 2, "one running and one idle state: {states:#?}");
    for state in states {
        let ServerEvent::ForegroundState { start_message_id, .. } = state else {
            unreachable!()
        };
        assert_eq!(start_message_id.as_deref(), Some(first.as_str()));
    }
    h.close(vec![editor]).await;
}

#[tokio::test]
async fn foreground_not_running_is_last_event_after_done() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    editor
        .send(json!({"type": "message", "id": 10, "content": "finish"}))
        .await;
    let events = editor.until(is_idle).await;
    let done = position(&events, "Done", |event| matches!(event, ServerEvent::Done { id: 10 }));
    assert_eq!(
        done + 1,
        events.len() - 1,
        "the idle state directly follows Done: {events:#?}"
    );

    // Only the barrier's own reply and the swarm status bookkeeping (which
    // reports the member ready once the turn task finished) may follow.
    let after = barrier(&mut editor, 20).await;
    assert!(
        after.iter().all(|(_, event)| matches!(
            event,
            ServerEvent::Ack { id: 20 } | ServerEvent::Error { id: 20, .. } | ServerEvent::SwarmStatus { .. }
        )),
        "nothing of the turn follows its idle state: {after:#?}"
    );
    h.close(vec![editor]).await;
}

#[tokio::test]
async fn aborted_turn_reports_cancelled_via_guard() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    let gate = h.provider.push_gated_text("never sent");
    editor
        .send(json!({"type": "message", "id": 10, "content": "stop me"}))
        .await;
    editor.until(is_running).await;

    editor.send(json!({"type": "cancel", "id": 11})).await;
    let events = editor.until(is_idle).await;
    assert_eq!(
        stop_reason(events.last().expect("the idle state")).as_deref(),
        Some("cancelled"),
        "{events:#?}"
    );
    assert!(
        events.iter().any(|event| matches!(event, ServerEvent::Done { id: 10 })),
        "the cancelled request is answered before the idle state: {events:#?}"
    );
    // Nothing of the cancelled turn may follow its idle state, including the
    // cancel reply's own Interrupted and Done.
    let after = barrier(&mut editor, 12).await;
    assert!(
        !after.iter().any(|(_, event)| matches!(
            event,
            ServerEvent::Done { id: 10 } | ServerEvent::Interrupted | ServerEvent::TurnStopped { .. }
        )),
        "the idle state is the cancelled turn's last event: {after:#?}"
    );
    gate.add_permits(1);
    h.close(vec![editor]).await;
}

#[tokio::test]
async fn v1_owner_tool_diff_has_no_v2_fields() {
    let h = Harness::new();
    // The v1 editor is the only ACP connection, so it owns the session's ACP
    // installation and receives the tool diff.
    let (mut v1, _) = h.connect_other(None, true).await;
    let gone = h.work.path().join("gone.txt");
    let before = h.work.path().join("before.txt");
    let after = h.work.path().join("after.txt");
    let added = h.work.path().join("added.txt");
    std::fs::write(&gone, "bye\n").expect("fixture");
    std::fs::write(&before, "old\n").expect("fixture");
    let patch = format!(
        "*** Begin Patch\n*** Delete File: {}\n*** Update File: {}\n*** Move to: {}\n@@\n-old\n+new\n*** Add File: {}\n+hi\n*** End Patch",
        gone.display(),
        before.display(),
        after.display(),
        added.display()
    );
    h.provider
        .push_tool_call("call-patch", "apply_patch", json!({"patch_text": patch}));
    v1.send(json!({"type": "message", "id": 10, "content": "patch"})).await;
    let lines = v1
        .until_raw(|event| matches!(event, ServerEvent::Done { id: 10 }))
        .await;
    let diff_line = lines
        .iter()
        .find(|(_, event)| matches!(event, ServerEvent::ToolDiff { id, .. } if id == "call-patch"))
        .map(|(line, _)| line.clone())
        .unwrap_or_else(|| panic!("the v1 owner receives the diff: {lines:#?}"));
    assert!(!diff_line.contains("\"deleted\""), "{diff_line}");
    assert!(!diff_line.contains("\"old_path\""), "{diff_line}");
    assert!(!diff_line.contains("\"created\""), "{diff_line}");
    assert!(diff_line.contains(&gone.display().to_string()), "{diff_line}");
    assert!(!before.exists() && after.exists() && !gone.exists() && added.exists());
    h.close(vec![v1]).await;
}

#[tokio::test]
async fn peer_turn_reports_foreground_state_with_content() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    let session_id = h.only_session().await;
    let (mut desktop, _) = h.connect_other(Some(&session_id), false).await;

    desktop
        .send(json!({"type": "message", "id": 50, "content": "from the desktop"}))
        .await;
    let events = editor.until(is_idle).await;
    let (start_id, user_content, _) = running_state(&events);
    assert!(start_id.is_some(), "a peer turn names its start message: {events:#?}");
    assert_eq!(user_content.as_deref(), Some("from the desktop"));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ServerEvent::UserMessageInserted { .. })),
        "insertion answers only the connection that sent the prompt: {events:#?}"
    );
    let done = position(&events, "the peer Done", |event| {
        matches!(event, ServerEvent::Done { id: 50 })
    });
    assert!(done < events.len() - 1, "the idle state comes after the peer's Done");
    assert_eq!(stop_reason(events.last().expect("idle")).as_deref(), Some("end_turn"));
    desktop
        .until(|event| matches!(event, ServerEvent::Done { id: 50 }))
        .await;
    h.close(vec![editor, desktop]).await;
}

#[tokio::test]
async fn auto_reminder_turn_has_start_id_but_no_user_content() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    editor
        .send(json!({
            "type": "message",
            "id": 10,
            "content": "",
            "system_reminder": "Continue after reload",
        }))
        .await;
    let events = editor.until(is_idle).await;
    let (start_id, user_content, _) = running_state(&events);
    assert_eq!(start_id.as_deref(), Some(inserted_id(&events, 10).as_str()));
    assert_eq!(
        user_content, None,
        "an [auto] reminder is not a user message: {events:#?}"
    );
    h.close(vec![editor]).await;
}

#[tokio::test]
async fn agent_initiated_live_turn_reports_state() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    let session_id = h.only_session().await;

    let started = crate::server::live_turn::run_live_turn_if_idle(
        &session_id,
        "wake up",
        None,
        &h.sessions,
        h.live_turn_context(),
    )
    .await;
    assert!(started, "the idle live session takes the wake turn");
    let events = editor.until(is_idle).await;
    let (start_id, user_content, _) = running_state(&events);
    assert!(start_id.is_some());
    assert_eq!(user_content.as_deref(), Some("wake up"));
    let done = position(&events, "the wake Done", |event| {
        matches!(event, ServerEvent::Done { id: 0 })
    });
    assert_eq!(
        done + 1,
        events.len() - 1,
        "the idle state follows the wake turn's Done"
    );
    assert_eq!(stop_reason(events.last().expect("idle")).as_deref(), Some("end_turn"));
    h.close(vec![editor]).await;
}

#[tokio::test]
async fn attach_snapshot_reports_awaiting_user_while_permission_pending() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, true).await;
    let session_id = h.only_session().await;
    editor
        .send(json!({"type": "set_session_approval_mode", "id": 2, "mode": "write"}))
        .await;
    editor.until(|event| matches!(event, ServerEvent::Ack { id: 2 })).await;

    let marker = h.work.path().join("approved.marker");
    h.provider.push_tool_call(
        "call-ask",
        "bash",
        json!({"command": format!("touch '{}'", marker.display()), "intent": "qa"}),
    );
    editor
        .send(json!({"type": "message", "id": 10, "content": "ask first"}))
        .await;
    let events = editor
        .until(|event| matches!(event, ServerEvent::ToolPermissionRequested { .. }))
        .await;
    let (_, _, run_id) = running_state(&events);
    let Some(ServerEvent::ToolPermissionRequested { request_id, .. }) = events.last().cloned() else {
        unreachable!()
    };

    let mut second = h.connect();
    second
        .send(json!({
            "type": "subscribe",
            "id": 1,
            "working_dir": h.work_dir(),
            "target_session_id": session_id,
            "acp": {"client_capabilities": {}, "permission_requests": false, "protocol_version": 2},
        }))
        .await;
    let attach = second
        .until(|event| matches!(event, ServerEvent::ForegroundState { snapshot: true, .. }))
        .await;
    let Some(ServerEvent::ForegroundState {
        running,
        awaiting_user,
        run_id: snapshot_run,
        ..
    }) = attach.last().cloned()
    else {
        unreachable!()
    };
    assert!(running, "the attach sees the running turn");
    assert!(awaiting_user, "the attach sees the pending permission");
    assert_eq!(snapshot_run, run_id);

    editor
        .send(json!({"type": "permission_decision", "id": 11, "request_id": request_id, "decision": "allow"}))
        .await;
    let events = editor.until(is_idle).await;
    assert_eq!(stop_reason(events.last().expect("idle")).as_deref(), Some("end_turn"));
    assert!(marker.exists(), "the approved call ran");
    h.close(vec![editor, second]).await;
}

#[tokio::test]
async fn withdraw_before_insert_leaves_peer_turn_running() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    let session_id = h.only_session().await;
    let (mut desktop, _) = h.connect_other(Some(&session_id), false).await;

    let gate = h.provider.push_gated_text("peer reply");
    desktop
        .send(json!({"type": "message", "id": 50, "content": "peer prompt"}))
        .await;
    editor.until(is_running).await;

    editor
        .send(json!({"type": "message", "id": 10, "content": "withdraw me"}))
        .await;
    editor
        .send(json!({"type": "withdraw_message", "id": 11, "message_request_id": 10}))
        .await;
    let withdrawn = editor
        .until(|event| matches!(event, ServerEvent::Done { id: 11 } | ServerEvent::Error { id: 11, .. }))
        .await;
    assert!(
        matches!(withdrawn.last(), Some(ServerEvent::Done { id: 11 })),
        "the queued prompt is withdrawn: {withdrawn:#?}"
    );

    gate.add_permits(1);
    let peer = desktop
        .until(|event| matches!(event, ServerEvent::Done { id: 50 } | ServerEvent::Error { id: 50, .. }))
        .await;
    assert!(
        matches!(peer.last(), Some(ServerEvent::Done { id: 50 })),
        "the peer turn finished normally: {peer:#?}"
    );
    assert!(
        !peer
            .iter()
            .any(|event| matches!(event, ServerEvent::TurnStopped { .. } | ServerEvent::Interrupted)),
        "the withdrawal did not stop the peer turn: {peer:#?}"
    );
    let events = editor.until(is_idle).await;
    assert_eq!(stop_reason(events.last().expect("idle")).as_deref(), Some("end_turn"));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ServerEvent::UserMessageInserted { id: 10, .. })),
        "the withdrawn prompt is never inserted: {events:#?}"
    );

    // The connection is free again and the withdrawn text was never stored.
    editor
        .send(json!({"type": "message", "id": 12, "content": "next"}))
        .await;
    let next = editor.until(is_idle).await;
    inserted_id(&next, 12);
    let (_, messages) = history(&mut editor, 20).await;
    assert!(
        !messages.iter().any(|message| message.content.contains("withdraw me")),
        "{messages:#?}"
    );
    h.close(vec![editor, desktop]).await;
}

#[tokio::test]
async fn withdraw_after_insert_reports_already_inserted() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    let gate = h.provider.push_gated_text("slow");
    editor
        .send(json!({"type": "message", "id": 10, "content": "keep me"}))
        .await;
    editor
        .until(|event| matches!(event, ServerEvent::UserMessageInserted { id: 10, .. }))
        .await;
    editor
        .send(json!({"type": "withdraw_message", "id": 11, "message_request_id": 10}))
        .await;
    let reply = editor
        .until(|event| matches!(event, ServerEvent::Done { id: 11 } | ServerEvent::Error { id: 11, .. }))
        .await;
    assert!(
        matches!(reply.last(), Some(ServerEvent::Error { message, .. }) if message == "already inserted"),
        "{reply:#?}"
    );
    gate.add_permits(1);
    let events = editor.until(is_idle).await;
    assert_eq!(stop_reason(events.last().expect("idle")).as_deref(), Some("end_turn"));
    h.close(vec![editor]).await;
}

#[tokio::test]
async fn bash_exit_code_reaches_v2_tool_exit() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    h.provider
        .push_tool_call("call-exit", "bash", json!({"command": "exit 3", "intent": "qa"}));
    editor
        .send(json!({"type": "message", "id": 10, "content": "fail"}))
        .await;
    let events = editor.until(is_idle).await;
    let exit = position(
        &events,
        "the tool exit",
        |event| matches!(event, ServerEvent::ToolExit { id, .. } if id == "call-exit"),
    );
    let done = position(
        &events,
        "the tool result",
        |event| matches!(event, ServerEvent::ToolDone { id, .. } if id == "call-exit"),
    );
    assert!(exit < done, "the exit precedes the tool result: {events:#?}");
    let ServerEvent::ToolExit {
        exit_code,
        signal,
        backgrounded,
        ..
    } = &events[exit]
    else {
        unreachable!()
    };
    assert_eq!(*exit_code, Some(3));
    assert_eq!(*signal, None);
    assert!(!backgrounded);
    h.close(vec![editor]).await;
}

#[tokio::test]
async fn background_bash_tool_exit_is_backgrounded() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    h.provider.push_tool_call(
        "call-bg",
        "bash",
        json!({"command": "true", "intent": "qa", "run_in_background": true, "notify": false}),
    );
    editor
        .send(json!({"type": "message", "id": 10, "content": "background"}))
        .await;
    let events = editor.until(is_idle).await;
    let exit = events
        .iter()
        .find(|event| matches!(event, ServerEvent::ToolExit { id, .. } if id == "call-bg"))
        .unwrap_or_else(|| panic!("no exit for the background call: {events:#?}"));
    assert!(
        matches!(
            exit,
            ServerEvent::ToolExit {
                exit_code: None,
                signal: None,
                backgrounded: true,
                ..
            }
        ),
        "{exit:?}"
    );
    h.close(vec![editor]).await;
}

#[tokio::test]
async fn apply_patch_delete_and_move_diff_flags() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    let gone = h.work.path().join("gone.txt");
    let before = h.work.path().join("before.txt");
    let after = h.work.path().join("after.txt");
    let added = h.work.path().join("added.txt");
    std::fs::write(&gone, "bye\n").expect("fixture");
    std::fs::write(&before, "old\n").expect("fixture");
    let patch = format!(
        "*** Begin Patch\n*** Delete File: {}\n*** Update File: {}\n*** Move to: {}\n@@\n-old\n+new\n*** Add File: {}\n+hi\n*** End Patch",
        gone.display(),
        before.display(),
        after.display(),
        added.display()
    );
    h.provider
        .push_tool_call("call-patch", "apply_patch", json!({"patch_text": patch}));
    editor
        .send(json!({"type": "message", "id": 10, "content": "patch"}))
        .await;
    let events = editor.until(is_idle).await;
    let diffs = events
        .iter()
        .find_map(|event| match event {
            ServerEvent::ToolDiff { id, diffs } if id == "call-patch" => Some(diffs.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no diff for the patch: {events:#?}"));

    let deleted = diffs
        .iter()
        .find(|diff| diff.path == gone.display().to_string())
        .unwrap_or_else(|| panic!("no entry for the deleted file: {diffs:#?}"));
    assert!(deleted.deleted);
    assert_eq!(deleted.old_text.as_deref(), Some("bye\n"));
    assert_eq!(deleted.new_text, "");
    assert_eq!(deleted.old_path, None);
    assert!(!deleted.created);

    let moved = diffs
        .iter()
        .find(|diff| diff.path == after.display().to_string())
        .unwrap_or_else(|| panic!("no entry for the moved file: {diffs:#?}"));
    assert!(!moved.deleted && !moved.created);
    assert_eq!(moved.old_path.as_deref(), Some(before.display().to_string().as_str()));
    assert_eq!(moved.old_text.as_deref(), Some("old\n"));
    assert_eq!(moved.new_text, "new\n");

    let created = diffs
        .iter()
        .find(|diff| diff.path == added.display().to_string())
        .unwrap_or_else(|| panic!("no entry for the added file: {diffs:#?}"));
    assert!(created.created && !created.deleted);
    assert_eq!(created.old_path, None);
    assert_eq!(created.new_text, "hi\n");
    assert!(!before.exists() && after.exists() && !gone.exists() && added.exists());
    h.close(vec![editor]).await;
}

#[tokio::test]
async fn cancel_of_queued_v2_prompt_withdraws_it() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    let session_id = h.only_session().await;
    let mut observer = h.connect_v2(Some(&session_id), false).await;
    let (mut desktop, _) = h.connect_other(Some(&session_id), false).await;

    let gate = h.provider.push_gated_text("peer reply");
    desktop
        .send(json!({"type": "message", "id": 50, "content": "peer prompt"}))
        .await;
    observer.until(is_running).await;
    editor.until(is_running).await;

    // Prompt 10 waits for the agent the peer turn holds; then it is cancelled.
    editor
        .send(json!({"type": "message", "id": 10, "content": "mine"}))
        .await;
    barrier(&mut editor, 11).await;
    editor.send(json!({"type": "cancel", "id": 12})).await;
    let reply = editor
        .until(|event| matches!(event, ServerEvent::Done { id: 10 }))
        .await;
    assert!(
        reply.iter().any(|event| matches!(event, ServerEvent::Interrupted)),
        "the cancel answers the prompt as interrupted: {reply:#?}"
    );
    assert!(
        !reply.iter().any(is_idle),
        "the cancel reply comes before any idle state: {reply:#?}"
    );

    gate.add_permits(1);
    let mut rest = editor.until(is_idle).await;
    rest.extend(barrier(&mut editor, 14).await.into_iter().map(|(_, event)| event));
    let idle_at = position(&rest, "the peer's idle state", is_idle);
    assert!(
        !rest[idle_at + 1..].iter().any(|event| matches!(
            event,
            ServerEvent::Done { id: 10 } | ServerEvent::Interrupted | ServerEvent::UserMessageInserted { id: 10, .. }
        )),
        "nothing of request 10 follows an idle state: {rest:#?}"
    );
    let everything: Vec<&ServerEvent> = reply.iter().chain(rest.iter()).collect();
    assert!(
        !everything
            .iter()
            .any(|event| matches!(event, ServerEvent::UserMessageInserted { id: 10, .. })),
        "the cancelled prompt is never stored: {everything:#?}"
    );
    assert!(
        !everything.iter().any(|event| matches!(
            event,
            ServerEvent::ForegroundState { running: true, user_content: Some(content), .. } if content == "mine"
        )),
        "the cancelled prompt never runs: {everything:#?}"
    );
    desktop
        .until(|event| matches!(event, ServerEvent::Done { id: 50 } | ServerEvent::Error { id: 50, .. }))
        .await;

    let (_, messages) = history(&mut editor, 20).await;
    assert!(
        !messages.iter().any(|message| message.content.contains("mine")),
        "the cancelled prompt is not in history: {messages:#?}"
    );
    h.close(vec![editor, observer, desktop]).await;
}

/// The cancel holds the cancelled turn's idle state until its own reply is
/// queued, so the waiting turn's `running` and text cannot overtake it.
#[tokio::test]
async fn cancelled_idle_precedes_the_queued_peer_turn() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    let session_id = h.only_session().await;
    let mut desktop = h.connect_v2(Some(&session_id), false).await;

    let gate = h.provider.push_gated_text("never sent");
    h.provider.push(Step {
        gate: None,
        events: text_events("peer reply"),
    });
    editor
        .send(json!({"type": "message", "id": 10, "content": "stop me"}))
        .await;
    let (_, _, own_run) = running_state(&editor.until(is_running).await);
    desktop
        .send(json!({"type": "message", "id": 50, "content": "peer prompt"}))
        .await;
    barrier(&mut desktop, 51).await;

    editor.send(json!({"type": "cancel", "id": 11})).await;
    let is_peer_idle = |event: &ServerEvent| matches!(event, ServerEvent::ForegroundState { running: false, snapshot: false, run_id, .. } if *run_id != own_run);
    let mut events = editor.until(is_peer_idle).await;
    desktop
        .until(|event| matches!(event, ServerEvent::Done { id: 50 }))
        .await;
    events.extend(barrier(&mut editor, 12).await.into_iter().map(|(_, event)| event));

    let own_idle = position(
        &events,
        "the cancelled turn's idle state",
        |event| matches!(event, ServerEvent::ForegroundState { running: false, snapshot: false, run_id, .. } if *run_id == own_run),
    );
    assert_eq!(
        stop_reason(&events[own_idle]).as_deref(),
        Some("cancelled"),
        "{events:#?}"
    );
    let peer_running = position(
        &events,
        "the peer turn's running state",
        |event| matches!(event, ServerEvent::ForegroundState { running: true, snapshot: false, run_id, .. } if *run_id != own_run),
    );
    let (_, _, peer_run) = running_state(&events[peer_running..]);
    let peer_text = position(
        &events,
        "the peer reply",
        |event| matches!(event, ServerEvent::TextDelta { text } if text == "peer reply"),
    );
    let peer_idle = position(&events, "the peer turn's idle state", is_peer_idle);
    assert!(
        own_idle < peer_running && peer_running < peer_text && peer_text < peer_idle,
        "own idle {own_idle} < peer running {peer_running} < peer text {peer_text} < peer idle {peer_idle}: {events:#?}"
    );
    for run in [&own_run, &peer_run] {
        let idles = events
            .iter()
            .filter(|event| {
                matches!(event, ServerEvent::ForegroundState { running: false, snapshot: false, run_id, .. } if run_id == run)
            })
            .count();
        assert_eq!(idles, 1, "exactly one idle state for run {run}: {events:#?}");
    }
    gate.add_permits(1);
    h.close(vec![editor, desktop]).await;
}

/// A prompt sent the instant the previous turn's terminal event arrives must
/// not meet "Already processing a message". The request loop learns the turn
/// finished before the turn task waits for its idle state to be forwarded; the
/// next turn's task still waits for the agent the previous one keeps.
#[tokio::test]
async fn prompt_sent_right_after_done_is_not_busy() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    let mut events = Vec::new();
    for id in 10..16 {
        editor
            .send(json!({"type": "message", "id": id, "content": format!("turn {id}")}))
            .await;
        events.extend(
            editor
                .until(|event| matches!(event, ServerEvent::Done { id: done } if *done == id))
                .await,
        );
    }
    assert!(
        !events.iter().any(|event| matches!(
            event,
            ServerEvent::Error { message, .. } if message.contains("Already processing")
        )),
        "a prompt sent right after Done is accepted: {events:#?}"
    );
    for id in 10..16 {
        inserted_id(&events, id);
    }
    h.close(vec![editor]).await;
}

/// The cancel of a prompt still waiting for the agent another surface's turn
/// holds aborts that prompt at once instead of waiting out the cooperative
/// window: the waiting task cannot end while the cancel parks the peer's lock.
#[tokio::test]
async fn cancel_of_a_queued_prompt_does_not_wait_for_the_peer_turn() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    let session_id = h.only_session().await;
    let (mut desktop, _) = h.connect_other(Some(&session_id), false).await;

    let gate = h.provider.push_gated_text("peer reply");
    desktop
        .send(json!({"type": "message", "id": 50, "content": "peer prompt"}))
        .await;
    editor.until(is_running).await;

    // Prompt 10 waits for the agent the peer turn holds; then it is cancelled.
    editor
        .send(json!({"type": "message", "id": 10, "content": "mine"}))
        .await;
    barrier(&mut editor, 11).await;

    let sent = Instant::now();
    editor.send(json!({"type": "cancel", "id": 12})).await;
    let reply = editor
        .until(|event| matches!(event, ServerEvent::Done { id: 10 }))
        .await;
    let elapsed = sent.elapsed();
    assert!(
        reply.iter().any(|event| matches!(event, ServerEvent::Interrupted)),
        "the cancel answers the queued prompt as interrupted: {reply:#?}"
    );
    assert!(
        elapsed < Duration::from_millis(400),
        "the cancel of a queued prompt replies at once instead of burning the \
         cooperative window ({elapsed:?}): {reply:#?}"
    );
    assert!(
        !reply.iter().any(|event| matches!(
            event,
            ServerEvent::UserMessageInserted { id: 10, .. } | ServerEvent::ForegroundState { .. }
        )),
        "the cancelled prompt never starts a turn: {reply:#?}"
    );

    gate.add_permits(1);
    desktop
        .until(|event| matches!(event, ServerEvent::Done { id: 50 } | ServerEvent::Error { id: 50, .. }))
        .await;
    h.close(vec![editor, desktop]).await;
}

/// Text the user did not type names no user message: a whitespace-only prompt
/// (like a continuation without text) must not reach a v2 editor as
/// `user_message { text: "" }`.
#[tokio::test]
async fn whitespace_only_prompt_reports_no_user_content() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    editor
        .send(json!({"type": "message", "id": 10, "content": "   "}))
        .await;
    let events = editor.until(is_idle).await;
    let (start_id, user_content, _) = running_state(&events);
    assert!(start_id.is_some(), "the turn still starts: {events:#?}");
    assert_eq!(
        user_content, None,
        "a whitespace-only prompt is not a user message: {events:#?}"
    );
    h.close(vec![editor]).await;
}

/// A withdrawal wins only while the prompt waits for an agent another
/// surface's turn holds, so the session is still running: the withdrawn
/// connection must not report its member ready over that turn.
#[tokio::test]
async fn withdraw_does_not_report_the_session_ready_over_a_peer_turn() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    let session_id = h.only_session().await;
    let (mut desktop, _) = h.connect_other(Some(&session_id), false).await;

    let gate = h.provider.push_gated_text("peer reply");
    desktop
        .send(json!({"type": "message", "id": 50, "content": "peer prompt"}))
        .await;
    editor.until(is_running).await;

    editor
        .send(json!({"type": "message", "id": 10, "content": "withdraw me"}))
        .await;
    barrier(&mut editor, 11).await;
    editor
        .send(json!({"type": "withdraw_message", "id": 12, "message_request_id": 10}))
        .await;
    let withdrawn = editor
        .until(|event| matches!(event, ServerEvent::Done { id: 12 } | ServerEvent::Error { id: 12, .. }))
        .await;
    assert!(
        matches!(withdrawn.last(), Some(ServerEvent::Done { id: 12 })),
        "the queued prompt is withdrawn: {withdrawn:#?}"
    );
    let status = {
        let members = h.swarm_members.read().await;
        members
            .get(&session_id)
            .map(|member| member.status.clone())
            .unwrap_or_else(|| panic!("the session is a swarm member"))
    };
    assert_eq!(status, "running", "the peer turn is still running the session");

    gate.add_permits(1);
    desktop
        .until(|event| matches!(event, ServerEvent::Done { id: 50 } | ServerEvent::Error { id: 50, .. }))
        .await;
    h.close(vec![editor, desktop]).await;
}
