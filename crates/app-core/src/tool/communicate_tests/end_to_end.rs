#[tokio::test]
async fn communicate_list_and_await_members_work_end_to_end() {
    let _env_lock = crate::storage::lock_test_env();
    let runtime_dir = tempfile::TempDir::new().expect("runtime tempdir");
    let repo_dir = std::env::current_dir().expect("repo cwd");
    let socket_path = runtime_dir.path().join("horon-agent.sock");
    let _runtime = EnvGuard::set("HORON_AGENT_RUNTIME_DIR", runtime_dir.path());
    // Keep the developer's real config (agents.swarm_model, providers) out of spawns.
    let _home = EnvGuard::set("HORON_HOME", runtime_dir.path());
    let _socket = EnvGuard::set("HORON_AGENT_SOCKET", &socket_path);
    let _debug = EnvGuard::set("HORON_AGENT_DEBUG_CONTROL", "1");
    // Independently created root sessions own separate swarms (83dbc36dc);
    // opt both clients into one shared swarm explicitly.
    let _swarm = EnvGuard::set("HORON_AGENT_SWARM_ID", "communicate-e2e-shared-swarm");

    let provider: Arc<dyn Provider> = Arc::new(DelayedTestProvider {
        delay: Duration::from_millis(300),
    });
    let server = Arc::new(Server::new(provider));
    let mut server_task = {
        let server = Arc::clone(&server);
        tokio::spawn(async move { server.run().await })
    };

    let socket_path = runtime_dir.path().join("horon-agent.sock");
    wait_for_server_socket(&socket_path, &mut server_task)
        .await
        .expect("server socket should be ready");

    let mut watcher = RawClient::connect(&socket_path).await.expect("watcher should connect");
    let mut peer = RawClient::connect(&socket_path).await.expect("peer should connect");
    watcher.subscribe(&repo_dir).await.expect("watcher subscribe");
    peer.subscribe(&repo_dir).await.expect("peer subscribe");

    let watcher_session = watcher.session_id().await.expect("watcher session id");
    let peer_session = peer.session_id().await.expect("peer session id");

    let tool = CommunicateTool::new();
    let ctx = test_ctx(&watcher_session, &repo_dir);

    let list_output = tool
        .execute(json!({"action": "list"}), ctx.clone())
        .await
        .expect("communicate list should succeed");
    assert!(
        list_output.output.contains("Status: ready"),
        "expected communicate list to render member status, got: {}",
        list_output.output
    );

    let peer_message_id = peer
        .send_message("Reply with a short acknowledgement.")
        .await
        .expect("peer message request should send");

    let running_members = wait_for_member_status(&mut watcher, &watcher_session, &peer_session, "running")
        .await
        .expect("peer should enter running state");
    let running_peer = running_members
        .iter()
        .find(|member| member.session_id == peer_session)
        .expect("peer should be listed while running");
    assert_eq!(running_peer.status.as_deref(), Some("running"));

    // Legacy background=false input is upgraded to a durable asynchronous wait.
    let await_output = tokio::time::timeout(
        Duration::from_secs(5),
        tool.execute(
            json!({
                "action": "await_members",
                "session_ids": [peer_session.clone()],
                "timeout_minutes": 1,
                "background": false
            }),
            ctx.clone(),
        ),
    )
    .await
    .expect("legacy blocking request should return promptly")
    .expect("await_members should start");
    assert!(
        await_output.output.contains("no longer supported") && await_output.output.contains("asynchronously"),
        "expected compatibility hand-off output, got: {}",
        await_output.output
    );

    peer.wait_for_done(peer_message_id)
        .await
        .expect("peer message should finish");

    let event = watcher
        .read_until(Duration::from_secs(5), |event| {
            matches!(
                event,
                ServerEvent::Notification {
                    notification_type: NotificationType::Message { scope: Some(scope), .. },
                    ..
                } if scope == "swarm_await"
            )
        })
        .await
        .expect("upgraded asynchronous wait should notify on completion");
    let ServerEvent::Notification { message, .. } = event else {
        panic!("expected swarm_await notification, got: {event:?}");
    };
    assert!(
        message.contains("(ready)"),
        "expected await_members to treat ready as done, got: {}",
        message
    );

    let ready_members = wait_for_member_status(&mut watcher, &watcher_session, &peer_session, "ready")
        .await
        .expect("peer should return to ready state");
    let ready_peer = ready_members
        .iter()
        .find(|member| member.session_id == peer_session)
        .expect("peer should still be listed when ready");
    assert_eq!(ready_peer.status.as_deref(), Some("ready"));

    server_task.abort();
}

