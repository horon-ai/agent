use super::tests::cases::canonical_tempdir;
use super::*;
use anyhow::{Context, Result};

fn text(value: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text: value.into(),
        cache_control: None,
    }]
}

#[test]
fn task7_recent_lists_event_original_without_creating_sqlite() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-event-store-recent-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut session = Session::create_with_id("catalog_event".into(), None, None);
    session.add_message(Role::User, text("catalog first prompt"));
    session.save()?;
    let original = std::fs::read(session.event_log_path()?)?;

    let entries = crate::recent_session_index::recent(10)?;

    for suffix in ["", "-wal", "-shm"] {
        assert!(
            !home_path.join(format!("session-metadata-v1.sqlite3{suffix}")).exists(),
            "runtime listing created SQLite {suffix}"
        );
    }
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].session_id, session.id);
    assert_eq!(entries[0].display_title(), Some("catalog first prompt"));
    assert_eq!(std::fs::read(session.event_log_path()?)?, original);
    Ok(())
}

#[test]
fn task7_new_session_saves_beside_unreceipted_legacy_database() -> Result<()> {
    use horon_agent_session_store as store;
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-event-store-unreceipted-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    // The operator's real HOME still carries the legacy catalog database without an
    // offline completion receipt. A brand-new session's own event log consumes no
    // catalog metadata, so that HOME-wide gate must not block saving or loading it.
    std::fs::write(home_path.join("session-metadata-v1.sqlite3"), b"legacy")?;

    let mut session = Session::create_with_id("db_blocked_home".into(), None, None);
    session.add_message(Role::User, text("first"));
    session.save()?;
    let prefix = std::fs::read(session.event_log_path()?)?;
    session.add_message(Role::Assistant, text("second"));
    session.save()?;
    let restored = Session::load(&session.id)?;
    let history = restored.history_messages()?;
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].content_preview(), "first");
    assert_eq!(history[1].content_preview(), "second");
    assert!(std::fs::read(session.event_log_path()?)?.starts_with(&prefix));

    // The catalog gate is unchanged: DB-derived metadata still needs its receipt.
    let paths = store::SessionPaths::new(home_path.join("sessions"));
    assert_eq!(
        store::legacy::require_index_migration(&paths).unwrap_err().code,
        store::StoreErrorCode::MigrationRequired
    );

    // T7-P3 stays binding: a same-ID legacy snapshot beside canonical events is
    // still refused instead of being admitted as a receipt-free replica.
    std::fs::write(
        home_path.join(format!("sessions/{}.json", session.id)),
        serde_json::to_vec(&session)?,
    )?;
    let refused = Session::load(&session.id).unwrap_err();
    assert_eq!(
        refused.downcast_ref::<store::StoreError>().map(|error| error.code),
        Some(store::StoreErrorCode::MigrationRequired)
    );
    Ok(())
}

#[test]
fn task7_incomplete_first_file_migration_resumes_on_load_and_restart() -> Result<()> {
    use horon_agent_session_store as store;
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-event-store-first-file-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut legacy = Session::create_with_id("task7_first_file".into(), None, None);
    legacy.add_message(Role::User, text("preserved first file"));
    legacy.status = SessionStatus::Closed;
    legacy.last_pid = None;
    let original = serde_json::to_vec(&legacy)?;
    let root = home_path.join("sessions");
    crate::storage::create_store_dir(&root, false)?;
    let original_path = root.join("task7_first_file.json");
    std::fs::write(&original_path, &original)?;
    let paths = store::SessionPaths::new(root);
    let id = store::SessionId::parse("task7_first_file")?;
    let source = store::legacy::LegacySource::new(&home_path, id.clone())?;
    let interrupted = source
        .migrate_for_runtime_with_hook(
            &paths,
            Box::new(|point| {
                if point == store::legacy::MigrationPoint::RuntimeSourcesPublished {
                    Err(std::io::Error::other("first file publication interrupted"))
                } else {
                    Ok(())
                }
            }),
        )
        .unwrap_err();
    assert_eq!(interrupted.code, store::StoreErrorCode::Io);
    let prefix = std::fs::read(paths.events_log(&id)?)?;

    let mut loaded = Session::load(id.as_str())?;
    assert_eq!(loaded.history_messages()?.len(), 1);
    assert!(!paths.session_dir(&id)?.join(".legacy-first-file.json").exists());
    loaded.save()?;
    drop(loaded);
    let restarted = Session::load(id.as_str())?;

    assert_eq!(
        restarted.history_messages()?[0].content_preview(),
        "preserved first file"
    );
    assert_eq!(std::fs::read(original_path)?, original);
    assert!(std::fs::read(paths.events_log(&id)?)?.starts_with(&prefix));
    for suffix in ["", "-wal", "-shm"] {
        assert!(!home_path.join(format!("session-metadata-v1.sqlite3{suffix}")).exists());
    }
    Ok(())
}

#[test]
fn append_restart_keeps_original_prefix_and_never_creates_legacy_authority() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-event-store-append-restart-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut session = Session::create_with_id("event_restart".into(), None, None);
    session.add_message(Role::User, text("first"));
    session.save()?;
    let path = session.event_log_path()?;
    let prefix = std::fs::read(&path)?;
    assert!(!session_path(&session.id)?.exists());
    assert!(!session_journal_path(&session.id)?.exists());
    session.add_message(Role::Assistant, text("second"));
    session.save()?;
    assert!(std::fs::read(&path)?.starts_with(&prefix));
    let restarted = Session::load(&session.id)?;
    let history = restarted.history_messages()?;
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].content_preview(), "first");
    assert_eq!(history[1].content_preview(), "second");
    assert_eq!(restarted.display_title(), Some("first"));
    Ok(())
}

