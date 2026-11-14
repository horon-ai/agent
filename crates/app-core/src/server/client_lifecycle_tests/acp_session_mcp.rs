use super::*;
use crate::protocol::SessionMcpServer;
#[cfg(unix)]
use crate::tool::session_mcp::tests::{context, watch_fifo};
use crate::tool::session_mcp::tests::{pid_alive, python3_unavailable, read_pid, stdio_server};
use serde_json::json;

#[derive(Clone)]
struct IdleProvider;

#[async_trait]
impl Provider for IdleProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        anyhow::bail!("no turn runs in this test")
    }

    fn name(&self) -> &str {
        "idle"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

async fn send(writer: &mut crate::transport::WriteHalf, value: serde_json::Value) {
    writer.write_all(format!("{value}\n").as_bytes()).await.unwrap();
}

async fn configure(writer: &mut crate::transport::WriteHalf, id: u64, servers: Vec<SessionMcpServer>) {
    let request = Request::ConfigureSessionMcp { id, servers };
    send(writer, serde_json::to_value(&request).unwrap()).await;
}

async fn next_matching(
    reader: &mut BufReader<crate::transport::ReadHalf>,
    stop: impl Fn(&ServerEvent) -> bool,
) -> ServerEvent {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let mut line = String::new();
            assert!(
                reader.read_line(&mut line).await.unwrap() > 0,
                "the daemon closed the stream"
            );
            let event: ServerEvent = serde_json::from_str(&line).unwrap();
            if stop(&event) {
                return event;
            }
        }
    })
    .await
    .expect("daemon event timeout")
}

async fn reply(reader: &mut BufReader<crate::transport::ReadHalf>, id: u64) -> ServerEvent {
    next_matching(reader, |event| {
        matches!(event, ServerEvent::Done { id: done } if *done == id)
            || matches!(event, ServerEvent::Error { id: failed, .. } if *failed == id)
    })
    .await
}

async fn connected(reader: &mut BufReader<crate::transport::ReadHalf>, server: &str) {
    let expected = vec![format!("{server}:1")];
    next_matching(
        reader,
        |event| matches!(event, ServerEvent::McpStatus { servers } if *servers == expected),
    )
    .await;
}

/// MCP tool names a turn started by `connection` sees on `agent`'s session,
/// or, with no connection, what a call outside any turn sees.
async fn mcp_tools_for(agent: &Arc<Mutex<Agent>>, session: &str, connection: Option<&str>) -> Vec<String> {
    let _turn = connection.map(|connection| crate::tool::acp_session::claim_turn(session, Some(connection)));
    mcp_tools(agent).await
}

async fn mcp_tools(agent: &Arc<Mutex<Agent>>) -> Vec<String> {
    let mut names: Vec<String> = agent
        .lock()
        .await
        .registry()
        .tool_names()
        .await
        .into_iter()
        .filter(|name| name.starts_with("mcp__"))
        .collect();
    names.sort();
    names
}

/// The connection registry the daemon hands back, shared by every connection it opens.
type ConnectionMap = Arc<RwLock<HashMap<String, ClientConnectionInfo>>>;

/// One opened connection: its server task and the client half of the socket pair.
type OpenedConnection = (tokio::task::JoinHandle<Result<()>>, crate::transport::Stream);

/// The connection loop under test: `connect()` opens one client connection
/// to a fresh daemon state shared by every connection it opens.
fn daemon() -> (SessionAgents, ConnectionMap, impl Fn() -> OpenedConnection) {
    let provider_template: Arc<dyn Provider> = Arc::new(IdleProvider);

    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::new()));
    let global_session_id = Arc::new(RwLock::new(String::new()));
    let client_count = Arc::new(RwLock::new(0usize));
    let client_connections = Arc::new(RwLock::new(HashMap::new()));
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let shared_context = Arc::new(RwLock::new(HashMap::new()));
    let plan_proposals = Arc::new(RwLock::new(HashMap::new()));
    let swarm_plans = Arc::new(RwLock::new(HashMap::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::new()));
    let file_touch = FileTouchService::new();
    let channel_subscriptions = Arc::new(RwLock::new(HashMap::new()));
    let channel_subscriptions_by_session = Arc::new(RwLock::new(HashMap::new()));
    let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _) = broadcast::channel(8);
    let (global_event_tx, _) = broadcast::channel(8);
    let global_is_processing = Arc::new(RwLock::new(false));
    let shutdown_signals = Arc::new(RwLock::new(HashMap::new()));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let mcp_pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());

    let (outer_sessions, outer_connections) = (Arc::clone(&sessions), Arc::clone(&client_connections));
    let connect = move || {
        let (server_stream, client_stream) = crate::transport::Stream::pair().expect("socket pair");
        let server_task = tokio::spawn(handle_client(
            server_stream,
            Arc::clone(&sessions),
            global_event_tx.clone(),
            provider_template.clone(),
            global_is_processing.clone(),
            global_session_id.clone(),
            client_count.clone(),
            Arc::clone(&client_connections),
            swarm_members.clone(),
            swarms_by_id.clone(),
            shared_context.clone(),
            plan_proposals.clone(),
            swarm_plans.clone(),
            swarm_coordinators.clone(),
            file_touch.clone(),
            channel_subscriptions.clone(),
            channel_subscriptions_by_session.clone(),
            event_history.clone(),
            event_counter.clone(),
            swarm_event_tx.clone(),
            "horon-agent-test".to_string(),
            "🧪".to_string(),
            mcp_pool.clone(),
            shutdown_signals.clone(),
            soft_interrupt_queues.clone(),
            AwaitMembersRuntime::default(),
            SwarmMutationRuntime::default(),
            std::time::Duration::ZERO,
        ));
        (server_task, client_stream)
    };
    (outer_sessions, outer_connections, connect)
}

