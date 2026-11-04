use super::helpers::*;
use horon_agent_session_store::legacy::*;
use horon_agent_session_store::*;
use rusqlite::Connection;
use std::fs;

fn empty_index_home() -> std::path::PathBuf {
    let home = super::support::temp_root("task7-completion");
    directory(&home);
    let connection = Connection::open(home.join("session-metadata-v1.sqlite3")).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE recent_sessions(session_id TEXT PRIMARY KEY, working_dir TEXT,
         generated_title TEXT, custom_title TEXT, todo_title TEXT, saved INTEGER,
         save_label TEXT, updated_at_ms INTEGER, last_active_at_ms INTEGER);",
        )
        .unwrap();
    drop(connection);
    println!("TASK7_COMPLETION_FIXTURE={}", home.display());
    home
}

#[test]
fn task7_session_completion_never_blesses_new_source_stamps_with_old_inventory() {
    for point in [
        MigrationPoint::RuntimeSourcesPublished,
        MigrationPoint::ReceiptDirectorySync,
    ] {
        let home = empty_index_home();
        let database = home.join("session-metadata-v1.sqlite3");
        let db = Connection::open(&database).unwrap();
        db.execute_batch(
            "INSERT INTO recent_sessions(session_id,generated_title,saved,updated_at_ms)
             VALUES('old','old snapshot',0,100);",
        )
        .unwrap();
        drop(db);
        let paths = paths(&home);
        let injected = database.clone();

        let failure = source(&home)
            .migrate_with_hook(
                &paths,
                Box::new(move |at| {
                    if at == point {
                        let writer = Connection::open(&injected).unwrap();
                        writer
                            .execute_batch(
                                "INSERT INTO recent_sessions(session_id,generated_title,saved,updated_at_ms)
                     VALUES('two','not migrated',0,200);",
                            )
                            .unwrap();
                    }
                    Ok(())
                }),
            )
            .unwrap_err();

        assert_eq!(failure.code, StoreErrorCode::SourceConflict);
        assert_eq!(
            require_index_migration(&paths).unwrap_err().code,
            StoreErrorCode::MigrationRequired
        );
        assert!(catalog::list(&paths, ReadProbe::default()).is_err());
        assert!(!paths.events_log(&SessionId::parse("two").unwrap()).unwrap().exists());
        let published = fs::read(paths.events_log(&SessionId::parse("old").unwrap()).unwrap()).unwrap();
        assert!(!published.is_empty(), "already durable original must remain");
        let changed_database = fs::read(&database).unwrap();
        assert_eq!(
            source(&home).migrate(&paths).unwrap_err().code,
            StoreErrorCode::SourceConflict
        );
        assert_eq!(
            fs::read(paths.events_log(&SessionId::parse("old").unwrap()).unwrap()).unwrap(),
            published
        );
        assert_eq!(fs::read(database).unwrap(), changed_database);
    }
}

#[test]
fn task7_home_completion_keeps_captured_snapshot_binding_at_late_publication() {
    for point in [
        MigrationPoint::IndexInventoryPublished,
        MigrationPoint::IndexHeaderPublish,
    ] {
        let home = empty_index_home();
        let paths = paths(&home);
        let database = home.join("session-metadata-v1.sqlite3");

        let failure = complete_index_migration_with_hook(
            &home,
            &paths,
            Box::new(move |at| {
                if at == point {
                    let writer = Connection::open(&database).unwrap();
                    writer
                        .execute_batch(
                            "INSERT INTO recent_sessions(session_id,saved,updated_at_ms) VALUES('late',0,200);",
                        )
                        .unwrap();
                }
                Ok(())
            }),
        )
        .unwrap_err();

        assert_eq!(failure.code, StoreErrorCode::SourceConflict);
        assert_eq!(
            require_index_migration(&paths).unwrap_err().code,
            StoreErrorCode::MigrationRequired
        );
        assert_eq!(
            complete_index_migration(&home, &paths).unwrap_err().code,
            StoreErrorCode::MigrationRequired
        );
        assert!(!paths.events_log(&SessionId::parse("late").unwrap()).unwrap().exists());
    }
}

#[test]
fn task7_empty_home_completion_is_durable_idempotent_and_does_not_invent_sessions() {
    use std::os::unix::fs::MetadataExt;
    let home = empty_index_home();
    let paths = paths(&home);
    let database = home.join("session-metadata-v1.sqlite3");
    let original = fs::read(&database).unwrap();
    assert_eq!(
        require_index_migration(&paths).unwrap_err().code,
        StoreErrorCode::MigrationRequired
    );

    let result = complete_index_migration(&home, &paths).unwrap();

    assert_eq!(result.session_count, 0);
    require_index_migration(&paths).unwrap();
    assert!(catalog::list(&paths, ReadProbe::default()).unwrap().is_empty());
    let marker = paths.root().join(".legacy-index-completed.json");
    let inode = fs::metadata(&marker).unwrap().ino();
    assert_eq!(complete_index_migration(&home, &paths).unwrap(), result);
    assert_eq!(fs::metadata(marker).unwrap().ino(), inode);
    let missing = LegacySource::new(&home, SessionId::parse("missing").unwrap()).unwrap();
    assert_eq!(
        missing.migrate(&paths).unwrap_err().code,
        StoreErrorCode::UnknownSession
    );
    assert!(
        !paths
            .events_log(&SessionId::parse("missing").unwrap())
            .unwrap()
            .exists()
    );
    assert_eq!(fs::read(database).unwrap(), original);
}

