#![allow(clippy::await_holding_lock)]
use super::*;
use crate::message::{Message, ToolDefinition};
use crate::provider::EventStream;
use async_trait::async_trait;

struct NoRequests;
#[async_trait]
impl Provider for NoRequests {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
        anyhow::bail!("target attachment must not invoke a provider")
    }
    fn name(&self) -> &str {
        "mock"
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

struct Home {
    _temp: tempfile::TempDir,
    old: Option<std::ffi::OsString>,
}
impl Home {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let old = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", temp.path());
        Self { _temp: temp, old }
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        if let Some(old) = self.old.take() {
            crate::env::set_var("HORON_HOME", old);
        } else {
            crate::env::remove_var("HORON_HOME");
        }
    }
}

fn subscribe(target: &str) -> Request {
    Request::Subscribe {
        system_prompt: None,
        supports_user_response: false,
        supports_ask_requests: false,
        id: 71,
        working_dir: None,
        target_session_id: Some(target.into()),
        client_instance_id: None,
        client_has_local_history: false,
        allow_session_takeover: false,
        crash_on_disconnect: false,
        continue_on_disconnect: false,
        terminal_env: vec![],
        acp: None,
    }
}

async fn live_agent(id: &str, root: &str) -> Arc<Mutex<Agent>> {
    let provider: Arc<dyn Provider> = Arc::new(NoRequests);
    let registry = Registry::new(provider.clone()).await;
    let mut session = crate::session::Session::create_with_id(id.into(), None, None);
    session.working_dir = Some(root.into());
    Arc::new(Mutex::new(Agent::new_with_session(provider, registry, session, None)))
}

#[tokio::test]
async fn target_subscribe_uses_live_unsaved_root_without_changing_it() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let id = "session_live_empty_attach";
    let agent = live_agent(id, "/workspace/live-original").await;
    let sessions = Arc::new(RwLock::new(HashMap::from([(id.into(), agent.clone())])));
    let members = Arc::new(RwLock::new(HashMap::new()));
    let mut request = subscribe(id);
    resolve_target_subscribe_working_dir(&mut request, &sessions, &members)
        .await
        .unwrap();
    assert_eq!(
        initial_subscribe_working_dir(&request).unwrap(),
        "/workspace/live-original"
    );
    assert_eq!(agent.lock().await.working_dir(), Some("/workspace/live-original"));
    assert!(!crate::session::session_exists(id));
}

#[tokio::test]
async fn target_subscribe_uses_persisted_root_when_no_live_agent_exists() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let mut session = crate::session::Session::create(None, Some("persisted".into()));
    session.working_dir = Some("/workspace/persisted-original".into());
    session.save().unwrap();
    let mut request = subscribe(&session.id);
    resolve_target_subscribe_working_dir(
        &mut request,
        &Arc::new(RwLock::new(HashMap::new())),
        &Arc::new(RwLock::new(HashMap::new())),
    )
    .await
    .unwrap();
    assert_eq!(
        initial_subscribe_working_dir(&request).unwrap(),
        "/workspace/persisted-original"
    );
}

#[tokio::test]
async fn target_subscribe_live_root_wins_over_stale_persisted_root() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let mut session = crate::session::Session::create(None, Some("persisted".into()));
    session.working_dir = Some("/workspace/stale".into());
    session.save().unwrap();
    let agent = live_agent(&session.id, "/workspace/live").await;
    let sessions = Arc::new(RwLock::new(HashMap::from([(session.id.clone(), agent)])));
    let mut request = subscribe(&session.id);
    resolve_target_subscribe_working_dir(&mut request, &sessions, &Arc::new(RwLock::new(HashMap::new())))
        .await
        .unwrap();
    assert_eq!(initial_subscribe_working_dir(&request).unwrap(), "/workspace/live");
}

