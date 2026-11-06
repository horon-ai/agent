use crate::test_support::*;

#[tokio::test]
async fn resume_session_restores_persisted_compaction_for_provider_context() -> Result<()> {
    let _env = setup_test_env()?;
    let runtime_dir = short_runtime_dir(format!(
        "horon-agent-compaction-resume-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir)?;
    let socket_path = runtime_dir.join("horon-agent.sock");
    let debug_socket_path = runtime_dir.join("horon-agent-debug.sock");

    let provider = CapturingCompactionProvider::new();
    let captured_messages = provider.captured_messages();
    let provider: Arc<dyn Provider> = Arc::new(provider);
    let server_instance = server::Server::new_with_paths(provider, socket_path.clone(), debug_socket_path.clone());
    let server_handle = tokio::spawn(async move { server_instance.run().await });

    let result = async {
        let mut session = Session::create_with_id(
            "session_resume_compaction_restore_test".to_string(),
            None,
            Some("resume compaction restore test".to_string()),
        );
        session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: "older user turn".to_string(),
                cache_control: None,
            }],
        );
        session.add_message(
            Role::Assistant,
            vec![ContentBlock::Text {
                text: "older assistant turn".to_string(),
                cache_control: None,
            }],
        );
        session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: "recent preserved turn".to_string(),
                cache_control: None,
            }],
        );
        session.compaction = Some(StoredCompactionState {
            summary_text: "Worked on Gemini OAuth reload fixes.".to_string(),
            openai_encrypted_content: None,
            native_window: Vec::new(),
            native_route: None,
            covers_up_to_turn: 2,
            original_turn_count: 2,
            compacted_count: 2,
            structured: None,
        });
        session.save()?;

        wait_for_server_ready(&socket_path, &debug_socket_path).await?;
        let mut client = server::Client::connect_with_path(socket_path.clone()).await?;

        let subscribe_id = client.subscribe().await?;
        let _ = collect_until_done_unix(&mut client, subscribe_id).await?;

        let resume_id = client.resume_session(&session.id).await?;
        let _ = collect_until_history_unix(&mut client, resume_id).await?;

        let message_id = client.send_message("continue from the restored session").await?;
        let mut seen_events = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let event = timeout(Duration::from_secs(1), client.read_event()).await??;
            let is_done = matches!(event, ServerEvent::Done { id } if id == message_id);
            let is_error = matches!(event, ServerEvent::Error { id, .. } if id == message_id);
            seen_events.push(format!("{event:?}"));
            if is_done {
                break;
            }
            if is_error {
                anyhow::bail!(
                    "message request failed while validating compaction restore: {}",
                    seen_events.join(" | ")
                );
            }
        }

        let captured = captured_messages.lock().unwrap();
        assert_eq!(captured.len(), 1, "expected exactly one provider completion call");
        let provider_messages = &captured[0];
        assert!(
            provider_messages.len() >= 3,
            "expected summary + preserved tail + new user message"
        );

        let summary_text = flatten_text_blocks(&provider_messages[0]);
        assert!(summary_text.contains("Previous Conversation Summary"));
        assert!(summary_text.contains("Gemini OAuth reload fixes"));

        let joined = provider_messages
            .iter()
            .map(flatten_text_blocks)
            .collect::<Vec<_>>()
            .join("\n---\n");
        assert!(joined.contains("recent preserved turn"));
        assert!(joined.contains("continue from the restored session"));
        assert!(!joined.contains("older user turn"));
        assert!(!joined.contains("older assistant turn"));

        // The same session exists as originals in the event log, and the store
        // carries no SQLite database: the session survives on the event log
        // alone, so a restart can replay it.
        let home = std::env::var("HORON_AGENT_HOME").expect("HORON_AGENT_HOME");
        let store_dir = std::path::Path::new(&home).join("sessions");
        let log = store_dir
            .join("session_resume_compaction_restore_test")
            .join("events.jsonl");
        assert!(
            log.is_file(),
            "the session history lives in the event log: {}",
            log.display()
        );
        let sqlite: Vec<String> = std::fs::read_dir(&store_dir)
            .expect("read the session store")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains("sqlite"))
            .collect();
        assert!(
            sqlite.is_empty(),
            "the session store holds no SQLite database: {sqlite:?}"
        );
        let paths = horon_agent::session::event_store_api::SessionPaths::new(store_dir.clone());
        let session_id =
            horon_agent::session::event_store_api::SessionId::parse(
                "session_resume_compaction_restore_test",
            )
            .expect("session id");
        let graph = horon_agent::session::event_store_api::EventGraph::open(&paths, &session_id)
            .expect("open the graph");
        let projection = graph.replay(None).expect("replay the originals");
        let kinds: Vec<String> = projection
            .messages()
            .iter()
            .map(|message| serde_json::to_string(&message.message).expect("message json"))
            .collect();
        assert!(
            kinds.iter().any(|row| row.contains("recent preserved turn")),
            "the preserved turn is durable: {kinds:?}"
        );
        assert!(
            projection.compaction().is_none(),
            "a session seeded through Session::save restores its compaction from the session record, not a Compacted event"
        );
        assert!(
            graph
                .heads()
                .iter()
                .all(|head| graph.replay(Some(head)).is_ok()),
            "every retained head of the persisted session replays cleanly"
        );

        Ok::<_, anyhow::Error>(())
    }
    .await;

    abort_server_and_cleanup(&server_handle, &socket_path, &debug_socket_path);
    result
}