#[test]
fn task7_completion_rejects_nonempty_incomplete_and_changed_source_inventory() {
    let home = empty_index_home();
    let paths = paths(&home);
    complete_index_migration(&home, &paths).unwrap();
    let connection = Connection::open(home.join("session-metadata-v1.sqlite3")).unwrap();
    connection
        .execute_batch(
            "INSERT INTO recent_sessions(session_id,generated_title,saved,updated_at_ms)
         VALUES('old','not empty',0,10);",
        )
        .unwrap();
    drop(connection);
    let original = fs::read(home.join("session-metadata-v1.sqlite3")).unwrap();

    assert_eq!(
        require_index_migration(&paths).unwrap_err().code,
        StoreErrorCode::MigrationRequired
    );
    assert_eq!(
        complete_index_migration(&home, &paths).unwrap_err().code,
        StoreErrorCode::MigrationRequired
    );
    source(&home).migrate(&paths).unwrap();

    require_index_migration(&paths).unwrap();
    assert_eq!(complete_index_migration(&home, &paths).unwrap().session_count, 1);
    assert_eq!(fs::read(home.join("session-metadata-v1.sqlite3")).unwrap(), original);
}

#[test]
fn task7_home_completion_interruption_never_certifies_partial_publication() {
    for point in [
        MigrationPoint::IndexInventoryPublished,
        MigrationPoint::IndexHeaderPublish,
    ] {
        let home = empty_index_home();
        let paths = paths(&home);
        let original = fs::read(home.join("session-metadata-v1.sqlite3")).unwrap();

        let error = complete_index_migration_with_hook(
            &home,
            &paths,
            Box::new(move |at| {
                if at == point {
                    Err(std::io::Error::other("injected completion interruption"))
                } else {
                    Ok(())
                }
            }),
        )
        .unwrap_err();

        assert_eq!(error.code, StoreErrorCode::Io);
        assert_eq!(
            require_index_migration(&paths).unwrap_err().code,
            StoreErrorCode::MigrationRequired
        );
        assert_eq!(complete_index_migration(&home, &paths).unwrap().session_count, 0);
        require_index_migration(&paths).unwrap();
        assert_eq!(fs::read(home.join("session-metadata-v1.sqlite3")).unwrap(), original);
    }
}

#[test]
fn task7_completion_respects_live_legacy_pid_guard() {
    let home = empty_index_home();
    let paths = paths(&home);
    put(&home, "active_pids/live", std::process::id().to_string().as_bytes());
    let before = fs::read(home.join("session-metadata-v1.sqlite3")).unwrap();

    let error = complete_index_migration(&home, &paths).unwrap_err();

    assert_eq!(error.code, StoreErrorCode::Busy);
    assert_eq!(fs::read(home.join("session-metadata-v1.sqlite3")).unwrap(), before);
    assert!(!paths.root().join(".legacy-index-completed.json").exists());
}

fn rewrite_completion_header(path: &std::path::Path, mut header: serde_json::Value) {
    let value: IndexCompletion = serde_json::from_value(header["value"].clone()).unwrap();
    header["digest"] = serde_json::json!(sha256_hex(&serde_json::to_vec(&value).unwrap()));
    fs::write(path, serde_json::to_vec(&header).unwrap()).unwrap();
}

#[test]
fn task7_completion_rejects_corrupt_duplicate_trailing_unsafe_and_unbounded_records() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    for case in [
        "truncated",
        "trailing",
        "duplicate",
        "count",
        "invalid_id",
        "hash",
        "unknown",
        "header_hash",
        "oversize",
        "symlink",
        "permissions",
    ] {
        let home = empty_index_home();
        let paths = paths(&home);
        let connection = Connection::open(home.join("session-metadata-v1.sqlite3")).unwrap();
        connection
            .execute_batch("INSERT INTO recent_sessions(session_id,saved,updated_at_ms) VALUES('old',0,10);")
            .unwrap();
        drop(connection);
        source(&home).migrate(&paths).unwrap();
        let header_path = paths.root().join(".legacy-index-completed.json");
        let mut header: serde_json::Value = serde_json::from_slice(&fs::read(&header_path).unwrap()).unwrap();
        let inventory = paths.root().join(format!(
            ".legacy-index-inventory-{}.jsonl",
            header["value"]["inventory_digest"].as_str().unwrap()
        ));
        let old = fs::read(&inventory).unwrap();
        let mut row: serde_json::Value = serde_json::from_slice(&old).unwrap();
        let mut bytes = old.clone();
        match case {
            "truncated" => {
                bytes.pop().unwrap();
            }
            "trailing" => bytes.extend_from_slice(b"x"),
            "duplicate" => {
                bytes.extend_from_slice(&old);
                header["value"]["session_count"] = serde_json::json!(2);
            }
            "count" => header["value"]["session_count"] = serde_json::json!(2),
            "invalid_id" => row["session_id"] = serde_json::json!("../escape"),
            "hash" => row["completion_hash"] = serde_json::json!("z".repeat(64)),
            "unknown" => row["unrecognized"] = serde_json::json!(true),
            "header_hash" => header["value"]["inventory_digest"] = serde_json::json!("../escape"),
            "oversize" | "symlink" | "permissions" => {}
            _ => unreachable!(),
        }
        if matches!(case, "invalid_id" | "hash" | "unknown") {
            bytes = serde_json::to_vec(&row).unwrap();
            bytes.push(b'\n');
        }
        if !matches!(case, "header_hash" | "oversize" | "symlink" | "permissions") {
            let hash = sha256_hex(&bytes);
            let file = paths.root().join(format!(".legacy-index-inventory-{hash}.jsonl"));
            fs::write(&file, &bytes).unwrap();
            fs::set_permissions(file, fs::Permissions::from_mode(0o600)).unwrap();
            header["value"]["inventory_digest"] = serde_json::json!(hash);
            header["value"]["inventory_bytes"] = serde_json::json!(bytes.len());
        }
        rewrite_completion_header(&header_path, header);
        match case {
            "oversize" => {
                use std::io::Write;
                fs::OpenOptions::new()
                    .append(true)
                    .open(&header_path)
                    .unwrap()
                    .write_all(&vec![b' '; 4096])
                    .unwrap();
            }
            "symlink" => {
                let original = paths.root().join(".saved-completion");
                fs::rename(&header_path, &original).unwrap();
                symlink(original, &header_path).unwrap();
            }
            "permissions" => fs::set_permissions(&header_path, fs::Permissions::from_mode(0o644)).unwrap(),
            _ => {}
        }

        assert!(require_index_migration(&paths).is_err(), "{case}");
    }
}