#[tokio::test]
async fn communicate_await_members_background_returns_immediately_and_notifies() {
    let _env_lock = crate::storage::lock_test_env();
    let runtime_dir = tempfile::TempDir::new().expect("runtime tempdir");
    let repo_dir = std::env::current_dir().expect("repo cwd");
    let socket_path = runtime_dir.path().join("horon-agent.sock");
    let _runtime = EnvGuard::set("HORON_AGENT_RUNTIME_DIR", runtime_dir.path());
    // Keep the developer's real config (agents.swarm_model, providers) out of spawns.
    let _home = EnvGuard::set("HORON_HOME", runtime_dir.path());
    let _socket = EnvGuard::set("HORON_AGENT_SOCKET", &socket_path);
    let _debug = EnvGuard::set("HORON_AGENT_DEBUG_CONTROL", "1");
    // Independently created root sessions own separate swarms (83dbc36dc);
    // opt both clients into one shared swarm explicitly.
    let _swarm = EnvGuard::set("HORON_AGENT_SWARM_ID", "communicate-e2e-shared-swarm");

    let provider: Arc<dyn Provider> = Arc::new(DelayedTestProvider {
        delay: Duration::from_millis(300),
    });
    let server = Arc::new(Server::new(provider));
    let mut server_task = {
        let server = Arc::clone(&server);
        tokio::spawn(async move { server.run().await })
    };

    let socket_path = runtime_dir.path().join("horon-agent.sock");
    wait_for_server_socket(&socket_path, &mut server_task)
        .await
        .expect("server socket should be ready");

    let mut watcher = RawClient::connect(&socket_path).await.expect("watcher should connect");
    let mut peer = RawClient::connect(&socket_path).await.expect("peer should connect");
    watcher.subscribe(&repo_dir).await.expect("watcher subscribe");
    peer.subscribe(&repo_dir).await.expect("peer subscribe");

    let watcher_session = watcher.session_id().await.expect("watcher session id");
    let peer_session = peer.session_id().await.expect("peer session id");

    let tool = CommunicateTool::new();
    let ctx = test_ctx(&watcher_session, &repo_dir);

    // Put the peer into a running state so the await actually has to wait.
    let peer_message_id = peer
        .send_message("Reply with a short acknowledgement.")
        .await
        .expect("peer message request should send");
    wait_for_member_status(&mut watcher, &watcher_session, &peer_session, "running")
        .await
        .expect("peer should enter running state");

    // Background await (the default) must return promptly with a hand-off
    // message instead of blocking until the peer finishes.
    let await_output = tokio::time::timeout(
        Duration::from_secs(5),
        tool.execute(
            json!({
                "action": "await_members",
                "session_ids": [peer_session.clone()],
                "timeout_minutes": 1
            }),
            ctx.clone(),
        ),
    )
    .await
    .expect("background await should return promptly")
    .expect("await_members should succeed");
    assert!(
        await_output.output.contains("background"),
        "expected background hand-off message, got: {}",
        await_output.output
    );

    peer.wait_for_done(peer_message_id)
        .await
        .expect("peer message should finish");

    // The backgrounded watcher should deliver a swarm-await notification to the
    // requesting (watcher) session once the peer reaches ready.
    let event = watcher
        .read_until(Duration::from_secs(5), |event| {
            matches!(
                event,
                ServerEvent::Notification {
                    notification_type: NotificationType::Message { scope: Some(scope), .. },
                    ..
                } if scope == "swarm_await"
            )
        })
        .await
        .expect("background await should deliver a swarm_await notification");
    let ServerEvent::Notification { message, .. } = event else {
        panic!("expected swarm_await notification, got: {event:?}");
    };
    assert!(
        message.contains("Swarm await finished"),
        "expected swarm await completion body, got: {}",
        message
    );

    server_task.abort();
}