/// Real connection loop: an ACP connection's servers register on its own
/// session only, a new list or a re-subscribe stops the previous servers, a
/// plain connection is refused, a bad list leaves the running servers alone,
/// and the last server process exits when the editor disconnects.
#[tokio::test]
async fn session_mcp_child_exits_on_disconnect() {
    if python3_unavailable("session_mcp_child_exits_on_disconnect") {
        return;
    }
    let _lock = crate::storage::lock_test_env();
    let home = IsolatedReloadRecoveryEnv::new();
    crate::config::invalidate_config_cache();
    let work = home._home.path().to_path_buf();
    let (sessions, connections, connect) = daemon();
    let acp_subscribe = |id: u64| {
        json!({
            "type": "subscribe",
            "id": id,
            "working_dir": work,
            "acp": {"client_capabilities": {}, "permission_requests": false},
        })
    };

    let (acp_task, acp_stream) = connect();
    let (acp_reader, mut acp) = acp_stream.into_split();
    let mut acp_reader = BufReader::new(acp_reader);
    send(&mut acp, acp_subscribe(1)).await;
    assert!(matches!(reply(&mut acp_reader, 1).await, ServerEvent::Done { .. }));
    let acp_session_id = sessions.read().await.keys().next().expect("the ACP session").clone();
    let acp_agent = Arc::clone(&sessions.read().await[&acp_session_id]);
    let acp_connection = connections
        .read()
        .await
        .keys()
        .next()
        .expect("the ACP connection")
        .clone();
    let acp_turn = Some(acp_connection.as_str());

    let first_pid = work.join("first.pid");
    configure(&mut acp, 2, vec![stdio_server("first", &first_pid, "serve")]).await;
    let configured = reply(&mut acp_reader, 2).await;
    assert!(matches!(configured, ServerEvent::Done { .. }), "{configured:?}");
    connected(&mut acp_reader, "first").await;
    let first = read_pid(&first_pid);
    assert!(pid_alive(first));
    assert_eq!(
        mcp_tools_for(&acp_agent, &acp_session_id, acp_turn).await,
        [crate::mcp::dispatch_name("first", "echo")]
    );
    assert!(
        mcp_tools(&acp_agent).await.is_empty(),
        "outside the editor's own turn its tools are invisible"
    );

    // A plain connection gets no editor servers and cannot add any.
    let (plain_task, plain_stream) = connect();
    let (plain_reader, mut plain) = plain_stream.into_split();
    let mut plain_reader = BufReader::new(plain_reader);
    send(&mut plain, json!({"type": "subscribe", "id": 50, "working_dir": work})).await;
    assert!(matches!(reply(&mut plain_reader, 50).await, ServerEvent::Done { .. }));
    let plain_agent = {
        let sessions = sessions.read().await;
        let (_, agent) = sessions
            .iter()
            .find(|(id, _)| **id != acp_session_id)
            .expect("the plain session");
        Arc::clone(agent)
    };
    assert!(
        mcp_tools(&plain_agent).await.is_empty(),
        "another session sees no editor tool"
    );
    let plain_pid = work.join("plain.pid");
    configure(&mut plain, 51, vec![stdio_server("plain", &plain_pid, "serve")]).await;
    let refused = reply(&mut plain_reader, 51).await;
    assert!(
        matches!(refused, ServerEvent::Error { ref message, .. } if message == crate::tool::approval_override::NOT_ACP_MESSAGE),
        "{refused:?}"
    );
    assert!(!plain_pid.exists(), "a refused list starts nothing");

    // A bad list is refused whole and the running servers stay.
    let unusable = SessionMcpServer {
        command: None,
        ..stdio_server("broken", &work.join("broken.pid"), "serve")
    };
    configure(&mut acp, 3, vec![unusable]).await;
    let refused = reply(&mut acp_reader, 3).await;
    assert!(
        matches!(refused, ServerEvent::Error { ref message, .. } if message.contains("needs a command")),
        "{refused:?}"
    );
    assert!(pid_alive(first), "a refused list leaves the running server alone");

    // A new list replaces the old one.
    let second_pid = work.join("second.pid");
    configure(&mut acp, 4, vec![stdio_server("second", &second_pid, "serve")]).await;
    assert!(matches!(reply(&mut acp_reader, 4).await, ServerEvent::Done { .. }));
    assert!(!pid_alive(first), "the replaced server stopped before the reply");
    connected(&mut acp_reader, "second").await;
    let second = read_pid(&second_pid);
    assert_eq!(
        mcp_tools_for(&acp_agent, &acp_session_id, acp_turn).await,
        [crate::mcp::dispatch_name("second", "echo")]
    );

    // A re-subscribe starts a new attachment without the old servers.
    send(&mut acp, acp_subscribe(5)).await;
    assert!(matches!(reply(&mut acp_reader, 5).await, ServerEvent::Done { .. }));
    assert!(!pid_alive(second), "a re-subscribe stops the previous servers");
    assert!(mcp_tools_for(&acp_agent, &acp_session_id, acp_turn).await.is_empty());

    let third_pid = work.join("third.pid");
    configure(&mut acp, 6, vec![stdio_server("third", &third_pid, "serve")]).await;
    assert!(matches!(reply(&mut acp_reader, 6).await, ServerEvent::Done { .. }));
    connected(&mut acp_reader, "third").await;
    let third = read_pid(&third_pid);
    assert!(pid_alive(third));

    sessions.write().await.clear();
    drop(acp);
    drop(acp_reader);
    tokio::time::timeout(Duration::from_secs(10), acp_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        !pid_alive(third),
        "the server process exits when the editor disconnects"
    );
    assert!(
        mcp_tools_for(&acp_agent, &acp_session_id, acp_turn).await.is_empty(),
        "its tools leave the session"
    );

    drop(plain);
    drop(plain_reader);
    tokio::time::timeout(Duration::from_secs(10), plain_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

/// A disconnect stops an editor server whose tool call is still in flight.
/// The call holds its proxy, and with it the server's manager, so the server
/// exits only because the connection's cleanup disconnects it, and that
/// cleanup must not wait behind the unanswered call.
#[cfg(unix)]
#[tokio::test]
async fn session_mcp_disconnect_stops_server_with_call_in_flight() {
    if python3_unavailable("session_mcp_disconnect_stops_server_with_call_in_flight") {
        return;
    }
    let _lock = crate::storage::lock_test_env();
    let home = IsolatedReloadRecoveryEnv::new();
    crate::config::invalidate_config_cache();
    let work = home._home.path().to_path_buf();
    let (sessions, connections, connect) = daemon();

    let (acp_task, acp_stream) = connect();
    let (acp_reader, mut acp) = acp_stream.into_split();
    let mut acp_reader = BufReader::new(acp_reader);
    send(
        &mut acp,
        json!({
            "type": "subscribe",
            "id": 1,
            "working_dir": work,
            "acp": {"client_capabilities": {}, "permission_requests": false},
        }),
    )
    .await;
    assert!(matches!(reply(&mut acp_reader, 1).await, ServerEvent::Done { .. }));
    let session_id = sessions.read().await.keys().next().expect("the ACP session").clone();
    let connection_id = connections
        .read()
        .await
        .keys()
        .next()
        .expect("the ACP connection")
        .clone();
    let registry = sessions.read().await[&session_id].lock().await.registry();

    let fifo = work.join("slow.pid");
    let (pid, mut lines, exited) = watch_fifo(&fifo);
    configure(&mut acp, 2, vec![stdio_server("slow", &fifo, "block")]).await;
    assert!(matches!(reply(&mut acp_reader, 2).await, ServerEvent::Done { .. }));
    connected(&mut acp_reader, "slow").await;
    let pid = tokio::time::timeout(Duration::from_secs(20), pid)
        .await
        .expect("the stub started")
        .expect("the FIFO reader sent its PID");

    // The editor's own turn calls the tool; the server never answers.
    let turn = crate::tool::acp_session::claim_turn(&session_id, Some(&connection_id));
    assert!(turn.is_owning());
    let alias = crate::mcp::dispatch_name("slow", "echo");
    let call_session = session_id.clone();
    let call = tokio::spawn(async move { registry.execute(&alias, json!({}), context(&call_session)).await });
    let called = tokio::time::timeout(Duration::from_secs(20), lines.recv())
        .await
        .expect("the call reached the server");
    assert_eq!(called.as_deref(), Some("called"));
    assert!(pid_alive(pid));

    sessions.write().await.clear();
    drop(acp);
    drop(acp_reader);
    tokio::time::timeout(Duration::from_secs(10), acp_task)
        .await
        .expect("the disconnect cleanup does not wait behind the call")
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), exited)
        .await
        .expect("the disconnect stops the server with its call in flight")
        .expect("the FIFO reader saw the stub exit");
    let outcome = tokio::time::timeout(Duration::from_secs(10), call)
        .await
        .expect("the in-flight call ends with its server")
        .expect("the call task did not panic");
    assert!(outcome.is_err(), "the unanswered call fails: {outcome:?}");
    drop(turn);
}
