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

/// Whether the host confirms a created marker by flushing its directory entry.
///
/// Unix does, so the marker's own flush never runs there; Windows cannot flush a
/// directory, so the file flush is still the boundary. Every count below states
/// which of the two it expects instead of assuming the Unix shape.
fn confirmed_by_directory() -> bool {
    horon_agent_platform::directory_sync_is_effective()
}

/// The marker flush points this host actually observes.
fn marker_sync_points() -> Vec<IoPoint> {
    let mut points = Vec::new();
    if !confirmed_by_directory() {
        points.push(IoPoint::DirtyFileSync);
    }
    points.push(IoPoint::DirtyDirectorySync);
    points
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
            assert_eq!(
                count(&points, IoPoint::DirtyFileSync),
                usize::from(!confirmed_by_directory())
            );
            writer.rebuild_cache(None)
        })
        .unwrap();
    assert_eq!(
        count(&points, IoPoint::DirtyFileSync),
        usize::from(!confirmed_by_directory())
    );
    assert_eq!(count(&points, IoPoint::DirtyDirectorySync), 1);
    assert_eq!(count(&points, IoPoint::LogDataSync), 2);
    // Both originals were written, and one flush confirmed the batch's log.
    assert_eq!(count(&points, IoPoint::LogFileSync), 1);
    assert_eq!(count(&points, IoPoint::LogDirectorySync), 2);
    assert!(fs::read(paths.events_log(&id).unwrap()).unwrap().starts_with(&prefix));
    assert_eq!(full_scan(&paths, &id).unwrap().events.len(), 3);
    assert!(!paths.root().join(".catalog.dirty/cache").exists());
    drop(writer);
    fs::remove_dir_all(paths.root()).unwrap();
}

#[test]
fn failed_marker_sync_is_not_reused_by_the_reserved_byte_retry() {
    for point in marker_sync_points() {
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
        // The failed attempt is counted too: the host flush that failed is
        // retried, and a failure before the directory flush leaves it unspent.
        assert_eq!(
            count(&points, IoPoint::DirtyFileSync),
            if confirmed_by_directory() { 0 } else { 2 }
        );
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
                    // The replacement is empty, exactly as the writer makes it.
                    fs::File::create(&marker)?;
                    fs::set_permissions(&marker, fs::Permissions::from_mode(0o600))?;
                    fs::remove_file(prior)?;
                } else {
                    fs::remove_file(&marker)?;
                }
                writer.append(&second)?;
                writer.rebuild_cache(None)
            })
            .unwrap();
        assert_eq!(
            count(&points, IoPoint::DirtyFileSync),
            if confirmed_by_directory() { 0 } else { 2 }
        );
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

#[test]
fn dirty_marker_survives_with_directory_sync_only() {
    // Given a session whose marker is created by an ordinary append.
    let (paths, id, mut writer, root, points, _) = fixture(None);
    let confirmed = confirmed_by_directory();
    let marker = paths.root().join(".catalog.dirty/cache");
    let first = message(&root);
    // The marker lives only until the derived publication that clears it, so
    // its bytes are inspected inside the batch that created it.
    writer
        .batched(|writer| -> Result<(), StoreError> {
            writer.append(&first)?;
            // Then the marker is an empty file whose existence is the whole
            // message a crash has to preserve.
            assert!(marker.is_file(), "the marker must exist beside the originals");
            assert_eq!(fs::metadata(&marker)?.len(), 0, "the marker carries no bytes");
            Ok(())
        })
        .unwrap();
    // And only the flush that confirms a creation ran: the directory entry
    // where that is durable, the file itself where it is not.
    assert_eq!(count(&points, IoPoint::DirtyFileSync), usize::from(!confirmed));
    assert_eq!(count(&points, IoPoint::DirtyDirectorySync), 1);
    assert_eq!(full_scan(&paths, &id).unwrap().events.len(), 2);
    drop(writer);
    fs::remove_dir_all(paths.root()).unwrap();

    // When the confirming flush fails, the append is refused before any log
    // bytes are written, so a retry publishes exactly one record.
    let (paths, id, mut writer, root, points, armed) = fixture(Some(IoPoint::DirtyDirectorySync));
    let prefix = fs::read(paths.events_log(&id).unwrap()).unwrap();
    let next = message(&root);
    armed.store(true, Ordering::SeqCst);
    assert!(
        writer.append(&next).is_err(),
        "a failed marker flush cannot be acknowledged"
    );
    assert_eq!(fs::read(paths.events_log(&id).unwrap()).unwrap(), prefix);
    assert_eq!(count(&points, IoPoint::LogWrite), 0);
    assert_eq!(count(&points, IoPoint::DirtyFileSync), usize::from(!confirmed));
    writer.append(&next).unwrap();
    assert_eq!(full_scan(&paths, &id).unwrap().events.len(), 2);
    drop(writer);
    fs::remove_dir_all(paths.root()).unwrap();
}

#[test]
fn an_empty_marker_left_by_a_crash_repairs_the_next_listing() {
    // Given a session whose event log and listing row already exist.
    let (paths, id, mut writer, root, _points, _) = fixture(None);
    let prefix = fs::read(paths.events_log(&id).unwrap()).unwrap();
    let marker = paths.root().join(".catalog.dirty/cache");
    let next = message(&root);
    // When one save commits its log line and its empty marker, then the process
    // dies before the listing row is published: the batch publishes derived
    // state only after its last original, and nothing runs that publication.
    writer
        .batched(|writer| -> Result<(), StoreError> {
            writer.append(&next)?;
            Ok(())
        })
        .unwrap();
    let log = fs::read(paths.events_log(&id).unwrap()).unwrap();
    let marker_len = fs::metadata(&marker).unwrap().len();
    println!(
        "CRASH_WINDOW marker_exists={} marker_bytes={marker_len} log_before={} log_after={}",
        marker.is_file(),
        prefix.len(),
        log.len()
    );
    // Then the durable log grew, the marker carries no bytes, and the row is
    // still the previous one.
    assert!(log.starts_with(&prefix));
    assert!(marker.is_file(), "the crash left its marker behind");
    assert_eq!(
        marker_len, 0,
        "the marker is confirmed by its directory entry, not its bytes"
    );
    drop(writer);
    // When the next listing runs against that state.
    let probe = ReadProbe::default();
    let rows = catalog::list(&paths, probe.clone()).unwrap();
    println!("LISTING rows={} head={}", rows.len(), rows[0].head.as_str());
    // Then the marker is what makes it repair the row from the originals.
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].head, next.envelope().event_id);
    let log_path = paths.events_log(&id).unwrap();
    assert!(
        probe.receipts().unwrap().iter().any(|receipt| receipt.path == log_path),
        "the repair must read the original log"
    );
    assert!(!marker.exists(), "the repair clears the marker");
    let index = fs::read_to_string(paths.root().join(".catalog.index.jsonl")).unwrap();
    assert!(index.contains(next.envelope().event_id.as_str()));
    fs::remove_dir_all(paths.root()).unwrap();
}
