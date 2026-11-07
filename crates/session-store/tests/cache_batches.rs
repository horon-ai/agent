use horon_agent_message_types::{ContentBlock, Role};
use horon_agent_session_store::checkpoint::SessionCache;
use horon_agent_session_store::*;
use horon_agent_session_types::StoredMessage;
use std::fs;

mod support;

fn fixture() -> (SessionPaths, SessionId) {
    (
        SessionPaths::new(support::temp_root("cache")),
        SessionId::parse("cache").unwrap(),
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
        serde_json::json!({"message": StoredMessage {
            turn_start: false,
            id: uuid::Uuid::new_v4().to_string(), role: Role::User,
            content: vec![ContentBlock::Text { text: text.into(), cache_control: None }],
            display_role: None, timestamp: None, tool_duration_ms: None, token_usage: None,
        }}),
    )
    .seal()
    .unwrap()
}

fn seeded() -> (SessionPaths, SessionId, SealedEvent) {
    let (paths, id) = fixture();
    let root = created(&id);
    let message = appended(&root, "retained user intent");
    let mut writer = BlockingWriter::open(paths.clone(), id.clone()).unwrap();
    writer.enable_cache(ReadProbe::default());
    writer.append(&root).unwrap();
    writer.append(&message).unwrap();
    assert!(writer.cache_warning().is_none(), "{:?}", writer.cache_warning());
    fs::copy(
        paths.events_log(&id).unwrap(),
        paths.session_dir(&id).unwrap().join(".initial-events.raw"),
    )
    .unwrap();
    (paths, id, message)
}

fn metadata_changed(parent: &SealedEvent) -> SealedEvent {
    EventDraft::new(
        parent.envelope().session_id.clone(),
        EventId::new_v4(),
        Some(parent.envelope().event_id.clone()),
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::new(KnownEventKind::MetadataChanged),
        serde_json::json!({"metadata": SessionMetadata::default()}),
    )
    .seal()
    .unwrap()
}

fn publication_count(probe: &ReadProbe, path: &std::path::Path) -> usize {
    probe
        .publications()
        .unwrap()
        .iter()
        .filter(|receipt| receipt.path == path)
        .count()
}

#[test]
fn batched_commit_publishes_derived_state_once_and_keeps_every_original() {
    // Given a cache-enabled owner and a commit shaped like a save: appended
    // messages followed by the metadata record every save ends with.
    let (paths, id) = fixture();
    let root = created(&id);
    let mut writer = BlockingWriter::open(paths.clone(), id.clone()).unwrap();
    writer.enable_cache(ReadProbe::default());
    writer.append(&root).unwrap();
    let first = appended(&root, "first");
    let second = appended(&first, "second");
    let metadata = metadata_changed(&second);
    let log_path = paths.events_log(&id).unwrap();
    let prefix = fs::read(&log_path).unwrap();
    let probe = ReadProbe::default();
    writer.enable_cache(probe.clone());
    // When the whole commit runs as one synchronous batch and rebuilds once.
    writer
        .batched(|writer| -> std::result::Result<(), StoreError> {
            writer.append(&first)?;
            writer.append(&second)?;
            writer.append(&metadata)?;
            writer.rebuild_cache(None)?;
            Ok(())
        })
        .unwrap();
    // Then every original is durable and each appended byte only extends the prefix.
    let log = fs::read(&log_path).unwrap();
    assert!(
        log.starts_with(&prefix),
        "the durable prefix must stay byte-identical across a batched commit"
    );
    assert_eq!(full_scan(&paths, &id).unwrap().events.len(), 4);
    // And the derived state was published once for the whole batch.
    let checkpoint = paths.session_dir(&id).unwrap().join("checkpoint.json");
    assert_eq!(
        publication_count(&probe, &checkpoint),
        1,
        "one commit must publish its checkpoint once, not once per event"
    );
    assert_eq!(
        publication_count(&probe, &paths.root().join(".catalog.index.jsonl")),
        1,
        "one commit must publish its listing row once, not once per event"
    );
    assert!(writer.cache_warning().is_none(), "{:?}", writer.cache_warning());
    assert!(!paths.root().join(".catalog.dirty/cache").exists());
    drop(writer);
    // And the published listing names the committed head.
    assert_eq!(
        catalog::list(&paths, ReadProbe::default()).unwrap()[0].head,
        metadata.envelope().event_id
    );
}

