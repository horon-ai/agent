// End-to-end shapes of the per-session close barrier: an unrelated session
// resumes while another session's close is held, a session's own cold resume
// waits for that close before it loads anything, and a turn that outlives the
// settle deadline keeps the barrier closed.

use crate::server::client_disconnect_cleanup::{cleanup_client_connection, close_barrier_probe};
use std::time::Duration;

type SessionAgents = Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>>;

fn barrier_stored_message(id: &str, text: &str) -> crate::session::StoredMessage {
    crate::session::StoredMessage {
        original_input: None,
        arg_repairs: None,
        degradations: None,
        id: id.to_string(),
        role: crate::message::Role::User,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
        display_role: None,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
        turn_start: false,
    }
}

// Persist one session with a visible message so a close has something to save
// and a resume has something to load.
fn persisted_session(
    session_id: &str,
    text: &str,
    provider: Arc<dyn Provider>,
    registry: Registry,
) -> Arc<Mutex<Agent>> {
    let mut session = crate::session::Session::create_with_id(session_id.to_string(), None, None);
    session.model = Some("mock".to_string());
    session.replace_messages(vec![barrier_stored_message(&format!("{session_id}-msg"), text)]);
    session.save_prepared().expect("seed the session before the test");
    Arc::new(Mutex::new(Agent::new_with_session(provider, registry, session, None)))
}

// Hold the next save of `agent` at its preparation hook and report when the
// save has really started. The hook runs on the saving thread, so the release
// side is a plain channel wait rather than an async lock.
fn hold_next_save(agent: &Arc<Mutex<Agent>>) -> (tokio::sync::oneshot::Receiver<()>, std::sync::mpsc::Sender<()>) {
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let mut guard = agent.try_lock().expect("the fixture agent is idle");
    guard
        .set_replica_preparation_hook(Box::new(move || {
            let _ = entered.send(());
            let _ = release_rx.recv_timeout(Duration::from_secs(30));
        }))
        .expect("the save hook installs on the fixture session");
    (entered_rx, release_tx)
}

struct CleanupOutcome {
    retained: bool,
}

#[allow(clippy::too_many_arguments)]
async fn run_disconnect_cleanup(
    sessions: &SessionAgents,
    session_id: &str,
    processing: bool,
    connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    events: &mpsc::UnboundedSender<ServerEvent>,
    turn: &mut Option<tokio::task::JoinHandle<()>>,
) -> CleanupOutcome {
    let (swarm_events, _) = broadcast::channel(8);
    let retained = cleanup_client_connection(
        sessions,
        session_id,
        processing,
        turn,
        tokio::spawn(std::future::pending()),
        members,
        &Arc::new(RwLock::new(HashMap::new())),
        &Arc::new(RwLock::new(HashMap::new())),
        &Arc::new(RwLock::new(HashMap::new())),
        &FileTouchService::new(),
        &Arc::new(RwLock::new(HashMap::new())),
        &Arc::new(RwLock::new(HashMap::new())),
        connections,
        "conn_old",
        &Arc::new(RwLock::new(HashMap::new())),
        &Arc::new(RwLock::new(HashMap::new())),
        &Arc::new(RwLock::new(VecDeque::new())),
        &Arc::new(std::sync::atomic::AtomicU64::new(0)),
        &swarm_events,
        events,
        Duration::ZERO,
    )
    .await
    .expect("disconnect cleanup reports no error");
    CleanupOutcome {
        retained: retained.is_some(),
    }
}

struct ResumeOutcome {
    session_id: String,
    events: Vec<ServerEvent>,
    agent: Arc<Mutex<Agent>>,
}