/// A retained native compaction window must survive save → resume → retransmit
/// with the same items in the same order.
#[tokio::test]
async fn resume_session_replays_the_retained_native_compaction_window() -> Result<()> {
    let _env = setup_test_env()?;
    let runtime_dir = short_runtime_dir(format!(
        "horon-agent-native-window-resume-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir)?;
    let socket_path = runtime_dir.join("horon-agent.sock");
    let debug_socket_path = runtime_dir.join("horon-agent-debug.sock");

    let provider = CapturingCompactionProvider::new();
    let captured_messages = provider.captured_messages();
    let provider_name = Provider::name(&provider).to_string();
    let provider_model = Provider::model(&provider);
    let provider: Arc<dyn Provider> = Arc::new(provider);
    let server_instance = server::Server::new_with_paths(provider, socket_path.clone(), debug_socket_path.clone());
    let server_handle = tokio::spawn(async move { server_instance.run().await });

    // The canonical next context window: a retained message, the compaction
    // item, and a retained reasoning item, in that order.
    let window = vec![
        serde_json::json!({
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": "retained before compaction" }],
        }),
        serde_json::json!({
            "type": "compaction",
            "id": "cmp_e2e",
            "encrypted_content": "enc_e2e_window",
        }),
        serde_json::json!({
            "type": "reasoning",
            "id": "rs_e2e",
            "summary": [],
            "encrypted_content": "enc_e2e_reasoning",
        }),
    ];
    let expected_items: Vec<String> = window.iter().map(|item| item.to_string()).collect();

    let result = async {
        let mut session = Session::create_with_id(
            "session_native_window_resume_test".to_string(),
            None,
            Some("native window resume test".to_string()),
        );
        session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: "older user turn".to_string(),
                cache_control: None,
            }],
        );
        session.add_message(
            Role::Assistant,
            vec![ContentBlock::Text {
                text: "older assistant turn".to_string(),
                cache_control: None,
            }],
        );
        session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: "recent preserved turn".to_string(),
                cache_control: None,
            }],
        );
        session.compaction = Some(StoredCompactionState {
            summary_text: String::new(),
            openai_encrypted_content: Some("enc_e2e_window".to_string()),
            native_window: window.clone(),
            native_route: Some(horon_agent::session::StoredNativeRoute {
                provider: provider_name.clone(),
                model: provider_model.clone(),
                route: None,
            }),
            covers_up_to_turn: 2,
            original_turn_count: 2,
            compacted_count: 2,
            structured: None,
        });
        session.save()?;

        wait_for_server_ready(&socket_path, &debug_socket_path).await?;
        let mut client = server::Client::connect_with_path(socket_path.clone()).await?;

        let subscribe_id = client.subscribe().await?;
        let _ = collect_until_done_unix(&mut client, subscribe_id).await?;

        let resume_id = client.resume_session(&session.id).await?;
        let _ = collect_until_history_unix(&mut client, resume_id).await?;

        let message_id = client.send_message("continue after the native window").await?;
        let mut seen_events = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let event = timeout(Duration::from_secs(1), client.read_event()).await??;
            let is_done = matches!(event, ServerEvent::Done { id } if id == message_id);
            let is_error = matches!(event, ServerEvent::Error { id, .. } if id == message_id);
            seen_events.push(format!("{event:?}"));
            if is_done {
                break;
            }
            if is_error {
                anyhow::bail!(
                    "message request failed while validating the native window: {}",
                    seen_events.join(" | ")
                );
            }
        }

        let captured = captured_messages.lock().unwrap();
        assert_eq!(captured.len(), 1, "expected exactly one provider completion call");
        let provider_messages = &captured[0];
        let window_block = provider_messages
            .iter()
            .flat_map(|message| message.content.iter())
            .find_map(|block| match block {
                ContentBlock::OpenAICompaction { window, .. } => Some(window),
                _ => None,
            })
            .expect("the restored native window must be retransmitted");

        assert_eq!(
            window_block.iter().map(|item| item.to_string()).collect::<Vec<_>>(),
            expected_items,
            "save/resume/retransmit must preserve the window bytes and order"
        );

        let joined = provider_messages
            .iter()
            .map(flatten_text_blocks)
            .collect::<Vec<_>>()
            .join("\n---\n");
        assert!(joined.contains("recent preserved turn"));
        assert!(joined.contains("continue after the native window"));
        assert!(!joined.contains("older user turn"));
        assert!(!joined.contains("older assistant turn"));

        Ok::<_, anyhow::Error>(())
    }
    .await;

    abort_server_and_cleanup(&server_handle, &socket_path, &debug_socket_path);
    result
}

