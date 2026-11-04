use horon_agent_session_store::catalog;
use horon_agent_session_store::checkpoint::SessionCache;
use horon_agent_session_store::legacy::LegacySource;
use horon_agent_session_store::*;
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::path::PathBuf;

struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("catalog-legacy-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        fs::DirBuilder::new().mode(0o700).create(root.join("sessions")).unwrap();
        println!("FIXTURE {}", root.display());
        Self(root)
    }

    fn paths(&self) -> SessionPaths {
        SessionPaths::new(self.0.join("sessions"))
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
        println!("CLEANED {}", self.0.display());
    }
}

fn seed(home: &Home, name: &str) -> SessionId {
    let id = SessionId::parse(name).unwrap();
    let event = EventDraft::new(
        id.clone(),
        EventId::new_v4(),
        None,
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::new(KnownEventKind::SessionCreated),
        json!({"metadata":SessionMetadata::default(),"branch_root":false}),
    )
    .seal()
    .unwrap();
    BlockingWriter::open(home.paths(), id.clone())
        .unwrap()
        .append(&event)
        .unwrap();
    id
}

#[test]
fn same_home_migration_keeps_originals_and_updates_recent_summary_and_listing() {
    // Given preserved legacy originals beside the same session's new event directory.
    let home = Home::new();
    let paths = home.paths();
    let id = SessionId::parse("old").unwrap();
    let raw = serde_json::to_vec(&json!({
        "id":"old","created_at":"2020-01-01T00:00:00Z",
        "updated_at":"2020-01-02T00:00:00Z","title":"legacy",
        "saved":true,"save_label":"keep",
        "messages":[{"id":"one","role":"user","content":[{"type":"text","text":"preserve me"}]}]
    }))
    .unwrap();
    let preserved = [
        ("old.json", raw.clone()),
        ("old.json.bak", raw),
        ("old.journal.jsonl", Vec::new()),
        ("notes with spaces.txt", b"preserved sidecar".to_vec()),
    ];
    for (name, bytes) in &preserved {
        fs::write(paths.root().join(name), bytes).unwrap();
        println!("PRESERVED {name} {}", sha256_hex(bytes));
    }
    LegacySource::new(&home.0, id.clone()).unwrap().migrate(&paths).unwrap();
    let original_log = fs::read(paths.events_log(&id).unwrap()).unwrap();
    let cache = SessionCache::new(paths.clone(), ReadProbe::default());
    // When rebuilding recent context and catalog rows after same-HOME migration.
    let recent = cache.recent(&id, None).unwrap();
    assert_eq!(recent.tail.len(), 1);
    assert_eq!(recent.tail[0].message.content_preview(), "preserve me");
    let summary_path = paths.session_dir(&id).unwrap().join("summary.json");
    let summary: Value = serde_json::from_slice(&fs::read(&summary_path).unwrap()).unwrap();
    assert_eq!(summary["value"]["session_id"], json!(id));
    assert_eq!(summary["value"]["head"], json!(recent.head));
    let rows = catalog::list(&paths, ReadProbe::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].session_id, id);
    assert_eq!(rows[0].title.as_deref(), Some("legacy"));
    assert!(rows[0].saved);
    assert_eq!(rows[0].save_label.as_deref(), Some("keep"));

    let mut metadata = recent.metadata.clone();
    metadata.title = Some("updated".into());
    let next = EventDraft::new(
        id.clone(),
        EventId::new_v4(),
        Some(recent.head),
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::new(KnownEventKind::MetadataChanged),
        json!({"metadata":metadata}),
    )
    .seal()
    .unwrap();
    BlockingWriter::open(paths.clone(), id.clone())
        .unwrap()
        .append(&next)
        .unwrap();
    let rows = catalog::list(&paths, ReadProbe::default()).unwrap();
    // Then actual public rows and summary follow new originals without touching old files.
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title.as_deref(), Some("updated"));
    assert_eq!(rows[0].head, next.envelope().event_id);
    assert_eq!(cache.recent(&id, None).unwrap().head, rows[0].head);
    let summary: Value = serde_json::from_slice(&fs::read(summary_path).unwrap()).unwrap();
    assert_eq!(summary["value"]["head"], json!(rows[0].head));
    for (name, bytes) in &preserved {
        assert_eq!(&fs::read(paths.root().join(name)).unwrap(), bytes);
    }
    assert!(
        fs::read(paths.events_log(&id).unwrap())
            .unwrap()
            .starts_with(&original_log)
    );
    println!("SAME_HOME recent=true summary=true listing=true updated_row=true originals_unchanged=true");
}

