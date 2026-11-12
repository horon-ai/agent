use super::*;

/// A provider whose warmup never finishes until the test releases it.
///
/// Attaching must not wait on this: the whole point of deferring the prewarm is
/// that a client is served while speculative provider work is still in flight.
struct GatedPrewarmProvider {
    entered: Arc<tokio::sync::Semaphore>,
    release: Arc<tokio::sync::Semaphore>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl Provider for GatedPrewarmProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(stream::empty()))
    }

    fn name(&self) -> &str {
        "gated-prewarm"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            entered: Arc::clone(&self.entered),
            release: Arc::clone(&self.release),
            calls: Arc::clone(&self.calls),
        })
    }

    async fn prewarm(&self, _tools: &[ToolDefinition], _system: &str) {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.entered.add_permits(1);
        // Held until the test releases it. A single permit is taken, so the wait
        // is a barrier rather than a sleep.
        let _ = self.release.acquire().await;
    }
}

/// The deferred warmup must not delay the tool surface a client is served.
///
/// The gate below stays closed for the whole assertion window, so a build path
/// that still awaited the prewarm would block here until the timeout.
#[tokio::test(flavor = "multi_thread")]
async fn loading_deferred_setup_attach_does_not_wait_for_prewarm() {
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider = Arc::new(GatedPrewarmProvider {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
        calls: Arc::clone(&calls),
    });

    let registry = crate::tool::Registry::new(provider.clone()).await;
    let defs = registry.definitions(None).await;
    assert!(
        !defs.is_empty(),
        "the tool surface must be available without waiting for a warmup"
    );

    // Release the gate so the spawned task cannot outlive the test.
    release.add_permits(8);
    let _ = tokio::time::timeout(Duration::from_secs(5), async { entered.acquire().await }).await;
}

struct LifecycleSocket {
    server_task: tokio::task::JoinHandle<anyhow::Result<()>>,
    writer: crate::transport::WriteHalf,
    reader: BufReader<crate::transport::ReadHalf>,
}

async fn spawn_lifecycle_socket(provider_template: Arc<dyn Provider>, sessions: SessionAgents) -> LifecycleSocket {
    let (server_stream, client_stream) = crate::transport::Stream::pair().expect("socket pair");
    let global_session_id = Arc::new(RwLock::new(String::new()));
    let client_count = Arc::new(RwLock::new(0usize));
    let client_connections = Arc::new(RwLock::new(HashMap::new()));
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let shared_context = Arc::new(RwLock::new(HashMap::new()));
    let swarm_plans = Arc::new(RwLock::new(HashMap::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::new()));
    let channel_subscriptions = Arc::new(RwLock::new(HashMap::new()));
    let channel_subscriptions_by_session = Arc::new(RwLock::new(HashMap::new()));
    let file_touch = FileTouchService::new();
    let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _) = broadcast::channel(8);
    let (global_event_tx, _) = broadcast::channel(8);
    let global_is_processing = Arc::new(RwLock::new(false));
    let shutdown_signals = Arc::new(RwLock::new(HashMap::new()));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let mcp_pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());

    let server_task = tokio::spawn(handle_client(
        server_stream,
        sessions,
        global_event_tx,
        provider_template,
        global_is_processing,
        global_session_id,
        client_count,
        client_connections,
        swarm_members,
        swarms_by_id,
        shared_context,
        swarm_plans,
        swarm_coordinators,
        file_touch,
        channel_subscriptions,
        channel_subscriptions_by_session,
        event_history,
        event_counter,
        swarm_event_tx,
        "horon-agent-test".to_string(),
        "🧪".to_string(),
        mcp_pool,
        shutdown_signals,
        soft_interrupt_queues,
        AwaitMembersRuntime::default(),
        SwarmMutationRuntime::default(),
        std::time::Duration::ZERO,
    ));

    let (client_reader, writer) = client_stream.into_split();
    LifecycleSocket {
        server_task,
        writer,
        reader: BufReader::new(client_reader),
    }
}

async fn send_client_request(socket: &mut LifecycleSocket, request: &Request) {
    let payload = serde_json::to_string(request).expect("serialize request") + "\n";
    socket
        .writer
        .write_all(payload.as_bytes())
        .await
        .expect("write request");
}