#[test]
fn task7_warm_home_guard_reads_bounded_inventory_not_large_event_manifests() {
    let home = empty_index_home();
    let paths = paths(&home);
    let mut raw = snapshot();
    raw["messages"] = serde_json::json!(
        (0..800)
            .map(|index| msg(&format!("m{index}"), "user"))
            .collect::<Vec<_>>()
    );
    put(&home, "sessions/old.json", &serde_json::to_vec(&raw).unwrap());
    let connection = Connection::open(home.join("session-metadata-v1.sqlite3")).unwrap();
    connection.execute_batch("INSERT INTO recent_sessions(session_id,generated_title,saved,updated_at_ms) VALUES('old','large history',0,10);").unwrap();
    drop(connection);
    source(&home).migrate(&paths).unwrap();
    catalog::list(&paths, ReadProbe::default()).unwrap();
    let manifest = paths
        .session_dir(&SessionId::parse("old").unwrap())
        .unwrap()
        .join("legacy-source-manifest.json");
    let manifest_bytes = fs::metadata(manifest).unwrap().len();
    assert!(manifest_bytes > 50_000);
    let probe = ReadProbe::default();

    catalog::list(&paths, probe.clone()).unwrap();
    let sources = runtime_sources(&paths, &SessionId::parse("old").unwrap(), probe.clone())
        .unwrap()
        .unwrap();
    assert!(sources.source_files.contains_key("sessions/old.json"));

    let reads = probe.receipts().unwrap();
    assert!(reads.iter().all(|read| read.path.file_name().is_none_or(|name| {
        name != "legacy-source-manifest.json" && name != "events.jsonl" && !name.to_string_lossy().contains(".sqlite3")
    })));
    let guard_bytes: usize = reads
        .iter()
        .filter(|read| {
            read.path.file_name().is_some_and(|name| {
                name.to_string_lossy().starts_with(".legacy-index-") || name == "legacy-completed.sha256"
            })
        })
        .map(|read| read.bytes)
        .sum();
    assert!(guard_bytes > 0 && guard_bytes <= 4096 + 1024 + 64);
    let source_bytes: usize = reads
        .iter()
        .filter(|read| {
            read.path
                .file_name()
                .is_some_and(|name| name == "legacy-runtime-sources.json")
        })
        .map(|read| read.bytes)
        .sum();
    assert!(source_bytes > 0 && u64::try_from(source_bytes).unwrap() < manifest_bytes / 10);
    println!(
        "WARM_GUARD manifest_bytes={manifest_bytes} guard_bytes={guard_bytes} runtime_source_bytes={source_bytes} original_bytes=0 rows=1"
    );
}

#[test]
fn task7_runtime_source_lookup_never_repairs_an_old_completed_receipt() {
    let home = fixture();
    let paths = paths(&home);
    let id = SessionId::parse("old").unwrap();
    source(&home).migrate(&paths).unwrap();
    let directory = paths.session_dir(&id).unwrap();
    let manifest = fs::read(directory.join("legacy-source-manifest.json")).unwrap();
    let completed = fs::read(directory.join("legacy-completed.sha256")).unwrap();
    let original = fs::read(paths.events_log(&id).unwrap()).unwrap();
    fs::remove_file(directory.join("legacy-runtime-sources.json")).unwrap();

    assert_eq!(
        runtime_sources(&paths, &id, ReadProbe::default()).unwrap_err().code,
        StoreErrorCode::MigrationRequired
    );
    assert_eq!(
        source(&home).migrate_for_runtime(&paths).unwrap_err().code,
        StoreErrorCode::MigrationRequired
    );
    assert!(!directory.join("legacy-runtime-sources.json").exists());
    assert_eq!(fs::read(paths.events_log(&id).unwrap()).unwrap(), original);
    source(&home).migrate(&paths).unwrap();

    assert!(runtime_sources(&paths, &id, ReadProbe::default()).unwrap().is_some());
    assert_eq!(
        fs::read(directory.join("legacy-source-manifest.json")).unwrap(),
        manifest
    );
    assert_eq!(fs::read(directory.join("legacy-completed.sha256")).unwrap(), completed);
    let replica = SessionPaths::new(home.join("receipt-free-replica/sessions"));
    replication::import_events(&replica, &export(&paths)).unwrap();
    assert!(runtime_sources(&replica, &id, ReadProbe::default()).unwrap().is_none());
}

