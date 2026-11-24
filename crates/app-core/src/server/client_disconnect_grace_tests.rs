#![allow(clippy::await_holding_lock)]

use super::*;
use crate::protocol::ServerEvent;
use crate::provider::{EventStream, Provider};
use crate::session::{Session, SessionStatus};
use crate::tool::Registry;
use async_trait::async_trait;
use std::time::Instant;
use tokio::time::timeout;

struct NoRequests;
#[async_trait]
impl Provider for NoRequests {
    async fn complete(
        &self,
        _: &[crate::message::Message],
        _: &[crate::message::ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        anyhow::bail!("disconnect tests must not request a provider")
    }
    fn name(&self) -> &str {
        "mock"
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

struct Home {
    _dir: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
}
impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", dir.path());
        Self { _dir: dir, previous }
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
    }
}

struct Fixture {
    id: String,
    agent: Arc<Mutex<Agent>>,
    sessions: SessionAgents,
    members: Arc<RwLock<HashMap<String, SwarmMember>>>,
    connections: Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    events: mpsc::UnboundedSender<ServerEvent>,
}
impl Fixture {
    async fn new(persisted: bool) -> Self {
        let provider: Arc<dyn Provider> = Arc::new(NoRequests);
        let registry = Registry::new(provider.clone()).await;
        let mut session = Session::create(None, None);
        if persisted {
            session.title = Some("Explicit saved panel".into());
            session.add_message(
                crate::message::Role::User,
                vec![crate::message::ContentBlock::Text {
                    text: "saved conversation".into(),
                    cache_control: None,
                }],
            );
            session.save().unwrap();
        }
        let id = session.id.clone();
        let mut agent = Agent::new_with_session(provider, registry, session, None);
        agent.set_memory_enabled(false);
        let agent = Arc::new(Mutex::new(agent));
        let (events, _) = mpsc::unbounded_channel();
        let members = Arc::new(RwLock::new(HashMap::from([(
            id.clone(),
            SwarmMember {
                session_id: id.clone(),
                event_tx: events.clone(),
                event_txs: HashMap::from([("original".into(), events.clone())]),
                working_dir: None,
                swarm_id: None,
                swarm_enabled: false,
                status: "ready".into(),
                detail: None,
                task_label: None,
                friendly_name: None,
                report_back_to_session_id: None,
                latest_completion_report: None,
                role: "agent".into(),
                joined_at: Instant::now(),
                last_status_change: Instant::now(),
                is_headless: false,
                output_tail: None,
                todo_progress: None,
                todo_items: Vec::new(),
                runtime: Default::default(),
            },
        )])));
        let sessions = Arc::new(RwLock::new(HashMap::from([(id.clone(), agent.clone())])));
        let connections = Arc::new(RwLock::new(HashMap::from([(
            "original".into(),
            connection("original", &id),
        )])));
        Self {
            id,
            agent,
            sessions,
            members,
            connections,
            events,
        }
    }

    async fn cleanup(&self, processing: bool, grace: Duration) {
        assert!(!self.cleanup_task(processing, grace, &mut None).await);
    }

    async fn cleanup_task(
        &self,
        processing: bool,
        grace: Duration,
        task: &mut Option<tokio::task::JoinHandle<()>>,
    ) -> bool {
        let (swarm_events, _) = broadcast::channel(8);
        cleanup_client_connection(
            &self.sessions,
            &self.id,
            processing,
            task,
            &self.members,
            &Arc::new(RwLock::new(HashMap::new())),
            &Arc::new(RwLock::new(HashMap::new())),
            &Arc::new(RwLock::new(HashMap::new())),
            &FileTouchService::new(),
            &self.connections,
            "original",
            &Arc::new(RwLock::new(HashMap::new())),
            &Arc::new(RwLock::new(HashMap::new())),
            &Arc::new(RwLock::new(std::collections::VecDeque::new())),
            &Arc::new(std::sync::atomic::AtomicU64::new(0)),
            &swarm_events,
            &self.events,
            grace,
        )
        .await
        .unwrap()
    }