#[test]
fn startup_and_remote_stubs_preserve_existing_history_on_save() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-event-store-startup-stub-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut original = Session::create_with_id("stub_preserved".into(), None, None);
    original.add_message(Role::User, text("never erase"));
    original.save()?;
    let prefix = std::fs::read(original.event_log_path()?)?;
    let mut stub = Session::load_startup_stub(&original.id)?;
    assert!(stub.active_context().is_empty());
    stub.rename_title(Some("renamed".into()));
    stub.save()?;
    let mut remote = Session::load_for_remote_startup(&original.id)?;
    remote.strip_transcript_for_remote_client();
    remote.status = SessionStatus::Reloaded;
    remote.save()?;
    let restored = Session::load(&original.id)?;
    assert_eq!(restored.history_messages()?[0].content_preview(), "never erase");
    assert_eq!(restored.display_title(), Some("renamed"));
    assert!(std::fs::read(original.event_log_path()?)?.starts_with(&prefix));
    Ok(())
}

#[test]
fn stale_facade_cannot_acknowledge_overwriting_another_save() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-event-store-stale-facade-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut first = Session::create_with_id("source_conflict".into(), None, None);
    first.add_message(Role::User, text("base"));
    first.save()?;
    let mut stale = Session::load(&first.id)?;
    first.add_message(Role::Assistant, text("new source"));
    first.save()?;
    stale.add_message(Role::Assistant, text("stale"));
    assert!(stale.save().unwrap_err().to_string().contains("SourceConflict"));
    let history = Session::load(&first.id)?.history_messages()?;
    assert_eq!(history.len(), 2);
    assert_eq!(history[1].content_preview(), "new source");
    Ok(())
}

/// A live turn must be able to finish after another writer records a resume
/// marker for the same session.
///
/// This is the product path behind `SourceConflict: session originals changed
/// since this facade was loaded`. A turn parked on an approval keeps its facade
/// open; when the approval arrives, the ambient runner records "a resume turn
/// was created for this request" by appending a `MetadataChanged` marker to the
/// same log. The marker restates the metadata the head already projects, so it
/// changes nothing the running turn depends on — but the turn's own save used to
/// compare the whole-file hash, see a difference, and drop its answer.
///
/// The test writes the marker through the real store rather than mocking the
/// conflict away, and asserts on the outcome the user sees: the turn's final
/// message is in the transcript.
#[test]
fn a_resume_marker_written_mid_turn_does_not_drop_the_turn() -> Result<()> {
    use horon_agent_session_store as store;

    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-event-store-concurrent-marker-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);

    let mut live = Session::create_with_id("concurrent_marker".into(), None, None);
    live.add_message(Role::User, text("do the long job"));
    live.save()?;

    // A second facade, exactly as the ambient runner builds one, records the
    // resume marker while the first is still mid-turn.
    Session::append_resume_key(&live.id, "req_mid_turn")?;

    // The turn finishes and writes its answer through the same facade it began
    // with. Before the fix this returned `SourceConflict` and the answer was lost.
    live.add_message(Role::Assistant, text("RESULT approved"));
    live.save()?;

    let reloaded = Session::load(&live.id)?;
    let history = reloaded.history_messages()?;
    assert_eq!(history.len(), 2, "the turn's own message must reach the transcript");
    assert_eq!(history[1].content_preview(), "RESULT approved");
    assert!(
        Session::journal_has_resume_key(&live.id, "req_mid_turn")?,
        "adopting the marker must keep the other writer's record"
    );

    // The marker is inert: replaying it must not have changed the metadata the
    // turn wrote, and the log must hold exactly one marker for this request.
    let log = std::fs::read_to_string(live.event_log_path()?)?;
    assert_eq!(
        log.lines()
            .filter(|line| line.contains("\"resume_key\":\"req_mid_turn\""))
            .count(),
        1,
        "the marker must be recorded once"
    );

    // The adopted log stays a single chain: every event after the first has a
    // parent, so the answer is not a second branch the reader would have to pick.
    let id = store::SessionId::parse(&live.id)?;
    let paths = store::SessionPaths::new(home_path.join("sessions"));
    let report = store::full_scan(&paths, &id)?;
    assert!(report.damage.is_none(), "the adopted log must still parse");
    assert!(
        report
            .events
            .iter()
            .skip(1)
            .all(|event| event.parent_event_id.is_some()),
        "adopting a metadata tail must not start a new root"
    );
    Ok(())
}

/// A transcript event appended by someone else is still a conflict.
///
/// Adopting it would interleave two conversations under one head, so the save
/// must keep failing instead of silently merging them. This pins the boundary of
/// the fix above: only metadata-only tails are adopted.
#[test]
fn a_foreign_transcript_event_is_still_a_source_conflict() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-event-store-foreign-transcript-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);

    let mut live = Session::create_with_id("foreign_transcript".into(), None, None);
    live.add_message(Role::User, text("do the long job"));
    live.save()?;

    let mut other = Session::load(&live.id)?;
    other.add_message(Role::Assistant, text("another conversation"));
    other.save()?;

    live.add_message(Role::Assistant, text("my answer"));
    let error = live.save().expect_err("a foreign transcript event must not be adopted");
    assert!(
        error.to_string().contains("SourceConflict"),
        "expected SourceConflict, got {error}"
    );

    let history = Session::load(&live.id)?.history_messages()?;
    assert_eq!(history.len(), 2, "the conflicting save must not have written anything");
    assert_eq!(history[1].content_preview(), "another conversation");
    Ok(())
}