#[test]
fn task7_new_file_only_transaction_resumes_without_relabeling_old_completion() {
    for point in [MigrationPoint::ParentSync, MigrationPoint::RuntimeSourcesPublished] {
        let home = fixture();
        let paths = paths(&home);
        let id = SessionId::parse("old").unwrap();
        let raw = fs::read(home.join("sessions/old.json")).unwrap();
        let failure = source(&home)
            .migrate_for_runtime_with_hook(
                &paths,
                Box::new(move |at| {
                    if at == point {
                        Err(std::io::Error::other("initial file publication interrupted"))
                    } else {
                        Ok(())
                    }
                }),
            )
            .unwrap_err();
        assert_eq!(failure.code, StoreErrorCode::Io);
        let prefix = fs::read(paths.events_log(&id).unwrap()).unwrap();
        let directory = paths.session_dir(&id).unwrap();
        assert!(!directory.join("legacy-completed.sha256").exists());
        assert_eq!(
            runtime_sources(&paths, &id, ReadProbe::default()).unwrap_err().code,
            StoreErrorCode::MigrationIncomplete
        );

        source(&home).migrate_for_runtime(&paths).unwrap();

        assert!(runtime_sources(&paths, &id, ReadProbe::default()).unwrap().is_some());
        assert!(!directory.join(".legacy-first-file.json").exists());
        assert_eq!(fs::read(paths.events_log(&id).unwrap()).unwrap(), prefix);
        assert_eq!(fs::read(home.join("sessions/old.json")).unwrap(), raw);
        source(&home).migrate_for_runtime(&paths).unwrap();
        assert_eq!(fs::read(paths.events_log(&id).unwrap()).unwrap(), prefix);
    }
}

