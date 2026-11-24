#[tokio::test]
async fn handle_resume_session_cold_restore_from_shared_session_preserves_parent_session() -> Result<()> {
    let _guard = crate::storage::lock_test_env();
    let (_runtime, prev_runtime) = setup_runtime_dir()?;

    let parent_session_id = "session_shared_parent_cold_restore";
    let child_session_id = "session_cold_child_target";

    let mut parent_session =
        crate::session::Session::create_with_id(parent_session_id.to_string(), None, Some("Parent".to_string()));
    parent_session.save()?;
    let mut child_session =
        crate::session::Session::create_with_id(child_session_id.to_string(), None, Some("Child".to_string()));
    child_session.save()?;

    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let registry = Registry::new(provider.clone()).await;
    let parent_agent = Arc::new(Mutex::new(build_test_agent_with_id(
        provider.clone(),
        registry.clone(),
        parent_session_id,
        Vec::new(),
    )));

    let sessions = Arc::new(RwLock::new(HashMap::from([(
        parent_session_id.to_string(),
        Arc::clone(&parent_agent),
    )])));
    let shutdown_signals = Arc::new(RwLock::new(HashMap::<String, InterruptSignal>::new()));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let now = Instant::now();
    let client_connections = Arc::new(RwLock::new(HashMap::from([
        (
            "conn_parent".to_string(),
            ClientConnectionInfo {
                client_id: "conn_parent".to_string(),
                session_id: parent_session_id.to_string(),
                client_instance_id: None,
                connected_at: now,
                last_seen: now,
                is_processing: false,
                current_tool_name: None,
                terminal_env: Vec::new(),
                disconnect_tx: mpsc::unbounded_channel().0,
            },
        ),
        (
            "conn_side".to_string(),
            ClientConnectionInfo {
                client_id: "conn_side".to_string(),
                session_id: parent_session_id.to_string(),
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

    let (parent_event_tx, mut parent_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let (side_event_tx, mut side_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let mut parent_member = test_swarm_member(parent_session_id, "ready");
    parent_member.event_tx = parent_event_tx.clone();
    parent_member.event_txs = HashMap::from([
        ("conn_parent".to_string(), parent_event_tx.clone()),
        ("conn_side".to_string(), side_event_tx.clone()),
    ]);
    let swarm_members = Arc::new(RwLock::new(HashMap::from([(
        parent_session_id.to_string(),
        parent_member,
    )])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        "swarm-test".to_string(),
        HashSet::from([parent_session_id.to_string()]),
    )])));
    let file_touch = FileTouchService::new();
    let swarm_plans = Arc::new(RwLock::new(HashMap::<String, VersionedPlan>::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::<String, String>::new()));
    let client_count = Arc::new(RwLock::new(2usize));
    let (writer, _peer_stream) = test_writer()?;
    let event_history = Arc::new(RwLock::new(VecDeque::<SwarmEvent>::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _swarm_event_rx) = broadcast::channel::<SwarmEvent>(8);
    let mcp_pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());

    let mut side_agent = Arc::clone(&parent_agent);
    let mut side_session_id = parent_session_id.to_string();

    handle_resume_session(
        88,
        child_session_id.to_string(),
        None,
        None,
        false,
        false,
        &mut side_session_id,
        "conn_side",
        &mut side_agent,
        &provider,
        &registry,
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
        &side_event_tx,
        &mcp_pool,
        &event_history,
        &event_counter,
        &swarm_event_tx,
        crate::server::foreground_state::HistoryIds::default(),
    )
    .await?;

    let events = collect_events_until_done(&mut side_event_rx, 88).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ServerEvent::Done { id } if *id == 88)),
        "expected Done event for child resume, got {events:?}"
    );
    assert!(
        !events.iter().any(|event| matches!(event, ServerEvent::Error { .. })),
        "cold restore from shared parent must not emit an error: {events:?}"
    );

    assert_eq!(side_session_id, child_session_id);
    assert!(
        !Arc::ptr_eq(&parent_agent, &side_agent),
        "cold restore must allocate a distinct Agent while another client owns the parent"
    );
    assert_eq!(parent_agent.lock().await.session_id(), parent_session_id);
    assert_eq!(side_agent.lock().await.session_id(), child_session_id);

    {
        let sessions_guard = sessions.read().await;
        assert!(
            sessions_guard
                .get(parent_session_id)
                .is_some_and(|agent| Arc::ptr_eq(agent, &parent_agent)),
            "shared parent session must remain mapped to the original parent Agent"
        );
        assert!(
            sessions_guard
                .get(child_session_id)
                .is_some_and(|agent| Arc::ptr_eq(agent, &side_agent)),
            "restored child session must be mapped to the new child Agent"
        );
    }

    while parent_event_rx.try_recv().is_ok() {}
    while side_event_rx.try_recv().is_ok() {}

    crate::server::fanout_live_client_event(
        &swarm_members,
        child_session_id,
        ServerEvent::SessionId {
            session_id: child_session_id.to_string(),
        },
    )
    .await;
    assert!(
        matches!(
            side_event_rx.try_recv(),
            Ok(ServerEvent::SessionId { ref session_id }) if session_id == child_session_id
        ),
        "child fanout must reach the side connection"
    );
    assert!(
        parent_event_rx.try_recv().is_err(),
        "child fanout must not leak to the parent connection"
    );

    crate::server::fanout_live_client_event(
        &swarm_members,
        parent_session_id,
        ServerEvent::SessionId {
            session_id: parent_session_id.to_string(),
        },
    )
    .await;
    assert!(
        matches!(
            parent_event_rx.try_recv(),
            Ok(ServerEvent::SessionId { ref session_id }) if session_id == parent_session_id
        ),
        "parent fanout must still reach the parent connection"
    );
    assert!(
        side_event_rx.try_recv().is_err(),
        "parent fanout must not reach the detached side connection"
    );

    restore_runtime_dir(prev_runtime);
    Ok(())
}
