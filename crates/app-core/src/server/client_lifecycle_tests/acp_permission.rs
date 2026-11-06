use super::*;
use crate::config::ApprovalMode;
use serde_json::json;
use std::collections::VecDeque;

/// A daemon wake turn that takes the idle agent between the ACP turn's start
/// and the moment that turn holds the agent must not run under the ACP claim.
#[tokio::test]
async fn permission_bridge_not_owned_by_daemon_turn_in_claim_window() {
    let _guard = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    let session_id = "session_acp_claim_window";
    let provider = AcpBridgeTurnProvider::new();
    let turn_gate = provider.hold_turn();
    let provider: Arc<dyn Provider> = Arc::new(provider);
    let registry = Registry::new(Arc::clone(&provider)).await;
    let mut session = crate::session::Session::create_with_id(session_id.to_string(), None, None);
    session.model = Some("acp-bridge-turn".to_string());
    let agent = Arc::new(Mutex::new(Agent::new_with_session(provider, registry, session, None)));

    let (client_tx, mut client_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let acp_token = crate::tool::acp_session::install(session_id, "conn-acp", client_tx.clone());
    let bridge_token = crate::tool::permission_bridge::install(session_id, "conn-acp", client_tx.clone());
    let override_token = crate::tool::approval_override::set(session_id, "conn-acp", ApprovalMode::Write);

    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _) = broadcast::channel(8);
    let (processing_done_tx, _processing_done_rx) = mpsc::unbounded_channel();
    let mut client_is_processing = false;
    let mut processing_message_id = None;
    let mut processing_session_id = None;
    let mut processing_task = None;

    start_processing_message(
        ProcessingMessage {
            id: 61,
            content: "editor turn".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            v2: None,
        },
        session_id,
        Some("conn-acp"),
        &mut ProcessingState {
            client_is_processing: &mut client_is_processing,
            message_id: &mut processing_message_id,
            session_id: &mut processing_session_id,
            task: &mut processing_task,
            turn_acquired: &Arc::new(AtomicBool::new(false)),
        },
        &agent,
        &client_tx,
        &processing_done_tx,
        Vec::new(),
        &SwarmStatusRefs {
            members: &swarm_members,
            swarms_by_id: &swarms_by_id,
            event_history: &event_history,
            event_counter: &event_counter,
            event_tx: &swarm_event_tx,
        },
    )
    .await;

    // On this current-thread runtime the accepted ACP turn has not run yet, so
    // the agent is idle: exactly the moment a daemon wake turn can take it.
    let daemon_turn = agent.try_lock().expect("the idle agent is free for a daemon wake turn");
    let assert_daemon_turn_owns_nothing = || {
        assert!(
            !crate::tool::acp_session::turn_owned_by(session_id, "conn-acp"),
            "the daemon turn holding the agent must not run under the ACP claim"
        );
        assert!(
            crate::tool::permission_bridge::for_session(session_id).is_none(),
            "the daemon turn's tools must not ask the editor"
        );
        assert_eq!(
            crate::tool::approval_override::get(session_id),
            None,
            "the daemon turn keeps the configured approval mode"
        );
    };
    assert_daemon_turn_owns_nothing();
    tokio::task::yield_now().await;
    assert_daemon_turn_owns_nothing();
    drop(daemon_turn);

    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), client_rx.recv())
            .await
            .expect("the ACP turn starts once the agent is free")
            .expect("the client channel stays open");
        if matches!(event, ServerEvent::TextDelta { ref text } if text == "turn started") {
            break;
        }
    }
    assert!(
        crate::tool::permission_bridge::for_session(session_id).is_some(),
        "the ACP turn owns the bridge once it holds the agent"
    );
    assert_eq!(
        crate::tool::approval_override::get(session_id),
        Some(ApprovalMode::Write)
    );

    turn_gate.send(true).expect("the turn gate stays open");
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), client_rx.recv())
            .await
            .expect("the ACP turn finishes")
            .expect("the client channel stays open");
        if matches!(event, ServerEvent::Done { id: 61 }) {
            break;
        }
    }
    if let Some(task) = processing_task.take() {
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .expect("the turn task ends")
            .expect("the turn task does not panic");
    }
    assert!(
        crate::tool::permission_bridge::for_session(session_id).is_none(),
        "a finished ACP turn leaves no owner behind"
    );

    crate::tool::approval_override::clear(session_id, override_token);
    crate::tool::permission_bridge::clear(session_id, bridge_token);
    crate::tool::acp_session::clear(session_id, acp_token);
}

/// Answers each provider call with the next scripted response.
#[derive(Clone, Default)]
struct ScriptedToolProvider {
    script: Arc<std::sync::Mutex<VecDeque<Vec<StreamEvent>>>>,
}