#[tokio::test]
async fn communicate_run_plan_with_empty_plan_returns_inline_even_in_background_mode() {
    let _env_lock = crate::storage::lock_test_env();
    let runtime_dir = tempfile::TempDir::new().expect("runtime tempdir");
    let repo_dir = std::env::current_dir().expect("repo cwd");
    let socket_path = runtime_dir.path().join("horon-agent.sock");
    let _runtime = EnvGuard::set("HORON_AGENT_RUNTIME_DIR", runtime_dir.path());
    // Keep the developer's real config (agents.swarm_model, providers) out of spawns.
    let _home = EnvGuard::set("HORON_HOME", runtime_dir.path());
    let _socket = EnvGuard::set("HORON_AGENT_SOCKET", &socket_path);
    let _debug = EnvGuard::set("HORON_AGENT_DEBUG_CONTROL", "1");

    let provider: Arc<dyn Provider> = Arc::new(DelayedTestProvider {
        delay: Duration::from_millis(50),
    });
    let server = Arc::new(Server::new(provider));
    let mut server_task = {
        let server = Arc::clone(&server);
        tokio::spawn(async move { server.run().await })
    };

    let socket_path = runtime_dir.path().join("horon-agent.sock");
    wait_for_server_socket(&socket_path, &mut server_task)
        .await
        .expect("server socket should be ready");

    let mut client = RawClient::connect(&socket_path).await.expect("client should connect");
    client.subscribe(&repo_dir).await.expect("subscribe");
    let session = client.session_id().await.expect("session id");

    let tool = CommunicateTool::new();
    let ctx = test_ctx(&session, &repo_dir);

    // Background is the default; with no plan the validation happens inline and
    // no background task should be started.
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tool.execute(json!({"action": "run_plan"}), ctx.clone()),
    )
    .await
    .expect("run_plan should return promptly")
    .expect("run_plan should succeed");
    assert!(
        output.output.contains("No swarm plan items to run."),
        "expected inline empty-plan response, got: {}",
        output.output
    );
    assert!(
        output.metadata.is_none(),
        "empty plan must not start a background driver"
    );

    server_task.abort();
}

#[tokio::test]
async fn communicate_status_returns_busy_snapshot_for_running_member() {
    let _env_lock = crate::storage::lock_test_env();
    let runtime_dir = tempfile::TempDir::new().expect("runtime tempdir");
    let repo_dir = std::env::current_dir().expect("repo cwd");
    let socket_path = runtime_dir.path().join("horon-agent.sock");
    let _runtime = EnvGuard::set("HORON_AGENT_RUNTIME_DIR", runtime_dir.path());
    // Keep the developer's real config (agents.swarm_model, providers) out of spawns.
    let _home = EnvGuard::set("HORON_HOME", runtime_dir.path());
    let _socket = EnvGuard::set("HORON_AGENT_SOCKET", &socket_path);
    let _debug = EnvGuard::set("HORON_AGENT_DEBUG_CONTROL", "1");
    // Independently created root sessions own separate swarms (83dbc36dc);
    // opt both clients into one shared swarm explicitly.
    let _swarm = EnvGuard::set("HORON_AGENT_SWARM_ID", "communicate-e2e-shared-swarm");

    let provider: Arc<dyn Provider> = Arc::new(DelayedTestProvider {
        delay: Duration::from_millis(300),
    });
    let server = Arc::new(Server::new(provider));
    let mut server_task = {
        let server = Arc::clone(&server);
        tokio::spawn(async move { server.run().await })
    };

    wait_for_server_socket(&socket_path, &mut server_task)
        .await
        .expect("server socket should be ready");

    let mut watcher = RawClient::connect(&socket_path).await.expect("watcher should connect");
    let mut peer = RawClient::connect(&socket_path).await.expect("peer should connect");
    watcher.subscribe(&repo_dir).await.expect("watcher subscribe");
    peer.subscribe(&repo_dir).await.expect("peer subscribe");

    let watcher_session = watcher.session_id().await.expect("watcher session id");
    let peer_session = peer.session_id().await.expect("peer session id");
    let tool = CommunicateTool::new();
    let ctx = test_ctx(&watcher_session, &repo_dir);

    let peer_message_id = peer
        .send_message("Reply with a short acknowledgement.")
        .await
        .expect("peer message request should send");

    wait_for_member_status(&mut watcher, &watcher_session, &peer_session, "running")
        .await
        .expect("peer should enter running state");

    let snapshot = watcher
        .comm_status(&watcher_session, &peer_session)
        .await
        .expect("comm_status should succeed while peer is busy");
    assert_eq!(snapshot.session_id, peer_session);
    assert_eq!(snapshot.status.as_deref(), Some("running"));
    assert!(
        snapshot
            .activity
            .as_ref()
            .is_some_and(|activity| activity.is_processing)
    );

    let output = tool
        .execute(
            json!({
                "action": "status",
                "target_session": peer_session.clone()
            }),
            ctx,
        )
        .await
        .expect("status action should succeed");
    assert!(output.output.contains("Lifecycle: running"));
    assert!(output.output.contains("Activity: busy"));

    peer.wait_for_done(peer_message_id)
        .await
        .expect("peer message should finish");

    server_task.abort();
}