fn old_receipt_fixture() -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let home = super::support::temp_root("task7-upgrade");
    directory(&home);
    let mut original = snapshot();
    original["working_dir"] = serde_json::json!("/preserved/project");
    original["saved"] = serde_json::json!(true);
    original["save_label"] = serde_json::json!("preserved label");
    put(&home, "sessions/old.json", &serde_json::to_vec(&original).unwrap());
    let db = Connection::open(home.join("session-metadata-v1.sqlite3")).unwrap();
    db.execute_batch(
        "CREATE TABLE recent_sessions(session_id TEXT PRIMARY KEY, working_dir TEXT,
         generated_title TEXT, custom_title TEXT, todo_title TEXT, saved INTEGER,
         save_label TEXT, updated_at_ms INTEGER, last_active_at_ms INTEGER);
         INSERT INTO recent_sessions VALUES('old',NULL,'generated',NULL,NULL,1,NULL,1000,NULL);",
    )
    .unwrap();
    drop(db);
    // Construct a historical artifact in a new destination, never truncate a log.
    let preview = SessionPaths::new(home.join("preview-store"));
    let report = source(&home).migrate(&preview).unwrap();
    let id = SessionId::parse("old").unwrap();
    let events = full_scan(&preview, &id).unwrap().events;
    assert_eq!(
        events.last().unwrap().kind.known(),
        Some(KnownEventKind::MetadataChanged)
    );
    let paths = paths(&home);
    let mut writer = BlockingWriter::open(paths.clone(), id.clone()).unwrap();
    for event in &events[..events.len() - 1] {
        writer.append(&SealedEvent::Inline(event.clone())).unwrap();
    }
    drop(writer);
    let target = paths.session_dir(&id).unwrap();
    for name in report.receipt.source_files.keys() {
        put(
            &target,
            &format!("legacy-originals/{name}"),
            &fs::read(home.join(name)).unwrap(),
        );
        fs::set_permissions(
            target.join("legacy-originals").join(name),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let mut old = serde_json::to_value(report.receipt).unwrap();
    old["events"].as_array_mut().unwrap().pop();
    for key in ["display_recovery_version", "index_session_ids", "index_stamps"] {
        old.as_object_mut().unwrap().remove(key);
    }
    let bytes = canonical_json(&old).unwrap().into_bytes();
    put(&target, "legacy-source-manifest.json", &bytes);
    put(&target, "legacy-completed.sha256", sha256_hex(&bytes).as_bytes());
    for name in ["legacy-source-manifest.json", "legacy-completed.sha256"] {
        fs::set_permissions(target.join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    println!("TASK7_UPGRADE_FIXTURE={}", home.display());
    home
}

fn export(paths: &SessionPaths) -> replication::SessionEventPage {
    replication::export_events(
        paths,
        &replication::ExportRequest {
            protocol_version: 1,
            session_id: SessionId::parse("old").unwrap(),
            heads: Vec::new(),
            known_ids: Vec::new(),
            cursor: None,
        },
    )
    .unwrap()
}

#[test]
fn task7_old_receipt_upgrade_preserves_prefix_and_replica_display_without_receipts() {
    let home = old_receipt_fixture();
    let paths = paths(&home);
    let id = SessionId::parse("old").unwrap();
    let old_prefix = fs::read(paths.events_log(&id).unwrap()).unwrap();
    let snapshot = fs::read(home.join("sessions/old.json")).unwrap();
    let database = fs::read(home.join("session-metadata-v1.sqlite3")).unwrap();
    let replica = SessionPaths::new(home.join("replica-home/sessions"));
    replication::import_events(&replica, &export(&paths)).unwrap();

    let upgraded = source(&home).migrate(&paths).unwrap();

    assert_eq!(upgraded.outcome, MigrationOutcome::Published);
    let upgraded_bytes = fs::read(paths.events_log(&id).unwrap()).unwrap();
    assert!(upgraded_bytes.starts_with(&old_prefix));
    assert!(upgraded_bytes.len() > old_prefix.len());
    let again = source(&home).migrate(&paths).unwrap();
    assert_eq!(again.outcome, MigrationOutcome::AlreadyPublished);
    assert_eq!(fs::read(paths.events_log(&id).unwrap()).unwrap(), upgraded_bytes);
    require_index_migration(&paths).unwrap();
    replication::import_events(&replica, &export(&paths)).unwrap();
    let replica_view = EventGraph::open(&replica, &id).unwrap().replay(None).unwrap();
    assert_eq!(replica_view.metadata().save_label.as_deref(), Some("preserved label"));
    assert_eq!(
        replica_view.metadata().working_dir.as_deref(),
        Some("/preserved/project")
    );
    assert!(
        !replica
            .session_dir(&id)
            .unwrap()
            .join("legacy-source-manifest.json")
            .exists()
    );
    assert!(!replica.root().join("old.json").exists());
    assert_eq!(fs::read(home.join("sessions/old.json")).unwrap(), snapshot);
    assert_eq!(fs::read(home.join("session-metadata-v1.sqlite3")).unwrap(), database);

    // A later explicit clear at the identical timestamp is authoritative.
    let mut cleared = replica_view.metadata().clone();
    let original_time = cleared.updated_at;
    cleared.save_label = None;
    cleared.working_dir = None;
    let event = EventDraft::new(
        id.clone(),
        EventId::new_v4(),
        Some(replica_view.head().clone()),
        OriginId::new_v4(),
        original_time,
        EventKind::new(KnownEventKind::MetadataChanged),
        serde_json::json!(MetadataChangedPayload {
            metadata: cleared,
            resume_key: None,
        }),
    )
    .seal()
    .unwrap();
    let mut writer = BlockingWriter::open(replica.clone(), id.clone()).unwrap();
    writer.append(&event).unwrap();
    drop(writer);
    let rows = catalog::list(&replica, ReadProbe::default()).unwrap();
    assert_eq!(rows[0].metadata_updated_at, original_time);
    assert_eq!(rows[0].save_label, None);
    assert_eq!(rows[0].working_dir, None);
    replication::import_events(&paths, &export(&replica)).unwrap();
    let rows = catalog::list(&paths, ReadProbe::default()).unwrap();
    assert_eq!(rows[0].save_label, None);
    assert_eq!(rows[0].working_dir, None);
}

#[test]
fn task7_old_receipt_with_local_clear_refuses_ambiguous_recovery_without_writes() {
    let home = old_receipt_fixture();
    let paths = paths(&home);
    let id = SessionId::parse("old").unwrap();
    let view = EventGraph::open(&paths, &id).unwrap().replay(None).unwrap();
    let mut clear = view.metadata().clone();
    clear.save_label = None;
    clear.working_dir = None;
    let event = EventDraft::new(
        id.clone(),
        EventId::new_v4(),
        Some(view.head().clone()),
        OriginId::new_v4(),
        clear.updated_at,
        EventKind::new(KnownEventKind::MetadataChanged),
        serde_json::json!(MetadataChangedPayload {
            metadata: clear,
            resume_key: None,
        }),
    )
    .seal()
    .unwrap();
    let mut writer = BlockingWriter::open(paths.clone(), id.clone()).unwrap();
    writer.append(&event).unwrap();
    drop(writer);
    let before = fs::read(paths.events_log(&id).unwrap()).unwrap();
    let receipt = fs::read(paths.session_dir(&id).unwrap().join("legacy-completed.sha256")).unwrap();

    let error = source(&home).migrate(&paths).unwrap_err();

    assert_eq!(error.code, StoreErrorCode::SourceConflict);
    assert_eq!(fs::read(paths.events_log(&id).unwrap()).unwrap(), before);
    assert_eq!(
        fs::read(paths.session_dir(&id).unwrap().join("legacy-completed.sha256")).unwrap(),
        receipt
    );
    let current = EventGraph::open(&paths, &id).unwrap().replay(None).unwrap();
    assert_eq!(current.metadata().save_label, None);
    assert_eq!(current.metadata().working_dir, None);
}

#[test]
fn task7_interrupted_old_receipt_upgrade_resumes_without_duplicate_recovery() {
    for point in [
        MigrationPoint::UpgradeAppended,
        MigrationPoint::UpgradeManifestPublished,
    ] {
        let home = old_receipt_fixture();
        let paths = paths(&home);
        let id = SessionId::parse("old").unwrap();
        let prefix = fs::read(paths.events_log(&id).unwrap()).unwrap();
        let error = source(&home)
            .migrate_with_hook(
                &paths,
                Box::new(move |at| {
                    if at == point {
                        Err(std::io::Error::other("injected upgrade interruption"))
                    } else {
                        Ok(())
                    }
                }),
            )
            .unwrap_err();
        assert_eq!(error.code, StoreErrorCode::Io);
        assert!(fs::read(paths.events_log(&id).unwrap()).unwrap().starts_with(&prefix));
        assert_eq!(
            require_index_migration(&paths).unwrap_err().code,
            StoreErrorCode::MigrationRequired
        );

        source(&home).migrate(&paths).unwrap();

        let after = fs::read(paths.events_log(&id).unwrap()).unwrap();
        require_index_migration(&paths).unwrap();
        source(&home).migrate(&paths).unwrap();
        assert_eq!(fs::read(paths.events_log(&id).unwrap()).unwrap(), after);
        let view = EventGraph::open(&paths, &id).unwrap().replay(None).unwrap();
        assert_eq!(view.metadata().save_label.as_deref(), Some("preserved label"));
        assert_eq!(view.metadata().working_dir.as_deref(), Some("/preserved/project"));
    }
}

#[test]
fn task7_old_receipt_multiple_heads_are_preserved_without_a_timestamp_winner() {
    let home = old_receipt_fixture();
    let paths = paths(&home);
    let id = SessionId::parse("old").unwrap();
    let view = EventGraph::open(&paths, &id).unwrap().replay(None).unwrap();
    let mut writer = BlockingWriter::open(paths.clone(), id.clone()).unwrap();
    for label in [None, Some("another branch")] {
        let mut metadata = view.metadata().clone();
        metadata.save_label = label.map(str::to_owned);
        let event = EventDraft::new(
            id.clone(),
            EventId::new_v4(),
            Some(view.head().clone()),
            OriginId::new_v4(),
            metadata.updated_at,
            EventKind::new(KnownEventKind::MetadataChanged),
            serde_json::json!(MetadataChangedPayload {
                metadata,
                resume_key: None,
            }),
        )
        .seal()
        .unwrap();
        writer.append(&event).unwrap();
    }
    drop(writer);
    let before = fs::read(paths.events_log(&id).unwrap()).unwrap();
    let heads = EventGraph::open(&paths, &id).unwrap().heads().to_vec();
    assert_eq!(heads.len(), 2);

    let error = source(&home).migrate(&paths).unwrap_err();

    assert_eq!(error.code, StoreErrorCode::SourceConflict);
    assert_eq!(fs::read(paths.events_log(&id).unwrap()).unwrap(), before);
    assert_eq!(EventGraph::open(&paths, &id).unwrap().heads(), heads);
}

#[test]
fn wal_only_metadata_is_read_from_copy_without_touching_original_database() {
    // Given a committed row that lives only in WAL, with the original connection held.
    let home = fixture();
    let db = Connection::open(home.join("session-metadata-v1.sqlite3")).unwrap();
    db.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
        CREATE TABLE recent_sessions(session_id TEXT PRIMARY KEY, working_dir TEXT,
        generated_title TEXT, custom_title TEXT, todo_title TEXT, saved INTEGER,
        save_label TEXT, updated_at_ms INTEGER, last_active_at_ms INTEGER);
        PRAGMA wal_checkpoint(TRUNCATE);
        INSERT INTO recent_sessions VALUES('old','/legacy/project','generated',NULL,'todo',1,
        'saved title',1600000000000,1600000001234);",
    )
    .unwrap();
    assert!(
        fs::metadata(home.join("session-metadata-v1.sqlite3-wal"))
            .unwrap()
            .len()
            > 0
    );
    let before = hashes(&home);
    // When public migration reads the guarded copy in its read-only transaction.
    let report = source(&home).migrate(&paths(&home)).unwrap();
    // Then WAL-only values reach the projection and all three original files are unchanged.
    let view = view(&home);
    assert_eq!(view.metadata().title.as_deref(), Some("saved title"));
    assert!(view.metadata().saved);
    assert_eq!(view.metadata().save_label.as_deref(), Some("saved title"));
    assert_eq!(view.metadata().working_dir.as_deref(), Some("/legacy/project"));
    assert_eq!(view.metadata().legacy_extensions["last_active_at_ms"], 1600000001234i64);
    assert!(report.receipt.index_imported);
    assert_eq!(
        read_receipt(&paths(&home), &SessionId::parse("old").unwrap()).unwrap(),
        report.receipt
    );
    preserved(&home, &before);
    // Keep a byte-identical evidence copy of the live originals before SQLite's owner closes.
    for name in [
        "session-metadata-v1.sqlite3",
        "session-metadata-v1.sqlite3-wal",
        "session-metadata-v1.sqlite3-shm",
    ] {
        put(
            &home,
            &format!("before-owner-close/{name}"),
            &fs::read(home.join(name)).unwrap(),
        );
    }
}

#[test]
fn custom_title_precedes_saved_todo_generated_and_old_schema_is_supported() {
    // Given the historical schema before saved/save_label columns existed.
    let home = fixture();
    let db = Connection::open(home.join("session-metadata-v1.sqlite3")).unwrap();
    db.execute_batch(
        "CREATE TABLE recent_sessions(session_id TEXT PRIMARY KEY, working_dir TEXT,
        generated_title TEXT, custom_title TEXT, todo_title TEXT,
        updated_at_ms INTEGER, last_active_at_ms INTEGER);
        INSERT INTO recent_sessions VALUES('old',NULL,'generated',' custom ','todo',1234,NULL);",
    )
    .unwrap();
    drop(db);
    let before = hashes(&home);
    // When migrating without any schema ALTER.
    source(&home).migrate(&paths(&home)).unwrap();
    // Then historical display precedence and absence of saved metadata are honored.
    let view = view(&home);
    assert_eq!(view.metadata().title.as_deref(), Some("custom"));
    assert!(!view.metadata().saved);
    preserved(&home, &before);
}

#[test]
fn task7_db_only_rows_require_complete_offline_inventory_without_invented_messages() {
    let home = super::support::temp_root("task7-db-only");
    directory(&home);
    let db_path = home.join("session-metadata-v1.sqlite3");
    let db = Connection::open(&db_path).unwrap();
    db.execute_batch(
        "CREATE TABLE recent_sessions(session_id TEXT PRIMARY KEY, working_dir TEXT,
         generated_title TEXT, custom_title TEXT, todo_title TEXT, saved INTEGER,
         save_label TEXT, updated_at_ms INTEGER, last_active_at_ms INTEGER);
         INSERT INTO recent_sessions VALUES('one','/old/project','generated',NULL,'todo',1,
         'saved label',1000,100);
         INSERT INTO recent_sessions VALUES('two',NULL,'second',NULL,NULL,0,NULL,900,NULL);",
    )
    .unwrap();
    drop(db);
    let original = fs::read(&db_path).unwrap();
    let paths = paths(&home);
    assert_eq!(
        require_index_migration(&paths).unwrap_err().code,
        StoreErrorCode::MigrationRequired
    );
    let one = SessionId::parse("one").unwrap();
    let two = SessionId::parse("two").unwrap();
    let first = LegacySource::new(&home, one.clone()).unwrap();
    assert_eq!(
        first.migrate_for_runtime(&paths).unwrap_err().code,
        StoreErrorCode::MigrationRequired
    );
    assert!(!paths.root().exists());

    first.migrate(&paths).unwrap();
    assert_eq!(
        require_index_migration(&paths).unwrap_err().code,
        StoreErrorCode::MigrationRequired
    );
    LegacySource::new(&home, two.clone()).unwrap().migrate(&paths).unwrap();
    require_index_migration(&paths).unwrap();
    first.migrate_for_runtime(&paths).unwrap();
    for id in [&one, &two] {
        let view = EventGraph::open(&paths, id).unwrap().replay(None).unwrap();
        assert!(view.messages().is_empty(), "DB rows never become invented messages");
    }
    let listed = catalog::list(&paths, ReadProbe::default()).unwrap();
    let row = listed.iter().find(|row| row.session_id == one).unwrap();
    assert_eq!(row.metadata_updated_at.timestamp_millis(), 1000);
    assert_eq!(row.last_active_at_ms, Some(100));
    assert_eq!(row.generated_title.as_deref(), Some("generated"));
    assert_eq!(row.todo_title.as_deref(), Some("todo"));
    assert_eq!(row.save_label.as_deref(), Some("saved label"));
    assert_eq!(row.working_dir.as_deref(), Some("/old/project"));
    assert_eq!(fs::read(db_path).unwrap(), original);
    println!("TASK7_DB_ONLY_FIXTURE={}", home.display());
}

