use super::*;

#[cfg(unix)]
#[test]
fn session_replication_local_publication_is_private_and_catalog_readable() {
    use std::os::unix::fs::PermissionsExt;
    // Given actual Session publication, not manually chmodded fixture files.
    let _home = Home::new();
    let mut session = seed("replica_permissions");
    session.add_message(Role::User, text("second publication"));
    session.save().unwrap();
    // When the same data is read by the strict common catalog.
    let listed = crate::recent_session_index::recent(10).unwrap();
    // Then every local state file is owner-only and the normal listing works.
    let directory = session.event_log_path().unwrap().parent().unwrap().to_owned();
    // A plain commit publishes the local runtime, the explicit-selection flag
    // and the derived caches; every one of them stays owner-only.
    for name in ["local-runtime.json", "local-head-explicit.json", "facade-cache.json"] {
        assert_eq!(
            std::fs::metadata(directory.join(name))
                .unwrap_or_else(|error| panic!("{name} is published: {error}"))
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "{name} must be owner-only"
        );
    }
    // An implicit commit records no local-head.json: every reader ignores the
    // file without the explicit flag, so recording it would cost a flush per
    // commit for a value nothing reads. An explicit selection still writes it,
    // and then it is owner-only like the rest.
    let head = directory.join("local-head.json");
    if let Ok(metadata) = std::fs::metadata(&head) {
        assert_eq!(
            metadata.permissions().mode() & 0o777,
            0o600,
            "local-head.json must be owner-only"
        );
    }
    assert!(!head.exists(), "an implicit commit must not write local-head.json");
}

#[tokio::test]
async fn session_replication_remote_pid_cwd_epoch_and_head_never_activate_runtime() {
    // Given a valid remote root with a current-PID spoof in inert provenance.
    let home = Home::new();
    let id = store::SessionId::parse("replica_foreign").unwrap();
    let metadata = store::SessionMetadata {
        working_dir: Some(home.directory.path().display().to_string()),
        migration_epoch: u64::MAX,
        legacy_extensions: std::collections::BTreeMap::from([
            ("status".into(), serde_json::json!("active")),
            ("last_pid".into(), serde_json::json!(std::process::id())),
        ]),
        ..Default::default()
    };
    let event = store::EventDraft::new(
        id.clone(),
        store::EventId::new_v4(),
        None,
        store::OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        store::EventKind::new(store::KnownEventKind::SessionCreated),
        serde_json::json!({"metadata": metadata, "branch_root": false}),
    )
    .seal()
    .unwrap();
    let wire = SessionEventPage {
        protocol_version: REPLICATION_PROTOCOL_VERSION,
        session_id: id.clone(),
        heads: vec![store::EventId::new_v4()],
        events: vec![event.envelope().canonical_record().unwrap()],
        next_cursor: None,
        blob_refs: vec![],
        record_refs: vec![],
    };
    let sessions = Arc::new(RwLock::new(HashMap::new()));
    // When an unowned session imports through the real internal server adapter.
    import_session_events(&sessions, wire, None).await.unwrap();
    // Then neither the remote metadata nor its advertised head grants ownership.
    assert!(sessions.read().await.is_empty());
    assert!(!crate::storage::active_session_ids().contains(&id.as_str().to_owned()));
    assert!(!home.directory.path().join("session_leases").exists());
    let loaded = Session::load(id.as_str()).unwrap();
    assert_eq!(loaded.status, crate::session::SessionStatus::Closed);
    assert_eq!(loaded.last_pid, None);
    assert_eq!(loaded.working_dir, None);
    assert_eq!(loaded.migration_epoch, 0);
    assert_eq!(
        loaded.selected_event_head().as_deref(),
        Some(event.envelope().event_id.as_str())
    );
    let graph = store::EventGraph::open(&home.paths(), &id).unwrap();
    assert_eq!(graph.replay(None).unwrap().metadata().migration_epoch, u64::MAX);
}

