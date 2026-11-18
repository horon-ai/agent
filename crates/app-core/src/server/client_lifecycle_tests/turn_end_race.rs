//! A turn's end against the next prompt: a client that answers a terminal
//! event with its next message must find that turn closed.

use super::*;

/// A prompt sent the instant the previous turn's terminal event arrives must
/// not meet "Already processing a message". The request loop learns the turn
/// finished before the turn task waits for its idle state to be forwarded; the
/// next turn's task still waits for the agent the previous one keeps.
#[tokio::test]
async fn prompt_sent_right_after_done_is_not_busy() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    let mut events = Vec::new();
    for id in 10..16 {
        editor
            .send(json!({"type": "message", "id": id, "content": format!("turn {id}")}))
            .await;
        events.extend(
            editor
                .until(|event| matches!(event, ServerEvent::Done { id: done } if *done == id))
                .await,
        );
    }
    assert!(
        !events.iter().any(|event| matches!(
            event,
            ServerEvent::Error { message, .. } if message.contains("Already processing")
        )),
        "a prompt sent right after Done is accepted: {events:#?}"
    );
    for id in 10..16 {
        inserted_id(&events, id);
    }
    h.close(vec![editor]).await;
}

/// The request loop can wake with the finished turn's report and the next
/// prompt both waiting, when the prompt arrives while the loop is busy with
/// another request. It must close the turn before it reads the prompt.
///
/// Deterministic: a `GetHistory` writes its reply and then reads the swarm plan
/// table, which this test holds; nothing a turn's end or the event writer does
/// touches that table. The turn ends and its `Done` reaches the client while
/// the loop waits there, and the client sends its next prompt. Released, the
/// loop finds both inputs ready at once.
#[tokio::test]
async fn a_prompt_waiting_beside_the_turn_end_is_not_busy() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    let gate = h.provider.push_gated_text("first reply");
    editor
        .send(json!({"type": "message", "id": 10, "content": "first"}))
        .await;
    editor.until(is_running).await;

    // The plan read happens only for a session that has a swarm.
    let session_id = h.only_session().await;
    if let Some(member) = h.swarm_members.write().await.get_mut(&session_id) {
        member.swarm_id = Some(format!("session:{session_id}"));
    }
    let plans = h.swarm_plans.write().await;
    editor.send(json!({"type": "get_history", "id": 11})).await;
    // The reply is written before the plan read.
    editor
        .until(|event| matches!(event, ServerEvent::History { id: 11, .. }))
        .await;
    editor.send(json!({"type": "ping", "id": 13})).await;
    let pong = tokio::time::timeout(
        Duration::from_millis(300),
        editor.until(|event| matches!(event, ServerEvent::Pong { id: 13, .. })),
    )
    .await;
    assert!(
        pong.is_err(),
        "the request loop is parked on the swarm plan table: {pong:?}"
    );

    gate.add_permits(1);
    let first = editor
        .until(|event| matches!(event, ServerEvent::Done { id: 10 } | ServerEvent::Error { id: 10, .. }))
        .await;
    assert!(
        matches!(first.last(), Some(ServerEvent::Done { id: 10 })),
        "the first turn ends: {first:#?}"
    );
    editor
        .send(json!({"type": "message", "id": 12, "content": "second"}))
        .await;
    drop(plans);

    let second = editor
        .until(|event| matches!(event, ServerEvent::Done { id: 12 } | ServerEvent::Error { id: 12, .. }))
        .await;
    assert!(
        matches!(second.last(), Some(ServerEvent::Done { id: 12 })),
        "the prompt that waited beside the turn end runs instead of meeting \
         \"Already processing a message\": {second:#?}"
    );
    inserted_id(&second, 12);
    h.close(vec![editor]).await;
}

