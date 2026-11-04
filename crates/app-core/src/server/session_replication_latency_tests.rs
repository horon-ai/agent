//! Todo 16 reservation measurement: how long an unrelated create or remove waits
//! while a durable replication holds the session-map read reservation. The numbers
//! are printed, never asserted as budgets; the decision is in
//! `.omo/evidence/agent-runtime-performance-2/task-16-agent-runtime-performance-2.md`.

use super::*;
use crate::message::StreamEvent;
use crate::provider::{EventStream, Provider};
use crate::server::remove_session_entry;

#[derive(Clone)]
struct LatencyProvider;

#[async_trait::async_trait]
impl Provider for LatencyProvider {
    async fn complete(
        &self,
        _: &[crate::message::Message],
        _: &[crate::message::ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::iter([
            Ok(StreamEvent::TextDelta("latency".into())),
            Ok(StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".into()),
            }),
        ])))
    }
    fn name(&self) -> &str {
        "openai"
    }
    fn model(&self) -> String {
        "local-model".into()
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn agent_for(session: Session) -> Arc<Mutex<Agent>> {
    Arc::new(Mutex::new(Agent::new_with_session(
        Arc::new(LatencyProvider),
        crate::tool::Registry::empty(),
        session,
        None,
    )))
}

fn chained_page(session: &Session, count: usize) -> SessionEventPage {
    let mut parent = Some(store::EventId::parse(session.selected_event_head().unwrap()).unwrap());
    let mut events = Vec::with_capacity(count);
    for index in 0..count {
        let event = store::EventDraft::new(
            store::SessionId::parse(&session.id).unwrap(),
            store::EventId::new_v4(),
            parent.clone(),
            store::OriginId::new_v4(),
            chrono::DateTime::UNIX_EPOCH,
            store::EventKind::new(store::KnownEventKind::MessageAppended),
            serde_json::json!({"message": StoredMessage {
                turn_start: false,
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::User,
                content: text(&format!("replicated {index}")),
                display_role: None,
                timestamp: None,
                tool_duration_ms: None,
                token_usage: None,
            }}),
        )
        .seal()
        .unwrap();
        parent = Some(event.envelope().event_id.clone());
        events.push(event.envelope().canonical_record().unwrap());
    }
    SessionEventPage {
        protocol_version: REPLICATION_PROTOCOL_VERSION,
        session_id: store::SessionId::parse(&session.id).unwrap(),
        heads: vec![parent.unwrap()],
        events,
        next_cursor: None,
        blob_refs: vec![],
        record_refs: vec![],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_map_reservation_blocks_unrelated_removal_for_the_measured_hold() {
    let _home = Home::new();
    let agent = agent_for(seed("replica_hold"));
    let other = agent_for(seed("replica_hold_other"));
    let sessions = Arc::new(RwLock::new(HashMap::from([
        ("replica_hold".to_string(), Arc::clone(&agent)),
        ("replica_hold_other".to_string(), Arc::clone(&other)),
    ])));

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    {
        let mut guard = agent.lock().await;
        guard
            .set_replica_preparation_hook(Box::new(move || {
                ready_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            }))
            .unwrap();
    }

    let clear_map = Arc::clone(&sessions);
    let clear_agent = Arc::clone(&agent);
    let clear = tokio::spawn(async move { clear_live_session(&clear_map, "replica_hold", &clear_agent).await });
    tokio::time::timeout(Duration::from_secs(10), ready_rx)
        .await
        .unwrap()
        .unwrap();

    let hold_started = std::time::Instant::now();
    let removal_map = Arc::clone(&sessions);
    let removal = tokio::spawn(async move { remove_session_entry(&removal_map, "replica_hold_other").await.is_some() });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let removal_finished_while_held = removal.is_finished();
    release_tx.send(()).unwrap();
    let removed = removal.await.unwrap();
    let removal_waited_ms = hold_started.elapsed().as_millis();
    clear.await.unwrap().unwrap();

    eprintln!(
        "t16_reservation_hold={{removal_finished_while_held:{removal_finished_while_held},removal_waited_ms:{removal_waited_ms},hold_floor_ms:200}}"
    );
    assert!(
        removed,
        "the unrelated session must still be removed once the hold ends"
    );
    assert!(
        !removal_finished_while_held,
        "a held reservation must block unrelated removal, or the reservation is not held"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_map_reservation_hold_for_a_real_import_is_the_import_duration() {
    let _home = Home::new();
    let mut measurements = Vec::new();
    for events in [1usize, 20, 60, 150] {
        let agent = agent_for(seed(&format!("replica_import_{events}")));
        let wire = {
            let guard = agent.lock().await;
            chained_page(guard.session_for_split(), events)
        };
        let sessions = Arc::new(RwLock::new(HashMap::from([(
            format!("replica_import_{events}"),
            Arc::clone(&agent),
        )])));
        let unrelated = agent_for(seed(&format!("replica_import_other_{events}")));
        sessions
            .write()
            .await
            .insert(format!("replica_import_other_{events}"), unrelated);

        let import_map = Arc::clone(&sessions);
        let import_started = std::time::Instant::now();
        let import = tokio::spawn(async move { import_session_events(&import_map, wire, None).await });
        let removal_map = Arc::clone(&sessions);
        let removal_id = format!("replica_import_other_{events}");
        let removal_started = std::time::Instant::now();
        let removal = tokio::spawn(async move { remove_session_entry(&removal_map, &removal_id).await.is_some() });
        let removed = removal.await.unwrap();
        let removal_ms = removal_started.elapsed().as_millis();
        let imported = import.await.unwrap();
        let import_ms = import_started.elapsed().as_millis();
        assert!(
            imported.is_ok(),
            "the import must land so its hold is the real one: {imported:?}"
        );
        assert!(removed);
        measurements.push(format!(
            "{{events:{events},import_ms:{import_ms},unrelated_removal_ms:{removal_ms},ms_per_event:{:.3}}}",
            import_ms as f64 / events as f64
        ));
    }
    eprintln!("t16_reservation_import={}", measurements.join(","));
}