#[tokio::test]
async fn communicate_spawn_reports_completion_back_to_spawner() {
    let _env_lock = crate::storage::lock_test_env();
    let runtime_dir = tempfile::TempDir::new().expect("runtime tempdir");
    let repo_dir = std::env::current_dir().expect("repo cwd");
    let socket_path = runtime_dir.path().join("horon-agent.sock");
    let _runtime = EnvGuard::set("HORON_AGENT_RUNTIME_DIR", runtime_dir.path());
    // Keep the developer's real config (agents.swarm_model, providers) out of spawns.
    let _home = EnvGuard::set("HORON_HOME", runtime_dir.path());
    let _socket = EnvGuard::set("HORON_AGENT_SOCKET", &socket_path);
    let _debug = EnvGuard::set("HORON_AGENT_DEBUG_CONTROL", "1");

    let provider: Arc<dyn Provider> = Arc::new(DelayedTestProvider {
        delay: Duration::from_millis(100),
    });
    let server = Arc::new(Server::new(provider));
    let mut server_task = {
        let server = Arc::clone(&server);
        tokio::spawn(async move { server.run().await })
    };

    let socket_path = runtime_dir.path().join("horon-agent.sock");
    wait_for_server_socket(&socket_path, &mut server_task)
        .await
        .expect("server socket should be ready");

    let mut watcher = RawClient::connect(&socket_path).await.expect("watcher should connect");
    watcher.subscribe(&repo_dir).await.expect("watcher subscribe");

    let watcher_session = watcher.session_id().await.expect("watcher session id");
    let tool = CommunicateTool::new();
    let ctx = test_ctx(&watcher_session, &repo_dir);

    let spawn_output = tool
        .execute(
            json!({
                "action": "spawn",
                "label": "report-back worker",
                "prompt": "Reply with exactly AUTH_TEST_OK and nothing else."
            }),
            ctx,
        )
        .await
        .expect("spawn with prompt should succeed");
    let spawned_session = spawn_output
        .output
        .strip_prefix("Spawned new agent: ")
        .expect("spawn output should include session id")
        .trim()
        .to_string();

    watcher
        .read_until(Duration::from_secs(15), |event| {
            matches!(
                event,
                ServerEvent::Notification {
                    from_session,
                    notification_type: crate::protocol::NotificationType::Message {
                        scope: Some(scope),
                        tldr: None,
                    },
                    message,
                    ..
                } if from_session == &spawned_session
                    && scope == "swarm"
                    && message.contains("finished their work and is ready for more")
            )
        })
        .await
        .expect("spawner should receive completion report-back notification");

    server_task.abort();
}

