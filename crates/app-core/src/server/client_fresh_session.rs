use super::*;

pub(super) struct FreshSessionContext<'a> {
    pub(super) id: u64,
    pub(super) client_is_processing: bool,
    pub(super) client_event_tx: &'a mpsc::UnboundedSender<ServerEvent>,
    pub(super) prewarm_task: &'a mut Option<tokio::task::JoinHandle<()>>,
    pub(super) client_session_id: &'a mut String,
    pub(super) friendly_name: &'a mut Option<String>,
    pub(super) session_control: &'a mut SessionControlHandle,
    pub(super) provisional_session: &'a mut bool,
    pub(super) agent: &'a mut Arc<Mutex<Agent>>,
    pub(super) registry: &'a mut Registry,
    pub(super) provider: &'a mut Arc<dyn Provider>,
    pub(super) subscribe_working_dir: Option<&'a str>,
    pub(super) initial_working_dir: &'a str,
    pub(super) terminal_env: &'a [(String, String)],
    pub(super) provider_template: &'a Arc<dyn Provider>,
    pub(super) client_connection_id: &'a str,
    pub(super) client_connections: &'a Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    pub(super) client_session_id_for_events: &'a Arc<std::sync::RwLock<String>>,
    pub(super) sessions: &'a SessionAgents,
    pub(super) shutdown_signals: &'a Arc<RwLock<HashMap<String, InterruptSignal>>>,
    pub(super) soft_interrupt_queues: &'a SessionInterruptQueues,
    pub(super) swarm_members: &'a Arc<RwLock<HashMap<String, SwarmMember>>>,
    pub(super) swarms_by_id: &'a Arc<RwLock<HashMap<String, HashSet<String>>>>,
    pub(super) file_touch: &'a FileTouchService,
    pub(super) swarm_plans: &'a Arc<RwLock<HashMap<String, VersionedPlan>>>,
    pub(super) swarm_coordinators: &'a Arc<RwLock<HashMap<String, String>>>,
}

pub(super) async fn switch_to_fresh_session(ctx: FreshSessionContext<'_>) -> bool {
    if ctx.client_is_processing {
        if let Err(err) = ctx.client_event_tx.send(ServerEvent::Error {
            id: ctx.id,
            message: "Cannot create a new session while a turn is active on this connection".to_string(),
            retry_after_secs: None,
        }) {
            crate::logging::warn(&format!("Failed to send fresh-session busy error: {err}"));
        }
        return false;
    }
    if let Some(handle) = ctx.prewarm_task.take() {
        handle.abort();
    }
    let old_session_id = ctx.client_session_id.clone();
    super::super::state::unregister_session_event_sender(ctx.swarm_members, &old_session_id, ctx.client_connection_id)
        .await;
    let working_dir_for_agent = ctx.subscribe_working_dir.unwrap_or(ctx.initial_working_dir);
    let new_provider = ctx.provider_template.fork_for_new_session();
    let new_registry = Registry::new(new_provider.clone()).await;
    let mut new_agent = crate::hooks::with_client_terminal_env(ctx.terminal_env.to_vec(), async {
        Agent::new_provisional_with_initial_working_dir(
            Arc::clone(&new_provider),
            new_registry.clone(),
            Some(working_dir_for_agent),
        )
    })
    .await;
    new_agent.set_memory_enabled(crate::config::config().features.memory);
    new_agent.set_persistent_runtime(true);
    let new_session_id = new_agent.session_id().to_string();
    *ctx.friendly_name = new_agent.session_short_name().map(str::to_string);
    *ctx.session_control = SessionControlHandle::new(
        new_session_id.clone(),
        new_agent.soft_interrupt_queue(),
        new_agent.background_tool_signal(),
        new_agent.graceful_shutdown_signal(),
    );
    {
        let mut signals = ctx.shutdown_signals.write().await;
        signals.insert(new_session_id.clone(), ctx.session_control.stop_current_turn_signal());
    }
    register_session_interrupt_queue(
        ctx.soft_interrupt_queues,
        &new_session_id,
        new_agent.soft_interrupt_queue(),
    )
    .await;
    let new_agent_arc = Arc::new(Mutex::new(new_agent));
    {
        let mut sessions_guard = ctx.sessions.write().await;
        sessions_guard.insert(new_session_id.clone(), Arc::clone(&new_agent_arc));
    }
    {
        let mut connections = ctx.client_connections.write().await;
        if let Some(info) = connections.get_mut(ctx.client_connection_id) {
            info.session_id = new_session_id.clone();
            info.is_processing = false;
            info.current_tool_name = None;
        }
    }
    *ctx.client_session_id_for_events
        .write()
        .unwrap_or_else(|e| e.into_inner()) = new_session_id.clone();
    *ctx.client_session_id = new_session_id;
    let old_agent = std::mem::replace(ctx.agent, new_agent_arc);
    let detached = super::super::client_session::cleanup_detached_source_session_if_unused(
        &old_session_id,
        ctx.client_connection_id,
        old_agent,
        ctx.sessions,
        ctx.shutdown_signals,
        ctx.soft_interrupt_queues,
        ctx.client_connections,
        ctx.swarm_members,
        ctx.swarms_by_id,
        ctx.file_touch,
        ctx.swarm_plans,
        ctx.swarm_coordinators,
    )
    .await;
    super::super::client_disconnect_cleanup::blocking_session_io(move || drop(detached));
    crate::process_memory::release_retained_heap_debounced("server_session_switched", Duration::from_secs(5));
    *ctx.registry = new_registry;
    *ctx.provider = new_provider;
    *ctx.provisional_session = true;
    crate::runtime_memory_log::emit_event(
        crate::runtime_memory_log::RuntimeMemoryLogEvent::new("session_created", "new_live_session_attached")
            .with_session_id(ctx.client_session_id.clone())
            .force_attribution(),
    );
    true
}
