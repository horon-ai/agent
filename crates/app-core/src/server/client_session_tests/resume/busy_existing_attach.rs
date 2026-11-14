#[tokio::test]
async fn handle_resume_session_allows_live_attach_when_existing_agent_is_busy() -> Result<()> {
    let _guard = crate::storage::lock_test_env();
    let (_runtime, prev_runtime) = setup_runtime_dir()?;

    let target_session_id = "session_existing_live_busy";
    let temp_session_id = "session_temp_connecting_busy";

    let persisted_message = crate::session::StoredMessage {
        original_input: None,
        arg_repairs: None,
        degradations: None,
        turn_start: false,
        id: "msg-live-busy".to_string(),
        role: crate::message::Role::User,
        content: vec![crate::message::ContentBlock::Text {
            text: "persisted busy attach history".to_string(),
            cache_control: None,
        }],
        display_role: None,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
    };

    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let existing_registry = Registry::new(provider.clone()).await;
    let existing_agent = Arc::new(Mutex::new(build_test_agent_with_id(
        provider.clone(),
        existing_registry,
        target_session_id,
        vec![persisted_message],
    )));

    let new_registry = Registry::new(provider.clone()).await;
    let mut new_agent = Arc::new(Mutex::new(build_test_agent_with_id(
        provider.clone(),
        new_registry.clone(),
        temp_session_id,
        Vec::new(),
    )));

    let sessions = Arc::new(RwLock::new(HashMap::from([
        (target_session_id.to_string(), Arc::clone(&existing_agent)),
        (temp_session_id.to_string(), Arc::clone(&new_agent)),
    ])));
    let shutdown_signals = Arc::new(RwLock::new(HashMap::<String, InterruptSignal>::new()));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let now = Instant::now();
    let client_connections = Arc::new(RwLock::new(HashMap::from([
        (
            "conn_existing".to_string(),
            ClientConnectionInfo {
                client_id: "conn_existing".to_string(),
                session_id: target_session_id.to_string(),
                client_instance_id: None,
                connected_at: now,
                last_seen: now,
                is_processing: true,
                current_tool_name: Some("bash".to_string()),
                terminal_env: Vec::new(),
                disconnect_tx: mpsc::unbounded_channel().0,
            },
        ),
        (
            "conn_new".to_string(),
            ClientConnectionInfo {
                client_id: "conn_new".to_string(),
                session_id: temp_session_id.to_string(),
                client_instance_id: None,
                connected_at: now,
                last_seen: now,
                is_processing: false,
                current_tool_name: None,
                terminal_env: Vec::new(),
                disconnect_tx: mpsc::unbounded_channel().0,
            },
        ),
    ])));
    let swarm_members = Arc::new(RwLock::new(HashMap::<String, SwarmMember>::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::<String, HashSet<String>>::new()));
    let file_touch = FileTouchService::new();
    let swarm_plans = Arc::new(RwLock::new(HashMap::<String, VersionedPlan>::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::<String, String>::new()));
    let client_count = Arc::new(RwLock::new(2usize));
    let (writer, peer_stream) = test_writer()?;
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let event_history = Arc::new(RwLock::new(VecDeque::<SwarmEvent>::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _swarm_event_rx) = broadcast::channel::<SwarmEvent>(8);
    let mcp_pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());

    let mut client_session_id = temp_session_id.to_string();
    let _busy_guard = existing_agent.lock().await;

    handle_resume_session(
        77,
        target_session_id.to_string(),
        None,
        None,
        false,
        false,
        &mut client_session_id,
        "conn_new",
        &mut new_agent,
        &provider,
        &new_registry,
        &sessions,
        &shutdown_signals,
        &soft_interrupt_queues,
        &client_connections,
        &swarm_members,
        &swarms_by_id,
        &file_touch,
        &swarm_plans,
        &swarm_coordinators,
        &client_count,
        &writer,
        "test-server",
        "🌿",
        &client_event_tx,
        &mcp_pool,
        &event_history,
        &event_counter,
        &swarm_event_tx,
        false,
    )
    .await?;

    // The desktop follows a target-aware resume with the normal subscribe
    // bookkeeping while the original client can still be processing. That
    // bookkeeping must never wait for the live agent lock, otherwise the
    // desktop's immediately following state request remains unread and times
    // out after ten seconds.
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        handle_subscribe(
            77,
            Some("/tmp/horon-agent-busy-desktop-attach".to_string()),
            false,
            target_session_id,
            "conn_new",
            &None,
            &existing_agent,
            &new_registry,
            true,
            &swarm_members,
            &swarms_by_id,
            &swarm_plans,
            &swarm_coordinators,
            &client_event_tx,
            &mcp_pool,
            &event_history,
            &event_counter,
            &swarm_event_tx,
        ),
    )
    .await
    .expect("subscribe bookkeeping must not wait for a busy live agent");

    // Resume and subscribe both answer request id 77, so each emits its own
    // Done. Collect both batches, otherwise the assertions below only ever see
    // resume's events and subscribe's are invisible.
    let mut events = collect_events_until_done(&mut client_event_rx, 77).await;
    events.extend(collect_events_until_done(&mut client_event_rx, 77).await);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ServerEvent::Done { id } if *id == 77)),
        "expected Done event for busy live attach, got {events:?}"
    );
    assert!(
        !events.iter().any(|event| matches!(event, ServerEvent::Error { .. })),
        "busy live attach should not emit error events: {events:?}"
    );
    // A remote (gateway) client has no other way to learn its session id, and
    // without it a dropped connection cannot reattach: the next Subscribe
    // carries no `target_session_id`, so the server hands it a fresh session
    // and the in-flight turn becomes unreachable.
    assert!(
        events.iter().any(|event| matches!(
            event,
            ServerEvent::SessionId { session_id } if session_id == target_session_id
        )),
        "subscribe must report the bound session id so clients can reattach: {events:?}"
    );

    let mut peer_reader = tokio::io::BufReader::new(peer_stream);
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        tokio::io::AsyncBufReadExt::read_line(&mut peer_reader, &mut line),
    )
    .await
    .expect("history should be written promptly")?;
    let event: ServerEvent = serde_json::from_str(line.trim())?;
    match event {
        ServerEvent::History {
            session_id, messages, ..
        } => {
            assert_eq!(session_id, target_session_id);
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].content, "persisted busy attach history");
        }
        other => panic!("expected history event, got {other:?}"),
    }

    restore_runtime_dir(prev_runtime);
    Ok(())
}