#[tokio::test]
async fn communicate_spawn_with_prompt_and_summary_work_end_to_end() {
    let _env_lock = crate::storage::lock_test_env();
    let runtime_dir = tempfile::TempDir::new().expect("runtime tempdir");
    let repo_dir = std::env::current_dir().expect("repo cwd");
    let socket_path = runtime_dir.path().join("horon-agent.sock");
    let _runtime = EnvGuard::set("HORON_AGENT_RUNTIME_DIR", runtime_dir.path());
    // Keep the developer's real config (agents.swarm_model, providers) out of spawns.
    let _home = EnvGuard::set("HORON_HOME", runtime_dir.path());
    let _socket = EnvGuard::set("HORON_AGENT_SOCKET", &socket_path);
    let _debug = EnvGuard::set("HORON_AGENT_DEBUG_CONTROL", "1");

    let provider: Arc<dyn Provider> = Arc::new(DelayedTestProvider {
        delay: Duration::from_millis(100),
    });
    let server = Arc::new(Server::new(provider));
    let mut server_task = {
        let server = Arc::clone(&server);
        tokio::spawn(async move { server.run().await })
    };

    let socket_path = runtime_dir.path().join("horon-agent.sock");
    wait_for_server_socket(&socket_path, &mut server_task)
        .await
        .expect("server socket should be ready");

    let mut watcher = RawClient::connect(&socket_path).await.expect("watcher should connect");
    watcher.subscribe(&repo_dir).await.expect("watcher subscribe");

    let watcher_session = watcher.session_id().await.expect("watcher session id");
    let tool = CommunicateTool::new();
    let ctx = test_ctx(&watcher_session, &repo_dir);

    let spawn_output = tool
        .execute(
            json!({
                "action": "spawn",
                "label": "summary worker",
                "prompt": "Reply with a short acknowledgement."
            }),
            ctx.clone(),
        )
        .await
        .expect("spawn with prompt should succeed");
    let spawned_session = spawn_output
        .output
        .strip_prefix("Spawned new agent: ")
        .expect("spawn output should include session id")
        .trim()
        .to_string();
    assert!(!spawned_session.is_empty(), "spawned session id should not be empty");

    wait_for_member_presence(&mut watcher, &watcher_session, &spawned_session)
        .await
        .expect("spawned member should appear in swarm list");

    let summary_output = {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            match tool
                .execute(
                    json!({
                        "action": "summary",
                        "target_session": spawned_session
                    }),
                    ctx.clone(),
                )
                .await
            {
                Ok(output) => break output,
                Err(err)
                    if (err.to_string().contains("Unknown session") || err.to_string().contains(" is busy;"))
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(err) => panic!("summary for spawned agent should succeed: {err}"),
            }
        }
    };
    assert!(
        summary_output.output.contains("Tool call summary for")
            || summary_output.output.contains("No tool calls found for"),
        "unexpected summary output: {}",
        summary_output.output
    );

    server_task.abort();
}

