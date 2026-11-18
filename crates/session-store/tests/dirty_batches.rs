use horon_agent_message_types::{ContentBlock, Role};
use horon_agent_session_store::*;
use horon_agent_session_types::StoredMessage;
use horon_agent_time::Timestamp;
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
        Timestamp::UNIX_EPOCH,
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
    message_after(&parent.envelope().session_id, parent.envelope().event_id.clone())
}

fn message_after(session: &SessionId, parent: EventId) -> SealedEvent {
    EventDraft::new(
        session.clone(),
        EventId::new_v4(),
        Some(parent),
        OriginId::new_v4(),
        Timestamp::UNIX_EPOCH,
        EventKind::new(KnownEventKind::MessageAppended),
        serde_json::json!({"message": StoredMessage { original_input: None, arg_repairs: None,
        degradations: None,
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
fn one_batch_creates_one_unflushed_marker_and_commits_each_original_before_returning() {
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
            assert_eq!(count(&points, IoPoint::DirtyCreated), 1);
            writer.rebuild_cache(None)
        })
        .unwrap();
    // The marker is created once for the batch and never flushed.
    assert_eq!(count(&points, IoPoint::DirtyCreated), 1);
    assert_eq!(count(&points, IoPoint::DirtyFileSync), 0);
    assert_eq!(count(&points, IoPoint::DirtyDirectorySync), 0);
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
fn failed_marker_create_is_not_reused_by_the_reserved_byte_retry() {
    let (paths, id, mut writer, root, points, armed) = fixture(Some(IoPoint::DirtyCreated));
    let prefix = fs::read(paths.events_log(&id).unwrap()).unwrap();
    let first = message(&root);
    let second = message(&first);
    armed.store(true, Ordering::SeqCst);
    let failed = writer.batched(|writer| writer.append(&first));
    assert!(failed.is_err());
    assert_eq!(fs::read(paths.events_log(&id).unwrap()).unwrap(), prefix);
    assert_eq!(count(&points, IoPoint::LogWrite), 0, "no original before its marker");
    writer
        .batched(|writer| -> Result<(), StoreError> {
            writer.append(&first)?;
            writer.append(&second)?;
            writer.rebuild_cache(None)
        })
        .unwrap();
    // The failed attempt counts too, and the retry creates the marker again.
    assert_eq!(count(&points, IoPoint::DirtyCreated), 2);
    assert_eq!(count(&points, IoPoint::DirtyDirectorySync), 0);
    assert_eq!(full_scan(&paths, &id).unwrap().events.len(), 3);
    assert_eq!(count(&points, IoPoint::LogPublished), 2);
    assert!(!paths.root().join(".catalog.dirty/cache").exists());
    drop(writer);
    fs::remove_dir_all(paths.root()).unwrap();
}

#[test]
fn a_removed_or_replaced_marker_is_recreated_within_the_same_batch() {
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
        assert_eq!(count(&points, IoPoint::DirtyCreated), 2);
        assert_eq!(count(&points, IoPoint::DirtyDirectorySync), 0);
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
fn the_marker_is_created_before_the_originals_and_never_flushed() {
    // Given a session whose marker is created by an ordinary append.
    let (paths, id, mut writer, root, points, _) = fixture(None);
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
    // And no flush ran for it: the stamp check on every derived file, not the
    // marker, is what keeps a crash from serving a stale cache.
    assert_eq!(count(&points, IoPoint::DirtyCreated), 1);
    assert_eq!(count(&points, IoPoint::DirtyFileSync), 0);
    assert_eq!(count(&points, IoPoint::DirtyDirectorySync), 0);
    assert_eq!(full_scan(&paths, &id).unwrap().events.len(), 2);
    drop(writer);
    fs::remove_dir_all(paths.root()).unwrap();

    // When the marker cannot be created, the append is refused before any log
    // bytes are written, so a retry publishes exactly one record.
    let (paths, id, mut writer, root, points, armed) = fixture(Some(IoPoint::DirtyCreated));
    let prefix = fs::read(paths.events_log(&id).unwrap()).unwrap();
    let next = message(&root);
    armed.store(true, Ordering::SeqCst);
    assert!(
        writer.append(&next).is_err(),
        "a failed marker create cannot be acknowledged"
    );
    assert_eq!(fs::read(paths.events_log(&id).unwrap()).unwrap(), prefix);
    assert_eq!(count(&points, IoPoint::LogWrite), 0);
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
    assert_eq!(marker_len, 0, "the marker carries no bytes");
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

/// A session whose listing row and checkpoint are current at one message, then
/// one more save whose original and marker reached the disk while nothing
/// derived was published: the window a crash leaves behind.
fn crash_window() -> (SessionPaths, SessionId, SealedEvent, SealedEvent) {
    let (paths, id, mut writer, root, _points, _) = fixture(None);
    let first = message(&root);
    writer
        .batched(|writer| -> Result<(), StoreError> {
            writer.append(&first)?;
            writer.rebuild_cache(None)
        })
        .unwrap();
    let second = message(&first);
    writer.batched(|writer| writer.append(&second).map(|_| ())).unwrap();
    drop(writer);
    (paths, id, first, second)
}

/// The listing and the context a reader gets must both end at `head`.
fn assert_readers_serve(paths: &SessionPaths, id: &SessionId, head: &EventId, messages: usize, case: &str) {
    let rows = catalog::list(paths, ReadProbe::default()).unwrap();
    assert_eq!(rows.len(), 1, "{case}");
    assert_eq!(&rows[0].head, head, "{case}: the listing stopped at a stale head");
    let tail = checkpoint::SessionCache::new(paths.clone(), ReadProbe::default())
        .tail_view(id, None)
        .unwrap();
    assert_eq!(tail.messages.len(), messages, "{case}: the context lost a message");
}

#[test]
fn a_marker_lost_to_power_loss_never_serves_a_stale_listing_or_context() {
    // The marker is not flushed, so a power loss may drop its entry or the
    // whole marker directory. The original log was flushed before the save
    // returned, so the readers must still reach the second message through
    // the stamp check alone.
    for lose in ["nothing", "the marker", "the marker directory"] {
        let (paths, id, _first, second) = crash_window();
        let dirty = paths.root().join(".catalog.dirty");
        match lose {
            "the marker" => fs::remove_file(dirty.join("cache")).unwrap(),
            "the marker directory" => fs::remove_dir_all(&dirty).unwrap(),
            _ => {}
        }
        assert_readers_serve(&paths, &id, &second.envelope().event_id, 2, lose);
        // A later save works on the repaired store, marker directory or not.
        let mut writer = BlockingWriter::with_hook(paths.clone(), id.clone(), Box::new(|_| Ok(()))).unwrap();
        writer.enable_cache(ReadProbe::default());
        let third = message(&second);
        writer.append(&third).unwrap();
        drop(writer);
        assert_readers_serve(&paths, &id, &third.envelope().event_id, 3, lose);
        fs::remove_dir_all(paths.root()).unwrap();
    }
}

const CRASH_CHILD_ROOT: &str = "HORON_AGENT_STORE_TEST_CRASH_ROOT";

/// Child half of the kill test: one save writes its original, then the process
/// dies inside the batch before its log flush or any derived publication.
#[test]
fn crash_child_dies_inside_a_save() {
    let Ok(root) = std::env::var(CRASH_CHILD_ROOT) else {
        return;
    };
    let paths = SessionPaths::new(root);
    let id = SessionId::parse("cache").unwrap();
    let mut writer = BlockingWriter::with_hook(paths.clone(), id.clone(), Box::new(|_| Ok(()))).unwrap();
    writer.enable_cache(ReadProbe::default());
    let last = full_scan(&paths, &id).unwrap().events.last().unwrap().event_id.clone();
    let next = message_after(&id, last);
    let _ = writer.batched(|writer| -> Result<(), StoreError> {
        writer.append(&next)?;
        std::process::abort()
    });
}

#[test]
fn a_writer_killed_inside_a_save_leaves_originals_the_next_reader_serves() {
    for lose_marker in [false, true] {
        let (paths, id, mut writer, root, _points, _) = fixture(None);
        let first = message(&root);
        writer
            .batched(|writer| -> Result<(), StoreError> {
                writer.append(&first)?;
                writer.rebuild_cache(None)
            })
            .unwrap();
        drop(writer);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "crash_child_dies_inside_a_save",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CRASH_CHILD_ROOT, paths.root())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(!status.success(), "the child must die inside its save");
        if lose_marker {
            fs::remove_dir_all(paths.root().join(".catalog.dirty")).unwrap();
        }
        let events = full_scan(&paths, &id).unwrap().events;
        assert_eq!(events.len(), 3, "the killed save's original stays in the log");
        let case = if lose_marker { "killed, marker lost" } else { "killed" };
        assert_readers_serve(&paths, &id, &events[2].event_id, 2, case);
        fs::remove_dir_all(paths.root()).unwrap();
    }
}

unsafe extern "C" {
    fn fork() -> i32;
    fn pipe(fds: *mut i32) -> i32;
    fn close(fd: i32) -> i32;
    fn read(fd: i32, buffer: *mut std::ffi::c_void, count: usize) -> isize;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn _exit(code: i32) -> !;
}

/// A forked child that keeps its copy of every open description, and with it the
/// writer lock, until this value is dropped.
struct ForkedCopy {
    pid: i32,
    release: i32,
}

impl ForkedCopy {
    /// Forks the test process. The child runs nothing but async-signal-safe
    /// calls: it waits for the release pipe to close, then leaves.
    fn of_current_process() -> Self {
        let mut ends = [0i32; 2];
        assert_eq!(unsafe { pipe(ends.as_mut_ptr()) }, 0, "the release pipe must open");
        let pid = unsafe { fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            let mut byte = [0u8; 1];
            unsafe {
                close(ends[1]);
                while read(ends[0], byte.as_mut_ptr().cast(), 1) > 0 {}
                close(ends[0]);
                _exit(0);
            }
        }
        unsafe { close(ends[0]) };
        Self { pid, release: ends[1] }
    }
}

impl Drop for ForkedCopy {
    fn drop(&mut self) {
        let mut status = 0;
        unsafe {
            close(self.release);
            waitpid(self.pid, &mut status, 0);
        }
    }
}

/// A child of `fork` holds a copy of every open description, including the
/// `flock` the writer owns, and the child keeps that copy until it exits.
/// Releasing by closing only this process's handle therefore leaves the session
/// owned past the writer's own drop, and the next save is refused with Busy.
/// The failing macOS integration run of
/// `a_marker_lost_to_power_loss_never_serves_a_stale_listing_or_context` is that
/// window: a sibling test forked, the reader's temporary repair writer finished,
/// and the save that followed saw the session still owned.
#[test]
fn a_finished_writer_releases_a_lock_a_forked_child_copied() {
    let (paths, id, mut writer, root, _points, _) = fixture(None);
    let first = message(&root);
    writer.batched(|writer| writer.append(&first).map(|_| ())).unwrap();
    // The fork happens while the writer holds the lock, so the child inherits it.
    let child = ForkedCopy::of_current_process();
    // A live writer still excludes a second one.
    let busy = BlockingWriter::open(paths.clone(), id.clone())
        .err()
        .expect("a live writer must exclude a second writer");
    assert_eq!(busy.code, StoreErrorCode::Busy);
    drop(writer);
    // The finished writer released the description itself, so the next save is
    // served even though the child still holds its copy.
    let mut writer = BlockingWriter::with_hook(paths.clone(), id.clone(), Box::new(|_| Ok(())))
        .expect("a finished writer must not leave the session owned");
    writer.enable_cache(ReadProbe::default());
    let second = message(&first);
    writer.append(&second).unwrap();
    drop(writer);
    assert_readers_serve(&paths, &id, &second.envelope().event_id, 2, "forked copy");
    drop(child);
    fs::remove_dir_all(paths.root()).unwrap();
}