#[allow(clippy::too_many_arguments)]
async fn cold_resume(
    provider: &Arc<dyn Provider>,
    sessions: &SessionAgents,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    target_session_id: &str,
    temporary_session_id: &str,
    connection_id: &str,
) -> Result<ResumeOutcome> {
    let mut provisional_agent = Arc::new(Mutex::new(build_test_agent_with_id(
        Arc::clone(provider),
        Registry::new(Arc::clone(provider)).await,
        temporary_session_id,
        Vec::new(),
    )));
    cold_resume_with_agent(
        provider,
        sessions,
        client_connections,
        members,
        target_session_id,
        temporary_session_id,
        connection_id,
        &mut provisional_agent,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn cold_resume_with_agent(
    provider: &Arc<dyn Provider>,
    sessions: &SessionAgents,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    target_session_id: &str,
    temporary_session_id: &str,
    connection_id: &str,
    provisional_agent: &mut Arc<Mutex<Agent>>,
) -> Result<ResumeOutcome> {
    let registry = Registry::new(Arc::clone(provider)).await;
    let shutdown_signals = Arc::new(RwLock::new(HashMap::<String, InterruptSignal>::new()));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let file_touch = FileTouchService::new();
    let channel_subscriptions = Arc::new(RwLock::new(HashMap::new()));
    let channel_subscriptions_by_session = Arc::new(RwLock::new(HashMap::new()));
    let swarm_plans: Arc<RwLock<HashMap<String, VersionedPlan>>> = Arc::new(RwLock::new(HashMap::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id: Arc<RwLock<HashMap<String, HashSet<String>>>> = Arc::new(RwLock::new(HashMap::new()));
    let client_count = Arc::new(RwLock::new(2usize));
    let (writer, _peer) = test_writer()?;
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let event_history = Arc::new(RwLock::new(VecDeque::<SwarmEvent>::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _swarm_event_rx) = broadcast::channel::<SwarmEvent>(8);
    let mcp_pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());
    let mut client_session_id = temporary_session_id.to_string();

    let agent = handle_resume_session(
        11,
        target_session_id.to_string(),
        None,
        None,
        true,
        true,
        &mut client_session_id,
        connection_id,
        &mut Arc::clone(provisional_agent),
        provider,
        &registry,
        sessions,
        &shutdown_signals,
        &soft_interrupt_queues,
        client_connections,
        members,
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

    let mut events = Vec::new();
    while let Ok(event) = client_event_rx.try_recv() {
        events.push(event);
    }
    Ok(ResumeOutcome {
        session_id: client_session_id,
        events,
        agent,
    })
}

fn assert_no_error_events(events: &[ServerEvent]) {
    for event in events {
        assert!(
            !matches!(event, ServerEvent::Error { .. }),
            "the resume must not report an error: {event:?}"
        );
    }
}

fn connection(session_id: &str, processing: bool) -> ClientConnectionInfo {
    let (disconnect_tx, _) = mpsc::unbounded_channel();
    let now = Instant::now();
    ClientConnectionInfo {
        client_id: String::new(),
        session_id: session_id.to_string(),
        client_instance_id: None,
        connected_at: now,
        last_seen: now,
        is_processing: processing,
        current_tool_name: None,
        terminal_env: Vec::new(),
        disconnect_tx,
    }
}

fn named_connection(name: &str, session_id: &str, processing: bool) -> (String, ClientConnectionInfo) {
    let mut info = connection(session_id, processing);
    info.client_id = name.to_string();
    (name.to_string(), info)
}

fn record_owner_liveness_at_release(
    session_id: &str,
    agent: &Arc<Mutex<Agent>>,
) -> Arc<std::sync::Mutex<Option<usize>>> {
    let observed = Arc::new(std::sync::Mutex::new(None));
    let weak = Arc::downgrade(agent);
    let slot = Arc::clone(&observed);
    close_barrier_probe::on_release(session_id, move || {
        *slot.lock().expect("observer lock") = Some(weak.strong_count());
    });
    observed
}

fn wait_for_published_gate() -> impl std::future::Future<Output = ()> {
    let published = close_barrier_probe::observe(|| close_barrier_probe::published_gates() > 0);
    async move {
        tokio::time::timeout(Duration::from_secs(10), published)
            .await
            .expect("the removal publishes its close barrier");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn held_close_of_one_session_does_not_block_another_sessions_cold_resume() -> Result<()> {
    let _guard = crate::storage::lock_test_env();
    let (_runtime, prev_runtime) = setup_runtime_dir()?;
    crate::server::clear_reload_marker();

    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let closing_session_id = "session_barrier_held_close_a";
    let resuming_session_id = "session_barrier_held_close_b";
    let temporary_session_id = "session_barrier_held_close_temp";

    let closing_agent = persisted_session(
        closing_session_id,
        "closing session transcript",
        Arc::clone(&provider),
        Registry::new(Arc::clone(&provider)).await,
    );
    persisted_session(
        resuming_session_id,
        "resuming session transcript",
        Arc::clone(&provider),
        Registry::new(Arc::clone(&provider)).await,
    );

    let (save_entered, save_release) = hold_next_save(&closing_agent);
    let owner_alive_at_release = record_owner_liveness_at_release(closing_session_id, &closing_agent);

    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([(
        closing_session_id.to_string(),
        Arc::clone(&closing_agent),
    )])));
    // The connection's own reference is dropped before cleanup, exactly as
    // `handle_client` does, so the map removal holds the last one.
    drop(closing_agent);
    let client_connections = Arc::new(RwLock::new(HashMap::from([
        named_connection("conn_old", closing_session_id, true),
        named_connection("conn_new", temporary_session_id, false),
    ])));
    let members: Arc<RwLock<HashMap<String, SwarmMember>>> = Arc::new(RwLock::new(HashMap::new()));
    let (closing_events, _closing_events_rx) = mpsc::unbounded_channel::<ServerEvent>();

    let cleanup_sessions = Arc::clone(&sessions);
    let cleanup_connections = Arc::clone(&client_connections);
    let cleanup_members = Arc::clone(&members);
    let cleanup = tokio::spawn(async move {
        let mut turn = None;
        run_disconnect_cleanup(
            &cleanup_sessions,
            closing_session_id,
            false,
            &cleanup_connections,
            &cleanup_members,
            &closing_events,
            &mut turn,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(10), save_entered)
        .await
        .expect("the closing session reaches its terminal save")
        .expect("the save hook reports entry");

    let resumed = tokio::time::timeout(
        Duration::from_secs(10),
        cold_resume(
            &provider,
            &sessions,
            &client_connections,
            &members,
            resuming_session_id,
            temporary_session_id,
            "conn_new",
        ),
    )
    .await
    .expect("the unrelated session resumes without waiting for the held close save")?;
    assert_eq!(resumed.session_id, resuming_session_id);
    let registered = sessions
        .read()
        .await
        .get(resuming_session_id)
        .cloned()
        .expect("the resumed session is registered");
    assert!(Arc::ptr_eq(&resumed.agent, &registered));
    assert_no_error_events(&resumed.events);
    assert!(
        !cleanup.is_finished(),
        "the closing session must still be saving while the other session resumed"
    );

    save_release.send(()).expect("release the held save");
    tokio::time::timeout(Duration::from_secs(10), cleanup)
        .await
        .expect("the closing cleanup finishes once its save is released")
        .expect("cleanup join");
    assert!(
        !sessions.read().await.contains_key(closing_session_id),
        "the closed session is gone from the map"
    );
    assert_eq!(
        *owner_alive_at_release.lock().expect("observer lock"),
        Some(0),
        "the close barrier opens only after the removed owner's own reference is released"
    );
    assert_eq!(close_barrier_probe::published_gates(), 0);

    restore_runtime_dir(prev_runtime);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cold_resume_waits_for_its_own_crashed_save_before_loading() -> Result<()> {
    let _guard = crate::storage::lock_test_env();
    let (_runtime, prev_runtime) = setup_runtime_dir()?;
    crate::server::clear_reload_marker();

    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let session_id = "session_barrier_own_crashed_save";
    let temporary_session_id = "session_barrier_own_crashed_temp";

    let crashing_agent = persisted_session(
        session_id,
        "interrupted session transcript",
        Arc::clone(&provider),
        Registry::new(Arc::clone(&provider)).await,
    );
    let (save_entered, save_release) = hold_next_save(&crashing_agent);
    let owner_alive_at_release = record_owner_liveness_at_release(session_id, &crashing_agent);

    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([(
        session_id.to_string(),
        Arc::clone(&crashing_agent),
    )])));
    drop(crashing_agent);
    let client_connections = Arc::new(RwLock::new(HashMap::from([
        named_connection("conn_old", session_id, true),
        named_connection("conn_new", temporary_session_id, false),
    ])));
    let members: Arc<RwLock<HashMap<String, SwarmMember>>> = Arc::new(RwLock::new(HashMap::new()));
    let (closing_events, _closing_events_rx) = mpsc::unbounded_channel::<ServerEvent>();

    let cleanup_sessions = Arc::clone(&sessions);
    let cleanup_connections = Arc::clone(&client_connections);
    let cleanup_members = Arc::clone(&members);
    let cleanup = tokio::spawn(async move {
        let mut turn = None;
        run_disconnect_cleanup(
            &cleanup_sessions,
            session_id,
            true,
            &cleanup_connections,
            &cleanup_members,
            &closing_events,
            &mut turn,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(10), save_entered)
        .await
        .expect("the crashing session reaches its crashed save")
        .expect("the save hook reports entry");
    assert_eq!(
        close_barrier_probe::published_gates(),
        1,
        "the removal published its close barrier before the slow save"
    );

    let resume_reached = close_barrier_probe::observe(|| close_barrier_probe::waits(session_id) > 0);
    let resume = tokio::spawn({
        let provider = Arc::clone(&provider);
        let sessions = Arc::clone(&sessions);
        let client_connections = Arc::clone(&client_connections);
        let members = Arc::clone(&members);
        async move {
            cold_resume(
                &provider,
                &sessions,
                &client_connections,
                &members,
                session_id,
                temporary_session_id,
                "conn_new",
            )
            .await
        }
    });

    let reached = tokio::time::timeout(Duration::from_secs(10), resume_reached).await;
    assert!(
        reached.is_ok(),
        "the cold resume must reach its own close barrier (finished={}, waits={})",
        resume.is_finished(),
        close_barrier_probe::waits(session_id)
    );
    assert!(
        !resume.is_finished(),
        "the resume must still be waiting while the crashed save is held"
    );

    save_release.send(()).expect("release the held save");
    tokio::time::timeout(Duration::from_secs(10), cleanup)
        .await
        .expect("the crashing cleanup finishes once its save is released")
        .expect("cleanup join");
    let resumed = tokio::time::timeout(Duration::from_secs(10), resume)
        .await
        .expect("the cold resume proceeds once the barrier opens")
        .expect("resume join")?;
    assert_eq!(
        resumed.session_id, session_id,
        "the cold resume restored the session it waited for"
    );
    assert_no_error_events(&resumed.events);

    let restored = crate::session::Session::load(session_id)?;
    assert!(
        matches!(restored.status, crate::session::SessionStatus::Active),
        "the resume's own save landed after the crashed save: {:?}",
        restored.status
    );
    assert_eq!(
        *owner_alive_at_release.lock().expect("observer lock"),
        Some(0),
        "waiters are released only after the old owner (and its writer) is gone"
    );
    assert_eq!(
        close_barrier_probe::published_gates(),
        0,
        "a completed close and resume leave no barrier entry behind"
    );

    restore_runtime_dir(prev_runtime);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_cold_resumers_cannot_both_become_the_owner() -> Result<()> {
    let _guard = crate::storage::lock_test_env();
    let (_runtime, prev_runtime) = setup_runtime_dir()?;
    crate::server::clear_reload_marker();

    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let session_id = "session_barrier_double_resume";

    let closing_agent = persisted_session(
        session_id,
        "contended transcript",
        Arc::clone(&provider),
        Registry::new(Arc::clone(&provider)).await,
    );
    let (save_entered, save_release) = hold_next_save(&closing_agent);

    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([(
        session_id.to_string(),
        Arc::clone(&closing_agent),
    )])));
    drop(closing_agent);
    let client_connections = Arc::new(RwLock::new(HashMap::from([
        named_connection("conn_old", session_id, true),
        named_connection("conn_r1", "temp_r1", false),
        named_connection("conn_r2", "temp_r2", false),
    ])));
    let members: Arc<RwLock<HashMap<String, SwarmMember>>> = Arc::new(RwLock::new(HashMap::new()));
    let (closing_events, _closing_events_rx) = mpsc::unbounded_channel::<ServerEvent>();

    let cleanup_sessions = Arc::clone(&sessions);
    let cleanup_connections = Arc::clone(&client_connections);
    let cleanup_members = Arc::clone(&members);
    let cleanup = tokio::spawn(async move {
        let mut turn = None;
        run_disconnect_cleanup(
            &cleanup_sessions,
            session_id,
            true,
            &cleanup_connections,
            &cleanup_members,
            &closing_events,
            &mut turn,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(10), save_entered)
        .await
        .expect("the closing session reaches its crashed save")
        .expect("the save hook reports entry");

    let mut r1_agent = Arc::new(Mutex::new(build_test_agent_with_id(
        Arc::clone(&provider),
        Registry::new(Arc::clone(&provider)).await,
        "temp_r1",
        Vec::new(),
    )));
    let mut r2_agent = Arc::new(Mutex::new(build_test_agent_with_id(
        Arc::clone(&provider),
        Registry::new(Arc::clone(&provider)).await,
        "temp_r2",
        Vec::new(),
    )));

    let r1_reached = close_barrier_probe::observe(|| close_barrier_probe::waits(session_id) > 0);
    let r1 = tokio::spawn({
        let provider = Arc::clone(&provider);
        let sessions = Arc::clone(&sessions);
        let client_connections = Arc::clone(&client_connections);
        let members = Arc::clone(&members);
        async move {
            cold_resume_with_agent(
                &provider,
                &sessions,
                &client_connections,
                &members,
                session_id,
                "temp_r1",
                "conn_r1",
                &mut r1_agent,
            )
            .await
        }
    });
    // The first resumer holds the claim and waits on the held close.
    tokio::time::timeout(Duration::from_secs(10), r1_reached)
        .await
        .expect("the first resumer waits on the held close");

    let r2_reached = close_barrier_probe::observe(|| close_barrier_probe::claim_waits(session_id) >= 2);
    let r2 = tokio::spawn({
        let provider = Arc::clone(&provider);
        let sessions = Arc::clone(&sessions);
        let client_connections = Arc::clone(&client_connections);
        let members = Arc::clone(&members);
        async move {
            cold_resume_with_agent(
                &provider,
                &sessions,
                &client_connections,
                &members,
                session_id,
                "temp_r2",
                "conn_r2",
                &mut r2_agent,
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(10), r2_reached)
        .await
        .expect("the second resumer queues on the id claim instead of restoring beside the first");

    save_release.send(()).expect("release the held save");
    tokio::time::timeout(Duration::from_secs(10), cleanup)
        .await
        .expect("the close finishes")
        .expect("cleanup join");
    let first = tokio::time::timeout(Duration::from_secs(10), r1)
        .await
        .expect("the first resumer finishes once the barrier opens")
        .expect("r1 join")?;
    assert_eq!(first.session_id, session_id, "the first resumer owns the session");

    let second = tokio::time::timeout(Duration::from_secs(10), r2)
        .await
        .expect("the queued resumer finishes after the claim is handed over")
        .expect("r2 join")?;
    assert!(
        Arc::ptr_eq(&second.agent, &first.agent),
        "the queued resumer must attach the owner the first one published, not restore its own copy"
    );
    let registered = sessions
        .read()
        .await
        .get(session_id)
        .cloned()
        .expect("the session stays registered");
    assert!(
        Arc::ptr_eq(&registered, &first.agent),
        "one cold resume publishes the owner; the later one must not overwrite it"
    );

    restore_runtime_dir(prev_runtime);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_resume_releases_the_source_only_with_its_barrier() -> Result<()> {
    let _guard = crate::storage::lock_test_env();
    let (_runtime, prev_runtime) = setup_runtime_dir()?;
    crate::server::clear_reload_marker();

    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let source_id = "session_barrier_cancel_source";
    let target_id = "session_barrier_cancel_target";

    let source_agent = Arc::new(Mutex::new(build_test_agent_with_id(
        Arc::clone(&provider),
        Registry::new(Arc::clone(&provider)).await,
        source_id,
        Vec::new(),
    )));
    let target_agent = Arc::new(Mutex::new(build_test_agent_with_id(
        Arc::clone(&provider),
        Registry::new(Arc::clone(&provider)).await,
        target_id,
        Vec::new(),
    )));
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([
        (source_id.to_string(), Arc::clone(&source_agent)),
        (target_id.to_string(), Arc::clone(&target_agent)),
    ])));
    let client_connections = Arc::new(RwLock::new(HashMap::from([named_connection(
        "conn_cancel",
        source_id,
        false,
    )])));
    let members: Arc<RwLock<HashMap<String, SwarmMember>>> = Arc::new(RwLock::new(HashMap::new()));

    // Park the handler after its source barrier exists: this write guard is the
    // first await of the detached cleanup that runs past the barrier.
    let shutdown_signals = Arc::new(RwLock::new(HashMap::<String, InterruptSignal>::new()));
    let park = shutdown_signals.write().await;

    let owner_alive_at_release = record_owner_liveness_at_release(source_id, &source_agent);
    let source_weak = Arc::downgrade(&source_agent);

    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let published = close_barrier_probe::observe(|| close_barrier_probe::published_gates() > 0);
    let caller = tokio::spawn({
        let provider = Arc::clone(&provider);
        let sessions = Arc::clone(&sessions);
        let client_connections = Arc::clone(&client_connections);
        let members = Arc::clone(&members);
        let shutdown_signals = Arc::clone(&shutdown_signals);
        let source_agent = Arc::clone(&source_agent);
        async move {
            // The caller owns this binding and moves it into the handler, exactly
            // as `handle_client` does; nothing here keeps a second reference.
            let mut agent = source_agent;
            let registry = Registry::new(Arc::clone(&provider)).await;
            let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
            let swarms_by_id: Arc<RwLock<HashMap<String, HashSet<String>>>> = Arc::new(RwLock::new(HashMap::new()));
            let file_touch = FileTouchService::new();
            let channel_subscriptions = Arc::new(RwLock::new(HashMap::new()));
            let channel_subscriptions_by_session = Arc::new(RwLock::new(HashMap::new()));
            let swarm_plans: Arc<RwLock<HashMap<String, VersionedPlan>>> = Arc::new(RwLock::new(HashMap::new()));
            let swarm_coordinators = Arc::new(RwLock::new(HashMap::new()));
            let client_count = Arc::new(RwLock::new(1usize));
            let (writer, _peer) = test_writer().expect("writer");
            let (client_event_tx, _client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
            let mcp_pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());
            let event_history = Arc::new(RwLock::new(VecDeque::<SwarmEvent>::new()));
            let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let (swarm_event_tx, _swarm_event_rx) = broadcast::channel::<SwarmEvent>(8);
            let mut connection_id = source_id.to_string();
            tokio::select! {
                biased;
                _ = cancel_rx => {}
                _ = handle_resume_session(
                    21,
                    target_id.to_string(),
                    None,
                    None,
                    false,
                    false,
                    &mut connection_id,
                    "conn_cancel",
                    &mut agent,
                    &provider,
                    &registry,
                    &sessions,
                    &shutdown_signals,
                    &soft_interrupt_queues,
                    &client_connections,
                    &members,
                    &swarms_by_id,
                    &file_touch,
                    &channel_subscriptions,
                    &channel_subscriptions_by_session,
                    &swarm_plans,
                    &swarm_coordinators,
                    &client_count,
                    &writer,
                    "test-server",
                    "test",
                    &client_event_tx,
                    &mcp_pool,
                    &event_history,
                    &event_counter,
                    &swarm_event_tx,
                                        false,
                ) => {}
            }
            // The handler owned the reference; a cancelled call leaves nothing.
            sessions.read().await.contains_key(source_id)
        }
    });
    // Only the emulated caller's binding may hold the source from here on.
    drop(source_agent);

    // Wait until the handler published the source barrier and parked behind it.
    tokio::time::timeout(Duration::from_secs(10), published)
        .await
        .expect("the source barrier is published before the park");

    cancel_tx.send(()).expect("cancel the resume");
    let still_registered = tokio::time::timeout(Duration::from_secs(10), caller)
        .await
        .expect("the cancelled caller finishes")
        .expect("caller join");

    drop(park);
    let observed = *owner_alive_at_release.lock().expect("observer lock");
    assert_eq!(
        observed,
        Some(0),
        "a cancelled resume must not leave a reference to the source owner alive when its barrier opens (source registered while cancelled: {still_registered})"
    );
    assert!(source_weak.upgrade().is_none());

    restore_runtime_dir(prev_runtime);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn turn_past_the_settle_deadline_keeps_the_barrier_closed() -> Result<()> {
    let _guard = crate::storage::lock_test_env();
    let (_runtime, prev_runtime) = setup_runtime_dir()?;
    crate::server::clear_reload_marker();

    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let closing_session_id = "session_barrier_settle_a";
    let resuming_session_id = "session_barrier_settle_b";
    let temporary_session_id = "session_barrier_settle_temp";

    let closing_agent = persisted_session(
        closing_session_id,
        "long turn transcript",
        Arc::clone(&provider),
        Registry::new(Arc::clone(&provider)).await,
    );
    persisted_session(
        resuming_session_id,
        "unrelated transcript",
        Arc::clone(&provider),
        Registry::new(Arc::clone(&provider)).await,
    );

    // The turn owns an Agent clone and has no await point, so abort() cannot
    // cancel it and the settle really reaches its diagnostic deadline.
    let (release, release_rx) = std::sync::mpsc::channel::<()>();
    let (started, started_rx) = tokio::sync::oneshot::channel();
    let turn_agent = Arc::clone(&closing_agent);
    let mut processing_task = Some(tokio::spawn({
        async move {
            let _held = turn_agent;
            tokio::task::block_in_place(move || {
                started.send(()).expect("the turn reports that it started");
                release_rx.recv().expect("the test releases the turn");
            });
        }
    }));
    tokio::time::timeout(Duration::from_secs(5), started_rx)
        .await
        .expect("the turn task starts")
        .expect("the turn reports its start");

    let owner_alive_at_release = record_owner_liveness_at_release(closing_session_id, &closing_agent);
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([(
        closing_session_id.to_string(),
        Arc::clone(&closing_agent),
    )])));
    drop(closing_agent);
    let client_connections = Arc::new(RwLock::new(HashMap::from([
        named_connection("conn_old", closing_session_id, true),
        named_connection("conn_new", temporary_session_id, false),
    ])));
    let members: Arc<RwLock<HashMap<String, SwarmMember>>> = Arc::new(RwLock::new(HashMap::new()));
    let (closing_events, _closing_events_rx) = mpsc::unbounded_channel::<ServerEvent>();

    let cleanup_sessions = Arc::clone(&sessions);
    let cleanup_connections = Arc::clone(&client_connections);
    let cleanup_members = Arc::clone(&members);
    let published = wait_for_published_gate();
    let deadline_reached =
        close_barrier_probe::observe(|| close_barrier_probe::settle_deadlines(closing_session_id) > 0);
    let cleanup = tokio::spawn(async move {
        run_disconnect_cleanup(
            &cleanup_sessions,
            closing_session_id,
            true,
            &cleanup_connections,
            &cleanup_members,
            &closing_events,
            &mut processing_task,
        )
        .await
    });

    published.await;
    let resumed = tokio::time::timeout(
        Duration::from_secs(10),
        cold_resume(
            &provider,
            &sessions,
            &client_connections,
            &members,
            resuming_session_id,
            temporary_session_id,
            "conn_new",
        ),
    )
    .await
    .expect("another session resumes while this close is still settling")?;
    assert_eq!(resumed.session_id, resuming_session_id);
    assert_no_error_events(&resumed.events);

    tokio::time::timeout(Duration::from_secs(10), deadline_reached)
        .await
        .expect("the close reaches the settle deadline branch");
    assert!(
        !cleanup.is_finished(),
        "a turn past the deadline keeps the close and its barrier open"
    );
    assert_eq!(
        close_barrier_probe::published_gates(),
        1,
        "the barrier stays published while the turn still owns the old Agent"
    );

    release.send(()).expect("release the blocked turn");
    let outcome = tokio::time::timeout(Duration::from_secs(20), cleanup)
        .await
        .expect("the close finishes once the turn ends")
        .expect("cleanup join");
    assert!(!outcome.retained);
    assert_eq!(
        *owner_alive_at_release.lock().expect("observer lock"),
        Some(0),
        "the barrier opens only after the long turn released the old Agent"
    );
    assert_eq!(close_barrier_probe::published_gates(), 0);
    assert!(!sessions.read().await.contains_key(closing_session_id));

    restore_runtime_dir(prev_runtime);
    Ok(())
}