#[test]
fn renderer_rewind_selects_original_head_and_undo_restores_it() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-event-store-rewind-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut session = Session::create_with_id("rewind_events".into(), None, None);
    session.add_message(Role::User, text("one"));
    session.add_message(Role::Assistant, text("two"));
    session.add_message(Role::User, text("three"));
    session.save()?;
    let head = session.selected_event_head().unwrap();
    let original = std::fs::read(session.event_log_path()?)?;
    assert_eq!(session.rewind_to_rendered_message(1)?, 2);
    assert_eq!(session.history_messages()?.len(), 1);
    session.select_event_head(&head)?;
    assert_eq!(session.history_messages()?.len(), 3);
    assert!(std::fs::read(session.event_log_path()?)?.starts_with(&original));
    Ok(())
}

#[test]
fn failed_sync_has_no_success_and_retries_the_reserved_event_ids() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-event-store-retry-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut session = Session::create_with_id("retry_events".into(), None, None);
    let mut fail = true;
    session.set_event_io_hook(Box::new(move |point| {
        if point == horon_agent_session_store::IoPoint::LogDataSync && fail {
            fail = false;
            return Err(std::io::Error::other("injected sync failure"));
        }
        Ok(())
    }))?;
    session.add_message(Role::User, text("durable exactly once"));
    assert!(session.save().is_err());
    let reserved = session.pending_event_ids();
    assert!(!reserved.is_empty());
    session.save()?;
    assert!(session.pending_event_ids().is_empty());
    let events = std::fs::read_to_string(session.event_log_path()?)?;
    for id in reserved {
        let count = events
            .lines()
            .map(serde_json::from_str::<serde_json::Value>)
            .collect::<std::result::Result<Vec<_>, _>>()?
            .iter()
            .filter(|event| event["event_id"] == id)
            .count();
        assert_eq!(count, 1);
    }
    assert_eq!(Session::load(&session.id)?.history_messages()?.len(), 1);
    Ok(())
}

#[test]
fn compaction_releases_prefix_but_export_and_renderer_keep_full_history() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-event-store-compaction-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut session = Session::create_with_id("bounded_context".into(), None, None);
    session.provider_key = Some("openai".into());
    session.model = Some("test-model".into());
    session.route_api_method = Some("openai-api".into());
    session.add_message(Role::User, text("old question"));
    session.add_message(Role::Assistant, text("old answer"));
    session.add_message(Role::User, text("current question"));
    session.save()?;
    let prefix = std::fs::read(session.event_log_path()?)?;
    session.compaction = Some(StoredCompactionState {
        summary_text: "old question answered".into(),
        openai_encrypted_content: None,
        native_window: Vec::new(),
        native_route: None,
        covers_up_to_turn: 2,
        original_turn_count: 3,
        compacted_count: 2,
        structured: None,
    });
    session.save()?;
    assert_eq!(session.active_context().len(), 1);
    assert_eq!(session.active_history_offset(), 2);
    assert_eq!(session.history_messages()?.len(), 3);
    assert_eq!(session.compaction_for_active_context().unwrap().compacted_count, 0);
    assert!(std::fs::read(session.event_log_path()?)?.starts_with(&prefix));
    let loaded = Session::load(&session.id)?;
    assert_eq!(loaded.active_context().len(), 1);
    let rendered = render_messages(&loaded);
    assert!(rendered.iter().any(|row| row.content == "old answer"));
    assert!(rendered.iter().any(|row| row.content == "current question"));
    assert_eq!(serde_json::to_value(&loaded)?["messages"].as_array().unwrap().len(), 3);
    assert_eq!(loaded.history_page(None, 2)?.len(), 2);
    Ok(())
}

#[test]
fn compacted_payload_carries_structured_usage_and_restoration() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-event-store-payload-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut session = Session::create_with_id("structured_payload".into(), None, None);
    session.provider_key = Some("openai".into());
    session.model = Some("payload-model".into());
    session.route_api_method = Some("openai-api".into());
    session.add_message(Role::User, text("old question"));
    session.add_message(Role::Assistant, text("old answer"));
    session.add_message(Role::User, text("current question"));
    session.save()?;
    session.compaction = Some(StoredCompactionState {
        summary_text: "## Goal\nfinish task 12".into(),
        openai_encrypted_content: None,
        native_window: Vec::new(),
        native_route: None,
        covers_up_to_turn: 2,
        original_turn_count: 3,
        compacted_count: 2,
        structured: Some(crate::session::StoredSummaryMetadata {
            input_tokens: Some(4_321),
            output_tokens: Some(123),
            restoration: Some(crate::session::StoredRestorationManifest {
                entries: vec![crate::session::StoredRestorationEntry {
                    path: "crates/base/src/compaction/summary.rs".into(),
                    kind: "file".into(),
                    note: Some("read".into()),
                }],
                total_bytes: 30_000,
            }),
            ..Default::default()
        }),
    });
    session.save()?;

    let paths = super::event_store::paths()?;
    let id = horon_agent_session_store::SessionId::parse(&session.id)?;
    let graph = super::event_store::graph(&paths, &id)?;
    let projection = graph.replay(None)?;
    let (_compaction_id, payload) = projection.compaction().expect("compacted event present");
    assert_eq!(payload.summary_text, "## Goal\nfinish task 12");
    assert_eq!(payload.input_tokens, Some(4_321));
    assert_eq!(payload.output_tokens, Some(123));
    assert_eq!(payload.outcome, horon_agent_session_store::CompactionOutcome::Summary);
    assert_eq!(payload.provider, "openai");
    assert_eq!(payload.model, "payload-model");
    assert_eq!(payload.route, "openai-api");
    assert_eq!(payload.settings_digest.len(), 64);
    let restoration = payload.restoration.as_ref().expect("restoration manifest");
    assert_eq!(restoration.entries.len(), 1);
    assert_eq!(restoration.entries[0].kind, "file");
    assert_eq!(restoration.total_bytes, 30_000);
    assert!(payload.source_head_id.is_some());
    let covered_message = projection.messages().get(1).expect("second message").event_id.clone();
    assert_eq!(payload.covered_through_event_id, covered_message);
    assert_eq!(
        projection.messages().get(2).map(|message| message.event_id.clone()),
        payload.first_kept_event_id
    );
    assert_eq!(
        payload.source_prefix_digest,
        graph.prefix_digest(
            payload.source_head_id.as_ref().expect("source head"),
            &payload.covered_through_event_id,
            payload.prior_compaction_id.as_ref(),
        )?
    );
    let reloaded = Session::load(&session.id)?;
    let restored_state = reloaded.compaction.expect("reloaded compaction state");
    assert_eq!(
        restored_state.structured.as_ref().and_then(|meta| meta.input_tokens),
        Some(4_321)
    );
    assert_eq!(
        restored_state
            .structured
            .as_ref()
            .and_then(|meta| meta.restoration.as_ref())
            .map(|manifest| manifest.entries.len()),
        Some(1)
    );
    Ok(())
}