    async fn wait_for_detach(&self) {
        timeout(Duration::from_secs(1), async {
            while self.connections.read().await.contains_key("original") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cleanup releases attachment registry promptly");
    }

    async fn attach_successor(&self) {
        // Reserve the same live Agent under the same registry lock order used
        // by claim_live_target_agent, then register the real event attachment.
        let mut connections = self.connections.write().await;
        assert!(Arc::ptr_eq(
            self.sessions.read().await.get(&self.id).unwrap(),
            &self.agent
        ));
        connections.insert("successor".into(), connection("successor", &self.id));
        drop(connections);
        let (sender, _) = mpsc::unbounded_channel();
        crate::server::register_session_event_sender(&self.members, &self.id, "successor", sender).await;
    }
}

fn connection(name: &str, id: &str) -> ClientConnectionInfo {
    let (disconnect_tx, _) = mpsc::unbounded_channel();
    ClientConnectionInfo {
        client_id: name.into(),
        session_id: id.into(),
        client_instance_id: None,
        connected_at: Instant::now(),
        last_seen: Instant::now(),
        is_processing: false,
        current_tool_name: None,
        terminal_env: vec![],
        disconnect_tx,
    }
}

#[tokio::test]
async fn unsaved_idle_session_retains_same_agent_for_reattachment() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let fixture = Fixture::new(false).await;
    let ((), ()) = tokio::join!(fixture.cleanup(false, Duration::from_secs(2)), async {
        fixture.wait_for_detach().await;
        assert!(!crate::session::session_exists(&fixture.id));
        // These locks must remain available during the reconnect grace.
        let _agent = fixture.agent.try_lock().expect("grace cannot hold agent lock");
        drop(_agent);
        fixture.attach_successor().await;
    });
    assert!(Arc::ptr_eq(
        fixture.sessions.read().await.get(&fixture.id).unwrap(),
        &fixture.agent
    ));
    assert!(fixture.connections.read().await.contains_key("successor"));
    assert!(fixture.members.read().await.contains_key(&fixture.id));
    assert!(!crate::session::session_exists(&fixture.id));
}

#[tokio::test]
async fn unsaved_idle_session_expires_without_persisting_or_leaking() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let fixture = Fixture::new(false).await;
    let grace = Duration::from_millis(60);
    let start = Instant::now();
    timeout(Duration::from_secs(1), fixture.cleanup(false, grace))
        .await
        .unwrap();
    assert!(start.elapsed() >= grace);
    assert!(!fixture.sessions.read().await.contains_key(&fixture.id));
    assert!(!fixture.members.read().await.contains_key(&fixture.id));
    assert!(fixture.connections.read().await.is_empty());
    assert!(!crate::session::session_exists(&fixture.id));
}

#[tokio::test]
async fn unsaved_idle_session_without_swarm_member_still_expires() {
    // A client that disconnects before subscribe registers a swarm member
    // (window closed mid-subscribe) must not be mistaken for a replaced
    // attachment. Previously this leaked the Agent for the server lifetime.
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let fixture = Fixture::new(false).await;
    fixture.members.write().await.clear();
    timeout(
        Duration::from_secs(1),
        fixture.cleanup(false, Duration::from_millis(60)),
    )
    .await
    .unwrap();
    assert!(!fixture.sessions.read().await.contains_key(&fixture.id));
    assert!(fixture.connections.read().await.is_empty());
}

#[tokio::test]
async fn old_grace_cannot_remove_successor_that_already_detached_again() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let fixture = Fixture::new(false).await;
    tokio::join!(fixture.cleanup(false, Duration::from_millis(100)), async {
        fixture.wait_for_detach().await;
        fixture.attach_successor().await;
        detach_client_attachment(&fixture.id, "successor", &fixture.connections, &fixture.members).await;
    });
    assert!(fixture.connections.read().await.is_empty());
    assert!(
        fixture.sessions.read().await.contains_key(&fixture.id),
        "successor owns its own grace/cleanup"
    );
    assert!(fixture.members.read().await.contains_key(&fixture.id));
}

