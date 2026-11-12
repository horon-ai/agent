//! `delete_session` at the daemon: what it refuses and what it removes.
//!
//! The refusal rules are the safety property. A delete that ran under a live
//! writer could be followed by that writer appending to a file the caller was
//! told was gone, so every test here drives the real handler rather than a copy
//! of its conditions.

use super::*;
use crate::message::{Message, ToolDefinition};
use crate::protocol::ServerEvent;
use crate::provider::EventStream;
use crate::server::client_session_delete::handle_delete_session;

/// A sandboxed home plus runtime dir, so no test reads or writes the real store.
struct Sandbox {
    prev_home: Option<std::ffi::OsString>,
    prev_runtime: Option<std::ffi::OsString>,
    _home: tempfile::TempDir,
    _runtime: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let home = tempfile::TempDir::new().expect("home");
        let runtime = tempfile::TempDir::new().expect("runtime");
        let prev_home = std::env::var_os("HORON_HOME");
        let prev_runtime = std::env::var_os("HORON_AGENT_RUNTIME_DIR");
        crate::env::set_var("HORON_HOME", home.path());
        crate::env::set_var("HORON_AGENT_RUNTIME_DIR", runtime.path());
        Self {
            prev_home,
            prev_runtime,
            _home: home,
            _runtime: runtime,
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        match self.prev_home.take() {
            Some(previous) => crate::env::set_var("HORON_HOME", previous),
            None => crate::env::remove_var("HORON_HOME"),
        }
        match self.prev_runtime.take() {
            Some(previous) => crate::env::set_var("HORON_AGENT_RUNTIME_DIR", previous),
            None => crate::env::remove_var("HORON_AGENT_RUNTIME_DIR"),
        }
    }
}

fn no_connections() -> Arc<RwLock<HashMap<String, ClientConnectionInfo>>> {
    Arc::new(RwLock::new(HashMap::new()))
}

fn connection(name: &str, session_id: &str) -> ClientConnectionInfo {
    let (disconnect_tx, _) = mpsc::unbounded_channel();
    ClientConnectionInfo {
        client_id: name.into(),
        session_id: session_id.into(),
        client_instance_id: None,
        connected_at: std::time::Instant::now(),
        last_seen: std::time::Instant::now(),
        is_processing: false,
        current_tool_name: None,
        terminal_env: vec![],
        disconnect_tx,
    }
}

fn no_sessions() -> SessionAgents {
    Arc::new(RwLock::new(HashMap::new()))
}

/// A stored transcript plus one file outside it, both keyed by the id.
fn seed_session(session_id: &str) -> Vec<std::path::PathBuf> {
    let home = crate::storage::horon_agent_dir().expect("home");
    let mut seeded = Vec::new();
    for path in [
        home.join("sessions").join(session_id),
        home.join("todos").join(format!("{session_id}.json")),
    ] {
        if path.extension().is_none() {
            // The store refuses a directory that is not owner-private, so the
            // fixture creates it the way the store itself does.
            crate::storage::create_store_dir(&path, true).expect("session dir");
            std::fs::write(path.join("events.jsonl"), b"{}\n").expect("events");
        } else {
            crate::storage::create_store_dir(path.parent().expect("parent"), true).expect("dir");
            std::fs::write(&path, b"state\n").expect("file");
        }
        seeded.push(path);
    }
    seeded
}

