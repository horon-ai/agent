use super::*;

fn refresh_failure_case(partial: bool) {
    use std::os::unix::fs::DirBuilderExt;
    // Given a real writer and a fault in the derived runtime destination only.
    let home = Home::new();
    let id = if partial {
        "refresh_partial"
    } else {
        "refresh_committed"
    };
    let directory = home.directory.path().join("sessions").join(id);
    let runtime = directory.join("local-runtime.json");
    let held = directory.join("local-runtime.held");
    let hook_runtime = runtime.clone();
    let hook_held = held.clone();
    let step = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hook_step = Arc::clone(&step);
    let mut session = Session::create_with_id(id.into(), None, None);
    session
        .set_event_io_hook(Box::new(move |point| {
            if point == store::IoPoint::LogDataSync {
                match hook_step.load(std::sync::atomic::Ordering::SeqCst) {
                    1 => {
                        std::fs::rename(&hook_runtime, &hook_held)?;
                        std::fs::DirBuilder::new().mode(0o700).create(&hook_runtime)?;
                        hook_step.store(if partial { 2 } else { 0 }, std::sync::atomic::Ordering::SeqCst);
                    }
                    2 => {
                        hook_step.store(0, std::sync::atomic::Ordering::SeqCst);
                        return Err(std::io::Error::from_raw_os_error(28));
                    }
                    _ => {}
                }
            }
            Ok(())
        }))
        .unwrap();
    session.add_message(Role::User, text("local acknowledged prefix"));
    session.save().unwrap();
    let before = std::fs::read(session.event_log_path().unwrap()).unwrap();
    let generation = session.event_generation().unwrap();
    let mut wire = successor(&session, "acknowledged remote");
    let first = wire.events[0].event_id.clone();
    if partial {
        let tail = page(&session, Some(first.clone()), "reserved remote");
        wire.events.extend(tail.events);
        wire.heads = tail.heads;
    }
    step.store(1, std::sync::atomic::Ordering::SeqCst);
    // When originals commit, but the subsequent local-runtime publication fails.
    let error = session.import_replica(wire.clone(), None).unwrap_err();
    let error = error
        .downcast_ref::<store::StoreError>()
        .expect("typed post-import failure");
    assert_eq!(error.code, store::StoreErrorCode::Io);
    assert_eq!(error.details.accepted_ids, vec![first.as_str()]);
    assert_eq!(error.details.retry_cursor.is_some(), partial);
    let cursor = error.details.retry_cursor.clone();
    let after = std::fs::read(session.event_log_path().unwrap()).unwrap();
    assert!(after.starts_with(&before));
    let committed_generation = session.event_generation().unwrap();
    assert_ne!(committed_generation, generation);
    assert!(session.ensure_event_current().is_err());
    println!(
        "TASK9_REFRESH_FAILURE partial={partial} details={}",
        error.wire_detail()
    );
    home.snapshot(if partial { "refresh-partial" } else { "refresh-complete" })
        .unwrap();
    // Then repaired publication and an identical retry neither duplicate nor discard originals.
    std::fs::remove_dir(&runtime).unwrap();
    std::fs::rename(&held, &runtime).unwrap();
    assert!(
        session.ensure_event_current().is_err(),
        "repair alone must not unblock a stale facade"
    );
    let result = session.import_replica(wire.clone(), cursor).unwrap();
    assert_eq!(result.accepted_ids.len(), wire.events.len());
    assert_eq!(std::fs::read(session.event_log_path().unwrap()).unwrap(), after);
    assert_eq!(session.event_generation().unwrap(), committed_generation);
    assert_eq!(session.history_messages().unwrap().len(), 1 + wire.events.len());
    session.ensure_event_current().unwrap();
}

#[test]
fn session_replication_refresh_failure_keeps_complete_import_ids_without_new_cursor() {
    refresh_failure_case(false);
}

#[test]
fn session_replication_refresh_failure_preserves_existing_partial_error_details() {
    refresh_failure_case(true);
}

#[test]
fn session_replication_invalid_cold_import_never_recovers_originals_first() {
    use std::io::Write;
    // Given a damaged tail owned by no actor and a malformed incoming page.
    let home = Home::new();
    let session = seed("replica_invalid_cold");
    let path = session.event_log_path().unwrap();
    let mut wire = successor(&session, "invalid remote");
    wire.events[0].content_hash = "0".repeat(64);
    drop(session);
    let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"{incomplete-original").unwrap();
    file.sync_all().unwrap();
    drop(file);
    let original = std::fs::read(&path).unwrap();
    std::fs::write(home.directory.path().join("before-invalid-import.jsonl"), &original).unwrap();
    // When the passive import rejects that page.
    let result = Session::import_replica_for_id(wire, None);
    home.snapshot("invalid-cold-import").unwrap();
    // Then validation precedes even recovery, preserving the damaged original bytes.
    assert!(result.is_err());
    assert_eq!(std::fs::read(path).unwrap(), original);
}