#[test]
fn clearing_compaction_or_changing_model_restores_original_context() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-event-store-invalidate-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    for clear in [true, false] {
        let mut session = Session::create_with_id(format!("invalidate_{clear}"), None, None);
        session.provider_key = Some("openai".into());
        session.model = Some("first-model".into());
        session.route_api_method = Some("openai-api".into());
        session.add_message(Role::User, text("old question"));
        session.add_message(Role::Assistant, text("old answer"));
        session.add_message(Role::User, text("latest request"));
        session.compaction = Some(StoredCompactionState {
            summary_text: "old work summarized".into(),
            openai_encrypted_content: None,
            native_window: Vec::new(),
            native_route: None,
            covers_up_to_turn: 2,
            original_turn_count: 3,
            compacted_count: 2,
            structured: None,
        });
        session.save()?;
        assert_eq!(session.active_context().len(), 1);
        let prefix = std::fs::read(session.event_log_path()?)?;
        if clear {
            session.compaction = None;
        } else {
            session.model = Some("second-model".into());
        }
        session.save()?;
        assert_eq!(session.active_context().len(), 3);
        assert!(session.compaction_for_active_context().is_none());
        let restored = Session::load(&session.id)?;
        assert_eq!(restored.active_context().len(), 3);
        assert!(restored.compaction_for_active_context().is_none());
        assert!(std::fs::read(session.event_log_path()?)?.starts_with(&prefix));
    }
    Ok(())
}

#[test]
fn remote_startup_save_does_not_reappend_compacted_history() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-event-store-remote-startup-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut session = Session::create_with_id("remote_compacted".into(), None, None);
    session.provider_key = Some("openai".into());
    session.model = Some("model".into());
    session.route_api_method = Some("openai-api".into());
    session.add_message(Role::User, text("old question"));
    session.add_message(Role::Assistant, text("old answer"));
    session.add_message(Role::User, text("latest question"));
    session.compaction = Some(StoredCompactionState {
        summary_text: "old work".into(),
        openai_encrypted_content: None,
        native_window: Vec::new(),
        native_route: None,
        covers_up_to_turn: 2,
        original_turn_count: 3,
        compacted_count: 2,
        structured: None,
    });
    session.save()?;
    let mut remote = Session::load_for_remote_startup(&session.id)?;
    assert_eq!(remote.active_context().len(), 3);
    assert_eq!(remote.visible_conversation_message_count(), 3);
    remote.save()?;
    let restored = Session::load(&session.id)?;
    assert_eq!(restored.history_messages()?.len(), 3);
    assert_eq!(restored.active_context().len(), 1);
    Ok(())
}

#[test]
fn resident_context_reports_the_released_prefix_after_compaction() -> Result<()> {
    use horon_agent_session_store as store;
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-event-store-resident-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut session = Session::create_with_id("resident_bound".into(), None, None);
    for index in 0..12 {
        session.add_message(Role::User, text(&format!("{index:02}{}", "z".repeat(6000))));
    }
    session.save()?;
    let name = session.id.clone();
    drop(session);

    let uncompacted = Session::load(&name)?;
    let profile = uncompacted.debug_memory_profile();
    assert_eq!(profile["messages"]["count"], 12);
    assert_eq!(profile["messages"]["released_prefix_count"], 0);
    assert_eq!(
        profile["messages"]["resident_tail_budget_bytes"],
        store::INLINE_TAIL_BYTES as u64
    );
    drop(uncompacted);

    let paths = store::SessionPaths::new(home_path.join("sessions"));
    let id = store::SessionId::parse(&name)?;
    let graph = store::EventGraph::open(&paths, &id)?;
    let order = graph.ordered_ids().to_vec();
    let head = graph.heads()[0].clone();
    println!(
        "RESIDENT_BOUND fixture_events={} heads={}",
        order.len(),
        graph.heads().len()
    );
    let covered = order[order.len() - 5].clone();
    let first_kept = order[order.len() - 4].clone();
    let payload = store::CompactedPayload {
        summary_text: "covered prefix summary".into(),
        source_head_id: Some(head.clone()),
        covered_through_event_id: covered.clone(),
        source_prefix_digest: graph.prefix_digest(&head, &covered, None)?,
        first_kept_event_id: Some(first_kept),
        prior_compaction_id: None,
        provider: "openai".into(),
        model: "model".into(),
        route: "route".into(),
        settings_digest: store::sha256_hex(b"settings"),
        generation: 1,
        input_tokens: None,
        output_tokens: None,
        outcome: store::CompactionOutcome::Summary,
        native_window: Vec::new(),
        restoration: None,
    };
    let event = store::EventDraft::new(
        id.clone(),
        store::EventId::new_v4(),
        Some(head),
        store::OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        store::EventKind::new(store::KnownEventKind::Compacted),
        serde_json::json!(payload),
    )
    .seal()?;
    let mut writer = store::BlockingWriter::open(paths.clone(), id.clone())?;
    writer.enable_cache(store::ReadProbe::default());
    writer.append(&event)?;
    drop(writer);

    let compacted = Session::load(&name)?;
    let history = compacted.history_messages()?;
    let resident = compacted.active_context().len();
    let profile = compacted.debug_memory_profile();
    let reported = usize::try_from(profile["messages"]["count"].as_u64().unwrap())?;
    let released = usize::try_from(profile["messages"]["released_prefix_count"].as_u64().unwrap())?;
    println!(
        "RESIDENT_BOUND history={} resident={resident} released={released} budget_bytes={}",
        history.len(),
        profile["messages"]["resident_tail_budget_bytes"]
    );
    assert_eq!(reported, resident);
    assert_eq!(
        reported + released,
        history.len(),
        "every original message is either resident or released, never both"
    );
    assert!(
        released > 0 && resident < history.len(),
        "a compacted session must release its summarized prefix"
    );
    assert!(
        profile["messages"]["json_bytes"].as_u64().unwrap() > 0,
        "the resident messages carry their canonical bytes in the profile"
    );
    Ok(())
}

