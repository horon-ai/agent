use super::*;
use crate::message::{ContentBlock, Role};
use crate::session::{SessionEventJob, StoredMessage};
use std::sync::mpsc;
use std::time::Duration;
use store::replication::{REPLICATION_PROTOCOL_VERSION, SessionEventPage};

#[path = "session_replication_admission_tests.rs"]
mod admission;
#[path = "session_replication_import_tests.rs"]
mod imports;
#[path = "session_replication_job_tests.rs"]
mod jobs;
#[path = "session_replication_latency_tests.rs"]
mod latency;
#[path = "session_replication_process_tests.rs"]
mod processes;
#[path = "session_replication_runtime_tests.rs"]
mod runtime;

struct Home {
    _guard: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
    directory: tempfile::TempDir,
}
impl Home {
    fn new() -> Self {
        let guard = crate::storage::lock_test_env();
        let directory = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("HORON_AGENT_HOME");
        crate::env::set_var("HORON_AGENT_HOME", directory.path());
        Self {
            _guard: guard,
            previous,
            directory,
        }
    }
    fn paths(&self) -> store::SessionPaths {
        store::SessionPaths::new(self.directory.path().canonicalize().unwrap().join("sessions"))
    }

    fn snapshot(&self, phase: &str) -> std::io::Result<()> {
        let Some(root) = std::env::var_os("TASK9_PROOF_DIR") else {
            return Ok(());
        };
        let destination = std::path::PathBuf::from(root).join(format!("{phase}-{}", uuid::Uuid::new_v4()));
        fn copy(source: &std::path::Path, destination: &std::path::Path) -> std::io::Result<()> {
            crate::storage::create_store_dir(destination, false)?;
            for entry in std::fs::read_dir(source)? {
                let entry = entry?;
                let target = destination.join(entry.file_name());
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    copy(&entry.path(), &target)?;
                } else if kind.is_file() {
                    std::fs::copy(entry.path(), &target)?;
                    horon_agent_platform::fs::set_file_permissions_owner_only(&target)?;
                } else {
                    return Err(std::io::Error::other("fixture evidence contains a non-regular entry"));
                }
            }
            Ok(())
        }
        copy(self.directory.path(), &destination)?;
        println!("TASK9_FIXTURE {}", destination.display());
        Ok(())
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        if let Err(error) = self.snapshot("final") {
            eprintln!("TASK9_EVIDENCE_ERROR {error}");
        }
        match &self.previous {
            Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
            None => crate::env::remove_var("HORON_AGENT_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

fn text(value: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text: value.into(),
        cache_control: None,
    }]
}
fn seed(id: &str) -> Session {
    let mut session = Session::create_with_id(id.into(), None, None);
    session.provider_key = Some("openai".into());
    session.model = Some("local-model".into());
    session.route_api_method = Some("openai-api".into());
    session.add_message(Role::User, text("original"));
    session.save().unwrap();
    session
}
fn page(session: &Session, parent: Option<store::EventId>, value: &str) -> SessionEventPage {
    let event = store::EventDraft::new(
        store::SessionId::parse(&session.id).unwrap(),
        store::EventId::new_v4(),
        parent,
        store::OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        store::EventKind::new(store::KnownEventKind::MessageAppended),
        serde_json::json!({"message": StoredMessage {
            turn_start: false,
            id: uuid::Uuid::new_v4().to_string(), role: Role::User, content: text(value),
            display_role: None, timestamp: None, tool_duration_ms: None, token_usage: None,
        }}),
    )
    .seal()
    .unwrap();
    SessionEventPage {
        protocol_version: REPLICATION_PROTOCOL_VERSION,
        session_id: store::SessionId::parse(&session.id).unwrap(),
        heads: vec![event.envelope().event_id.clone()],
        events: vec![event.envelope().canonical_record().unwrap()],
        next_cursor: None,
        blob_refs: vec![],
        record_refs: vec![],
    }
}
fn successor(session: &Session, value: &str) -> SessionEventPage {
    page(
        session,
        Some(store::EventId::parse(session.selected_event_head().unwrap()).unwrap()),
        value,
    )
}
fn error_code(error: &anyhow::Error) -> store::StoreErrorCode {
    error.downcast_ref::<store::StoreError>().unwrap().code
}

#[test]
fn session_replication_idle_reuses_writer_and_refreshes_tail_without_duplicate_originals() {
    // Given a live actor that still owns the actual filesystem lock.
    let home = Home::new();
    let mut session = seed("replica_idle");
    let wire = successor(&session, "remote suffix");
    let original = std::fs::read(session.event_log_path().unwrap()).unwrap();
    assert_eq!(
        store::replication::import_events(&home.paths(), &wire)
            .unwrap_err()
            .code,
        store::StoreErrorCode::Busy
    );
    let before = session.event_generation().unwrap();
    // When the same actor imports and then receives an identical retransmission.
    session.import_replica(wire.clone(), None).unwrap();
    let changed = session.event_generation().unwrap();
    let imported = std::fs::read(session.event_log_path().unwrap()).unwrap();
    session.import_replica(wire, None).unwrap();
    // Then one original was added, the actor identity stayed, and both readers see it.
    assert_eq!(before.incarnation, changed.incarnation);
    assert!(changed.generation > before.generation);
    assert_eq!(session.event_generation().unwrap(), changed);
    assert_eq!(std::fs::read(session.event_log_path().unwrap()).unwrap(), imported);
    assert!(imported.starts_with(&original));
    assert_eq!(
        session.active_context().last().unwrap().content_preview(),
        "remote suffix"
    );
    assert_eq!(Session::load(&session.id).unwrap().active_context().len(), 2);
}

#[test]
fn session_replication_save_sync_returns_busy_before_save_release() {
    // Given a real save paused at the filesystem synchronization boundary.
    let _home = Home::new();
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let hook_armed = Arc::clone(&armed);
    let mut session = Session::create_with_id("replica_busy".into(), None, None);
    session
        .set_event_io_hook(Box::new(move |point| {
            if point == store::IoPoint::LogDataSync && hook_armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
                ready_tx.send("SAVE_READY").unwrap();
                release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            }
            Ok(())
        }))
        .unwrap();
    session.add_message(Role::User, text("first"));
    session.save().unwrap();
    let mut importer = session.clone();
    let wire = successor(&session, "remote");
    session.add_message(Role::User, text("local"));
    armed.store(true, std::sync::atomic::Ordering::SeqCst);
    let save = std::thread::spawn(move || {
        session.save().unwrap();
        session
    });
    assert_eq!(ready_rx.recv_timeout(Duration::from_secs(10)).unwrap(), "SAVE_READY");
    // When import races the paused save, it must return before release.
    let result = importer.import_replica(wire, None);
    release_tx.send(()).unwrap();
    let saved = save.join().unwrap();
    // Then the nonqueued refusal leaves the local save intact.
    assert_eq!(error_code(&result.unwrap_err()), store::StoreErrorCode::Busy);
    assert_eq!(saved.history_messages().unwrap().len(), 2);
}