/// A cancel writes its `Interrupted`/`Done` straight to the client while the
/// cancelled turn's own events still wait in that turn's forwarder, so the
/// reply could overtake the turn's last tool frame. A flush returns only once
/// the forwarder has handed on everything queued before it.
#[tokio::test]
async fn a_reply_after_a_fanout_flush_follows_the_queued_turn_events() {
    let h = Harness::new();
    let session = "session_fanout_flush_order";
    let (client_tx, mut client_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let turn_tx = crate::server::state::session_event_fanout_sender_with_fallback(
        session.to_string(),
        Arc::clone(&h.swarm_members),
        client_tx.clone(),
    );
    for id in 1..=3 {
        turn_tx.send(ServerEvent::Done { id }).expect("the forwarder is live");
    }
    assert!(
        client_rx.try_recv().is_err(),
        "the forwarder has not run yet, so a direct reply now would overtake the turn's events"
    );

    crate::server::fanout_flush::flush(session, Duration::from_secs(10)).await;
    client_tx
        .send(ServerEvent::Interrupted)
        .expect("the client channel is open");

    let mut order = Vec::new();
    while let Ok(event) = client_rx.try_recv() {
        order.push(match event {
            ServerEvent::Done { id } => format!("done {id}"),
            ServerEvent::Interrupted => "interrupted".to_string(),
            other => panic!("unexpected event {other:?}"),
        });
    }
    assert_eq!(order, ["done 1", "done 2", "done 3", "interrupted"]);
    crate::server::state::unregister_session_event_sink(session, &turn_tx);
}

/// #355: a context message (`no_reply`) written on a turn's terminal event must
/// be stored, not refused as busy.
///
/// The finished turn's task keeps the agent until its idle state reached every
/// attachment, and that window opens after the terminal event: the request
/// therefore arrives with no turn registered any more and the agent still held.
/// Deterministic: the test holds the agent itself, exactly like that tail, and
/// releases it only after the daemon read the request.
#[tokio::test]
async fn a_context_message_written_on_the_turn_end_is_stored_not_refused() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    editor
        .send(json!({"type": "message", "id": 10, "content": "first"}))
        .await;
    editor
        .until(|event| matches!(event, ServerEvent::Done { id: 10 }))
        .await;
    // The idle state is the turn's last event, so once it is out the turn is
    // fully closed and the agent free; the hold below is a tail this test owns.
    editor.until(is_idle).await;
    let session_id = h.only_session().await;
    let agent = {
        let sessions = h.sessions.read().await;
        Arc::clone(sessions.get(&session_id).expect("the subscribed session"))
    };
    let (held_tx, held_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let holder_agent = Arc::clone(&agent);
    let holder = tokio::spawn(async move {
        let _held = holder_agent.lock().await;
        let _ = held_tx.send(());
        let _ = release_rx.await;
    });
    held_rx.await.expect("the holder takes the agent");
    assert!(
        !crate::turn_cancel_registry::has_active_turn(&session_id),
        "precondition: a finished turn registers no active turn"
    );

    let context = json!({
        "type": "message",
        "id": 60,
        "content": "written on the turn end",
        "no_reply": true,
    });
    editor.send(context).await;
    // The daemon read the request while the agent is held, so it must wait for
    // the tail out instead of answering busy. Only a reply to request 60 fails
    // this: unrelated traffic, such as a model-catalog update, is consumed.
    // `read_line` is cancel safe, so dropping the read at the deadline loses no
    // bytes.
    editor.until(|event| matches!(event, ServerEvent::Ack { id: 60 })).await;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(100);
    while let Ok((_, event)) = tokio::time::timeout_at(deadline, editor.next_raw()).await {
        assert!(
            !matches!(
                event,
                ServerEvent::ContextMessageAdded { id: 60 } | ServerEvent::Error { id: 60, .. }
            ),
            "a context message on the turn end waits for the tail: {event:?}"
        );
    }

    drop(release_tx);
    holder.await.expect("the holder ends");
    let reply = editor
        .until(|event| {
            matches!(event, ServerEvent::ContextMessageAdded { id: 60 })
                || matches!(event, ServerEvent::Error { id: 60, .. })
        })
        .await;
    assert!(
        matches!(reply.last(), Some(ServerEvent::ContextMessageAdded { id: 60 })),
        "the context message is stored and acknowledged: {reply:#?}"
    );

    // Exactly once, and no turn started for it.
    let (_, messages) = history(&mut editor, 61).await;
    assert_eq!(
        messages
            .iter()
            .filter(|row| row.content.contains("written on the turn end"))
            .count(),
        1,
        "the context message is stored exactly once: {messages:#?}"
    );
    h.close(vec![editor]).await;
}

/// A turn that really is running answers busy at once and stores nothing: the
/// wait above is only for a finished turn's tail.
#[tokio::test]
async fn a_context_message_during_a_live_turn_is_refused_without_waiting() {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    let session_id = "session_context_message_live_turn";
    let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
        forked: Arc::new(AtomicBool::new(false)),
    });
    let registry = Registry::new(Arc::clone(&provider)).await;
    let mut session = crate::session::Session::create_with_id(session_id.to_string(), None, None);
    session.model = Some("panic-on-fork".to_string());
    let agent = Arc::new(Mutex::new(Agent::new_with_session(provider, registry, session, None)));
    let before = agent.lock().await.message_count();
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();

    // A turn another surface runs: registered, and the session busy.
    let active_turn = crate::turn_cancel_registry::register_active_turn(session_id, InterruptSignal::new());
    let started = Instant::now();
    crate::server::client_lifecycle::append_context_message(
        70,
        "not yet",
        Vec::new(),
        session_id,
        false,
        &agent,
        &client_event_tx,
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "a running turn is refused at once, not waited out"
    );
    assert_busy_error(&mut client_event_rx, 70, "context_message").await;
    assert!(client_event_rx.try_recv().is_err(), "exactly one reply");
    assert_eq!(agent.lock().await.message_count(), before, "nothing is stored");
    assert!(
        crate::turn_cancel_registry::has_active_turn(session_id),
        "the refusal leaves the running turn's registration alone"
    );
    assert_eq!(
        crate::turn_cancel_registry::active_turn_signals(session_id).len(),
        1,
        "a cancel still has the running turn's signal to fire"
    );
    drop(active_turn);
}

