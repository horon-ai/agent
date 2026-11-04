use super::*;

fn message(n: u128, parent: u128) -> SealedEvent {
    seal(
        n,
        Some(parent),
        "message_appended",
        json!({"message":{
            "id":format!("message-{n}"),"role":"user","content":[{"type":"text","text":"keep intent"}]
        }}),
    )
}

fn compacted(source: u128, covered: u128, kept: u128, digest: String) -> SealedEvent {
    seal(
        5,
        Some(4),
        "compacted",
        json!({
            "summary_text":"summary","source_head_id":eid(source),"covered_through_event_id":eid(covered),
            "first_kept_event_id":eid(kept),"source_prefix_digest":digest,"prior_compaction_id":null,
            "provider":"openai","model":"test","route":"api","settings_digest":"a".repeat(64),
            "generation":0,"outcome":"summary"
        }),
    )
}

#[test]
fn reset_reference_closure_is_exported_before_the_new_root() {
    // Given a reset whose parent is null but whose provenance references the old branch.
    let a = Home::new();
    let b = Home::new();
    let reset = seal(
        2,
        None,
        "session_created",
        json!({
            "metadata":SessionMetadata::default(),"branch_root":true,
            "reset_provenance":{"reset_at":chrono::DateTime::UNIX_EPOCH,"cleared_head_event_id":eid(9)}
        }),
    );
    seed(&a, &[root(), unknown(9, 1), reset]);
    // When only the reset head is exported and imported.
    let wire = export_events(&a.paths(), &request(&[2])).unwrap();
    assert_eq!(
        wire.events.iter().map(|e| e.event_id.clone()).collect::<Vec<_>>(),
        vec![eid(1), eid(9), eid(2)]
    );
    assert_eq!(
        import_events(&b.paths(), &wire).unwrap_err().code,
        StoreErrorCode::ConflictHeads
    );
    // Then referenced originals are present even though the new parent chain omits them.
    assert_eq!(a.bytes(), b.bytes());
}

#[test]
fn compaction_references_validate_before_any_new_record_is_written() {
    // Given known history, with a future-kind event preceding the compaction.
    let h = Home::new();
    seed(&h, &[root(), message(2, 1), message(3, 2)]);
    let before = h.bytes();
    let future = unknown(4, 3);
    // When a later compaction refers to a missing source.
    let bad = compacted(99, 2, 3, "a".repeat(64));
    let error = import_events(&h.paths(), &page(&[future, bad])).unwrap_err();
    // Then unknown events cannot mask reference validation of subsequent known events.
    assert_eq!(error.code, StoreErrorCode::MissingDependency);
    assert_eq!(before, h.bytes());
}

#[test]
fn valid_compaction_roundtrip_keeps_boundaries_and_bad_digest_is_rejected() {
    // Given a valid source-bound compaction with an uncompressed latest user message.
    let a = Home::new();
    let b = Home::new();
    let events = vec![root(), message(2, 1), message(3, 2), message(4, 3)];
    let graph =
        EventGraph::from_events(&sid(), &events.iter().map(|e| e.envelope().clone()).collect::<Vec<_>>()).unwrap();
    let digest = graph.prefix_digest(&eid(4), &eid(2), None).unwrap();
    let valid = compacted(4, 2, 3, digest);
    seed(&a, &events);
    seed(&a, &[valid]);
    let wire = export_events(&a.paths(), &request(&[5])).unwrap();
    // When importing and then attempting a malformed new boundary.
    import_events(&b.paths(), &wire).unwrap();
    let before = b.bytes();
    let bad = compacted(4, 2, 3, "0".repeat(64));
    let fresh = Home::new();
    seed(&fresh, &events);
    assert_eq!(
        import_events(&fresh.paths(), &page(&[bad])).unwrap_err().code,
        StoreErrorCode::InvalidBoundary
    );
    // Then the valid projection is identical and only the rejected fixture is unchanged.
    let projection = EventGraph::open(&b.paths(), &sid()).unwrap().replay(None).unwrap();
    assert_eq!(projection.active_messages().len(), 2);
    assert_eq!(before, b.bytes());
    assert_eq!(full_scan(&fresh.paths(), &sid()).unwrap().events.len(), 4);
}

#[test]
fn missing_revision_target_and_same_id_collision_have_distinct_errors() {
    // Given a durable root.
    let h = Home::new();
    seed(&h, &[root()]);
    let before = h.bytes();
    let revision = seal(
        3,
        Some(1),
        "message_revised",
        json!({
            "target_event_id":eid(99),"replacement":{
                "id":"revision","role":"user","content":[{"type":"text","text":"replacement"}]
            }
        }),
    );
    // When a missing reference or an identity collision appears after a valid first record.
    assert_eq!(
        import_events(&h.paths(), &page(&[unknown(2, 1), revision]))
            .unwrap_err()
            .code,
        StoreErrorCode::MissingDependency
    );
    let collision = seal(
        1,
        None,
        "session_created",
        json!({
            "metadata":SessionMetadata{title:Some("different".into()),..Default::default()},"branch_root":false
        }),
    );
    assert_eq!(
        import_events(&h.paths(), &page(&[unknown(2, 1), collision]))
            .unwrap_err()
            .code,
        StoreErrorCode::HashConflict
    );
    // Then neither valid prefix was mutated.
    assert_eq!(before, h.bytes());
}