#[test]
fn batched_commit_keeps_originals_when_its_single_rebuild_fails_and_retry_repairs_it() {
    // Given a one-shot derived publication failure armed for the batch rebuild.
    let (paths, id) = fixture();
    let root = created(&id);
    let armed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let gate = std::sync::Arc::clone(&armed);
    let mut writer = BlockingWriter::with_hook(
        paths.clone(),
        id.clone(),
        Box::new(move |point| {
            if point == IoPoint::CacheStaged && gate.swap(false, std::sync::atomic::Ordering::SeqCst) {
                return Err(std::io::Error::from_raw_os_error(28));
            }
            Ok(())
        }),
    )
    .unwrap();
    writer.enable_cache(ReadProbe::default());
    writer.append(&root).unwrap();
    let next = appended(&root, "batched");
    armed.store(true, std::sync::atomic::Ordering::SeqCst);
    // When the batch's single derived rebuild fails after its originals are durable.
    let mut rebuild_failed = false;
    writer
        .batched(|writer| -> std::result::Result<(), StoreError> {
            writer.append(&next)?;
            rebuild_failed = writer.rebuild_cache(None).is_err();
            Ok(())
        })
        .unwrap();
    // Then the original commit stands and the derived state stays due.
    assert!(
        rebuild_failed,
        "the armed publication failure must reach the batch rebuild"
    );
    assert!(writer.cache_warning().is_some());
    assert!(paths.root().join(".catalog.dirty/cache").is_file());
    assert_eq!(full_scan(&paths, &id).unwrap().events.len(), 2);
    // And retrying the same reserved event repairs it without a second original.
    assert_eq!(writer.append(&next).unwrap(), AppendOutcome::AlreadyDurable);
    assert!(writer.cache_warning().is_none(), "{:?}", writer.cache_warning());
    assert!(!paths.root().join(".catalog.dirty/cache").exists());
    assert_eq!(full_scan(&paths, &id).unwrap().events.len(), 2);
}

#[test]
fn catalog_contention_defers_the_row_and_never_fails_a_per_session_read() {
    // Given a published session that has to rebuild its own derived state.
    let (paths, id, head) = seeded();
    let session = paths.session_dir(&id).unwrap();
    fs::remove_file(session.join("checkpoint.json")).unwrap();
    // And a catalog owned by another writer.
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(paths.root().join(".catalog.lock"))
        .unwrap();
    held.try_lock().unwrap();
    // When this session rebuilds its derived state under that contention.
    let cache = SessionCache::new(paths.clone(), ReadProbe::default());
    let recent = cache.recent(&id, None).unwrap();
    // Then the per-session read succeeds from its own checkpoint.
    assert_eq!(recent.head, head.envelope().event_id);
    assert!(session.join("checkpoint.json").is_file());
    // And the deferred listing row stays discoverable through its dirty marker.
    assert!(paths.root().join(".catalog.dirty/cache").is_file());
    // And the shared listing retries rather than reporting a row it never published.
    assert_eq!(
        catalog::list(&paths, ReadProbe::default()).unwrap_err().code,
        StoreErrorCode::Busy
    );
    drop(held);
    // Then a released catalog publishes the due row and clears the marker.
    assert_eq!(
        catalog::list(&paths, ReadProbe::default()).unwrap()[0].head,
        head.envelope().event_id
    );
    assert!(!paths.root().join(".catalog.dirty/cache").exists());
}

#[test]
fn failed_nested_batch_does_not_mute_the_next_append() {
    let (paths, id) = fixture();
    let root = created(&id);
    let mut writer = BlockingWriter::open(paths.clone(), id.clone()).unwrap();
    writer.enable_cache(ReadProbe::default());
    writer.append(&root).unwrap();
    let probe = ReadProbe::default();
    writer.enable_cache(probe.clone());
    let first = appended(&root, "committed before the error");
    let result: Result<(), StoreError> = writer.batched(|outer| {
        outer.batched(|inner| {
            inner.append(&first)?;
            Err(StoreError::busy("injected batch failure"))
        })
    });
    assert!(result.is_err());
    assert!(paths.root().join(".catalog.dirty/cache").is_file());
    let second = appended(&first, "normal append after the error");
    writer.append(&second).unwrap();
    assert_eq!(full_scan(&paths, &id).unwrap().events.len(), 3);
    assert_eq!(
        publication_count(&probe, &paths.session_dir(&id).unwrap().join("checkpoint.json")),
        1
    );
    assert!(!paths.root().join(".catalog.dirty/cache").exists());
}

#[test]
fn caught_batch_panic_does_not_mute_a_reserved_event_retry() {
    let (paths, id) = fixture();
    let root = created(&id);
    let mut writer = BlockingWriter::open(paths.clone(), id.clone()).unwrap();
    writer.enable_cache(ReadProbe::default());
    writer.append(&root).unwrap();
    let probe = ReadProbe::default();
    writer.enable_cache(probe.clone());
    let next = appended(&root, "committed before the panic");
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        writer.batched::<(), StoreError>(|writer| {
            writer.append(&next).unwrap();
            panic!("injected batch panic")
        })
    }));
    assert!(failed.is_err());
    assert!(paths.root().join(".catalog.dirty/cache").is_file());
    let before = fs::read(paths.events_log(&id).unwrap()).unwrap();
    assert_eq!(writer.append(&next).unwrap(), AppendOutcome::AlreadyDurable);
    assert_eq!(before, fs::read(paths.events_log(&id).unwrap()).unwrap());
    assert_eq!(
        publication_count(&probe, &paths.session_dir(&id).unwrap().join("checkpoint.json")),
        1
    );
    assert!(!paths.root().join(".catalog.dirty/cache").exists());
}