/// `message` routes by the fields supplied (DM when `to_session` is set,
/// broadcast otherwise), while `broadcast` is a group send scoped to the
/// sender's spawned subtree (whole swarm when the sender is the coordinator).
/// Regression test for the bug where `message` and `broadcast` were identical
/// because the tool discarded `to_session`/`channel` for both.
#[tokio::test]
async fn communicate_message_routes_as_dm_while_broadcast_targets_swarm() {
    let _env_lock = crate::storage::lock_test_env();
    let runtime_dir = tempfile::TempDir::new().expect("runtime tempdir");
    let repo_dir = std::env::current_dir().expect("repo cwd");
    let socket_path = runtime_dir.path().join("horon-agent.sock");
    let _runtime = EnvGuard::set("HORON_AGENT_RUNTIME_DIR", runtime_dir.path());
    // Keep the developer's real config (agents.swarm_model, providers) out of spawns.
    let _home = EnvGuard::set("HORON_HOME", runtime_dir.path());
    let _socket = EnvGuard::set("HORON_AGENT_SOCKET", &socket_path);
    let _debug = EnvGuard::set("HORON_AGENT_DEBUG_CONTROL", "1");
    // Independently created root sessions own separate swarms (83dbc36dc);
    // opt both clients into one shared swarm explicitly.
    let _swarm = EnvGuard::set("HORON_AGENT_SWARM_ID", "communicate-e2e-shared-swarm");

    let provider: Arc<dyn Provider> = Arc::new(DelayedTestProvider {
        delay: Duration::from_millis(100),
    });
    let server = Arc::new(Server::new(provider));
    let mut server_task = {
        let server = Arc::clone(&server);
        tokio::spawn(async move { server.run().await })
    };

    wait_for_server_socket(&socket_path, &mut server_task)
        .await
        .expect("server socket should be ready");

    let mut sender = RawClient::connect(&socket_path).await.expect("sender should connect");
    let mut peer = RawClient::connect(&socket_path).await.expect("peer should connect");
    sender.subscribe(&repo_dir).await.expect("sender subscribe");
    peer.subscribe(&repo_dir).await.expect("peer subscribe");

    let sender_session = sender.session_id().await.expect("sender session id");
    let peer_session = peer.session_id().await.expect("peer session id");

    // Ensure both sessions are part of the same swarm before messaging.
    wait_for_member_presence(&mut sender, &sender_session, &peer_session)
        .await
        .expect("peer should join the swarm");

    let tool = CommunicateTool::new();
    let ctx = test_ctx(&sender_session, &repo_dir);

    // `message` with a `to_session` should arrive at the peer scoped as a DM.
    let dm_output = tool
        .execute(
            json!({
                "action": "message",
                "message": "ping-dm",
                "to_session": peer_session.clone()
            }),
            ctx.clone(),
        )
        .await
        .expect("message with to_session should succeed");
    assert!(
        dm_output.output.contains("Direct message sent to"),
        "message with to_session should report a DM, got: {}",
        dm_output.output
    );
    let dm_scope = peer
        .next_message_notification(Duration::from_secs(5))
        .await
        .expect("peer should receive the targeted message");
    assert_eq!(
        dm_scope.as_deref(),
        Some("dm"),
        "message with to_session should be delivered with dm scope"
    );

    // A long DM without a sender summary must still deliver.
    let long_dm_output = tool
        .execute(
            json!({
                "action": "dm",
                "message": format!("long-dm {}", "x".repeat(244)),
                "to_session": peer_session.clone()
            }),
            ctx.clone(),
        )
        .await
        .expect("long DM without tldr should succeed");
    assert!(long_dm_output.output.contains("Direct message sent to"));
    assert_eq!(
        peer.next_message_notification(Duration::from_secs(5))
            .await
            .expect("peer should receive long DM")
            .as_deref(),
        Some("dm")
    );

    // Broadcasts are scoped to the sender's spawned subtree; the coordinator
    // keeps whole-swarm reach as an escape hatch. The peer was not spawned by
    // the sender, so promote the sender to coordinator (self-promotion is
    // allowed while the swarm has no coordinator) so the broadcast reaches it.
    let assign_output = tool
        .execute(
            json!({
                "action": "assign_role",
                "target_session": sender_session.clone(),
                "role": "coordinator"
            }),
            ctx.clone(),
        )
        .await
        .expect("self-promotion to coordinator should succeed");
    assert!(
        assign_output.output.contains("Assigned role 'coordinator'"),
        "unexpected assign_role output: {}",
        assign_output.output
    );

    // `broadcast` should reach the peer scoped as a broadcast even though no
    // explicit target is supplied.
    let broadcast_output = tool
        .execute(
            json!({
                "action": "broadcast",
                "message": "ping-all"
            }),
            ctx.clone(),
        )
        .await
        .expect("broadcast should succeed");
    assert!(
        broadcast_output
            .output
            .contains("Broadcast sent to your spawned subtree"),
        "broadcast should report a subtree-scoped group send, got: {}",
        broadcast_output.output
    );
    let broadcast_scope = peer
        .next_message_notification(Duration::from_secs(5))
        .await
        .expect("peer should receive the broadcast");
    assert_eq!(
        broadcast_scope.as_deref(),
        Some("broadcast"),
        "broadcast should be delivered with broadcast scope"
    );

    server_task.abort();
}