/// Test that a simple text response works
#[tokio::test]
async fn test_simple_response() -> Result<()> {
    let _env = setup_test_env()?;
    let provider = MockProvider::new();

    // Queue a simple response
    provider.queue_response(vec![
        StreamEvent::TextDelta("Hello! ".to_string()),
        StreamEvent::TextDelta("How can I help?".to_string()),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".to_string()),
        },
        StreamEvent::SessionId("test-session-123".to_string()),
    ]);

    let provider: Arc<dyn horon_agent::provider::Provider> = Arc::new(provider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    let response = agent.run_once_capture("Say hello").await?;
    let saved = Session::load(agent.session_id())?;

    assert_eq!(response, "Hello! How can I help?");
    assert!(saved.is_debug, "test sessions should be marked debug");
    Ok(())
}

#[tokio::test]
async fn test_agent_clear_preserves_debug_flag() -> Result<()> {
    let _env = setup_test_env()?;
    let provider = MockProvider::new();
    let provider: Arc<dyn horon_agent::provider::Provider> = Arc::new(provider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.set_debug(true);
    let old_session_id = agent.session_id().to_string();

    agent.clear();

    assert_eq!(agent.session_id(), old_session_id);
    assert!(agent.is_debug());
    Ok(())
}

#[tokio::test]
async fn test_debug_create_session_marks_debug() -> Result<()> {
    let _env = setup_test_env()?;
    let runtime_dir = short_runtime_dir(format!(
        "horon-agent-debug-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir)?;
    let socket_path = runtime_dir.join("horon-agent.sock");
    let debug_socket_path = runtime_dir.join("horon-agent-debug.sock");

    let provider = MockProvider::new();
    let provider: Arc<dyn horon_agent::provider::Provider> = Arc::new(provider);
    let server_instance = server::Server::new_with_paths(provider, socket_path.clone(), debug_socket_path.clone());
    let server_handle = tokio::spawn(async move { server_instance.run().await });

    wait_for_server_ready(&socket_path, &debug_socket_path).await?;

    let session_id = debug_create_headless_session(debug_socket_path.clone()).await?;
    let session = Session::load(&session_id)?;
    assert!(session.is_debug);

    abort_server_and_cleanup(&server_handle, &socket_path, &debug_socket_path);

    Ok(())
}

#[tokio::test]
async fn test_clear_preserves_debug_for_resumed_debug_session() -> Result<()> {
    let _env = setup_test_env()?;
    let runtime_dir = short_runtime_dir(format!(
        "horon-agent-clear-debug-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir)?;
    let socket_path = runtime_dir.join("horon-agent.sock");
    let debug_socket_path = runtime_dir.join("horon-agent-debug.sock");

    let provider = MockProvider::new();
    let provider: Arc<dyn horon_agent::provider::Provider> = Arc::new(provider);
    let server_instance = server::Server::new_with_paths(provider, socket_path.clone(), debug_socket_path.clone());
    let server_handle = tokio::spawn(async move { server_instance.run().await });

    wait_for_server_ready(&socket_path, &debug_socket_path).await?;

    let debug_session_id = debug_create_headless_session(debug_socket_path.clone()).await?;
    let mut client = server::Client::connect_with_path(socket_path.clone()).await?;
    subscribe_client(&mut client).await?;
    let resume_id = client.resume_session(&debug_session_id).await?;

    // Drain resume completion so clear() events are unambiguous.
    let mut saw_resume_history = false;
    let resume_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < resume_deadline {
        let event = tokio::time::timeout(Duration::from_secs(1), client.read_event()).await??;
        match event {
            ServerEvent::Ack { .. } => continue,
            ServerEvent::History { id, .. } if id == resume_id => {
                saw_resume_history = true;
                break;
            }
            ServerEvent::Error { id, message, .. } if id == resume_id => {
                anyhow::bail!("resume_session failed: {}", message);
            }
            _ => {}
        }
    }
    if !saw_resume_history {
        anyhow::bail!("Timed out waiting for resume history event");
    }

    client.clear().await?;

    let mut new_session_id = None;
    let clear_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < clear_deadline {
        let event = tokio::time::timeout(Duration::from_secs(1), client.read_event()).await??;
        match event {
            ServerEvent::Ack { .. } => continue,
            ServerEvent::SessionId { session_id } => {
                new_session_id = Some(session_id);
            }
            ServerEvent::Done { .. } if new_session_id.is_some() => break,
            _ => {}
        }
    }

    let new_session_id = new_session_id.ok_or_else(|| anyhow::anyhow!("Did not receive new session id after clear"))?;
    // Clear selects a new empty root in the same identity: the old branches stay
    // reachable for rewind, and the debug binding survives the reset.
    assert_eq!(new_session_id, debug_session_id);
    let session = Session::load(&new_session_id)?;
    assert!(session.is_debug);
    assert!(session.history_messages()?.is_empty());

    abort_server_and_cleanup(&server_handle, &socket_path, &debug_socket_path);

    Ok(())
}