#[test]
fn session_replication_partial_prefix_retry_preserves_acknowledged_ids() {
    // Given two new records and failure at the second real log synchronization.
    let home = Home::new();
    let remaining = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&remaining);
    let mut session = Session::create_with_id("replica_prefix".into(), None, None);
    session
        .set_event_io_hook(Box::new(move |point| {
            if point == store::IoPoint::LogDataSync
                && counter.try_update(
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                    |n| n.checked_sub(1),
                ) == Ok(1)
            {
                return Err(std::io::Error::other("second import record sync failure"));
            }
            Ok(())
        }))
        .unwrap();
    session.add_message(Role::User, text("original"));
    session.save().unwrap();
    let mut wire = successor(&session, "first imported");
    let acknowledged = wire.events[0].event_id.clone();
    let tail = page(&session, Some(acknowledged.clone()), "second imported");
    wire.events.extend(tail.events);
    wire.heads = tail.heads;
    remaining.store(2, std::sync::atomic::Ordering::SeqCst);
    // When only the first record is acknowledged and the same page is resumed.
    let error = session.import_replica(wire.clone(), None).unwrap_err();
    let error = error.downcast_ref::<store::StoreError>().unwrap();
    assert_eq!(error.details.accepted_ids, vec![acknowledged.as_str()]);
    let cursor = error.details.retry_cursor.clone().unwrap();
    println!("TASK9_PARTIAL_DETAIL {}", error.wire_detail());
    home.snapshot("partial-prefix").unwrap();
    session.import_replica(wire, Some(cursor)).unwrap();
    // Then retry adds no duplicate and the previously uncertain record is usable.
    assert_eq!(session.history_messages().unwrap().len(), 3);
    session.ensure_event_current().unwrap();
}

#[test]
fn session_replication_same_head_explicit_selection_survives_warm_facade_and_divergent_import() {
    // Given a saved session whose facade cache already exists and its current head selected explicitly.
    let home = Home::new();
    let mut session = seed("replica_same_head_explicit");
    let head = session.selected_event_head().unwrap();
    let before = std::fs::read(session.event_log_path().unwrap()).unwrap();
    // Selecting the already durable head changes only the local explicit flag, not the id or stamp.
    session.select_event_head(&head).unwrap();
    session.ensure_event_current().unwrap();
    let id = store::SessionId::parse(&session.id).unwrap();
    let flag: bool = serde_json::from_slice(
        &std::fs::read(home.paths().session_dir(&id).unwrap().join("local-head-explicit.json")).unwrap(),
    )
    .unwrap();
    assert!(flag, "explicit selection is durable before the import");
    // A remote sibling of the first event is a genuinely divergent second head.
    let first = store::full_scan(&home.paths(), &id).unwrap().events[0].event_id.clone();
    let wire = page(&session, Some(first), "unselected remote sibling");
    // When a divergent import retains the explicitly chosen local head.
    let error = session.import_replica(wire, None).unwrap_err();
    // Then the chosen head stays selected, the original prefix is untouched, and execution may run.
    assert_eq!(error_code(&error), store::StoreErrorCode::ConflictHeads);
    assert_eq!(session.selected_event_head().as_deref(), Some(head.as_str()));
    assert!(
        std::fs::read(session.event_log_path().unwrap())
            .unwrap()
            .starts_with(&before)
    );
    session.ensure_event_current().unwrap();
    assert_eq!(session.history_messages().unwrap().len(), 1);
    let graph = store::EventGraph::open(&home.paths(), &id).unwrap();
    assert_eq!(graph.heads().len(), 2);
    assert_eq!(
        Session::load(&session.id).unwrap().selected_event_head().as_deref(),
        Some(head.as_str())
    );
}

#[test]
fn session_replication_explicit_local_head_survives_divergent_import() {
    // Given a real, explicitly selected ancestor and an independent remote child.
    let home = Home::new();
    let mut session = seed("replica_explicit_head");
    let old_head = session.selected_event_head().unwrap();
    session.add_message(Role::User, text("local future"));
    session.save().unwrap();
    session.select_event_head(&old_head).unwrap();
    let wire = successor(&session, "remote branch");
    let generation = session.event_generation().unwrap();
    // When import retains another head and reports the conflict.
    let result = session.import_replica(wire, None);
    // Then local intent is not replaced by a remote head or timestamp winner.
    assert_eq!(error_code(&result.unwrap_err()), store::StoreErrorCode::ConflictHeads);
    assert_eq!(session.selected_event_head().as_deref(), Some(old_head.as_str()));
    assert_ne!(session.event_generation().unwrap(), generation);
    session.ensure_event_current().unwrap();
    assert_eq!(session.history_messages().unwrap().len(), 1);
    let graph = store::EventGraph::open(&home.paths(), &store::SessionId::parse(&session.id).unwrap()).unwrap();
    assert_eq!(graph.heads().len(), 2);
    assert_eq!(
        Session::load(&session.id).unwrap().selected_event_head().as_deref(),
        Some(old_head.as_str())
    );
}