fn assert_streamed_hash_matches_whole_file(
    paths: &horon_agent_session_store::SessionPaths,
    id: &horon_agent_session_store::SessionId,
    label: &str,
) -> Result<u64> {
    use horon_agent_session_store as store;
    let log = paths.events_log(id)?;
    let whole = std::fs::read(&log)?;
    let streamed = crate::session::event_store::source_hash(paths, id)?.context("log must exist")?;
    assert_eq!(
        streamed,
        store::sha256_hex(&whole),
        "{label}: the streamed digest must equal the whole-file digest"
    );
    Ok(whole.len() as u64)
}

fn grow_log_to(path: &std::path::Path, target: u64) -> Result<u64> {
    use std::io::Write;
    let seed = std::fs::read(path)?;
    assert!(!seed.is_empty(), "the seed log must hold at least one record");
    let mut file = std::fs::File::create(path)?;
    let mut written = 0u64;
    while written < target {
        file.write_all(&seed)?;
        written += seed.len() as u64;
    }
    file.sync_all()?;
    Ok(written)
}

struct RequestProbe {
    file: std::fs::File,
    max_request: usize,
}

impl std::io::Read for RequestProbe {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.max_request = self.max_request.max(buffer.len());
        std::io::Read::read(&mut self.file, buffer)
    }
}

#[test]
fn source_hash_streams_the_log_and_matches_the_whole_file_digest() -> Result<()> {
    use horon_agent_session_store as store;
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-source-hash-parity-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut session = Session::create_with_id("source_hash_parity".into(), None, None);
    session.add_message(Role::User, text("seed"));
    session.save()?;
    let id = store::SessionId::parse(&session.id)?;
    let paths = store::SessionPaths::new(home_path.join("sessions"));
    let log = paths.events_log(&id)?;
    let seed = std::fs::read(&log)?;

    std::fs::write(&log, b"")?;
    assert_eq!(
        crate::session::event_store::source_hash(&paths, &id)?.context("empty log exists")?,
        store::sha256_hex(b""),
        "an empty log must hash to the empty-string digest"
    );
    std::fs::write(&log, b"x")?;
    assert_eq!(assert_streamed_hash_matches_whole_file(&paths, &id, "one byte")?, 1);

    assert!(seed.len() > 1, "the seed log must hold real records");
    std::fs::write(&log, &seed)?;
    assert_streamed_hash_matches_whole_file(&paths, &id, "seed")?;

    for (label, target) in [
        ("64 KiB", 65_536u64),
        ("1 MiB", 1_048_576),
        ("8 MiB", 8_388_608),
        ("64 MiB", 67_108_864),
        ("200 MiB", 200 * 1024 * 1024),
    ] {
        let bytes = grow_log_to(&log, target)?;
        assert_eq!(
            assert_streamed_hash_matches_whole_file(&paths, &id, label)?,
            bytes,
            "{label}: the streamed and whole-file reads must cover the same bytes"
        );
        println!("PARITY label={label} bytes={bytes}");
    }
    Ok(())
}

#[test]
fn source_hash_matches_the_whole_file_digest_on_every_real_fixture() -> Result<()> {
    use horon_agent_session_store as store;
    let Ok(root) = std::env::var("SOURCE_HASH_FIXTURES") else {
        return Ok(());
    };
    for name in [
        "event-log-100",
        "event-log-1000",
        "event-log-10000",
        "event-log-100000",
        "event-log-200mib",
    ] {
        let log = std::path::Path::new(&root).join(name).join("events.jsonl");
        let mut file = std::fs::File::open(&log)?;
        let streamed = store::sha256_hex_stream(&mut file)?;
        let whole = std::fs::read(&log)?;
        assert_eq!(
            streamed.hex,
            store::sha256_hex(&whole),
            "{name}: the streamed digest must equal the whole-file digest"
        );
        assert_eq!(streamed.bytes, whole.len() as u64, "{name}: every byte must be hashed");
        println!("FIXTURE name={name} bytes={} sha256={}", whole.len(), streamed.hex);
    }
    Ok(())
}