async fn next_client_event(socket: &mut LifecycleSocket, step: &str) -> ServerEvent {
    let mut line = String::new();
    let read = tokio::time::timeout(Duration::from_secs(10), socket.reader.read_line(&mut line)).await;
    let bytes = match read {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => panic!("socket read failed while waiting for {step}: {error}"),
        Err(_) => panic!("timed out waiting for {step}; no server event arrived"),
    };
    assert!(bytes > 0, "server closed the socket while waiting for {step}");
    let event = decode_request_or_event(&line);
    eprintln!("[{step}] {event:?}");
    event
}

async fn collect_client_events_until(
    socket: &mut LifecycleSocket,
    step: &str,
    mut finished: impl FnMut(&ServerEvent) -> bool,
) -> Vec<ServerEvent> {
    let mut events = Vec::new();
    for _ in 0..64 {
        let event = next_client_event(socket, step).await;
        let stop = finished(&event);
        events.push(event);
        if stop {
            return events;
        }
    }
    panic!("{step}: expected server event was not received: {events:?}");
}

async fn close_lifecycle_socket(socket: LifecycleSocket) {
    drop(socket.writer);
    tokio::time::timeout(Duration::from_secs(10), socket.server_task)
        .await
        .expect("client lifecycle should finish after the socket closes")
        .expect("server task join")
        .expect("server task result");
}

fn stored_user_message(id: &str, text: &str) -> crate::session::StoredMessage {
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

fn recorded_message_texts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .map(|message| {
            message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect()
}

#[derive(Clone, Debug)]
struct RecordedTurn {
    messages: Vec<String>,
    tool_count: usize,
}

const TARGET_SYSTEM_PROMPT: &str = "target-reattach-system-prompt";

#[derive(Clone)]
struct ContextRecordingProvider {
    recorded: Arc<std::sync::Mutex<Vec<RecordedTurn>>>,
    warmup_open: Arc<std::sync::atomic::AtomicBool>,
    warmup_entered: Arc<tokio::sync::Semaphore>,
    warmup_release: Arc<tokio::sync::Semaphore>,
    warmup_systems: Arc<std::sync::Mutex<Vec<bool>>>,
}

impl Default for ContextRecordingProvider {
    fn default() -> Self {
        Self {
            recorded: Arc::default(),
            warmup_open: Arc::default(),
            warmup_entered: Arc::new(tokio::sync::Semaphore::new(0)),
            warmup_release: Arc::new(tokio::sync::Semaphore::new(0)),
            warmup_systems: Arc::default(),
        }
    }
}

#[async_trait]
impl Provider for ContextRecordingProvider {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        self.recorded.lock().expect("record lock").push(RecordedTurn {
            messages: recorded_message_texts(messages),
            tool_count: tools.len(),
        });
        Ok(Box::pin(stream::iter(vec![
            Ok(StreamEvent::TextDelta("ok".to_string())),
            Ok(StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".to_string()),
            }),
        ])))
    }

    fn name(&self) -> &str {
        "context-recording"
    }

    fn model(&self) -> String {
        "context-recording".to_string()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }

    async fn prewarm(&self, _tools: &[ToolDefinition], system: &str) {
        self.warmup_systems
            .lock()
            .expect("warmup lock")
            .push(system.trim() == TARGET_SYSTEM_PROMPT);
        if !self.warmup_open.load(Ordering::SeqCst) {
            self.warmup_entered.add_permits(1);
            let _ = self.warmup_release.acquire().await;
        }
    }
}

impl ContextRecordingProvider {
    fn placeholder_warmups(&self) -> usize {
        self.warmup_systems
            .lock()
            .expect("warmup lock")
            .iter()
            .filter(|is_target| !**is_target)
            .count()
    }

    fn open_warmup_gate(&self) {
        self.warmup_open.store(true, Ordering::SeqCst);
        self.warmup_release.add_permits(1);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn target_aware_reattach_serves_history_without_a_placeholder_prewarm() {
    let _guard = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();

    let provider = Arc::new(ContextRecordingProvider::default());
    let provider_any: Arc<dyn Provider> = provider.clone();
    let target_session_id = "session_target_aware_pending_prewarm";
    let registry = Registry::new(provider_any.clone()).await;
    let mut session = crate::session::Session::create_with_id(target_session_id.to_string(), None, None);
    session.model = Some("context-recording".to_string());
    session.system_prompt = Some(TARGET_SYSTEM_PROMPT.to_string());
    session.ensure_initial_session_context_message();
    let mut messages = session.active_context().to_vec();
    messages.push(stored_user_message("msg-target", "persisted reconnect prompt"));
    session.replace_messages(messages);
    let target_agent = Arc::new(Mutex::new(Agent::new_with_session(
        provider_any.clone(),
        registry,
        session,
        None,
    )));
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([(
        target_session_id.to_string(),
        Arc::clone(&target_agent),
    )])));