#[tokio::test]
async fn target_subscribe_busy_live_agent_uses_member_root_without_waiting() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let id = "session_busy_empty_attach";
    let agent = live_agent(id, "/workspace/busy-original").await;
    let sessions = Arc::new(RwLock::new(HashMap::from([(id.into(), agent.clone())])));
    let (event_tx, _) = mpsc::unbounded_channel();
    let now = std::time::Instant::now();
    let members = Arc::new(RwLock::new(HashMap::from([(
        id.into(),
        SwarmMember {
            session_id: id.into(),
            event_tx,
            event_txs: HashMap::new(),
            working_dir: Some("/workspace/busy-original".into()),
            swarm_id: None,
            swarm_enabled: false,
            status: "running".into(),
            detail: None,
            task_label: None,
            friendly_name: None,
            report_back_to_session_id: None,
            latest_completion_report: None,
            role: "agent".into(),
            joined_at: now,
            last_status_change: now,
            is_headless: false,
            output_tail: None,
            todo_progress: None,
            todo_items: vec![],
            runtime: Default::default(),
        },
    )])));
    let _busy = agent.lock().await;
    let mut request = subscribe(id);
    tokio::time::timeout(
        Duration::from_millis(100),
        resolve_target_subscribe_working_dir(&mut request, &sessions, &members),
    )
    .await
    .expect("must not wait on busy Agent")
    .unwrap();
    assert_eq!(
        initial_subscribe_working_dir(&request).unwrap(),
        "/workspace/busy-original"
    );
}

#[tokio::test]
async fn target_subscribe_unknown_target_never_uses_process_working_dir() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let mut request = subscribe("session_missing");
    let error = resolve_target_subscribe_working_dir(
        &mut request,
        &Arc::new(RwLock::new(HashMap::new())),
        &Arc::new(RwLock::new(HashMap::new())),
    )
    .await
    .unwrap_err();
    assert!(error.contains("Unknown session"));
    assert!(error.contains("session_missing"));
    let load_error = crate::session::Session::load_startup_stub("session_missing").unwrap_err();
    assert!(
        error.contains(&load_error.to_string()),
        "load cause must survive: {error}"
    );
    assert!(initial_subscribe_working_dir(&request).is_err());
}

#[tokio::test]
async fn target_subscribe_reports_corrupt_persisted_state_without_using_process_cwd() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let session = crate::session::Session::create(None, None);
    let path = crate::session::session_path(&session.id).unwrap();
    // Empty sessions are lazily persisted. Seed an actual broken snapshot.
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"{broken session json").unwrap();
    let cause = crate::session::Session::load_startup_stub(&session.id).unwrap_err();
    let mut request = subscribe(&session.id);
    let error = resolve_target_subscribe_working_dir(
        &mut request,
        &Arc::new(RwLock::new(HashMap::new())),
        &Arc::new(RwLock::new(HashMap::new())),
    )
    .await
    .unwrap_err();
    assert!(error.contains(&session.id), "{error}");
    assert!(error.contains(&cause.to_string()), "{error}");
    assert!(initial_subscribe_working_dir(&request).is_err());
}

#[tokio::test]
async fn target_subscribe_preserves_explicit_directory_and_its_validation() {
    let mut request = subscribe("session_explicit");
    if let Request::Subscribe { working_dir, .. } = &mut request {
        *working_dir = Some("/workspace/explicit".into());
    }
    resolve_target_subscribe_working_dir(
        &mut request,
        &Arc::new(RwLock::new(HashMap::new())),
        &Arc::new(RwLock::new(HashMap::new())),
    )
    .await
    .unwrap();
    assert_eq!(initial_subscribe_working_dir(&request).unwrap(), "/workspace/explicit");
    if let Request::Subscribe { working_dir, .. } = &mut request {
        *working_dir = Some("relative".into());
    }
    resolve_target_subscribe_working_dir(
        &mut request,
        &Arc::new(RwLock::new(HashMap::new())),
        &Arc::new(RwLock::new(HashMap::new())),
    )
    .await
    .unwrap();
    assert!(initial_subscribe_working_dir(&request).is_err());
}