#[test]
fn source_hash_peak_buffer_stays_fixed_while_the_log_grows() -> Result<()> {
    use horon_agent_session_store as store;
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-source-hash-buffer-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut session = Session::create_with_id("source_hash_buffer".into(), None, None);
    session.add_message(Role::User, text("seed"));
    session.save()?;
    let id = store::SessionId::parse(&session.id)?;
    let paths = store::SessionPaths::new(home_path.join("sessions"));
    let log = paths.events_log(&id)?;

    let mut small = RequestProbe {
        file: std::fs::File::open(&log)?,
        max_request: 0,
    };
    store::sha256_hex_stream(&mut small)?;
    let grown_bytes = grow_log_to(&log, 200 * 1024 * 1024)?;
    let mut large = RequestProbe {
        file: std::fs::File::open(&log)?,
        max_request: 0,
    };
    let digest = store::sha256_hex_stream(&mut large)?;
    assert_eq!(digest.bytes, grown_bytes);
    assert_eq!(small.max_request, store::HASH_STREAM_BUFFER);
    assert_eq!(
        large.max_request,
        store::HASH_STREAM_BUFFER,
        "a {grown_bytes}-byte log must not enlarge the hash buffer"
    );
    println!(
        "BUFFER bytes={grown_bytes} small_request={} large_request={}",
        small.max_request, large.max_request
    );
    Ok(())
}

#[test]
fn source_hash_changes_when_the_log_is_appended_between_two_calls() -> Result<()> {
    use horon_agent_session_store as store;
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-source-hash-stale-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut session = Session::create_with_id("source_hash_stale".into(), None, None);
    session.add_message(Role::User, text("first"));
    session.save()?;
    let id = store::SessionId::parse(&session.id)?;
    let paths = store::SessionPaths::new(home_path.join("sessions"));
    let before = crate::session::event_store::source_hash(&paths, &id)?.context("log exists")?;
    session.add_message(Role::Assistant, text("second"));
    session.save()?;
    let after = crate::session::event_store::source_hash(&paths, &id)?.context("log exists")?;
    assert_ne!(before, after, "an appended log must hash differently");
    Ok(())
}

#[test]
fn a_mutated_prefix_still_reports_a_source_conflict() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-source-hash-mutated-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut live = Session::create_with_id("source_hash_mutated".into(), None, None);
    live.add_message(Role::User, text("first"));
    live.save()?;
    let log = live.event_log_path()?;
    let mut bytes = std::fs::read(&log)?;
    let first_line_end = bytes.iter().position(|byte| *byte == b'\n').context("no first line")?;
    let index = first_line_end - 1;
    bytes[index] = if bytes[index] == b'0' { b'1' } else { b'0' };
    std::fs::write(&log, &bytes)?;

    live.add_message(Role::Assistant, text("second"));
    let error = live.save().expect_err("a mutated prefix must not be overwritten");
    assert!(
        error.to_string().contains("SourceConflict"),
        "expected SourceConflict, got {error}"
    );
    assert_eq!(std::fs::read(&log)?, bytes, "the refused save must not write");
    Ok(())
}

#[test]
fn a_truncated_prefix_still_reports_a_source_conflict() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-source-hash-truncated-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut live = Session::create_with_id("source_hash_truncated".into(), None, None);
    live.add_message(Role::User, text("first"));
    live.add_message(Role::Assistant, text("second"));
    live.save()?;
    let log = live.event_log_path()?;
    let original = std::fs::read(&log)?;
    let last_break = original[..original.len() - 1]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .context("only one record")?;
    let truncated = original[..=last_break].to_vec();
    std::fs::write(&log, &truncated)?;

    live.add_message(Role::User, text("third"));
    let error = live.save().expect_err("a truncated prefix must not be overwritten");
    assert!(
        error.to_string().contains("SourceConflict"),
        "expected SourceConflict, got {error}"
    );
    assert_eq!(std::fs::read(&log)?, truncated, "the refused save must not write");
    Ok(())
}

#[test]
fn a_torn_tail_is_refused_until_recovery_rewrites_the_complete_prefix() -> Result<()> {
    use horon_agent_session_store as store;
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-source-hash-torn-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut live = Session::create_with_id("source_hash_torn".into(), None, None);
    live.add_message(Role::User, text("first"));
    live.add_message(Role::Assistant, text("second"));
    live.save()?;
    let log = live.event_log_path()?;
    let original = std::fs::read(&log)?;
    std::fs::write(&log, &original[..original.len() - 10])?;
    let id = store::SessionId::parse(&live.id)?;
    let paths = store::SessionPaths::new(home_path.join("sessions"));

    let error = crate::session::event_store::graph(&paths, &id).expect_err("a torn tail must not replay");
    assert!(
        error
            .to_string()
            .contains("incomplete original tail requires writer recovery"),
        "expected writer recovery, got {error}"
    );

    let read_error = Session::load(&live.id).expect_err("a torn tail must not load as history");
    assert!(
        read_error.to_string().contains("torn original"),
        "expected a refused torn original, got {read_error}"
    );
    assert_eq!(
        std::fs::read(&log)?,
        original[..original.len() - 10].to_vec(),
        "a refused read must not repair the original behind the caller"
    );

    drop(live);
    let recovered = store::SessionPaths::new(home_path.join("recovered"));
    let report = store::recover_to(&paths, &id, &recovered)?;
    assert!(
        !report.torn_tail.is_empty(),
        "the report must name the torn bytes it dropped"
    );
    assert!(report.events.len() >= 2, "the complete records must survive");
    let rewritten = std::fs::read(recovered.events_log(&id)?)?;
    assert_eq!(
        rewritten.len() as u64,
        report.complete_offset,
        "recovery writes exactly the complete prefix the scan reached"
    );
    assert_eq!(
        rewritten,
        original[..rewritten.len()].to_vec(),
        "recovery keeps every complete record byte for byte"
    );
    assert!(
        rewritten.len() + report.torn_tail.len() < original.len(),
        "the torn bytes the scan found stop before the truncated end"
    );
    Ok(())
}

// ---- streaming commit validation (plan task 4) ------------------------------

/// The fixture log size the commit validation must handle without holding the
/// log in memory.
const COMMIT_FIXTURE_BYTES: u64 = 1024 * 1024;

