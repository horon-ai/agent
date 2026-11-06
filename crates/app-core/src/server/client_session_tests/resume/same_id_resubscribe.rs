/// A client that resumes the session it is already on reaches the session-map
/// bookkeeping with equal ids, where remove-then-insert tears down a LIVE
/// session's event sink and an approval then falls through to its default.
#[tokio::test]
async fn handle_resume_session_keeps_the_sink_when_the_id_is_unchanged() -> Result<()> {
    let _guard = crate::storage::lock_test_env();
    let (_runtime, prev_runtime) = setup_runtime_dir()?;

    let session_id = "session_same_id_resubscribe";

    let mut persisted =
        crate::session::Session::create_with_id(session_id.to_string(), None, Some("Same Id".to_string()));
    persisted.save()?;

    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let registry = Registry::new(provider.clone()).await;
    // The client is already on this session, so its own agent is the session's
    // agent and the resume takes the restore path with equal ids.
    let mut agent = Arc::new(Mutex::new(build_test_agent_with_id(
        provider.clone(),
        registry.clone(),
        session_id,
        Vec::new(),
    )));
    let sessions = Arc::new(RwLock::new(HashMap::from([(
        session_id.to_string(),
        Arc::clone(&agent),
    )])));

    let shutdown_signals = Arc::new(RwLock::new(HashMap::<String, InterruptSignal>::new()));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let now = Instant::now();
    let client_connections = Arc::new(RwLock::new(HashMap::from([(
        "conn_same".to_string(),
        ClientConnectionInfo {
            client_id: "conn_same".to_string(),
            session_id: session_id.to_string(),
            client_instance_id: None,
            connected_at: now,
            last_seen: now,
            is_processing: false,
            current_tool_name: None,
            terminal_env: Vec::new(),
            disconnect_tx: mpsc::unbounded_channel().0,
        },
    )])));
    let swarm_members = Arc::new(RwLock::new(HashMap::<String, SwarmMember>::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::<String, HashSet<String>>::new()));
    let file_touch = FileTouchService::new();
    let channel_subscriptions = Arc::new(RwLock::new(HashMap::<String, HashMap<String, HashSet<String>>>::new()));
    let channel_subscriptions_by_session =
        Arc::new(RwLock::new(HashMap::<String, HashMap<String, HashSet<String>>>::new()));
    let swarm_plans = Arc::new(RwLock::new(HashMap::<String, VersionedPlan>::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::<String, String>::new()));
    let client_count = Arc::new(RwLock::new(1usize));
    let (writer, _peer_stream) = test_writer()?;
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let event_history = Arc::new(RwLock::new(VecDeque::<SwarmEvent>::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _swarm_event_rx) = broadcast::channel::<SwarmEvent>(8);
    let mcp_pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());

    let (sink_tx, _sink_rx) = mpsc::unbounded_channel();
    crate::server::register_session_event_sink(session_id, sink_tx.clone());
    assert!(
        crate::server::state::session_event_sink(session_id).is_some(),
        "the sink must be registered before the resume, or this test proves nothing"
    );

    let mut client_session_id = session_id.to_string();

    handle_resume_session(
        45,
        session_id.to_string(),
        None,
        None,
        true,
        false,
        &mut client_session_id,
        "conn_same",
        &mut agent,
        &provider,
        &registry,
        &sessions,
        &shutdown_signals,
        &soft_interrupt_queues,
        &client_connections,
        &swarm_members,
        &swarms_by_id,
        &file_touch,
        &channel_subscriptions,
        &channel_subscriptions_by_session,
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

    let events = collect_events_until_done(&mut client_event_rx, 45).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ServerEvent::Done { id } if *id == 45)),
        "expected Done event for the same-id resume, got {events:?}"
    );
    assert!(
        !events.iter().any(|event| matches!(event, ServerEvent::Error { .. })),
        "a same-id resume must not emit error events: {events:?}"
    );

    assert_eq!(client_session_id, session_id);
    assert!(
        crate::server::state::session_event_sink(session_id).is_some(),
        "resuming the id the client is already on must not drop the live session's event sink"
    );
    assert!(
        sessions.read().await.contains_key(session_id),
        "the session must stay mapped under its own id"
    );

    // Leave no sink behind for a later test.
    crate::server::unregister_session_event_sink(session_id, &sink_tx);
    assert!(
        crate::server::state::session_event_sink(session_id).is_none(),
        "the test must clean up its own sink"
    );
    restore_runtime_dir(prev_runtime);
    Ok(())
}
