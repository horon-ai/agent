use super::*;

fn capture(session: &mut Session, home: &Home) -> SessionEventJob {
    let id = store::SessionId::parse(&session.id).unwrap();
    let graph = store::EventGraph::open(&home.paths(), &id).unwrap();
    let covered = graph.replay(None).unwrap().messages()[0].event_id.clone();
    session
        .capture_event_job(covered.as_str(), &store::sha256_hex(b"settings"))
        .unwrap()
}
fn summary() -> store::CompactedPayload {
    store::CompactedPayload {
        summary_text: "original work".into(),
        source_head_id: None,
        covered_through_event_id: store::EventId::new_v4(),
        source_prefix_digest: String::new(),
        first_kept_event_id: None,
        prior_compaction_id: None,
        provider: "openai".into(),
        model: "local-model".into(),
        route: "openai-api".into(),
        settings_digest: String::new(),
        generation: 0,
        input_tokens: None,
        output_tokens: None,
        outcome: store::CompactionOutcome::Summary,
        native_window: vec![],
        restoration: None,
    }
}

#[test]
fn session_replication_old_job_is_rejected_after_import_without_appending() {
    // Given a captured job and a newly imported durable suffix.
    let home = Home::new();
    let mut session = seed("replica_job");
    session.add_message(Role::User, text("latest intent"));
    session.save().unwrap();
    let job = capture(&mut session, &home);
    session.import_replica(successor(&session, "imported"), None).unwrap();
    let before = std::fs::read(session.event_log_path().unwrap()).unwrap();
    // When the old job tries to apply.
    let error = session
        .apply_event_job(&job, summary(), &job.settings_digest)
        .unwrap_err();
    // Then no stale compaction or unrelated metadata event is appended.
    assert_eq!(error_code(&error), store::StoreErrorCode::SourceConflict);
    assert_eq!(std::fs::read(session.event_log_path().unwrap()).unwrap(), before);
}

#[test]
fn session_replication_safe_local_suffix_keeps_prefix_job_applicable() {
    // Given a captured immutable prefix with a later local tail append.
    let home = Home::new();
    let mut session = seed("replica_suffix");
    session.add_message(Role::User, text("latest intent"));
    session.save().unwrap();
    let job = capture(&mut session, &home);
    session.add_message(Role::User, text("new local tail"));
    session.save().unwrap();
    let original = std::fs::read(session.event_log_path().unwrap()).unwrap();
    // When the source-bound result is committed.
    session.apply_event_job(&job, summary(), &job.settings_digest).unwrap();
    // Then all originals remain, with the compaction attached to the current branch.
    assert!(
        std::fs::read(session.event_log_path().unwrap())
            .unwrap()
            .starts_with(&original)
    );
    assert_eq!(session.history_messages().unwrap().len(), 3);
    let graph = store::EventGraph::open(&home.paths(), &job.session_id).unwrap();
    let projection = graph.replay(None).unwrap();
    assert_eq!(projection.compaction().unwrap().1.generation, job.generation.generation);
    assert_eq!(
        projection.active_messages().last().unwrap().message.content_preview(),
        "new local tail"
    );
    let mut reloaded = Session::load(&session.id).unwrap();
    assert_eq!(
        reloaded.compaction_for_active_context().unwrap().summary_text,
        "original work"
    );
    reloaded.save().unwrap();
    let after = store::EventGraph::open(&home.paths(), &job.session_id).unwrap();
    assert!(after.replay(None).unwrap().compaction().is_some());
}

#[test]
fn session_replication_last_actor_drop_changes_incarnation() {
    // Given a job token that owns no Arc to the writer.
    let home = Home::new();
    let mut session = seed("replica_incarnation");
    session.add_message(Role::User, text("latest"));
    session.save().unwrap();
    let job = capture(&mut session, &home);
    let id = session.id.clone();
    drop(session);
    // When a new writer takes the real lock for the same session.
    let mut reopened = Session::load(&id).unwrap();
    let new_job = capture(&mut reopened, &home);
    let before = std::fs::read(reopened.event_log_path().unwrap()).unwrap();
    // Then an old incarnation cannot apply even if its generation number recurs.
    assert_ne!(job.generation.incarnation, new_job.generation.incarnation);
    assert_eq!(
        error_code(
            &reopened
                .apply_event_job(&job, summary(), &job.settings_digest)
                .unwrap_err()
        ),
        store::StoreErrorCode::SourceConflict
    );
    assert_eq!(std::fs::read(reopened.event_log_path().unwrap()).unwrap(), before);
}

#[test]
fn session_replication_branch_model_revision_rewind_and_clear_reject_captured_jobs() {
    for mutation in ["model", "revision", "branch", "rewind", "clear", "settings"] {
        // Given a real durable prefix and an independently captured job.
        let home = Home::new();
        let mut session = seed(&format!("replica_job_{mutation}"));
        let first_head = session.selected_event_head().unwrap();
        session.add_message(Role::User, text("latest intent"));
        session.save().unwrap();
        let job = capture(&mut session, &home);
        let mut settings = job.settings_digest.clone();
        // When one actual invalidation path changes the captured context.
        match mutation {
            "model" => {
                session.model = Some("changed-local-model".into());
                session.save().unwrap();
            }
            "revision" => {
                let mut message = session.active_context()[0].clone();
                message.content = text("revised original");
                session.revise_message(message).unwrap();
                session.save().unwrap();
            }
            "branch" => session.select_event_head(&first_head).unwrap(),
            "rewind" => {
                session.rewind_to_rendered_message(1).unwrap();
            }
            "clear" => session.clear_event_branch().unwrap(),
            "settings" => settings = store::sha256_hex(b"changed settings"),
            _ => unreachable!(),
        }
        let before = std::fs::read(session.event_log_path().unwrap()).unwrap();
        let result = session.apply_event_job(&job, summary(), &settings);
        // Then stale application appends neither a compaction nor metadata.
        assert_eq!(
            error_code(&result.unwrap_err()),
            store::StoreErrorCode::SourceConflict,
            "{mutation}"
        );
        assert_eq!(
            std::fs::read(session.event_log_path().unwrap()).unwrap(),
            before,
            "{mutation}"
        );
    }
}