#[test]
fn session_replication_import_preserves_existing_local_binding_and_lease() {
    // Given a locally owned session and a real immutable local lease.
    let home = Home::new();
    let mut session = seed("replica_local_binding");
    session.working_dir = Some("/local/bound/project".into());
    session.migration_epoch = 7;
    session.mark_active();
    session.save().unwrap();
    crate::storage::write_session_lease(&crate::storage::SessionLease {
        session_id: session.id.clone(),
        epoch: 7,
        away_host: None,
        repo_root: session.working_dir.clone(),
        last_sync_snapshot: None,
        last_sync_head: None,
        updated_at: "local".into(),
    })
    .unwrap();
    let lease_path = home
        .directory
        .path()
        .join("session_leases")
        .join(format!("{}.json", session.id));
    let lease = std::fs::read(&lease_path).unwrap();
    let id = store::SessionId::parse(&session.id).unwrap();
    let graph = store::EventGraph::open(&home.paths(), &id).unwrap();
    let mut metadata = graph.replay(None).unwrap().metadata().clone();
    metadata.working_dir = Some("/remote/not-a-local-binding".into());
    metadata.migration_epoch = 999;
    let event = store::EventDraft::new(
        id.clone(),
        store::EventId::new_v4(),
        Some(store::EventId::parse(session.selected_event_head().unwrap()).unwrap()),
        store::OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        store::EventKind::new(store::KnownEventKind::MetadataChanged),
        serde_json::json!({"metadata": metadata}),
    )
    .seal()
    .unwrap();
    let mut wire = successor(&session, "unused");
    wire.heads = vec![event.envelope().event_id.clone()];
    wire.events = vec![event.envelope().canonical_record().unwrap()];
    // When remote metadata updates the original graph.
    session.import_replica(wire, None).unwrap();
    // Then execution continues to use only this host's explicit binding.
    assert_eq!(session.working_dir.as_deref(), Some("/local/bound/project"));
    assert_eq!(session.migration_epoch, 7);
    assert_eq!(session.last_pid, Some(std::process::id()));
    assert_eq!(std::fs::read(lease_path).unwrap(), lease);
    let restored = Session::load(&session.id).unwrap();
    assert_eq!(restored.working_dir, session.working_dir);
    assert_eq!(restored.migration_epoch, 7);
    session.mark_closed();
}

#[test]
fn session_replication_clear_keeps_identity_and_restores_original_branch() {
    // Given a durable selected branch with immutable originals.
    let home = Home::new();
    let mut session = seed("replica_clear");
    let id = session.id.clone();
    let previous_head = session.selected_event_head().unwrap();
    let original = std::fs::read(session.event_log_path().unwrap()).unwrap();
    let generation = session.event_generation().unwrap();
    // When Clear chooses a new empty reset branch in the same session.
    session.clear_event_branch().unwrap();
    // Then reset provenance and the old branch remain independently addressable.
    assert_eq!(session.id, id);
    assert!(session.history_messages().unwrap().is_empty());
    assert_ne!(session.event_generation().unwrap(), generation);
    assert!(
        std::fs::read(session.event_log_path().unwrap())
            .unwrap()
            .starts_with(&original)
    );
    let graph = store::EventGraph::open(&home.paths(), &store::SessionId::parse(&id).unwrap()).unwrap();
    let reset = graph
        .ordered_ids()
        .iter()
        .filter_map(|id| graph.event(id))
        .find(|event| {
            event.kind.known() == Some(store::KnownEventKind::SessionCreated)
                && event.payload_value().unwrap()["branch_root"] == true
        })
        .unwrap();
    assert_eq!(
        reset.payload_value().unwrap()["reset_provenance"]["cleared_head_event_id"],
        previous_head
    );
    session.select_event_head(&previous_head).unwrap();
    assert_eq!(session.history_messages().unwrap()[0].content_preview(), "original");
}

#[test]
fn session_replication_explicit_null_metadata_does_not_resurrect_local_binding() {
    // Given a distinct local execution cwd and canonical display metadata.
    let home = Home::new();
    let mut session = seed("replica_explicit_null");
    session.working_dir = Some("/local/binding".into());
    session.saved = true;
    session.save_label = Some("old label".into());
    session.save().unwrap();
    let id = store::SessionId::parse(&session.id).unwrap();
    let original = store::EventGraph::open(&home.paths(), &id).unwrap();
    let mut metadata = original.replay(None).unwrap().metadata().clone();
    let same_timestamp = metadata.updated_at;
    metadata.working_dir = None;
    metadata.save_label = None;
    let event = store::EventDraft::new(
        id.clone(),
        store::EventId::new_v4(),
        Some(store::EventId::parse(session.selected_event_head().unwrap()).unwrap()),
        store::OriginId::new_v4(),
        same_timestamp,
        store::EventKind::new(store::KnownEventKind::MetadataChanged),
        serde_json::json!({"metadata": metadata}),
    )
    .seal()
    .unwrap();
    let mut wire = successor(&session, "unused");
    wire.heads = vec![event.envelope().event_id.clone()];
    wire.events = vec![event.envelope().canonical_record().unwrap()];
    // When an explicit NULL arrives at exactly the old metadata timestamp.
    session.import_replica(wire, None).unwrap();
    session.save().unwrap();
    let mut restarted = Session::load(&session.id).unwrap();
    restarted.save().unwrap();
    // Then ordinary save/restart preserves canonical NULL, not a receipt/time fallback.
    let graph = store::EventGraph::open(&home.paths(), &id).unwrap();
    let projection = graph.replay(None).unwrap();
    assert_eq!(projection.metadata().working_dir, None);
    assert_eq!(projection.metadata().save_label, None);
    assert_eq!(restarted.working_dir.as_deref(), Some("/local/binding"));
    restarted.working_dir = None;
    restarted.save().unwrap();
    assert_eq!(Session::load(&session.id).unwrap().working_dir, None);
}