    let working_dir = tempfile::tempdir().expect("working dir");
    let working_dir = working_dir.path().to_string_lossy().into_owned();
    let mut socket = spawn_lifecycle_socket(provider_any, Arc::clone(&sessions)).await;
    send_client_request(
        &mut socket,
        &Request::Subscribe {
            system_prompt: None,
            supports_user_response: false,
            supports_ask_requests: false,
            id: 1,
            working_dir: Some(working_dir),
            target_session_id: Some(target_session_id.to_string()),
            client_instance_id: Some("reattach-instance".to_string()),
            client_has_local_history: false,
            allow_session_takeover: false,
            crash_on_disconnect: false,
            continue_on_disconnect: false,
            terminal_env: Vec::new(),
            acp: None,
        },
    )
    .await;
    assert!(matches!(
        next_client_event(&mut socket, "subscribe ack").await,
        ServerEvent::Ack { id: 1 }
    ));
    let subscribe_events = collect_client_events_until(&mut socket, "subscribe done", |event| {
        matches!(event, ServerEvent::Done { id: 1 })
    })
    .await;
    assert!(
        !subscribe_events
            .iter()
            .any(|event| matches!(event, ServerEvent::Error { .. })),
        "reattach must not fail: {subscribe_events:?}"
    );
    let replayed = subscribe_events
        .iter()
        .find_map(|event| match event {
            ServerEvent::History { id, messages, .. } if *id == 1 => Some(messages),
            _ => None,
        })
        .expect("the reattaching client receives History during Subscribe");
    assert!(
        replayed
            .iter()
            .any(|message| message.content.contains("persisted reconnect prompt")),
        "history must carry the stored transcript: {replayed:?}"
    );

    send_client_request(&mut socket, &Request::GetHistory { id: 2 }).await;
    let history_events = collect_client_events_until(&mut socket, "get-history response", |event| {
        matches!(event, ServerEvent::History { id: 2, .. })
    })
    .await;
    assert!(
        !history_events
            .iter()
            .any(|event| matches!(event, ServerEvent::Error { .. })),
        "a pending provider warmup must not break the reconnect: {history_events:?}"
    );
    assert_eq!(
        provider.placeholder_warmups(),
        0,
        "a target-aware reattach must not start the placeholder warmup"
    );
    provider.open_warmup_gate();
    send_client_request(
        &mut socket,
        &Request::Message {
            id: 3,
            content: "after reattach".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
    )
    .await;
    let turn_events = collect_client_events_until(&mut socket, "post-reattach turn", |event| {
        matches!(event, ServerEvent::Done { id: 3 })
    })
    .await;
    assert!(
        !turn_events
            .iter()
            .any(|event| matches!(event, ServerEvent::Error { .. })),
        "a message after reattach must run on the adopted session: {turn_events:?}"
    );
    let recorded = provider.recorded.lock().expect("record lock").clone();
    assert_eq!(recorded.len(), 1, "the post-reattach message starts exactly one turn");
    assert!(recorded[0].tool_count > 0, "the adopted session keeps its tools");
    assert_eq!(
        recorded[0]
            .messages
            .iter()
            .filter(|text| text.contains("# Session Context"))
            .count(),
        1,
        "the adopted session keeps its own context exactly once: {:?}",
        recorded[0].messages
    );
    assert_eq!(
        provider.placeholder_warmups(),
        0,
        "no placeholder warmup may run for a target-aware reattach"
    );
    close_lifecycle_socket(socket).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_away_from_a_warming_session_releases_the_source_agent() {
    let _guard = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();

    let provider = Arc::new(ContextRecordingProvider::default());
    let provider_any: Arc<dyn Provider> = provider.clone();

    // The target is live in memory, so the resume adopts it and detaches the
    // connection's provisional source: that is the switch whose source Arc the
    // warmup could otherwise keep alive.
    let target_session_id = "session_switch_away_target";
    let registry = Registry::new(provider_any.clone()).await;
    let mut target = crate::session::Session::create_with_id(target_session_id.to_string(), None, None);
    target.model = Some("context-recording".to_string());
    target.replace_messages(vec![stored_user_message("msg-switch-target", "live target prompt")]);
    let target_agent = Arc::new(Mutex::new(Agent::new_with_session(
        provider_any.clone(),
        registry,
        target,
        None,
    )));
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([(
        target_session_id.to_string(),
        Arc::clone(&target_agent),
    )])));

