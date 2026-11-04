use horon_agent_message_types::{ContentBlock, Role};
use horon_agent_session_store::*;
use horon_agent_session_types::StoredMessage;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

mod support;

type Observations = Arc<Mutex<Vec<IoPoint>>>;

fn fixture(
    failure: Option<IoPoint>,
) -> (
    SessionPaths,
    SessionId,
    BlockingWriter,
    SealedEvent,
    Observations,
    Arc<AtomicBool>,
) {
    let paths = SessionPaths::new(support::temp_root("dirty-batch"));
    let id = SessionId::parse("cache").unwrap();
    let points: Observations = Arc::default();
    let observed = Arc::clone(&points);
    let armed = Arc::new(AtomicBool::new(false));
    let fail = Arc::clone(&armed);
    let mut writer = BlockingWriter::with_hook(
        paths.clone(),
        id.clone(),
        Box::new(move |point| {
            observed.lock().unwrap().push(point);
            if Some(point) == failure && fail.swap(false, Ordering::SeqCst) {
                return Err(std::io::Error::from_raw_os_error(28));
            }
            Ok(())
        }),
    )
    .unwrap();
    writer.enable_cache(ReadProbe::default());
    let root = EventDraft::new(
        id.clone(),
        EventId::new_v4(),
        None,
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::new(KnownEventKind::SessionCreated),
        serde_json::json!({"metadata": SessionMetadata::default(), "branch_root": false}),
    )
    .seal()
    .unwrap();
    writer.append(&root).unwrap();
    points.lock().unwrap().clear();
    (paths, id, writer, root, points, armed)
}

fn message(parent: &SealedEvent) -> SealedEvent {
    EventDraft::new(
        parent.envelope().session_id.clone(),
        EventId::new_v4(),
        Some(parent.envelope().event_id.clone()),
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::new(KnownEventKind::MessageAppended),
        serde_json::json!({"message": StoredMessage {
            turn_start: false,
            id: uuid::Uuid::new_v4().to_string(), role: Role::User,
            content: vec![ContentBlock::Text { text: "retained".into(), cache_control: None }],
            display_role: None, timestamp: None, tool_duration_ms: None, token_usage: None,
        }}),
    )
    .seal()
    .unwrap()
}

fn count(points: &Observations, point: IoPoint) -> usize {
    points
        .lock()
        .unwrap()
        .iter()
        .filter(|&&observed| observed == point)
        .count()
}

#[test]
fn one_batch_keeps_one_durable_marker_and_commits_each_original_before_returning() {
    let (paths, id, mut writer, root, points, _) = fixture(None);
    let prefix = fs::read(paths.events_log(&id).unwrap()).unwrap();
    let first = message(&root);
    let second = message(&first);
    writer
        .batched(|writer| -> Result<(), StoreError> {
            writer.append(&first)?;
            assert_eq!(count(&points, IoPoint::LogPublished), 1);
            assert_eq!(full_scan(&paths, &id)?.events.len(), 2);
            writer.append(&second)?;
            assert_eq!(count(&points, IoPoint::LogPublished), 2);
            assert_eq!(count(&points, IoPoint::DirtyFileSync), 1);
            writer.rebuild_cache(None)
        })
        .unwrap();
    assert_eq!(count(&points, IoPoint::DirtyFileSync), 1);
    assert_eq!(count(&points, IoPoint::DirtyDirectorySync), 1);
    assert_eq!(count(&points, IoPoint::LogDataSync), 2);
    assert_eq!(count(&points, IoPoint::LogFileSync), 2);
    assert_eq!(count(&points, IoPoint::LogDirectorySync), 2);
    assert!(fs::read(paths.events_log(&id).unwrap()).unwrap().starts_with(&prefix));
    assert_eq!(full_scan(&paths, &id).unwrap().events.len(), 3);
    assert!(!paths.root().join(".catalog.dirty/cache").exists());
    drop(writer);
    fs::remove_dir_all(paths.root()).unwrap();
}