/// Live-attach a fresh connection to `target_session_id`, whose agent is
/// already in `sessions`, and return every event it emitted plus the reader
/// side of the History writer.
async fn live_attach_owed_target(
    provider: &Arc<dyn Provider>,
    existing_agent: &Arc<Mutex<Agent>>,
    target_session_id: &str,
) -> Result<(Vec<ServerEvent>, crate::transport::Stream)> {
    let temp_session_id = format!("{target_session_id}_connecting");
    let new_registry = Registry::new(provider.clone()).await;
    let mut new_agent = Arc::new(Mutex::new(build_test_agent_with_id(
        provider.clone(),
        new_registry.clone(),
        &temp_session_id,
        Vec::new(),
    )));
    let sessions = Arc::new(RwLock::new(HashMap::from([
        (target_session_id.to_string(), Arc::clone(existing_agent)),
        (temp_session_id.clone(), Arc::clone(&new_agent)),
    ])));
    let now = Instant::now();
    let client_connections = Arc::new(RwLock::new(HashMap::from([(
        "conn_owed".to_string(),
        ClientConnectionInfo {
            client_id: "conn_owed".to_string(),
            session_id: temp_session_id.clone(),
            client_instance_id: None,
            connected_at: now,
            last_seen: now,
            is_processing: false,
            current_tool_name: None,
            terminal_env: Vec::new(),
            disconnect_tx: mpsc::unbounded_channel().0,
        },
    )])));
    let (writer, peer_stream) = test_writer()?;
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let (swarm_event_tx, _swarm_event_rx) = broadcast::channel::<SwarmEvent>(8);
    let mut client_session_id = temp_session_id.clone();

    handle_resume_session(
        78,
        target_session_id.to_string(),
        None,
        None,
        false,
        false,
        &mut client_session_id,
        "conn_owed",
        &mut new_agent,
        provider,
        &new_registry,
        &sessions,
        &Arc::new(RwLock::new(HashMap::<String, InterruptSignal>::new())),
        &Arc::new(RwLock::new(HashMap::new())),
        &client_connections,
        &Arc::new(RwLock::new(HashMap::<String, SwarmMember>::new())),
        &Arc::new(RwLock::new(HashMap::<String, HashSet<String>>::new())),
        &FileTouchService::new(),
        &Arc::new(RwLock::new(HashMap::<String, VersionedPlan>::new())),
        &Arc::new(RwLock::new(HashMap::<String, String>::new())),
        &Arc::new(RwLock::new(2usize)),
        &writer,
        "test-server",
        "🌿",
        &client_event_tx,
        &Arc::new(crate::mcp::SharedMcpPool::from_default_config()),
        &Arc::new(RwLock::new(VecDeque::<SwarmEvent>::new())),
        &Arc::new(std::sync::atomic::AtomicU64::new(0)),
        &swarm_event_tx,
        false,
    )
    .await?;

    let mut events = Vec::new();
    while let Ok(event) = client_event_rx.try_recv() {
        events.push(event);
    }
    Ok((events, peer_stream))
}