#[tokio::test]
async fn communicate_spawn_many_starts_three_children_in_input_order() {
    let _env_lock = crate::storage::lock_test_env();
    let runtime_dir = tempfile::TempDir::new().expect("runtime tempdir");
    let repo_dir = std::env::current_dir().expect("repo cwd");
    let socket_path = runtime_dir.path().join("horon-agent.sock");
    let _runtime = EnvGuard::set("HORON_AGENT_RUNTIME_DIR", runtime_dir.path());
    let _home = EnvGuard::set("HORON_HOME", runtime_dir.path());
    let _socket = EnvGuard::set("HORON_AGENT_SOCKET", &socket_path);
    let _debug = EnvGuard::set("HORON_AGENT_DEBUG_CONTROL", "1");

    let provider: Arc<dyn Provider> = Arc::new(DelayedTestProvider {
        delay: Duration::from_millis(50),
    });
    let server = Arc::new(Server::new(provider));
    let mut server_task = {
        let server = Arc::clone(&server);
        tokio::spawn(async move { server.run().await })
    };
    let socket_path = runtime_dir.path().join("horon-agent.sock");
    wait_for_server_socket(&socket_path, &mut server_task)
        .await
        .expect("server socket should be ready");

    let mut watcher = RawClient::connect(&socket_path).await.expect("watcher should connect");
    watcher.subscribe(&repo_dir).await.expect("watcher subscribe");
    let watcher_session = watcher.session_id().await.expect("watcher session id");

    let tool = CommunicateTool::new();
    let ctx = test_ctx(&watcher_session, &repo_dir);
    let output = tool
        .execute(
            json!({
                "action": "spawn_many",
                "context": "You are one of three workers on the same repository.",
                "tasks": [
                    {"label": "first", "prompt": "Reply with FIRST."},
                    {"label": "second", "prompt": "Reply with SECOND."},
                    {"label": "third", "prompt": "Reply with THIRD."}
                ]
            }),
            ctx,
        )
        .await
        .expect("spawn_many with three valid tasks should succeed");

    assert!(
        output.output.starts_with("Spawned 3 of 3 agent(s):"),
        "unexpected spawn_many output: {}",
        output.output
    );
    let ids: Vec<&str> = output
        .output
        .lines()
        .filter_map(|line| line.strip_prefix("- "))
        .map(|line| line.rsplit_once(": ").expect("label: id").1)
        .collect();
    assert_eq!(ids.len(), 3, "one entry per task: {}", output.output);
    assert!(
        ids.iter().all(|id| !id.is_empty()),
        "every task should report a session id: {ids:?}"
    );
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 3, "each child needs its own session: {ids:?}");
    let labels: Vec<&str> = output
        .output
        .lines()
        .filter_map(|line| line.strip_prefix("- "))
        .map(|line| line.split_once(':').expect("label: id").0)
        .collect();
    assert_eq!(labels, ["first", "second", "third"], "input order is preserved");

    server_task.abort();
}

#[tokio::test]
async fn communicate_spawn_many_reports_a_failed_task_and_starts_the_rest() {
    let _env_lock = crate::storage::lock_test_env();
    let runtime_dir = tempfile::TempDir::new().expect("runtime tempdir");
    let repo_dir = std::env::current_dir().expect("repo cwd");
    let socket_path = runtime_dir.path().join("horon-agent.sock");
    let _runtime = EnvGuard::set("HORON_AGENT_RUNTIME_DIR", runtime_dir.path());
    let _home = EnvGuard::set("HORON_HOME", runtime_dir.path());
    let _socket = EnvGuard::set("HORON_AGENT_SOCKET", &socket_path);
    let _debug = EnvGuard::set("HORON_AGENT_DEBUG_CONTROL", "1");

    let provider: Arc<dyn Provider> = Arc::new(DelayedTestProvider {
        delay: Duration::from_millis(50),
    });
    let server = Arc::new(Server::new(provider));
    let mut server_task = {
        let server = Arc::clone(&server);
        tokio::spawn(async move { server.run().await })
    };
    let socket_path = runtime_dir.path().join("horon-agent.sock");
    wait_for_server_socket(&socket_path, &mut server_task)
        .await
        .expect("server socket should be ready");

    let mut watcher = RawClient::connect(&socket_path).await.expect("watcher should connect");
    watcher.subscribe(&repo_dir).await.expect("watcher subscribe");
    let watcher_session = watcher.session_id().await.expect("watcher session id");

    let tool = CommunicateTool::new();
    let ctx = test_ctx(&watcher_session, &repo_dir);
    let output = tool
        .execute(
            json!({
                "action": "spawn_many",
                "tasks": [
                    {"label": "kept-one", "prompt": "Reply with ONE."},
                    {"label": "broken", "agent": "no-such-definition", "prompt": "Never starts."},
                    {"label": "kept-two", "prompt": "Reply with TWO."}
                ]
            }),
            ctx,
        )
        .await
        .expect("one failed task must not fail the whole call");

    assert!(
        output.output.starts_with("Spawned 2 of 3 agent(s):"),
        "unexpected spawn_many output: {}",
        output.output
    );
    let broken = output
        .output
        .lines()
        .find(|line| line.starts_with("- broken:"))
        .expect("the failed task keeps its own entry");
    assert!(
        broken.contains("no-such-definition"),
        "the failure should name the unknown definition: {broken}"
    );
    let kept = output
        .output
        .lines()
        .filter(|line| line.starts_with("- kept-"))
        .filter(|line| !line.contains("failed:"))
        .count();
    assert_eq!(kept, 2, "the other tasks still start: {}", output.output);

    server_task.abort();
}