/// A hold that outlives the bound is refused rather than parked on: the
/// request loop must stay able to read the next request.
#[tokio::test(start_paused = true)]
async fn a_context_message_gives_up_when_the_tail_outlives_the_wait() {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    let session_id = "session_context_message_tail_outlives_wait";
    let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
        forked: Arc::new(AtomicBool::new(false)),
    });
    let registry = Registry::new(Arc::clone(&provider)).await;
    let mut session = crate::session::Session::create_with_id(session_id.to_string(), None, None);
    session.model = Some("panic-on-fork".to_string());
    let agent = Arc::new(Mutex::new(Agent::new_with_session(provider, registry, session, None)));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();

    // No turn is registered and the hold never ends: only the bound can decide.
    let (held_tx, held_rx) = tokio::sync::oneshot::channel();
    let holder_agent = Arc::clone(&agent);
    let _holder = tokio::spawn(async move {
        let _held = holder_agent.lock().await;
        let _ = held_tx.send(());
        std::future::pending::<()>().await;
    });
    held_rx.await.expect("the holder takes the agent");

    crate::server::client_lifecycle::append_context_message(
        71,
        "too late",
        Vec::new(),
        session_id,
        false,
        &agent,
        &client_event_tx,
    )
    .await;
    assert_busy_error(&mut client_event_rx, 71, "context_message").await;
    assert!(client_event_rx.try_recv().is_err(), "exactly one reply");
    assert!(agent.try_lock().is_err(), "the refusal took no lock");
}