/// A live agent that restored `session_id` and still owes its resume commit.
///
/// Like a cold resume, the restoring agent starts on a different (provisional)
/// session, so preparing does not close the very session it loads.
fn owed_live_agent(provider: &Arc<dyn Provider>, registry: Registry, session_id: &str) -> Arc<Mutex<Agent>> {
    let persisted = persisted_session(session_id, "owed commit history", provider.clone(), registry.clone());
    drop(persisted);
    let agent = Arc::new(Mutex::new(build_test_agent_with_id(
        provider.clone(),
        registry,
        &format!("{session_id}_provisional"),
        Vec::new(),
    )));
    agent
        .try_lock()
        .expect("the fixture agent is idle")
        .prepare_restored_session(session_id, None)
        .expect("prepare the restored session");
    assert!(crate::agent::resume_commit_owed(session_id));
    agent
}

async fn read_history_line(peer_stream: crate::transport::Stream) -> Result<ServerEvent> {
    let mut peer_reader = tokio::io::BufReader::new(peer_stream);
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        tokio::io::AsyncBufReadExt::read_line(&mut peer_reader, &mut line),
    )
    .await
    .expect("history should be written promptly")?;
    Ok(serde_json::from_str(line.trim())?)
}

#[tokio::test]
async fn live_attach_completes_an_owed_commit_before_done() -> Result<()> {
    let _guard = crate::storage::lock_test_env();
    let (_runtime, prev_runtime) = setup_runtime_dir()?;
    let session_id = "session_live_attach_owed_commit";
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let existing_agent = owed_live_agent(&provider, Registry::new(provider.clone()).await, session_id);

    let (events, peer_stream) = live_attach_owed_target(&provider, &existing_agent, session_id).await?;

    assert!(
        matches!(read_history_line(peer_stream).await?, ServerEvent::History { .. }),
        "History is delivered first"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ServerEvent::Done { id } if *id == 78)),
        "Done follows once the commit landed: {events:?}"
    );
    assert!(
        !events.iter().any(|event| matches!(event, ServerEvent::Error { .. })),
        "{events:?}"
    );
    assert!(
        !crate::agent::resume_commit_owed(session_id),
        "Done is only sent for a durable session"
    );
    restore_runtime_dir(prev_runtime);
    Ok(())
}

#[tokio::test]
async fn live_attach_reports_retryable_error_when_owed_commit_fails() -> Result<()> {
    let _guard = crate::storage::lock_test_env();
    let (_runtime, prev_runtime) = setup_runtime_dir()?;
    let session_id = "session_live_attach_owed_commit_fails";
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let existing_agent = owed_live_agent(&provider, Registry::new(provider.clone()).await, session_id);
    crate::storage::write_session_lease(&crate::storage::SessionLease {
        session_id: session_id.to_string(),
        epoch: 1,
        away_host: Some("cloud".to_string()),
        repo_root: None,
        last_sync_snapshot: None,
        last_sync_head: None,
        updated_at: "test".to_string(),
    })?;

    let (events, _peer_stream) = live_attach_owed_target(&provider, &existing_agent, session_id).await?;

    assert!(
        events.iter().any(|event| matches!(
            event,
            ServerEvent::Error {
                id: 78,
                retry_after_secs: Some(1),
                ..
            }
        )),
        "a failed owed commit is a retryable error: {events:?}"
    );
    assert!(
        !events.iter().any(|event| matches!(event, ServerEvent::Done { .. })),
        "no Done for a session that is not durable: {events:?}"
    );
    assert!(crate::agent::resume_commit_owed(session_id), "the barrier stays closed");
    crate::storage::remove_session_lease(session_id);
    restore_runtime_dir(prev_runtime);
    Ok(())
}
