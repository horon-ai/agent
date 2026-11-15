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
        editor.until(|event| matches!(event, ServerEvent::Pong { .. })),
    )
    .await;
    assert!(pong.is_err(), "the request loop is parked on the swarm plan table: {pong:?}");

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