#[tokio::test]
async fn saved_idle_session_stays_live_for_reconnect_grace() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let fixture = Fixture::new(true).await;
    let ((), ()) = tokio::join!(fixture.cleanup(false, Duration::from_millis(500)), async {
        fixture.wait_for_detach().await;
        fixture.attach_successor().await;
    });
    assert!(
        Arc::ptr_eq(fixture.sessions.read().await.get(&fixture.id).unwrap(), &fixture.agent),
        "a quick reattach finds the same live agent instead of a cold reload"
    );
    assert_eq!(
        Session::load(&fixture.id).unwrap().status,
        SessionStatus::Active,
        "no close ran while the session was reclaimed"
    );
}

#[tokio::test]
async fn saved_idle_session_closes_when_grace_expires() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let fixture = Fixture::new(true).await;
    let grace = Duration::from_millis(50);
    let start = Instant::now();
    timeout(Duration::from_secs(2), fixture.cleanup(false, grace))
        .await
        .unwrap();
    assert!(start.elapsed() >= grace);
    assert!(fixture.sessions.read().await.is_empty());
    assert_eq!(Session::load(&fixture.id).unwrap().status, SessionStatus::Closed);
}

#[tokio::test]
async fn interrupted_session_does_not_wait_for_reconnect_grace() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    crate::server::clear_reload_marker();
    let fixture = Fixture::new(false).await;
    timeout(Duration::from_secs(1), fixture.cleanup(true, Duration::from_secs(30)))
        .await
        .unwrap();
    assert!(fixture.sessions.read().await.is_empty());
    assert!(matches!(
        fixture.agent.lock().await.session_for_split().status,
        SessionStatus::Crashed { .. }
    ));
}

#[tokio::test]
async fn busy_successor_retains_owner_task_and_completion_receiver() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let fixture = Fixture::new(true).await;
    fixture.attach_successor().await;
    let (finish, wait) = tokio::sync::oneshot::channel();
    let (done, mut completions) = mpsc::unbounded_channel();
    let mut task = Some(tokio::spawn(async move {
        wait.await.unwrap();
        done.send("completed").unwrap();
    }));
    let retained = fixture.cleanup_task(true, Duration::ZERO, &mut task).await;
    assert!(retained, "successor returns ownership to lifecycle continuation");
    assert!(!task.as_ref().unwrap().is_finished());
    assert!(fixture.sessions.read().await.contains_key(&fixture.id));
    assert!(fixture.connections.read().await.contains_key("successor"));
    assert!(!fixture.connections.read().await.contains_key("original"));
    finish.send(()).unwrap();
    task.take().unwrap().await.expect("owner was not cancelled");
    assert_eq!(completions.recv().await, Some("completed"));
    fixture.cleanup(false, Duration::ZERO).await;
    assert!(fixture.sessions.read().await.contains_key(&fixture.id));
}

#[tokio::test]
async fn busy_without_successor_aborts_owner_and_marks_crashed() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    crate::server::clear_reload_marker();
    let fixture = Fixture::new(true).await;
    let (dropped, observed_drop) = tokio::sync::oneshot::channel::<()>();
    let mut task = Some(tokio::spawn(async move {
        let _guard = dropped;
        std::future::pending::<()>().await;
    }));
    assert!(!fixture.cleanup_task(true, Duration::ZERO, &mut task).await);
    assert!(task.is_none());
    assert!(timeout(Duration::from_secs(1), observed_drop).await.unwrap().is_err());
    assert!(fixture.sessions.read().await.is_empty());
    assert!(matches!(
        fixture.agent.lock().await.session_for_split().status,
        SessionStatus::Crashed { .. }
    ));
}