#[test]
fn a_real_session_directory_with_json_suffix_is_not_skipped() {
    // Given a legal session directory whose ID ends in .json.
    let home = Home::new();
    let id = seed(&home, "live.json");
    fs::write(home.paths().root().join("old.json"), b"legacy bytes").unwrap();
    // When listing through the actual catalog.
    let rows = catalog::list(&home.paths(), ReadProbe::default()).unwrap();
    // Then file type, not a filename suffix, determines session discovery.
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].session_id, id);
}

#[test]
fn session_symlinks_are_refused_even_when_named_like_legacy_files() {
    // Given directory, regular-file and dangling symlinks beside a real session.
    for kind in ["directory", "file", "dangling"] {
        let home = Home::new();
        let id = seed(&home, "live");
        let legacy = home.paths().root().join("old.json");
        fs::write(&legacy, b"legacy sentinel").unwrap();
        let target = match kind {
            "directory" => home.paths().session_dir(&id).unwrap(),
            "file" => legacy.clone(),
            _ => home.0.join("absent"),
        };
        let link = home.paths().root().join("alias.json");
        symlink(&target, &link).unwrap();
        // When enumerating, the symlink is not classified as an ordinary file.
        let error = catalog::list(&home.paths(), ReadProbe::default()).unwrap_err();
        // Then path safety still fails closed and leaves both link and target unchanged.
        assert_eq!(error.code, StoreErrorCode::UnsafePath);
        assert_eq!(fs::read_link(&link).unwrap(), target);
        assert_eq!(fs::read(&legacy).unwrap(), b"legacy sentinel");
    }
}

#[test]
fn unsafe_session_directory_permissions_are_still_refused() {
    // Given an actual event directory made non-private after creation.
    let home = Home::new();
    let id = seed(&home, "unsafe");
    let log = home.paths().events_log(&id).unwrap();
    let before = fs::read(&log).unwrap();
    fs::set_permissions(
        home.paths().session_dir(&id).unwrap(),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    // When the catalog tries to rebuild its missing derived row.
    let error = catalog::list(&home.paths(), ReadProbe::default()).unwrap_err();
    // Then normal writer permission checks are not bypassed by enumeration.
    assert_eq!(error.code, StoreErrorCode::UnsafePath);
    assert_eq!(fs::read(log).unwrap(), before);
}

#[test]
fn unsafe_directory_identity_is_not_ignored_as_a_preserved_file() {
    // Given an invalid identity that really is a directory.
    let home = Home::new();
    fs::DirBuilder::new()
        .mode(0o700)
        .create(home.paths().root().join("bad id"))
        .unwrap();
    // When the catalog enumerates it, identity validation still runs.
    let error = catalog::list(&home.paths(), ReadProbe::default()).unwrap_err();
    // Then malformed directories do not disappear through the legacy-file exception.
    assert_eq!(error.code, StoreErrorCode::UnsafeSessionId);
}

#[test]
fn warm_catalog_and_recent_refuse_non_private_session_directory() {
    // Given valid, already populated caches for a private real session directory.
    let home = Home::new();
    let paths = home.paths();
    let id = seed(&home, "warm");
    let directory = paths.session_dir(&id).unwrap();
    let log = paths.events_log(&id).unwrap();
    let original = fs::read(&log).unwrap();
    let cache = SessionCache::new(paths.clone(), ReadProbe::default());
    cache.recent(&id, None).unwrap();
    assert_eq!(catalog::list(&paths, ReadProbe::default()).unwrap().len(), 1);
    cache.recent(&id, None).unwrap();
    assert_eq!(catalog::list(&paths, ReadProbe::default()).unwrap().len(), 1);
    // When only directory permissions change, no log stamp or cached data changes.
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
    let rows = catalog::list(&paths, ReadProbe::default());
    let recent = cache.recent(&id, None);
    println!(
        "WARM list={:?} recent={:?}",
        rows.as_ref().map(Vec::len).map_err(|e| e.code),
        recent.as_ref().map(|r| r.tail.len()).map_err(|e| e.code)
    );
    // Then both public warm paths refuse without modifying or repairing originals.
    assert_eq!(fs::read(&log).unwrap(), original);
    assert_eq!(fs::metadata(&directory).unwrap().permissions().mode() & 0o777, 0o755);
    assert_eq!(rows.unwrap_err().code, StoreErrorCode::UnsafePath);
    assert_eq!(recent.unwrap_err().code, StoreErrorCode::UnsafePath);
}
