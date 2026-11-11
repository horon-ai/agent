//! Crash windows around derived publications.
//!
//! Only originals and the dirty marker are flushed. These tests reproduce the
//! window that leaves behind: a lost rename, a marker removal that never reached
//! the disk, a listing row that a originals-only append left stale, and a cache
//! file truncated by a crash. In every case the reader must rebuild from the
//! originals instead of trusting the derived bytes.

use horon_agent_message_types::{ContentBlock, Role};
use horon_agent_session_store::checkpoint::SessionCache;
use horon_agent_session_store::*;
use horon_agent_session_types::StoredMessage;
use std::fs;
use std::path::{Path, PathBuf};

mod support;

fn fixture() -> (SessionPaths, SessionId) {
    (
        SessionPaths::new(support::temp_root("durability")),
        SessionId::parse("durable").unwrap(),
    )
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
    .unwrap()
}

fn appended(parent: &SealedEvent, text: &str) -> SealedEvent {
    EventDraft::new(
        parent.envelope().session_id.clone(),
        EventId::new_v4(),
        Some(parent.envelope().event_id.clone()),
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::new(KnownEventKind::MessageAppended),
        serde_json::json!({"message": StoredMessage { original_input: None, arg_repairs: None,
        degradations: None,
            turn_start: false,
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: text.into(),
                cache_control: None,
            }],
            display_role: None,
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
        }}),
    )
    .seal()
    .unwrap()
}

/// A session whose derived cache is warm and whose listing row is current.
fn seeded() -> (SessionPaths, SessionId, SealedEvent) {
    let (paths, id) = fixture();
    let root = created(&id);
    let message = appended(&root, "retained user intent");
    let mut writer = BlockingWriter::open(paths.clone(), id.clone()).unwrap();
    writer.enable_cache(ReadProbe::default());
    writer.append(&root).unwrap();
    writer.append(&message).unwrap();
    assert!(writer.cache_warning().is_none(), "{:?}", writer.cache_warning());
    (paths, id, message)
}

fn marker(paths: &SessionPaths, id: &SessionId) -> PathBuf {
    paths.root().join(".catalog.dirty").join(id.as_str())
}

/// Recreate a dirty marker exactly as the writer creates it: an owner-only,
/// empty file whose existence is the whole message.
fn recreate_marker(path: &Path) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    horon_agent_platform::fs::request_owner_only_mode(&mut options);
    options.open(path).map(drop)
}

fn original_bytes(probe: &ReadProbe, paths: &SessionPaths, id: &SessionId) -> usize {
    probe
        .receipts()
        .unwrap()
        .iter()
        .filter(|r| r.path == paths.events_log(id).unwrap())
        .map(|r| r.bytes)
        .sum()
}

#[test]
fn lost_cache_rename_rebuilds_on_load() {
    // Given a warm cache and the checkpoint bytes the previous head published.
    let (paths, id, head) = seeded();
    let checkpoint = paths.session_dir(&id).unwrap().join("checkpoint.json");
    let previous = fs::read(&checkpoint).unwrap();
    let log_before = fs::read(paths.events_log(&id).unwrap()).unwrap();
    let next = appended(&head, "published after the crash window");
    // When the checkpoint rename is not durable - the name resolves to the new
    // file during publication, but the old bytes are what survived the crash.
    let stale = previous.clone();
    let target = checkpoint.clone();
    let armed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let observed = std::sync::Arc::clone(&armed);
    let mut writer = BlockingWriter::with_hook(
        paths.clone(),
        id.clone(),
        Box::new(move |point| {
            if observed.load(std::sync::atomic::Ordering::SeqCst)
                && point == IoPoint::CacheRenamed
                && fs::read(&target).ok().as_deref() != Some(stale.as_slice())
            {
                observed.store(false, std::sync::atomic::Ordering::SeqCst);
                fs::write(&target, &stale)?;
            }
            Ok(())
        }),
    )
    .unwrap();
    writer.enable_cache(ReadProbe::default());
    assert_eq!(writer.append(&next).unwrap(), AppendOutcome::Committed);
    assert!(
        !armed.load(std::sync::atomic::Ordering::SeqCst),
        "the checkpoint rename must have been observed"
    );
    drop(writer);
    // Then the log holds the new original while the derived file still names the
    // previous head, which is exactly what an unsynchronized rename can leave.
    assert!(fs::read(paths.events_log(&id).unwrap()).unwrap().len() > log_before.len());
    assert_eq!(fs::read(&checkpoint).unwrap(), previous);
    // When the session is loaded through the cache.
    let probe = ReadProbe::default();
    let context = SessionCache::new(paths.clone(), probe.clone())
        .recent(&id, None)
        .unwrap();
    // Then the stale stamp forces a rebuild from the originals, not a stale hit.
    assert_eq!(context.head, next.envelope().event_id);
    assert!(
        original_bytes(&probe, &paths, &id) > 0,
        "the rebuild must read the original log"
    );
    assert_ne!(
        fs::read(&checkpoint).unwrap(),
        previous,
        "the load must republish the checkpoint for the current head"
    );
    assert!(
        SessionCache::new(paths.clone(), ReadProbe::default())
            .recent(&id, None)
            .is_ok()
    );
}