    let mut socket = spawn_lifecycle_socket(provider_any, Arc::clone(&sessions)).await;
    let working_dir = tempfile::tempdir().expect("working dir");
    let working_dir = working_dir.path().to_string_lossy().into_owned();
    send_client_request(&mut socket, &subscribe_request(Some(&working_dir))).await;
    let subscribe_events = collect_client_events_until(&mut socket, "switch subscribe done", |event| {
        matches!(event, ServerEvent::Done { id: 1 })
    })
    .await;
    assert!(
        !subscribe_events
            .iter()
            .any(|event| matches!(event, ServerEvent::Error { .. })),
        "the fresh Subscribe must succeed: {subscribe_events:?}"
    );

    let warmup_entry = tokio::time::timeout(Duration::from_secs(5), provider.warmup_entered.acquire())
        .await
        .expect("the fresh session starts its deferred warmup")
        .expect("warmup entry permit");
    drop(warmup_entry);
    let source_session_id = sessions
        .read()
        .await
        .keys()
        .find(|id| id.as_str() != target_session_id)
        .cloned()
        .expect("the fresh subscribe registers its own session");
    let source_weak = {
        let source = sessions
            .read()
            .await
            .get(&source_session_id)
            .cloned()
            .expect("the live source agent");
        Arc::downgrade(&source)
    };
    assert!(source_weak.upgrade().is_some());
    // Observed inside the guard drop of the detached source: nothing may
    // reference it any more at that moment. A non-zero count would mean either
    // the caller's clone or the warmup clone outlived the source's barrier.
    let owner_refs_at_release = Arc::new(std::sync::Mutex::new(None));
    {
        let slot = Arc::clone(&owner_refs_at_release);
        let weak = source_weak.clone();
        crate::server::client_disconnect_cleanup::close_barrier_probe::on_release(&source_session_id, move || {
            *slot.lock().expect("observer lock") = Some(weak.strong_count())
        });
    }

    send_client_request(
        &mut socket,
        &Request::ResumeSession {
            id: 2,
            session_id: target_session_id.to_string(),
            client_instance_id: None,
            client_has_local_history: false,
            allow_session_takeover: false,
        },
    )
    .await;
    let resume_events = collect_client_events_until(&mut socket, "switch resume done", |event| {
        matches!(event, ServerEvent::Done { id: 2 })
    })
    .await;
    assert!(
        !resume_events
            .iter()
            .any(|event| matches!(event, ServerEvent::Error { .. })),
        "resuming away from the warming session must succeed: {resume_events:?}"
    );
    // Requests are handled in order, so this reply proves the settle that runs
    // inside the resume iteration has finished.
    send_client_request(&mut socket, &Request::GetHistory { id: 3 }).await;
    collect_client_events_until(&mut socket, "switch history response", |event| {
        matches!(event, ServerEvent::History { id: 3, .. })
    })
    .await;