impl ScriptedToolProvider {
    fn push_tool_call(&self, call_id: &str, tool: &str, input: serde_json::Value) {
        let mut script = self.script.lock().expect("script");
        script.push_back(vec![
            StreamEvent::ToolUseStart {
                id: call_id.to_string(),
                name: tool.to_string(),
            },
            StreamEvent::ToolInputDelta(input.to_string()),
            StreamEvent::ToolUseEnd,
            StreamEvent::MessageEnd {
                stop_reason: Some("tool_use".to_string()),
            },
        ]);
        script.push_back(vec![
            StreamEvent::TextDelta("tool round finished".to_string()),
            StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".to_string()),
            },
        ]);
    }
}

#[async_trait]
impl Provider for ScriptedToolProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let events = self.script.lock().expect("script").pop_front().unwrap_or_else(|| {
            vec![
                StreamEvent::TextDelta("nothing scripted".to_string()),
                StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                },
            ]
        });
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }

    fn name(&self) -> &str {
        "scripted-tool"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

async fn send_line(writer: &mut crate::transport::WriteHalf, value: serde_json::Value) {
    writer.write_all(format!("{value}\n").as_bytes()).await.unwrap();
}

/// Read events until `stop` matches, failing on any permission request.
async fn until_without_prompt(
    reader: &mut BufReader<crate::transport::ReadHalf>,
    stop: impl Fn(&ServerEvent) -> bool,
) -> ServerEvent {
    read_until(reader, stop, true).await
}

async fn read_until(
    reader: &mut BufReader<crate::transport::ReadHalf>,
    stop: impl Fn(&ServerEvent) -> bool,
    forbid_prompt: bool,
) -> ServerEvent {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let mut line = String::new();
            assert!(
                reader.read_line(&mut line).await.unwrap() > 0,
                "the daemon closed the stream"
            );
            let event: ServerEvent = serde_json::from_str(&line).unwrap();
            if forbid_prompt && matches!(event, ServerEvent::ToolPermissionRequested { .. }) {
                panic!("no permission request may reach the editor here, got {event:?}");
            }
            if stop(&event) {
                return event;
            }
        }
    })
    .await
    .expect("daemon event timeout")
}