#[tokio::test]
async fn idle_reconnect_grace_releases_writer_lock_immediately() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let fixture = Fixture::new(true).await;
    let id = fixture.id.clone();
    let home = std::path::PathBuf::from(std::env::var_os("HORON_HOME").unwrap());
    let paths = crate::session::event_store_api::SessionPaths::new(home.canonicalize().unwrap().join("sessions"));
    let sid = crate::session::event_store_api::SessionId::parse(&id).unwrap();

    // Before disconnect cleanup, the saved live session keeps its writer lock open.
    let busy = crate::session::event_store_api::BlockingWriter::open(paths.clone(), sid.clone());
    assert!(busy.is_err(), "live session should hold its writer lock before disconnect");

    let ((), ()) = tokio::join!(fixture.cleanup(false, Duration::from_secs(2)), async {
        fixture.wait_for_detach().await;
        // Wait briefly for release_idle_writer before grace sleep, well before 2s expires.
        tokio::time::sleep(Duration::from_millis(40)).await;
        let released = crate::session::event_store_api::BlockingWriter::open(paths.clone(), sid.clone());
        assert!(
            released.is_ok(),
            "idle session in reconnect grace must release .writer.lock FD immediately: {:?}",
            released.err()
        );
        drop(released);
        fixture.attach_successor().await;
    });
    assert!(
        Arc::ptr_eq(fixture.sessions.read().await.get(&id).unwrap(), &fixture.agent),
        "releasing the idle writer preserves the live Agent for fast reattach"
    );
}