#[tokio::test]
async fn communicate_spawn_many_rejects_duplicate_labels_before_spawning() {
    let tool = CommunicateTool::new();
    let ctx = test_ctx("dup-check", &std::env::current_dir().expect("repo cwd"));
    let error = tool
        .execute(
            json!({
                "action": "spawn_many",
                "tasks": [
                    {"label": "Worker", "prompt": "one"},
                    {"label": "worker", "prompt": "two"}
                ]
            }),
            ctx,
        )
        .await
        .expect_err("labels that differ only in case are duplicates");
    assert!(error.to_string().contains("unique labels"), "unexpected error: {error}");
}

#[tokio::test]
async fn communicate_spawn_many_rejects_an_empty_task_list() {
    let tool = CommunicateTool::new();
    let ctx = test_ctx("empty-check", &std::env::current_dir().expect("repo cwd"));
    let error = tool
        .execute(json!({"action": "spawn_many", "tasks": []}), ctx)
        .await
        .expect_err("an empty task list has nothing to spawn");
    assert!(
        error.to_string().contains("must not be empty"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn communicate_spawn_many_prepends_the_shared_context_and_task_overrides() {
    let params: CommunicateInput = serde_json::from_value(json!({
        "action": "spawn_many",
        "context": "Shared repository background.",
        "model": "default-model"
    }))
    .expect("params");
    let ctx = test_ctx("prepare-check", &std::env::current_dir().expect("repo cwd"));
    let with_override: super::agent_spawn::SpawnBatchTask = serde_json::from_value(json!({
        "label": " task ",
        "prompt": "Do the task.",
        "model": "task-model"
    }))
    .expect("task");
    let (label, request) = super::agent_spawn::prepare_task(&ctx, &params, &with_override).expect("prepared");
    assert_eq!(label, "task", "labels are trimmed");
    match request {
        crate::protocol::Request::CommSpawn {
            initial_message,
            model,
            label: request_label,
            ..
        } => {
            let message = initial_message.expect("the first message is set");
            assert!(
                message.starts_with("Shared repository background."),
                "shared context leads the first message: {message}"
            );
            assert!(
                message.ends_with("Do the task."),
                "the task message follows the context: {message}"
            );
            assert_eq!(model.as_deref(), Some("task-model"), "a task override wins");
            assert_eq!(request_label.as_deref(), Some("task"));
        }
        other => panic!("expected a spawn request, got {other:?}"),
    }

    let inherited: super::agent_spawn::SpawnBatchTask = serde_json::from_value(json!({
        "label": "plain",
        "prompt": "Do the other task."
    }))
    .expect("task");
    let (_, request) = super::agent_spawn::prepare_task(&ctx, &params, &inherited).expect("prepared");
    match request {
        crate::protocol::Request::CommSpawn { model, .. } => {
            assert_eq!(
                model.as_deref(),
                Some("default-model"),
                "the call-level model is a default"
            );
        }
        other => panic!("expected a spawn request, got {other:?}"),
    }
}