/// Manual QA on the real connection loop: an ACP connection in mode "write"
/// is asked before a shell call and its answer decides whether the call runs;
/// a file write runs without asking; a plain connection's turn on the same
/// session is never routed to the editor.
#[tokio::test]
async fn acp_permission_bridge_round_trip_and_plain_turn_never_prompts() {
    let _lock = crate::storage::lock_test_env();
    let home = IsolatedReloadRecoveryEnv::new();
    crate::config::invalidate_config_cache();
    let work = home._home.path().to_path_buf();
    let provider = ScriptedToolProvider::default();
    let provider_template: Arc<dyn Provider> = Arc::new(provider.clone());

    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::new()));
    let global_session_id = Arc::new(RwLock::new(String::new()));
    let client_count = Arc::new(RwLock::new(0usize));
    let client_connections = Arc::new(RwLock::new(HashMap::new()));
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let shared_context = Arc::new(RwLock::new(HashMap::new()));
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

    let connect = || {
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

    let (acp_task, acp_stream) = connect();
    let (acp_reader, mut acp) = acp_stream.into_split();
    let mut acp_reader = BufReader::new(acp_reader);
    send_line(
        &mut acp,
        json!({
            "type": "subscribe",
            "id": 1,
            "working_dir": work,
            "acp": {"client_capabilities": {}, "permission_requests": true},
        }),
    )
    .await;
    until_without_prompt(&mut acp_reader, |event| matches!(event, ServerEvent::Done { id: 1 })).await;
    let session_id = sessions
        .read()
        .await
        .keys()
        .next()
        .expect("a subscribed session")
        .clone();

    send_line(
        &mut acp,
        json!({"type": "set_session_approval_mode", "id": 2, "mode": "all"}),
    )
    .await;
    let refused = until_without_prompt(&mut acp_reader, |event| {
        matches!(event, ServerEvent::Ack { id: 2 } | ServerEvent::Error { id: 2, .. })
    })
    .await;
    assert!(
        matches!(refused, ServerEvent::Error { ref message, .. } if message.contains("use `off` or `write`")),
        "{refused:?}"
    );
    send_line(
        &mut acp,
        json!({"type": "set_session_approval_mode", "id": 3, "mode": "write"}),
    )
    .await;
    let accepted = until_without_prompt(&mut acp_reader, |event| {
        matches!(event, ServerEvent::Ack { id: 3 } | ServerEvent::Error { id: 3, .. })
    })
    .await;
    assert!(matches!(accepted, ServerEvent::Ack { id: 3 }), "{accepted:?}");

    // Approve: the shell call waits for the editor, then runs.
    let approved_marker = work.join("approved.marker");
    provider.push_tool_call(
        "call-approve",
        "bash",
        json!({"command": format!("touch '{}'", approved_marker.display()), "intent": "qa"}),
    );
    send_line(&mut acp, json!({"type": "message", "id": 10, "content": "approve run"})).await;
    let request = read_until(
        &mut acp_reader,
        |event| matches!(event, ServerEvent::ToolPermissionRequested { .. }),
        false,
    )
    .await;
    let ServerEvent::ToolPermissionRequested {
        request_id,
        tool_name,
        tool_call_id,
        ..
    } = request
    else {
        unreachable!()
    };
    assert_eq!(tool_name, "bash");
    assert_eq!(tool_call_id, "call-approve");
    assert!(!approved_marker.exists(), "the shell call waits for the editor");
    send_line(
        &mut acp,
        json!({"type": "permission_decision", "id": 11, "request_id": request_id, "decision": "allow"}),
    )
    .await;
    let done = until_without_prompt(&mut acp_reader, |event| {
        matches!(event, ServerEvent::Done { id: 10 } | ServerEvent::Error { id: 11, .. })
    })
    .await;
    assert!(matches!(done, ServerEvent::Done { id: 10 }), "{done:?}");
    assert!(approved_marker.exists(), "the approved shell call ran");
    send_line(
        &mut acp,
        json!({"type": "permission_decision", "id": 12, "request_id": request_id, "decision": "deny"}),
    )
    .await;
    // The connection loop acknowledges a decision on receipt; the refusal
    // follows as an error for the same id.
    let repeated = until_without_prompt(&mut acp_reader, |event| {
        matches!(event, ServerEvent::Error { id: 12, .. })
    })
    .await;
    assert!(
        matches!(repeated, ServerEvent::Error { ref message, .. } if message.contains("already decided")),
        "a second answer is refused: {repeated:?}"
    );

    // Deny: the shell call never runs.
    let denied_marker = work.join("denied.marker");
    provider.push_tool_call(
        "call-deny",
        "bash",
        json!({"command": format!("touch '{}'", denied_marker.display()), "intent": "qa"}),
    );
    send_line(&mut acp, json!({"type": "message", "id": 20, "content": "deny run"})).await;
    let request = read_until(
        &mut acp_reader,
        |event| matches!(event, ServerEvent::ToolPermissionRequested { .. }),
        false,
    )
    .await;
    let ServerEvent::ToolPermissionRequested { request_id, .. } = request else {
        unreachable!()
    };
    send_line(
        &mut acp,
        json!({"type": "permission_decision", "id": 21, "request_id": request_id, "decision": "deny"}),
    )
    .await;
    until_without_prompt(&mut acp_reader, |event| matches!(event, ServerEvent::Done { id: 20 })).await;
    assert!(!denied_marker.exists(), "a denied shell call never runs");

    // A plain file write runs in write mode without asking.
    let written = work.join("written.txt");
    provider.push_tool_call(
        "call-write",
        "write",
        json!({"file_path": written.display().to_string(), "content": "from the editor turn", "intent": "qa"}),
    );
    send_line(&mut acp, json!({"type": "message", "id": 30, "content": "write run"})).await;
    until_without_prompt(&mut acp_reader, |event| matches!(event, ServerEvent::Done { id: 30 })).await;
    assert_eq!(
        std::fs::read_to_string(&written).expect("the write ran without a request"),
        "from the editor turn"
    );

    // A plain connection's turn on the same session never reaches the editor
    // and does not inherit the editor's "write" mode.
    let (plain_task, plain_stream) = connect();
    let (plain_reader, mut plain) = plain_stream.into_split();
    let mut plain_reader = BufReader::new(plain_reader);
    send_line(
        &mut plain,
        json!({"type": "subscribe", "id": 50, "working_dir": work, "target_session_id": session_id}),
    )
    .await;
    until_without_prompt(&mut plain_reader, |event| matches!(event, ServerEvent::Done { id: 50 })).await;
    let plain_marker = work.join("plain.marker");
    provider.push_tool_call(
        "call-plain",
        "bash",
        json!({"command": format!("touch '{}'", plain_marker.display()), "intent": "qa"}),
    );
    send_line(&mut plain, json!({"type": "message", "id": 51, "content": "plain run"})).await;
    until_without_prompt(&mut plain_reader, |event| matches!(event, ServerEvent::Done { id: 51 })).await;
    until_without_prompt(&mut acp_reader, |event| matches!(event, ServerEvent::Done { id: 51 })).await;
    assert!(
        plain_marker.exists(),
        "the plain turn uses the configured mode (off), not the editor's write mode"
    );

    // Disconnecting the editor withdraws its bridge and its mode.
    sessions.write().await.clear();
    drop(acp);
    drop(acp_reader);
    tokio::time::timeout(Duration::from_secs(10), acp_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!crate::tool::approval_override::is_installed(&session_id));
    assert!(crate::tool::permission_bridge::bridge_for_connection(&session_id, "any").is_none());
    assert!(!crate::tool::acp_session::is_acp(&session_id));
    drop(plain);
    drop(plain_reader);
    tokio::time::timeout(Duration::from_secs(10), plain_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