#[tokio::test]
async fn disconnected_socket_closes_immediately_during_grace_and_prepare_disconnect_skips_grace() {
    use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _};

    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::new()));
    let provider: Arc<dyn Provider> = Arc::new(NoRequests);
    let (global_tx, _) = broadcast::channel(8);
    let (swarm_tx, _) = broadcast::channel(8);
    let (server_stream, client_stream) = crate::transport::Stream::pair().unwrap();

    let server = tokio::spawn(crate::server::client_lifecycle::handle_client(
        server_stream,
        Arc::clone(&sessions),
        global_tx.clone(),
        Arc::clone(&provider),
        Arc::new(RwLock::new(false)),
        Arc::new(RwLock::new(String::new())),
        Arc::new(RwLock::new(0usize)),
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(HashMap::new())),
        FileTouchService::new(),
        Arc::new(RwLock::new(std::collections::VecDeque::new())),
        Arc::new(std::sync::atomic::AtomicU64::new(0)),
        swarm_tx.clone(),
        "horon-agent-test".to_string(),
        "t".to_string(),
        Arc::new(crate::mcp::SharedMcpPool::from_default_config()),
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(HashMap::new())),
        super::super::AwaitMembersRuntime::default(),
        super::super::SwarmMutationRuntime::default(),
        Duration::from_secs(30),
    ));

    let home = tempfile::tempdir().unwrap();
    let working_dir = home.path().to_string_lossy().into_owned();
    let (reader, mut writer) = client_stream.into_split();
    let mut lines = tokio::io::BufReader::new(reader);
    let sub1 = serde_json::json!({"type":"subscribe","id":1,"working_dir":working_dir}).to_string() + "\n";
    writer.write_all(sub1.as_bytes()).await.unwrap();
    let mut buf = String::new();
    loop {
        buf.clear();
        let read = timeout(Duration::from_secs(5), lines.read_line(&mut buf))
            .await
            .expect("timed out waiting for subscribe Done")
            .unwrap();
        assert!(read > 0, "unexpected EOF waiting for subscribe Done; last={buf}");
        if let Ok(ServerEvent::Done { id: 1 }) = serde_json::from_str::<ServerEvent>(buf.trim()) {
            break;
        }
    }

    // Exercise prepare_disconnect -> immediate socket half-close; server must drop
    // its write half and finish lifecycle immediately rather than sleeping 30s.
    writer
        .write_all(b"{\"type\":\"prepare_disconnect\",\"id\":2}\n")
        .await
        .unwrap();
    loop {
        buf.clear();
        let read = timeout(Duration::from_secs(5), lines.read_line(&mut buf))
            .await
            .expect("timed out waiting for prepare_disconnect Done")
            .unwrap();
        assert!(read > 0, "unexpected EOF waiting for prepare_disconnect Done");
        if let Ok(ServerEvent::Done { id: 2 }) = serde_json::from_str::<ServerEvent>(buf.trim()) {
            break;
        }
    }
    writer.shutdown().await.unwrap();

    let mut inner_reader = lines.into_inner();
    let mut eof_buf = [0u8; 16];
    let n = timeout(Duration::from_secs(1), inner_reader.read(&mut eof_buf))
        .await
        .expect("server must close its stream write half immediately on client EOF")
        .unwrap();
    assert_eq!(n, 0);
    timeout(Duration::from_secs(2), server)
        .await
        .expect("prepared_disconnect lifecycle must finish immediately without 30s grace")
        .unwrap()
        .unwrap();
    assert!(sessions.read().await.is_empty());

    // Without prepare_disconnect, half-closing the write side still triggers immediate
    // server socket EOF on the read side even while the 30s reconnect grace is active.
    let (server_stream2, client_stream2) = crate::transport::Stream::pair().unwrap();
    let server2 = tokio::spawn(crate::server::client_lifecycle::handle_client(
        server_stream2,
        Arc::clone(&sessions),
        global_tx,
        provider,
        Arc::new(RwLock::new(false)),
        Arc::new(RwLock::new(String::new())),
        Arc::new(RwLock::new(0usize)),
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(HashMap::new())),
        FileTouchService::new(),
        Arc::new(RwLock::new(std::collections::VecDeque::new())),
        Arc::new(std::sync::atomic::AtomicU64::new(0)),
        swarm_tx,
        "horon-agent-test".to_string(),
        "t".to_string(),
        Arc::new(crate::mcp::SharedMcpPool::from_default_config()),
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(HashMap::new())),
        super::super::AwaitMembersRuntime::default(),
        super::super::SwarmMutationRuntime::default(),
        Duration::from_secs(30),
    ));
    let (reader2, mut writer2) = client_stream2.into_split();
    let mut lines2 = tokio::io::BufReader::new(reader2);
    let sub10 = serde_json::json!({"type":"subscribe","id":10,"working_dir":working_dir}).to_string() + "\n";
    writer2.write_all(sub10.as_bytes()).await.unwrap();
    loop {
        buf.clear();
        let read = timeout(Duration::from_secs(5), lines2.read_line(&mut buf))
            .await
            .expect("timed out waiting for second subscribe Done")
            .unwrap();
        assert!(read > 0, "unexpected EOF waiting for second subscribe Done");
        if let Ok(ServerEvent::Done { id: 10 }) = serde_json::from_str::<ServerEvent>(buf.trim()) {
            break;
        }
    }
    // Switching sessions via fresh_session must also evict the old idle session.
    let sub11 = serde_json::json!({"type":"subscribe","id":11,"working_dir":working_dir,"fresh_session":true}).to_string() + "\n";
    writer2.write_all(sub11.as_bytes()).await.unwrap();
    loop {
        buf.clear();
        let read = timeout(Duration::from_secs(5), lines2.read_line(&mut buf))
            .await
            .expect("timed out waiting for fresh_session subscribe Done")
            .unwrap();
        assert!(read > 0, "unexpected EOF waiting for fresh_session Done");
        if let Ok(ServerEvent::Done { id: 11 }) = serde_json::from_str::<ServerEvent>(buf.trim()) {
            break;
        }
    }
    assert_eq!(
        sessions.read().await.len(),
        1,
        "fresh_session switch must evict the previous unused session rather than retaining both"
    );
    writer2.shutdown().await.unwrap();
    let mut inner_reader2 = lines2.into_inner();
    let n2 = timeout(Duration::from_secs(1), inner_reader2.read(&mut eof_buf))
        .await
        .expect("server must close its stream write half immediately even during 30s grace")
        .unwrap();
    assert_eq!(n2, 0);
    server2.abort();
}
