use super::*;

#[test]
fn whole_page_rejection_preserves_originals_when_last_record_is_invalid() {
    // Given an existing root and a valid first record.
    let h = Home::new();
    seed(&h, &[root()]);
    let before = h.bytes();
    let mut wire = page(&[unknown(2, 1), unknown(3, 2)]);
    wire.events[1].event_id = eid(42);
    // When the last record has a different outer identity.
    assert_eq!(
        import_events(&h.paths(), &wire).unwrap_err().code,
        StoreErrorCode::InvalidRecord
    );
    // Then even the first record was not appended.
    assert_eq!(before, h.bytes());
}

#[test]
fn divergence_is_retained_without_selecting_a_timestamp_winner() {
    // Given two branches with a shared root.
    let h = Home::new();
    seed(&h, &[root(), unknown(2, 1)]);
    let before = h.bytes();
    // When the other branch arrives.
    let error = import_events(&h.paths(), &page(&[unknown(3, 1)])).unwrap_err();
    // Then the conflict reports both durable tips and never replaces the prefix.
    assert_eq!(error.code, StoreErrorCode::ConflictHeads);
    assert_eq!(error.details.conflict_heads, vec![eid(2).as_str(), eid(3).as_str()]);
    assert!(h.bytes().starts_with(&before));
    assert_eq!(EventGraph::open(&h.paths(), &sid()).unwrap().heads(), &[eid(2), eid(3)]);
}

#[test]
fn busy_guard_blocks_import_without_mutating_originals() {
    // Given a writer holding the real exclusive lock.
    let h = Home::new();
    let mut writer = BlockingWriter::open(h.paths(), sid()).unwrap();
    writer.append(&root()).unwrap();
    let before = h.bytes();
    // When another importer tries to write.
    assert_eq!(
        import_events(&h.paths(), &page(&[unknown(2, 1)])).unwrap_err().code,
        StoreErrorCode::Busy
    );
    // Then the owned writer can import serially without reacquiring its lock.
    assert_eq!(before, h.bytes());
    writer.import_events(&page(&[unknown(2, 1)])).unwrap();
    assert_eq!(full_scan(&h.paths(), &sid()).unwrap().events.len(), 2);
}

#[test]
fn schema_hash_path_and_record_limits_reject_the_entire_page() {
    // Given a durable root and a valid first incoming record.
    let h = Home::new();
    seed(&h, &[root()]);
    let original = h.bytes();
    let valid = page(&[unknown(2, 1), unknown(3, 2)]);
    let mut cases = Vec::new();
    let mut bad = valid.clone();
    bad.events[1].content_hash = "0".repeat(64);
    cases.push((bad, StoreErrorCode::HashConflict));
    let mut bad = valid.clone();
    bad.events[1].canonical_json = bad.events[1]
        .canonical_json
        .replace("\"schema_version\":1", "\"schema_version\":2");
    cases.push((bad, StoreErrorCode::UnsupportedVersion));
    let mut bad = valid.clone();
    bad.events[1].canonical_json.insert(0, ' ');
    cases.push((bad, StoreErrorCode::InvalidRecord));
    let mut bad = valid.clone();
    bad.session_id = serde_json::from_value(json!("../escape")).unwrap();
    cases.push((bad, StoreErrorCode::UnsafeSessionId));
    let mut bad = valid.clone();
    bad.events = vec![valid.events[0].clone(); 257];
    cases.push((bad, StoreErrorCode::RecordTooLarge));
    let mut bad = valid.clone();
    bad.protocol_version = 99;
    cases.push((bad, StoreErrorCode::UnsupportedVersion));
    // When each malformed page is passed through the public boundary.
    for (wire, code) in cases {
        assert_eq!(import_events(&h.paths(), &wire).unwrap_err().code, code);
        // Then no valid first record leaks through prevalidation.
        assert_eq!(h.bytes(), original);
    }
}

#[test]
fn prevalidation_does_not_recover_a_torn_tail_for_an_invalid_page() {
    // Given a complete root and an interrupted tail.
    use std::io::Write;
    let h = Home::new();
    seed(&h, &[root()]);
    let log = h.paths().events_log(&sid()).unwrap();
    let mut file = fs::OpenOptions::new().append(true).open(log).unwrap();
    file.write_all(b"{\"interrupted\":").unwrap();
    file.sync_all().unwrap();
    let before = h.bytes();
    // When the page contains a missing dependency.
    assert_eq!(
        import_events(&h.paths(), &page(&[unknown(2, 99)])).unwrap_err().code,
        StoreErrorCode::MissingDependency
    );
    // Then acquiring the import guard did not quarantine or truncate existing bytes.
    assert_eq!(h.bytes(), before);
}

#[test]
fn prevalidation_preserves_valid_durable_prefix_when_later_compaction_is_missing() {
    // Given a fresh destination with no event log.
    let h = Home::new();
    let blocked = seal(
        3,
        Some(2),
        "compaction_blocked",
        json!({
            "source_head_id":eid(99),"source_digest":"a".repeat(64),
            "provider":"openai","model":"test","route":"api","settings_digest":"a".repeat(64),
            "generation":0,"reason":"provider_error"
        }),
    );
    // When a known invalid reference follows an opaque event.
    assert_eq!(
        import_events(&h.paths(), &page(&[root(), unknown(2, 1), blocked]))
            .unwrap_err()
            .code,
        StoreErrorCode::MissingDependency
    );
    // Then the entire page was rejected, not just its last record.
    assert!(h.bytes().is_empty());
}