/// One commit's captured prefix with the commit's own bytes already appended:
/// the state a retry after a failed commit reads.
struct InterruptedCommit {
    _home: tempfile::TempDir,
    _env: tests::EnvVarGuard,
    session: Session,
    log: std::path::PathBuf,
    /// A directory standing where `select` publishes `local-head.json`.
    blocker: std::path::PathBuf,
    prefix: Vec<u8>,
    reserved: Vec<u8>,
}

impl InterruptedCommit {
    /// The log the refused retry must leave alone: prefix plus reserved bytes.
    fn captured(&self) -> Vec<u8> {
        let mut bytes = self.prefix.clone();
        bytes.extend_from_slice(&self.reserved);
        bytes
    }

    fn unblock(&self) -> Result<()> {
        std::fs::remove_dir(&self.blocker)?;
        Ok(())
    }

    /// How many times a reload sees a message with exactly `content`.
    fn occurrences(&self, content: &str) -> Result<usize> {
        let restored = Session::load(&self.session.id)?;
        Ok(restored
            .history_messages()?
            .iter()
            .filter(|message| message.content_preview() == content)
            .count())
    }
}

/// A session whose log is at least [`COMMIT_FIXTURE_BYTES`], whose next commit
/// appended its events and then failed: the reserved bytes are durable and the
/// facade still holds them as pending, which is exactly what a retry reads.
fn interrupted_commit() -> Result<InterruptedCommit> {
    let (home, home_path) = canonical_tempdir("horon-agent-streaming-commit-")?;
    let env = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut session = Session::create_with_id("streaming_commit".into(), None, None);
    for index in 0..40 {
        session.add_message(Role::User, text(&format!("seed {index} {}", "s".repeat(32 * 1024))));
    }
    session.save()?;
    let log = session.event_log_path()?;
    let prefix = std::fs::read(&log)?;
    assert!(
        prefix.len() as u64 >= COMMIT_FIXTURE_BYTES,
        "the fixture log is {} bytes, below {COMMIT_FIXTURE_BYTES}",
        prefix.len()
    );

    // Fail the commit where it stops being about the log: `select` publishes
    // `local-head-explicit.json` through a rename once anything stands at that
    // name, and a directory there refuses it. A session that never selected a
    // head explicitly keeps no flag file, so the directory is the only entry.
    let blocker = log
        .parent()
        .context("the log sits in the session directory")?
        .join("local-head-explicit.json");
    assert!(!blocker.exists(), "an implicit selection keeps no flag file");
    std::fs::create_dir(&blocker)?;
    session.add_message(Role::User, text("reserved tail"));
    session.save().expect_err("the blocked select must fail the commit");
    let committed = std::fs::read(&log)?;
    assert!(
        committed.len() > prefix.len(),
        "the failed commit must leave its reserved bytes on disk"
    );
    let reserved = committed[prefix.len()..].to_vec();
    Ok(InterruptedCommit {
        _home: home,
        _env: env,
        session,
        log,
        blocker,
        prefix,
        reserved,
    })
}

/// An exact retry of the reserved bytes commits once: the durable copy is
/// adopted, nothing is appended twice, and the turn continues.
#[test]
fn streaming_commit_validation_retries_the_exact_reserved_bytes() -> Result<()> {
    let _lock = tests::lock_env();
    let mut commit = interrupted_commit()?;
    commit.unblock()?;
    commit
        .session
        .add_message(Role::User, text("after the interrupted commit"));
    commit.session.save()?;

    let after = std::fs::read(&commit.log)?;
    let captured = commit.captured();
    assert!(
        after.starts_with(&captured),
        "the reserved bytes must stay in place exactly once"
    );
    assert!(
        after.len() > captured.len(),
        "the retry must also commit what the turn did after it"
    );
    assert_eq!(
        commit.occurrences("reserved tail")?,
        1,
        "a retried reserved event must not be duplicated"
    );
    assert_eq!(commit.occurrences("after the interrupted commit")?, 1);
    let restored = Session::load(&commit.session.id)?;
    assert_eq!(
        restored.history_messages()?.len(),
        42,
        "40 seeds, the reserved turn, and the retry turn"
    );
    println!(
        "MANUAL_QA case=a_exact_reserved_tail outcome=committed prefix={} reserved={} log={}",
        commit.prefix.len(),
        commit.reserved.len(),
        after.len()
    );
    Ok(())
}

/// A byte changed inside the captured prefix is a source conflict, and the
/// refused commit writes nothing.
#[test]
fn streaming_commit_validation_rejects_a_mutated_prefix() -> Result<()> {
    let _lock = tests::lock_env();
    let mut commit = interrupted_commit()?;
    let mut bytes = std::fs::read(&commit.log)?;
    let index = commit.prefix.len() / 2;
    bytes[index] = if bytes[index] == b'0' { b'1' } else { b'0' };
    std::fs::write(&commit.log, &bytes)?;
    commit.unblock()?;

    let error = commit
        .session
        .save()
        .expect_err("a mutated prefix must not be appended to");
    let message = error.to_string();
    assert!(
        message.contains("SourceConflict: original prefix was changed"),
        "expected a changed prefix, got {message}"
    );
    assert_eq!(std::fs::read(&commit.log)?, bytes, "the refused commit must not write");
    println!("MANUAL_QA case=b_mid_prefix_mutation outcome=refused error={message}");
    Ok(())
}

/// A log shorter than the captured prefix is a source conflict.
#[test]
fn streaming_commit_validation_rejects_a_truncated_prefix() -> Result<()> {
    let _lock = tests::lock_env();
    let mut commit = interrupted_commit()?;
    let bytes = std::fs::read(&commit.log)?;
    let truncated = bytes[..commit.prefix.len() / 2].to_vec();
    std::fs::write(&commit.log, &truncated)?;
    commit.unblock()?;

    let error = commit
        .session
        .save()
        .expect_err("a truncated prefix must not be appended to");
    let message = error.to_string();
    assert!(
        message.contains("SourceConflict: original prefix was truncated"),
        "expected a truncated prefix, got {message}"
    );
    assert_eq!(
        std::fs::read(&commit.log)?,
        truncated,
        "the refused commit must not write"
    );
    println!("MANUAL_QA case=c_truncated_prefix outcome=refused error={message}");
    Ok(())
}

