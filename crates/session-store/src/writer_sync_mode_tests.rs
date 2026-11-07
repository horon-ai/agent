//! The configured durability level reaches the original log append.

use crate::*;

fn private_root() -> std::path::PathBuf {
    let root = std::env::temp_dir()
        .canonicalize()
        .expect("the OS temporary directory resolves")
        .join(format!("session-sync-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).expect("fixture root");
    // The writer verifies that its root is owner-private, so the fixture has to
    // be as private as a real store root.
    horon_agent_platform::fs::set_dir_permissions_owner_only(&root).expect("private root");
    root
}

fn created(id: &SessionId) -> SealedEvent {
    EventDraft::new(
        id.clone(),
        EventId::new_v4(),
        None,
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::new(KnownEventKind::SessionCreated),
        serde_json::json!({"metadata": SessionMetadata::default(), "branch_root": false}),
    )
    .seal()
    .expect("sealed creation event")
}

#[test]
fn sync_mode_reaches_log_append() {
    let root = private_root();
    let id = SessionId::parse("fast-sync").expect("valid session id");
    let paths = SessionPaths::new(&root).with_sync_mode(SyncMode::Fast);
    let mut writer = BlockingWriter::open(paths, id.clone()).expect("open writer");
    let event = created(&id);

    // Opening a writer creates and flushes directories; only the append below
    // is allowed to explain the observation.
    let _ = crate::blobs::observation::take();
    writer.append(&event).expect("append the creation event");

    let observed = crate::blobs::observation::take();
    assert!(
        observed.iter().all(|(_, mode)| *mode == SyncMode::Fast),
        "every original boundary must use the configured level: {observed:?}"
    );
    let log_sync: Vec<_> = observed
        .iter()
        .filter(|(point, _)| *point == IoPoint::LogFileSync)
        .collect();
    assert_eq!(log_sync.len(), 1, "one flush confirms the log append: {observed:?}");
    assert_eq!(
        log_sync[0].1,
        SyncMode::Fast,
        "the log append must flush at the configured level"
    );

    std::fs::remove_dir_all(&root).expect("remove fixture root");
}
