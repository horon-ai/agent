use super::*;
use crate::message::StreamEvent;
use crate::provider::{EventStream, Provider};

#[derive(Clone)]
struct ReadyProvider {
    ready: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
#[async_trait::async_trait]
impl Provider for ReadyProvider {
    async fn complete(
        &self,
        _: &[crate::message::Message],
        _: &[crate::message::ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        self.ready.notify_one();
        self.release.notified().await;
        Ok(Box::pin(futures::stream::iter([
            Ok(StreamEvent::TextDelta("finished".into())),
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

#[tokio::test]
async fn session_replication_running_turn_returns_busy_before_provider_release() {
    // Given a real turn stopped inside a deterministic in-process provider.
    let _home = Home::new();
    let provider = Arc::new(ReadyProvider {
        ready: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    });
    let session = seed("replica_turn");
    let wire = successor(&session, "remote");
    let agent = Arc::new(Mutex::new(Agent::new_with_session(
        provider.clone(),
        crate::tool::Registry::empty(),
        session,
        None,
    )));
    let sessions = Arc::new(RwLock::new(HashMap::from([(
        "replica_turn".into(),
        Arc::clone(&agent),
    )])));
    let turn_agent = Arc::clone(&agent);
    let ready = provider.ready.notified();
    let turn = tokio::spawn(async move { turn_agent.lock().await.run_once_capture("local turn").await });
    tokio::time::timeout(Duration::from_secs(10), ready).await.unwrap();
    // When import is requested while the provider is still waiting.
    let result = tokio::time::timeout(Duration::from_secs(5), import_session_events(&sessions, wire, None)).await;
    provider.release.notify_one();
    turn.await.unwrap().unwrap();
    // Then import refused without waiting for that turn.
    assert_eq!(error_code(&result.unwrap().unwrap_err()), store::StoreErrorCode::Busy);
}

#[test]
fn session_replication_preparation_reservation_precedes_original_io() {
    // Given a save paused in preparation, before any original append.
    let _home = Home::new();
    let mut session = seed("replica_prepare");
    let mut importer = session.clone();
    let wire = successor(&session, "remote");
    let before = std::fs::read(session.event_log_path().unwrap()).unwrap();
    let log = session.event_log_path().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    session
        .set_event_preparation_hook(Box::new(move || {
            ready_tx.send("PREPARATION_READY").unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        }))
        .unwrap();
    session.add_message(Role::User, text("local"));
    let save = std::thread::spawn(move || {
        session.save().unwrap();
        session
    });
    assert_eq!(
        ready_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
        "PREPARATION_READY"
    );
    // When another façade imports before the save has touched the log.
    let result = importer.import_replica(wire, None);
    let at_preparation = std::fs::read(log).unwrap();
    release_tx.send(()).unwrap();
    save.join().unwrap();
    // Then admission, not an incidental log sync, enforces exclusion.
    assert_eq!(error_code(&result.unwrap_err()), store::StoreErrorCode::Busy);
    assert_eq!(at_preparation, before);
}

#[tokio::test]
async fn session_replication_clear_admission_keeps_same_arc_reserved_until_reset() {
    // Given the actual Clear adapter paused inside real save preparation.
    let _home = Home::new();
    let provider = Arc::new(ReadyProvider {
        ready: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    });
    let session = seed("replica_clear_race");
    let agent = Arc::new(Mutex::new(Agent::new_with_session(
        provider,
        crate::tool::Registry::empty(),
        session,
        None,
    )));
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let wire = {
        let mut guard = agent.lock().await;
        let wire = successor(guard.session_for_split(), "racing remote");
        guard
            .set_replica_preparation_hook(Box::new(move || {
                ready_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            }))
            .unwrap();
        wire
    };
    let sessions = Arc::new(RwLock::new(HashMap::from([(
        "replica_clear_race".into(),
        Arc::clone(&agent),
    )])));
    let clear_map = Arc::clone(&sessions);
    let clear_agent = Arc::clone(&agent);
    let clear = tokio::spawn(async move { clear_live_session(&clear_map, "replica_clear_race", &clear_agent).await });
    tokio::time::timeout(Duration::from_secs(10), ready_rx)
        .await
        .unwrap()
        .unwrap();
    // When import and map replacement race that same-ID reset.
    let imported = import_session_events(&sessions, wire, None).await;
    let replacement_blocked = sessions.try_write().is_err();
    release_tx.send(()).unwrap();
    clear.await.unwrap().unwrap();
    // Then admission is rejected, and Clear never replaces the live Arc.
    assert_eq!(error_code(&imported.unwrap_err()), store::StoreErrorCode::Busy);
    assert!(replacement_blocked);
    assert!(Arc::ptr_eq(
        sessions.read().await.get("replica_clear_race").unwrap(),
        &agent
    ));
    let guard = agent.lock().await;
    assert_eq!(guard.session_id(), "replica_clear_race");
    assert!(guard.session_for_split().history_messages().unwrap().is_empty());
}