fn drain_events(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> Vec<ServerEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

#[tokio::test]
async fn an_unconfirmed_delete_is_refused_and_touches_nothing() {
    let _lock = crate::storage::lock_test_env();
    let _sandbox = Sandbox::new();
    let session_id = "delete_refuses_unconfirmed";
    let seeded = seed_session(session_id);
    let (tx, mut rx) = mpsc::unbounded_channel();

    handle_delete_session(7, session_id.into(), false, &no_sessions(), &no_connections(), &tx).await;

    match drain_events(&mut rx).as_slice() {
        [ServerEvent::Error { id: 7, message, .. }] => {
            assert!(message.contains("confirm"), "{message}");
        }
        other => panic!("expected one error for the refused delete, got {other:?}"),
    }
    for path in &seeded {
        assert!(path.exists(), "a refused delete leaves {} alone", path.display());
    }
}

#[tokio::test]
async fn an_invalid_session_id_is_refused() {
    let _lock = crate::storage::lock_test_env();
    let _sandbox = Sandbox::new();
    let (tx, mut rx) = mpsc::unbounded_channel();

    handle_delete_session(8, "../../etc".into(), true, &no_sessions(), &no_connections(), &tx).await;

    match drain_events(&mut rx).as_slice() {
        [ServerEvent::Error { id: 8, message, .. }] => assert!(message.contains("invalid"), "{message}"),
        other => panic!("expected an invalid-id error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_session_with_a_running_turn_is_refused() {
    let _lock = crate::storage::lock_test_env();
    let _sandbox = Sandbox::new();
    let session_id = "delete_refuses_running_turn";
    let seeded = seed_session(session_id);
    let _turn = crate::turn_cancel_registry::register_active_turn(
        session_id,
        horon_agent_agent_runtime::InterruptSignal::new(),
    );
    let (tx, mut rx) = mpsc::unbounded_channel();

    handle_delete_session(9, session_id.into(), true, &no_sessions(), &no_connections(), &tx).await;

    match drain_events(&mut rx).as_slice() {
        [ServerEvent::Error { id: 9, message, .. }] => assert!(message.contains("open"), "{message}"),
        other => panic!("expected the open-session refusal, got {other:?}"),
    }
    for path in &seeded {
        assert!(path.exists(), "{} survives a refused delete", path.display());
    }
}

#[tokio::test]
async fn a_session_attached_to_a_client_is_refused() {
    let _lock = crate::storage::lock_test_env();
    let _sandbox = Sandbox::new();
    let session_id = "delete_refuses_attached";
    let seeded = seed_session(session_id);
    let connections = no_connections();
    connections
        .write()
        .await
        .insert("c1".into(), connection("c1", session_id));
    let (tx, mut rx) = mpsc::unbounded_channel();

    handle_delete_session(10, session_id.into(), true, &no_sessions(), &connections, &tx).await;

    match drain_events(&mut rx).as_slice() {
        [ServerEvent::Error { id: 10, message, .. }] => assert!(message.contains("open"), "{message}"),
        other => panic!("expected the attached-session refusal, got {other:?}"),
    }
    for path in &seeded {
        assert!(path.exists(), "{} survives a refused delete", path.display());
    }
}

#[tokio::test]
async fn a_confirmed_delete_removes_the_session_and_answers_done() {
    let _lock = crate::storage::lock_test_env();
    let _sandbox = Sandbox::new();
    let session_id = "delete_removes_confirmed";
    let seeded = seed_session(session_id);
    let (tx, mut rx) = mpsc::unbounded_channel();

    handle_delete_session(11, session_id.into(), true, &no_sessions(), &no_connections(), &tx).await;

    let events = drain_events(&mut rx);
    assert!(
        matches!(events.as_slice(), [ServerEvent::Done { id: 11 }]),
        "a completed delete answers Done for its own request: {events:?}"
    );
    for path in &seeded {
        assert!(!path.exists(), "{} must be gone after a delete", path.display());
    }
}

#[tokio::test]
async fn a_delete_also_removes_the_daemon_only_reload_state() {
    let _lock = crate::storage::lock_test_env();
    let _sandbox = Sandbox::new();
    let session_id = "delete_removes_reload_state";
    seed_session(session_id);
    let context = crate::server::reload_recovery::ReloadContext::path_for_session(session_id).expect("context path");
    let recovery = crate::server::reload_recovery::path_for_session(session_id).expect("recovery path");
    for path in [&context, &recovery, &context.with_extension("bak")] {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(path, b"{}\n").expect("file");
    }
    let (tx, mut rx) = mpsc::unbounded_channel();

    handle_delete_session(12, session_id.into(), true, &no_sessions(), &no_connections(), &tx).await;

    let events = drain_events(&mut rx);
    assert!(
        matches!(events.as_slice(), [ServerEvent::Done { id: 12 }]),
        "{events:?}"
    );
    for path in [&context, &recovery, &context.with_extension("bak")] {
        assert!(!path.exists(), "{} outlived the session it names", path.display());
    }
}

/// A resident agent with nothing attached and no turn running is idle, not
/// open: after a client detaches, the entry stays in the map for the reconnect
/// grace with nothing that can append to it. It is closed and purged, so a
/// session closed a moment ago can still be erased instead of failing for the
/// whole grace window.
/// `prepare_disconnect` answers `Done` only after the attachment is gone, so a
/// client that closes a session and immediately asks another connection to
/// delete it cannot be refused as "a client is attached". Before it released
/// the attachment, `Done` only dropped SDK ownership and the entry survived
/// until the daemon's reader noticed the half-close, which is a race: the ACP
/// delete-then-load test lost it in round 3 of 20.
#[tokio::test]
async fn prepare_disconnect_releases_the_attachment_before_it_answers() {
    let _lock = crate::storage::lock_test_env();
    let _sandbox = Sandbox::new();
    let session_id = "delete_after_prepare_disconnect";
    let seeded = seed_session(session_id);
    let connections = Arc::new(RwLock::new(HashMap::from([(
        "conn_1".to_string(),
        connection("conn_1", session_id),
    )])));
    let swarm_members: Arc<RwLock<HashMap<String, crate::server::SwarmMember>>> = Arc::new(RwLock::new(HashMap::new()));

    crate::server::client_disconnect_cleanup::detach_client_attachment(
        session_id,
        "conn_1",
        &connections,
        &swarm_members,
    )
    .await;

    assert!(
        connections.read().await.is_empty(),
        "the detach must release the attachment itself, not wait for EOF"
    );
    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_delete_session(9, session_id.into(), true, &no_sessions(), &connections, &tx).await;
    assert!(
        matches!(drain_events(&mut rx).as_slice(), [ServerEvent::Done { id: 9 }]),
        "a detached session is deletable"
    );
    for path in &seeded {
        assert!(!path.exists(), "{} survives the delete", path.display());
    }
}

#[tokio::test]
async fn an_idle_resident_agent_is_closed_and_deleted() {
    let _lock = crate::storage::lock_test_env();
    let _sandbox = Sandbox::new();
    let session_id = "delete_evicts_idle_resident";
    let seeded = seed_session(session_id);
    let provider: Arc<dyn Provider> = Arc::new(RefusingProvider);
    let registry = Registry::new(Arc::clone(&provider)).await;
    let agent = Arc::new(Mutex::new(Agent::new(provider, registry)));
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([(session_id.to_string(), agent)])));
    let (tx, mut rx) = mpsc::unbounded_channel();

    handle_delete_session(13, session_id.into(), true, &sessions, &no_connections(), &tx).await;

    let events = drain_events(&mut rx);
    assert!(
        matches!(events.as_slice(), [ServerEvent::Done { id: 13 }]),
        "an idle resident session is deleted, not refused: {events:?}"
    );
    for path in &seeded {
        assert!(!path.exists(), "{} must be gone after a delete", path.display());
    }
    assert!(
        !sessions.read().await.contains_key(session_id),
        "the evicted entry must leave the session map"
    );
}

/// A provider that would fail a turn. These tests never send one.
struct RefusingProvider;

#[async_trait::async_trait]
impl Provider for RefusingProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        anyhow::bail!("a delete test does not run turns")
    }

    fn name(&self) -> &str {
        "refusing"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(RefusingProvider)
    }
}