/// F3a: a target Subscribe the daemon cannot bind, because another owner holds
/// the session's store writer, is answered with the store's retryable refusal.
/// Before, the refusal was queued and then dropped with the connection's
/// forwarder, so the client saw only the Ack until the 30 s reconnect grace
/// closed the socket.
#[tokio::test]
async fn target_subscribe_refused_by_a_busy_writer_sends_the_refusal() {
    use crate::session::event_store_api as store;
    use tokio::io::AsyncBufReadExt as _;

    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    // Written through the store the way a prompted session is, with no read
    // cache, so restoring it needs the session's writer, which `held` owns.
    let home = std::path::PathBuf::from(std::env::var_os("HORON_HOME").unwrap());
    let paths = store::SessionPaths::new(home.canonicalize().unwrap().join("sessions"));
    let id = "session_busybind_1700000000000_0123456789abcdef".to_string();
    let session_id = store::SessionId::parse(&id).unwrap();
    let mut held = store::BlockingWriter::open(paths, session_id.clone()).unwrap();
    let metadata = store::SessionMetadata {
        working_dir: Some(home.display().to_string()),
        ..store::SessionMetadata::default()
    };
    let root = store::EventDraft::new(
        session_id.clone(),
        store::EventId::new_v4(),
        None,
        store::OriginId::new_v4(),
        metadata.created_at,
        store::EventKind::new(store::KnownEventKind::SessionCreated),
        serde_json::json!(store::SessionCreatedPayload {
            branch_root: false,
            reset_provenance: None,
            metadata,
        }),
    )
    .seal()
    .unwrap();
    held.append(&root).unwrap();
    let message = store::EventDraft::new(
        session_id,
        store::EventId::new_v4(),
        Some(root.envelope().event_id.clone()),
        store::OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        store::EventKind::new(store::KnownEventKind::MessageAppended),
        serde_json::json!({"message": crate::session::StoredMessage {
            turn_start: false,
            id: "m1".into(),
            role: crate::message::Role::User,
            content: vec![crate::message::ContentBlock::Text { text: "persisted".into(), cache_control: None }],
            display_role: None, timestamp: None, tool_duration_ms: None, token_usage: None,
        }}),
    )
    .seal()
    .unwrap();
    held.append(&message).unwrap();

    let (server_stream, client_stream) = crate::transport::Stream::pair().unwrap();
    let (global_event_tx, _) = broadcast::channel(8);
    let (swarm_event_tx, _) = broadcast::channel(8);
    let server = tokio::spawn(handle_client(
        server_stream,
        Arc::new(RwLock::new(HashMap::new())),
        global_event_tx,
        Arc::new(NoRequests),
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
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(std::collections::VecDeque::new())),
        Arc::new(std::sync::atomic::AtomicU64::new(0)),
        swarm_event_tx,
        "horon-agent-test".to_string(),
        "t".to_string(),
        Arc::new(crate::mcp::SharedMcpPool::from_default_config()),
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(HashMap::new())),
        AwaitMembersRuntime::default(),
        SwarmMutationRuntime::default(),
        std::time::Duration::ZERO,
    ));

    let (reader, mut writer) = client_stream.into_split();
    let mut lines = tokio::io::BufReader::new(reader).lines();
    let request = serde_json::to_string(&subscribe(&id)).unwrap() + "\n";
    writer.write_all(request.as_bytes()).await.unwrap();

    // Far inside the 30 s reconnect grace the silent shape waited out.
    let bound = Duration::from_secs(10);
    let mut refusal = None;
    while refusal.is_none() {
        let line = tokio::time::timeout(bound, lines.next_line())
            .await
            .expect("the daemon answers the refused Subscribe instead of falling silent")
            .unwrap()
            .expect("the daemon keeps the socket open while it answers");
        match serde_json::from_str::<ServerEvent>(line.trim()).unwrap() {
            ServerEvent::Error { id: 71, message, .. } => refusal = Some(message),
            ServerEvent::History { session_id, .. } => panic!("the busy session was bound: {session_id}"),
            ServerEvent::Done { id: 71 } => panic!("the refused Subscribe was answered Done"),
            _ => {}
        }
    }
    let message = refusal.unwrap();
    assert!(
        message.contains("[internal:busy] retryable") && message.contains("session writer is already owned"),
        "{message}"
    );
    drop(held);
    server.abort();
}