/// A short reserved write is only this commit's own bytes: the retry completes
/// them, keeps one copy of every event, and never appends behind foreign bytes.
#[test]
fn streaming_commit_validation_completes_a_partial_reserved_write() -> Result<()> {
    let _lock = tests::lock_env();
    let mut commit = interrupted_commit()?;
    let bytes = std::fs::read(&commit.log)?;
    let partial = bytes[..commit.prefix.len() + commit.reserved.len() / 2].to_vec();
    std::fs::write(&commit.log, &partial)?;
    commit.unblock()?;

    let outcome = commit.session.save();
    match outcome {
        Ok(()) => {
            let after = std::fs::read(&commit.log)?;
            assert!(
                after.starts_with(&commit.captured()),
                "the completed log must hold the reserved bytes exactly once"
            );
            assert_eq!(commit.occurrences("reserved tail")?, 1);
            println!(
                "MANUAL_QA case=d_partial_reserved_write outcome=completed prefix={} reserved={} partial={} log={}",
                commit.prefix.len(),
                commit.reserved.len(),
                partial.len() - commit.prefix.len(),
                after.len()
            );
        }
        Err(error) => panic!("a short tail of this commit's own bytes must be completed, got {error}"),
    }
    Ok(())
}

/// Bytes that are not this commit's own tail are a source conflict, even when
/// they follow the exact reserved bytes.
#[test]
fn streaming_commit_validation_rejects_unrelated_trailing_bytes() -> Result<()> {
    let _lock = tests::lock_env();
    let mut commit = interrupted_commit()?;
    let mut bytes = std::fs::read(&commit.log)?;
    bytes.extend_from_slice(b"{\"foreign\":true}\n");
    std::fs::write(&commit.log, &bytes)?;
    commit.unblock()?;

    let error = commit
        .session
        .save()
        .expect_err("an unrelated append must not be written behind");
    let message = error.to_string();
    assert!(
        message.contains("SourceConflict: non-reserved bytes followed the captured prefix"),
        "expected a non-reserved tail, got {message}"
    );
    assert_eq!(std::fs::read(&commit.log)?, bytes, "the refused commit must not write");
    println!("MANUAL_QA case=e_unrelated_trailing_bytes outcome=refused error={message}");
    Ok(())
}

/// A reserved tail whose bytes differ from what this commit would write is a
/// source conflict: the log must not absorb a different version of the same
/// events.
#[test]
fn streaming_commit_validation_rejects_a_diverging_reserved_tail() -> Result<()> {
    let _lock = tests::lock_env();
    let mut commit = interrupted_commit()?;
    let mut bytes = std::fs::read(&commit.log)?;
    let index = commit.prefix.len() + commit.reserved.len() / 2;
    bytes[index] = if bytes[index] == b'0' { b'1' } else { b'0' };
    std::fs::write(&commit.log, &bytes)?;
    commit.unblock()?;

    let error = commit
        .session
        .save()
        .expect_err("a diverging reserved tail must not be appended to");
    let message = error.to_string();
    assert!(
        message.contains("SourceConflict: non-reserved bytes followed the captured prefix"),
        "expected a non-reserved tail, got {message}"
    );
    assert_eq!(std::fs::read(&commit.log)?, bytes, "the refused commit must not write");
    println!("MANUAL_QA case=f_diverging_reserved_tail outcome=refused error={message}");
    Ok(())
}

/// The validation reads the log through one fixed buffer, so a log that grows
/// from a few bytes to 200 MiB never asks the file for a larger read: the peak
/// extra allocation stays at [`store::HASH_STREAM_BUFFER`] whatever the log size.
#[test]
fn streaming_commit_validation_reads_through_one_fixed_buffer() -> Result<()> {
    use horon_agent_session_store as store;
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-commit-buffer-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut session = Session::create_with_id("commit_buffer".into(), None, None);
    session.add_message(Role::User, text("seed"));
    session.save()?;
    let log = session.event_log_path()?;

    let seed_bytes = std::fs::metadata(&log)?.len();
    let mut small = RequestProbe {
        file: std::fs::File::open(&log)?,
        max_request: 0,
    };
    {
        let mut probe = &mut small;
        crate::session::event_commit::validate_commit_prefix(&mut probe, usize::try_from(seed_bytes)?, None)?;
        crate::session::event_commit::compare_reserved_tail(&mut probe, b"")?;
    }

    let grown_bytes = grow_log_to(&log, 200 * 1024 * 1024)?;
    let mut large = RequestProbe {
        file: std::fs::File::open(&log)?,
        max_request: 0,
    };
    {
        let mut probe = &mut large;
        crate::session::event_commit::validate_commit_prefix(&mut probe, usize::try_from(grown_bytes)?, None)?;
        crate::session::event_commit::compare_reserved_tail(&mut probe, b"")?;
    }

    assert_eq!(small.max_request, store::HASH_STREAM_BUFFER);
    assert_eq!(
        large.max_request,
        store::HASH_STREAM_BUFFER,
        "a {grown_bytes}-byte log must not enlarge the validation buffer"
    );
    println!(
        "BUFFER source=commit-validation small_bytes={seed_bytes} large_bytes={grown_bytes} small_request={} large_request={} budget={}",
        small.max_request,
        large.max_request,
        store::HASH_STREAM_BUFFER
    );
    Ok(())
}