#[test]
fn failed_marker_sync_is_not_reused_by_the_reserved_byte_retry() {
    for point in [IoPoint::DirtyFileSync, IoPoint::DirtyDirectorySync] {
        let (paths, id, mut writer, root, points, armed) = fixture(Some(point));
        let prefix = fs::read(paths.events_log(&id).unwrap()).unwrap();
        let first = message(&root);
        let second = message(&first);
        armed.store(true, Ordering::SeqCst);
        let failed = writer.batched(|writer| writer.append(&first));
        assert!(failed.is_err());
        assert_eq!(fs::read(paths.events_log(&id).unwrap()).unwrap(), prefix);
        writer
            .batched(|writer| -> Result<(), StoreError> {
                writer.append(&first)?;
                writer.append(&second)?;
                writer.rebuild_cache(None)
            })
            .unwrap();
        assert_eq!(count(&points, IoPoint::DirtyFileSync), 2);
        assert_eq!(
            count(&points, IoPoint::DirtyDirectorySync),
            if point == IoPoint::DirtyFileSync { 1 } else { 2 }
        );
        assert_eq!(full_scan(&paths, &id).unwrap().events.len(), 3);
        assert_eq!(count(&points, IoPoint::LogPublished), 2);
        assert!(!paths.root().join(".catalog.dirty/cache").exists());
        drop(writer);
        fs::remove_dir_all(paths.root()).unwrap();
    }
}

#[test]
fn a_removed_or_replaced_marker_is_resynced_within_the_same_batch() {
    for replace in [false, true] {
        let (paths, id, mut writer, root, points, _) = fixture(None);
        let first = message(&root);
        let second = message(&first);
        let marker = paths.root().join(".catalog.dirty/cache");
        writer
            .batched(|writer| -> Result<(), StoreError> {
                writer.append(&first)?;
                if replace {
                    // Keep the old inode allocated until the replacement exists.
                    let prior = marker.with_extension("prior");
                    fs::rename(&marker, &prior)?;
                    fs::write(&marker, b"dirty\n")?;
                    fs::set_permissions(&marker, fs::Permissions::from_mode(0o600))?;
                    fs::remove_file(prior)?;
                } else {
                    fs::remove_file(&marker)?;
                }
                writer.append(&second)?;
                writer.rebuild_cache(None)
            })
            .unwrap();
        assert_eq!(count(&points, IoPoint::DirtyFileSync), 2);
        assert_eq!(count(&points, IoPoint::DirtyDirectorySync), 2);
        assert_eq!(count(&points, IoPoint::LogPublished), 2);
        assert_eq!(full_scan(&paths, &id).unwrap().events.len(), 3);
        assert!(!marker.exists());
        drop(writer);
        fs::remove_dir_all(paths.root()).unwrap();
    }
}

#[test]
fn a_replaced_marker_cannot_redirect_a_later_append_into_the_originals() {
    let (paths, id, mut writer, root, points, _) = fixture(None);
    let first = message(&root);
    let second = message(&first);
    let log = paths.events_log(&id).unwrap();
    let marker = paths.root().join(".catalog.dirty/cache");
    writer
        .batched(|writer| -> Result<(), StoreError> {
            writer.append(&first)?;
            let prefix = fs::read(&log)?;
            fs::remove_file(&marker)?;
            std::os::unix::fs::symlink(&log, &marker)?;
            // O_NOFOLLOW is an I/O refusal in the ordinary writer contract.
            assert_eq!(writer.append(&second).unwrap_err().code, StoreErrorCode::Io);
            assert_eq!(fs::read(&log)?, prefix);
            Ok(())
        })
        .unwrap();
    assert_eq!(writer.append(&second).unwrap_err().code, StoreErrorCode::Io);
    assert_eq!(count(&points, IoPoint::LogPublished), 1);
    assert_eq!(full_scan(&paths, &id).unwrap().events.len(), 2);
    drop(writer);
    fs::remove_dir_all(paths.root()).unwrap();
}