    // The warmup provider gate is still closed, so only the connection's own
    // cancel-and-join could have released this Agent.
    assert_eq!(
        *owner_refs_at_release.lock().expect("observer lock"),
        Some(0),
        "the detached source's barrier opened with no remaining reference, so the caller's source clone was dropped before the guard and the warmup was settled before the handler ran"
    );
    assert!(
        source_weak.upgrade().is_none(),
        "leaving a warming session must release its Agent, and with it the session writer"
    );
    assert!(
        !sessions.read().await.contains_key(&source_session_id),
        "the detached source is no longer registered"
    );
    assert!(sessions.read().await.contains_key(target_session_id));
    close_lifecycle_socket(socket).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fresh_subscribe_and_first_message_confirm_the_context_once() {
    let _guard = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();

    let provider = Arc::new(ContextRecordingProvider::default());
    let provider_template: Arc<dyn Provider> = provider.clone();
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::new()));
    let mut socket = spawn_lifecycle_socket(provider_template, Arc::clone(&sessions)).await;
    let working_dir = tempfile::tempdir().expect("working dir");
    let working_dir = working_dir.path().to_string_lossy().into_owned();

    send_client_request(&mut socket, &subscribe_request(Some(&working_dir))).await;
    let subscribe_events = collect_client_events_until(&mut socket, "fresh subscribe done", |event| {
        matches!(event, ServerEvent::Done { id: 1 })
    })
    .await;
    assert!(
        !subscribe_events
            .iter()
            .any(|event| matches!(event, ServerEvent::Error { .. })),
        "a fresh Subscribe must succeed: {subscribe_events:?}"
    );

    let warmup_entry = tokio::time::timeout(Duration::from_secs(5), provider.warmup_entered.acquire())
        .await
        .expect("a fresh Subscribe starts the deferred provider warmup")
        .expect("warmup entry permit");
    drop(warmup_entry);
    assert!(
        provider.placeholder_warmups() >= 1,
        "a warmup for this fresh session entered while the gate was closed and Done was already received"
    );
    provider.open_warmup_gate();

    send_client_request(
        &mut socket,
        &Request::Message {
            id: 2,
            content: "hello once".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
    )
    .await;
    let turn_events = collect_client_events_until(&mut socket, "first message turn", |event| {
        matches!(event, ServerEvent::Done { id: 2 })
    })
    .await;
    assert!(
        !turn_events
            .iter()
            .any(|event| matches!(event, ServerEvent::Error { .. })),
        "the first message must run: {turn_events:?}"
    );

    let recorded = provider.recorded.lock().expect("record lock").clone();
    assert_eq!(recorded.len(), 1, "the first message starts exactly one provider turn");
    let turn = &recorded[0];
    assert!(turn.tool_count > 0, "the turn keeps the configured tools: {turn:?}");
    assert_eq!(
        turn.messages
            .iter()
            .filter(|text| text.contains("# Session Context"))
            .count(),
        1,
        "the session context reaches the provider exactly once: {:?}",
        turn.messages
    );
    assert_eq!(
        turn.messages.iter().filter(|text| text.contains("hello once")).count(),
        1
    );

    send_client_request(&mut socket, &Request::GetHistory { id: 3 }).await;
    let history_events = collect_client_events_until(&mut socket, "history response", |event| {
        matches!(event, ServerEvent::History { id: 3, .. })
    })
    .await;
    let history = history_events
        .iter()
        .find_map(|event| match event {
            ServerEvent::History { id, messages, .. } if *id == 3 => Some(messages),
            _ => None,
        })
        .expect("history response");
    assert_eq!(
        history
            .iter()
            .filter(|message| message.content.contains("hello once"))
            .count(),
        1,
        "the visible conversation round-trips: {history:?}"
    );
    let session_id = sessions
        .read()
        .await
        .keys()
        .next()
        .cloned()
        .expect("the fresh subscribe registers its session");
    let persisted = crate::session::Session::load(&session_id).expect("persisted session");
    assert_eq!(
        persisted
            .active_context()
            .iter()
            .filter(|message| message.content_preview().contains("# Session Context"))
            .count(),
        1,
        "the stored transcript keeps exactly one session context"
    );
    close_lifecycle_socket(socket).await;
}

/// A request tag this build does not know is refused without taking the
/// connection down.
///
/// Removed surfaces (the mirror-mode push among them) reach the daemon as an
/// unknown tag after an old client upgrades. They must fail to decode: the
/// daemon answers an uncorrelated error and keeps reading the socket.
#[tokio::test(flavor = "multi_thread")]
async fn unknown_request_tag_is_refused_and_the_socket_survives() {
    let provider: Arc<dyn Provider> = Arc::new(ContextRecordingProvider::default());
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::new()));
    let mut socket = spawn_lifecycle_socket(provider, Arc::clone(&sessions)).await;

    socket
        .writer
        .write_all(b"{\"type\":\"no_such_control_tag\",\"id\":71,\"session_id\":\"s\"}\n")
        .await
        .expect("write unknown request");
    match next_client_event(&mut socket, "unknown request").await {
        ServerEvent::Error { id, message, .. } => {
            assert_eq!(id, 0, "an undecodable line has no request id to answer");
            assert!(message.contains("Invalid request"), "{message}");
        }
        other => panic!("expected an invalid-request error, got {other:?}"),
    }

    send_client_request(&mut socket, &Request::Ping { id: 72 }).await;
    match next_client_event(&mut socket, "ping after refusal").await {
        ServerEvent::Pong { id, .. } => assert_eq!(id, 72, "the connection must still answer"),
        other => panic!("expected a pong, got {other:?}"),
    }
    close_lifecycle_socket(socket).await;
}