#[test]
fn task7_unreadable_migrated_database_is_not_opened_by_runtime_or_warm_catalog() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let home = fixture();
    let database = home.join("session-metadata-v1.sqlite3");
    let db = Connection::open(&database).unwrap();
    db.execute_batch(
        "CREATE TABLE recent_sessions(session_id TEXT PRIMARY KEY, generated_title TEXT,
         custom_title TEXT, working_dir TEXT, todo_title TEXT, saved INTEGER,
         save_label TEXT, updated_at_ms INTEGER, last_active_at_ms INTEGER);
         INSERT INTO recent_sessions VALUES('old','generated',NULL,NULL,'todo',0,NULL,1000,100);",
    )
    .unwrap();
    drop(db);
    let original = fs::read(&database).unwrap();
    let original_mode = fs::metadata(&database).unwrap().permissions();
    let paths = paths(&home);
    source(&home).migrate(&paths).unwrap();
    catalog::list(&paths, ReadProbe::default()).unwrap();
    fs::set_permissions(&database, fs::Permissions::from_mode(0o000)).unwrap();
    let before = fs::metadata(&database).unwrap();

    require_index_migration(&paths).unwrap();
    source(&home).migrate_for_runtime(&paths).unwrap();
    let probe = ReadProbe::default();
    let rows = catalog::list(&paths, probe.clone()).unwrap();

    assert_eq!(rows.len(), 1);
    assert!(probe.receipts().unwrap().iter().all(|read| {
        read.path
            .file_name()
            .is_none_or(|name| name != "events.jsonl" && !name.to_string_lossy().contains(".sqlite3"))
    }));
    let after = fs::metadata(&database).unwrap();
    assert_eq!(
        (
            before.ino(),
            before.len(),
            before.mtime(),
            before.ctime(),
            before.mode()
        ),
        (after.ino(), after.len(), after.mtime(), after.ctime(), after.mode())
    );
    fs::set_permissions(&database, original_mode).unwrap();
    assert_eq!(fs::read(&database).unwrap(), original);
    assert!(!home.join("session-metadata-v1.sqlite3-wal").exists());
    assert!(!home.join("session-metadata-v1.sqlite3-shm").exists());
    println!("TASK7_UNREADABLE_FIXTURE={}", home.display());
}