#[test]
fn session_replication_partial_sync_retry_keeps_details_and_invalidates_jobs() {
    // Given a real writer whose next log sync fails once.
    let _home = Home::new();
    let fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hook_fail = Arc::clone(&fail);
    let mut session = Session::create_with_id("replica_partial".into(), None, None);
    session
        .set_event_io_hook(Box::new(move |point| {
            if point == store::IoPoint::LogDataSync && hook_fail.swap(false, std::sync::atomic::Ordering::SeqCst) {
                return Err(std::io::Error::other("injected real sync failure"));
            }
            Ok(())
        }))
        .unwrap();
    session.add_message(Role::User, text("original"));
    session.save().unwrap();
    let wire = successor(&session, "durable after retry");
    let before = session.event_generation().unwrap();
    fail.store(true, std::sync::atomic::Ordering::SeqCst);
    // When the import fails and the same page resumes.
    let error = session.import_replica(wire.clone(), None).unwrap_err();
    let error = error.downcast_ref::<store::StoreError>().unwrap();
    assert_eq!(error.code, store::StoreErrorCode::Io);
    assert!(error.details.accepted_ids.is_empty());
    let cursor = error.details.retry_cursor.clone().expect("bound retry cursor");
    assert_ne!(session.event_generation().unwrap(), before);
    assert!(session.ensure_event_current().is_err());
    session.import_replica(wire, Some(cursor)).unwrap();
    // Then the uncertain prefix was reconciled, not duplicated.
    session.ensure_event_current().unwrap();
    assert_eq!(session.history_messages().unwrap().len(), 2);
    assert_eq!(
        session.active_context().last().unwrap().content_preview(),
        "durable after retry"
    );
}

#[test]
fn session_replication_divergence_preserves_both_heads_and_requires_selection() {
    // Given a remote child of the old head and a newer local child.
    let home = Home::new();
    let mut session = seed("replica_branches");
    let remote = successor(&session, "remote branch");
    session.add_message(Role::User, text("local branch"));
    session.save().unwrap();
    let local_head = session.selected_event_head().unwrap();
    let before = session.event_generation().unwrap();
    // When import commits a divergent head.
    let error = session.import_replica(remote.clone(), None).unwrap_err();
    // Then conflict details describe preserved originals and old jobs are invalidated.
    let error = error.downcast_ref::<store::StoreError>().unwrap();
    assert_eq!(error.code, store::StoreErrorCode::ConflictHeads);
    assert_eq!(error.details.accepted_ids, vec![remote.events[0].event_id.as_str()]);
    assert_eq!(error.details.conflict_heads.len(), 2);
    assert_ne!(session.event_generation().unwrap(), before);
    assert!(session.ensure_event_current().is_err());
    session.select_event_head(&local_head).unwrap();
    assert_eq!(
        session.active_context().last().unwrap().content_preview(),
        "local branch"
    );
    let graph = store::EventGraph::open(&home.paths(), &remote.session_id).unwrap();
    assert_eq!(graph.heads().len(), 2);
}
