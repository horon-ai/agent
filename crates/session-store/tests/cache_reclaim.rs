use horon_agent_session_store::cache_reclaim::reclaim_derived;
use horon_agent_session_store::*;
use horon_agent_time::Timestamp;
use std::fs;

mod support;

fn created(id: &SessionId) -> SealedEvent {
    EventDraft::new(
        id.clone(),
        EventId::new_v4(),
        None,
        OriginId::new_v4(),
        Timestamp::UNIX_EPOCH,
        EventKind::new(KnownEventKind::SessionCreated),
        serde_json::json!({"metadata": SessionMetadata::default(), "branch_root": false}),
    )
    .seal()
    .unwrap()
}

/// A cached writer that has acknowledged its first event, so derived files exist.
fn open_session(paths: &SessionPaths, id: &SessionId) -> BlockingWriter {
    let mut writer = BlockingWriter::open(paths.clone(), id.clone()).unwrap();
    writer.enable_cache(ReadProbe::default());
    writer.append(&created(id)).unwrap();
    writer
}

fn derived(paths: &SessionPaths, id: &SessionId) -> Vec<std::path::PathBuf> {
    let directory = paths.session_dir(id).unwrap();
    [
        "checkpoint.json",
        "sparse-index.jsonl",
        "summary.json",
        "facade-cache.json",
    ]
    .into_iter()
    .map(|name| directory.join(name))
    .filter(|path| path.exists())
    .collect()
}

#[test]
fn dry_run_writes_nothing_and_apply_removes_only_inactive_derived_files() {
    let paths = SessionPaths::new(support::temp_root("cache-reclaim"));
    let idle = SessionId::parse("idle").unwrap();
    let busy = SessionId::parse("busy").unwrap();
    drop(open_session(&paths, &idle));
    let _held = open_session(&paths, &busy);
    let idle_before = derived(&paths, &idle);
    let busy_before = derived(&paths, &busy);
    assert!(
        !idle_before.is_empty() && !busy_before.is_empty(),
        "the cached writer writes derived files"
    );
    let idle_log = fs::read(paths.events_log(&idle).unwrap()).unwrap();
    let busy_log = fs::read(paths.events_log(&busy).unwrap()).unwrap();

    let preview = reclaim_derived(&paths, false).unwrap();
    assert_eq!(preview.active, std::slice::from_ref(&busy));
    assert_eq!(preview.files.len(), idle_before.len());
    assert!(
        preview
            .files
            .iter()
            .all(|file| file.session_id == idle && file.bytes > 0)
    );
    assert_eq!(derived(&paths, &idle), idle_before, "a dry run removes nothing");

    let applied = reclaim_derived(&paths, true).unwrap();
    assert_eq!(applied.files, preview.files);
    assert_eq!(applied.bytes(), preview.bytes());
    assert!(derived(&paths, &idle).is_empty());
    assert_eq!(derived(&paths, &busy), busy_before, "an owned session keeps its caches");
    assert_eq!(fs::read(paths.events_log(&idle).unwrap()).unwrap(), idle_log);
    assert_eq!(fs::read(paths.events_log(&busy).unwrap()).unwrap(), busy_log);

    // The originals alone still answer reads, and the caches are rebuilt on demand.
    let summary = catalog::row(&paths, &idle, ReadProbe::default()).unwrap();
    assert_eq!(summary.session_id, idle);
    assert!(reclaim_derived(&paths, true).unwrap().files.len() <= idle_before.len());
}