fn foreign_snapshot() -> Vec<u8> {
    let mut raw = snapshot();
    raw["id"] = serde_json::json!("different");
    serde_json::to_vec(&raw).unwrap()
}

#[test]
fn task7_local_legacy_source_without_receipt_fails_closed_without_masking_foreign_shadow() {
    let home = fixture();
    let store_paths = paths(&home);
    let id = SessionId::parse("old").unwrap();

    // A legacy-only home has no published receipt yet, and its explicit migration
    // still restores the snapshot metadata without touching the original bytes.
    assert!(
        runtime_sources(&store_paths, &id, ReadProbe::default())
            .unwrap()
            .is_none()
    );
    let legacy_bytes = fs::read(home.join("sessions/old.json")).unwrap();
    source(&home).migrate(&store_paths).unwrap();
    let view = EventGraph::open(&store_paths, &id).unwrap().replay(None).unwrap();
    assert_eq!(view.metadata().title.as_deref(), Some("snapshot"));
    assert_eq!(fs::read(home.join("sessions/old.json")).unwrap(), legacy_bytes);

    // A replica that truly has no local legacy source stays receipt-free and valid.
    let replica = SessionPaths::new(home.join("receipt-free-replica/sessions"));
    replication::import_events(&replica, &export(&store_paths)).unwrap();
    assert!(runtime_sources(&replica, &id, ReadProbe::default()).unwrap().is_none());

    // Canonical events beside a same-ID local legacy snapshot are not source-free:
    // admitting them would skip the only check that binds the log to its originals.
    let collision = fixture();
    let collision_paths = paths(&collision);
    replication::import_events(&collision_paths, &export(&store_paths)).unwrap();
    let local = fs::read(collision.join("sessions/old.json")).unwrap();
    let log = fs::read(collision_paths.events_log(&id).unwrap()).unwrap();
    assert_eq!(
        runtime_sources(&collision_paths, &id, ReadProbe::default())
            .unwrap_err()
            .code,
        StoreErrorCode::MigrationRequired
    );
    assert_eq!(
        source(&collision)
            .migrate_for_runtime(&collision_paths)
            .unwrap_err()
            .code,
        StoreErrorCode::MigrationRequired
    );
    assert!(
        !collision_paths
            .session_dir(&id)
            .unwrap()
            .join("legacy-runtime-sources.json")
            .exists()
    );
    assert_eq!(fs::read(collision.join("sessions/old.json")).unwrap(), local);
    assert_eq!(fs::read(collision_paths.events_log(&id).unwrap()).unwrap(), log);

    // An unrelated body (another id, or something that is not a snapshot) can never
    // be migrated for this id, so it must not mask the valid canonical events.
    for shadow in [foreign_snapshot(), b"not JSON".to_vec()] {
        let shadow_home = fixture();
        put(&shadow_home, "sessions/old.json", &shadow);
        let shadow_paths = paths(&shadow_home);
        replication::import_events(&shadow_paths, &export(&store_paths)).unwrap();
        let before = fs::read(shadow_home.join("sessions/old.json")).unwrap();
        assert!(
            runtime_sources(&shadow_paths, &id, ReadProbe::default())
                .unwrap()
                .is_none()
        );
        let loaded = EventGraph::open(&shadow_paths, &id).unwrap().replay(None).unwrap();
        assert!(!loaded.messages().is_empty());
        assert_eq!(fs::read(shadow_home.join("sessions/old.json")).unwrap(), before);
    }
}

#[test]
fn task7_db_only_absent_row_is_not_silently_migrated() {
    let home = super::support::temp_root("task7-no-row");
    directory(&home);
    let db = Connection::open(home.join("session-metadata-v1.sqlite3")).unwrap();
    db.execute_batch("CREATE TABLE recent_sessions(session_id TEXT PRIMARY KEY);")
        .unwrap();
    drop(db);
    let id = SessionId::parse("absent").unwrap();
    let paths = paths(&home);

    let error = LegacySource::new(&home, id.clone())
        .unwrap()
        .migrate(&paths)
        .unwrap_err();

    assert_eq!(error.code, StoreErrorCode::UnknownSession);
    assert!(!paths.events_log(&id).unwrap().exists());
    assert_eq!(
        require_index_migration(&paths).unwrap_err().code,
        StoreErrorCode::MigrationRequired
    );
    println!("TASK7_ABSENT_ROW_FIXTURE={}", home.display());
}