#[test]
fn lost_dirty_removal_keeps_marker_and_rebuilds() {
    // Given a session whose marker removal does not survive the crash.
    let (paths, id) = fixture();
    let event = created(&id);
    let path = marker(&paths, &id);
    let recreated = path.clone();
    let mut writer = BlockingWriter::with_hook(
        paths.clone(),
        id.clone(),
        Box::new(move |point| {
            if point == IoPoint::DirtyRemoved {
                // The unlink reached the directory entry but not the disk, so the
                // marker is back the next time the directory is read.
                recreate_marker(&recreated)?;
            }
            Ok(())
        }),
    )
    .unwrap();
    writer.enable_cache(ReadProbe::default());
    writer.append(&event).unwrap();
    assert!(writer.cache_warning().is_none(), "{:?}", writer.cache_warning());
    drop(writer);
    // Then the surviving marker still marks the row as due.
    assert!(path.is_file(), "the lost removal must leave the marker behind");
    // When the next listing runs while that marker exists.
    let probe = ReadProbe::default();
    let rows = catalog::list(&paths, probe.clone()).unwrap();
    // Then the row is rebuilt from the originals instead of reused as current.
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].head, event.envelope().event_id);
    assert!(
        original_bytes(&probe, &paths, &id) > 0,
        "the rebuild must read the original log"
    );
    // And the repair clears the marker again.
    assert!(!path.exists(), "the repair must clear the due marker");
    assert_eq!(
        catalog::list(&paths, ReadProbe::default()).unwrap()[0].head,
        event.envelope().event_id
    );
}

#[test]
fn stale_catalog_row_is_repaired_from_dirty_marker() {
    // Given a warm listing row and an append that published originals only.
    let (paths, id, first) = seeded();
    let index = paths.root().join(".catalog.index.jsonl");
    assert!(String::from_utf8_lossy(&fs::read(&index).unwrap()).contains(first.envelope().event_id.as_str()));
    let next = appended(&first, "committed without a cache publication");
    let mut writer = BlockingWriter::open(paths.clone(), id.clone()).unwrap();
    writer.append(&next).unwrap();
    drop(writer);
    // Then the row still names the previous head while the marker says it is due.
    assert!(marker(&paths, &id).is_file());
    assert!(
        fs::read(&index)
            .map(|bytes| !String::from_utf8_lossy(&bytes).contains(next.envelope().event_id.as_str()))
            .unwrap()
    );
    // When a listing runs after the writer released the session.
    let rows = catalog::list(&paths, ReadProbe::default()).unwrap();
    // Then the due row is repaired from the originals and the marker is cleared.
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].head, next.envelope().event_id);
    assert!(!marker(&paths, &id).exists());
    assert!(String::from_utf8_lossy(&fs::read(&index).unwrap()).contains(next.envelope().event_id.as_str()));
}

#[test]
fn truncated_cache_file_falls_back_to_originals() {
    // Given a warm cache and a crash that left the checkpoint empty.
    let (paths, id, head) = seeded();
    let checkpoint = paths.session_dir(&id).unwrap().join("checkpoint.json");
    let directory = paths.session_dir(&id).unwrap();
    for name in ["checkpoint.json", "summary.json"] {
        fs::write(directory.join(name), b"").unwrap();
    }
    let log_before = fs::read(paths.events_log(&id).unwrap()).unwrap();
    // When the session is read.
    let probe = ReadProbe::default();
    let context = SessionCache::new(paths.clone(), probe.clone())
        .recent(&id, None)
        .unwrap();
    // Then the empty derived files are replaced by a rebuild from the originals.
    assert_eq!(context.head, head.envelope().event_id);
    assert_eq!(context.tail.len(), 1);
    assert!(
        original_bytes(&probe, &paths, &id) > 0,
        "the fallback must read the original log"
    );
    assert_eq!(fs::read(paths.events_log(&id).unwrap()).unwrap(), log_before);
    assert!(
        fs::metadata(&checkpoint).unwrap().len() > 0,
        "the checkpoint must be rebuilt"
    );
    assert!(
        fs::metadata(directory.join("summary.json")).unwrap().len() > 0,
        "the summary must be rebuilt"
    );
}
