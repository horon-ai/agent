use super::*;
use horon_agent_session_store::record_exchange::*;

fn mixed() -> (Home, SessionEventPage, CanonicalRecordRef) {
    let a = Home::new();
    let events = [root(), unknown(2, 1), unknown(3, 2)];
    seed(&a, &events);
    let reference = CanonicalRecordRef::from_record(&events[1].envelope().canonical_record().unwrap(), 1).unwrap();
    let mut wire = page(&events);
    wire.events.remove(1);
    wire.record_refs.push(reference.clone());
    (a, wire, reference)
}

#[test]
fn mixed_page_positions_rehydrate_exact_order_and_roles_are_separate() {
    let (a, wire, r) = mixed();
    let b = Home::new();
    let chunk = export_record_blob(&a.paths(), &sid(), &r, 0).unwrap();
    // A payload blob with identical bytes is NOT a published record-transfer blob.
    horon_agent_session_store::blob_exchange::import_blob(&b.paths(), &sid(), &chunk.chunk).unwrap();
    assert_eq!(
        import_events(&b.paths(), &wire).unwrap_err().code,
        StoreErrorCode::MissingDependency
    );
    assert!(b.bytes().is_empty());
    import_record_blob(&b.paths(), &sid(), &chunk).unwrap();
    let restored = wire.resolve_records(&b.paths()).unwrap();
    assert_eq!(
        restored.iter().map(|e| e.event_id.clone()).collect::<Vec<_>>(),
        vec![eid(1), eid(2), eid(3)]
    );
    import_events(&b.paths(), &wire).unwrap();
    assert_eq!(a.bytes(), b.bytes());
}

#[test]
fn malformed_reference_positions_and_identities_reject_whole_page() {
    let (a, wire, r) = mixed();
    let b = Home::new();
    import_record_blob(
        &b.paths(),
        &sid(),
        &export_record_blob(&a.paths(), &sid(), &r, 0).unwrap(),
    )
    .unwrap();
    let mut variants = Vec::new();
    let mut bad = wire.clone();
    bad.record_refs[0].position = 3;
    variants.push(bad);
    let mut bad = wire.clone();
    bad.record_refs.push(CanonicalRecordRef {
        position: 0,
        event_id: eid(99),
        ..r.clone()
    });
    variants.push(bad);
    let mut bad = wire.clone();
    bad.record_refs.push(r.clone());
    variants.push(bad);
    let mut bad = wire.clone();
    bad.record_refs[0].position = 0;
    variants.push(bad);
    let mut bad = wire.clone();
    bad.events.push(unknown(2, 1).envelope().canonical_record().unwrap());
    variants.push(bad);
    let mut bad = wire.clone();
    bad.record_refs[0].sha256 = "../unsafe".into();
    variants.push(bad);
    let mut bad = wire.clone();
    bad.record_refs[0].size += 1;
    variants.push(bad);
    let mut bad = wire.clone();
    bad.record_refs[0].content_hash = "0".repeat(64);
    variants.push(bad);
    let mut bad = wire.clone();
    bad.record_refs[0].event_id = eid(99);
    variants.push(bad);
    for bad in variants {
        let error = import_events(&b.paths(), &bad).unwrap_err();
        assert!(error.details.accepted_ids.is_empty());
        assert!(b.bytes().is_empty());
    }
    let mut conflicting = wire.clone();
    let mut other = unknown(2, 1).envelope().canonical_record().unwrap();
    other.content_hash = "0".repeat(64);
    conflicting.events.push(other);
    assert_eq!(
        import_events(&b.paths(), &conflicting).unwrap_err().code,
        StoreErrorCode::HashConflict
    );
    assert!(b.bytes().is_empty());
    import_events(&b.paths(), &wire).unwrap();
    assert_eq!(a.bytes(), b.bytes());
}

#[test]
fn missing_or_corrupt_record_blob_never_appends_an_inline_prefix() {
    let (a, wire, r) = mixed();
    let b = Home::new();
    assert_eq!(
        import_events(&b.paths(), &wire).unwrap_err().code,
        StoreErrorCode::MissingDependency
    );
    assert!(b.bytes().is_empty());
    import_record_blob(
        &b.paths(),
        &sid(),
        &export_record_blob(&a.paths(), &sid(), &r, 0).unwrap(),
    )
    .unwrap();
    let record_path = b
        .paths()
        .session_dir(&sid())
        .unwrap()
        .join(".record-transfer")
        .join(sid().as_str())
        .join("blobs")
        .join(format!("{}.json", r.sha256));
    let mut damaged = fs::read(&record_path).unwrap();
    damaged[0] ^= 1;
    fs::write(record_path, damaged).unwrap();
    assert_eq!(
        import_events(&b.paths(), &wire).unwrap_err().code,
        StoreErrorCode::PayloadDigestMismatch
    );
    assert!(b.bytes().is_empty());
}

#[test]
fn total_count_and_resolved_size_are_bounded_before_reference_allocation() {
    let (_, wire, r) = mixed();
    let b = Home::new();
    let mut count = wire.clone();
    count.events = vec![root().envelope().canonical_record().unwrap(); 256];
    assert_eq!(
        import_events(&b.paths(), &count).unwrap_err().code,
        StoreErrorCode::RecordTooLarge
    );
    let mut oversized = wire.clone();
    oversized.events.clear();
    oversized.record_refs = (0..3)
        .map(|position| CanonicalRecordRef {
            position,
            event_id: eid(10 + position as u128),
            size: 8_388_607,
            ..r.clone()
        })
        .collect();
    assert!(serde_json::to_vec(&oversized).unwrap().len() < MAX_WIRE_BYTES);
    assert_eq!(
        import_events(&b.paths(), &oversized).unwrap_err().code,
        StoreErrorCode::RecordTooLarge
    );
    oversized.record_refs[0].size = u64::MAX;
    assert_eq!(
        import_events(&b.paths(), &oversized).unwrap_err().code,
        StoreErrorCode::RecordTooLarge
    );
    assert!(!b.paths().root().exists());
}

#[test]
fn record_chunk_descriptor_and_busy_guard_are_checked() {
    let (a, _, r) = mixed();
    let b = Home::new();
    let mut chunk = export_record_blob(&a.paths(), &sid(), &r, 0).unwrap();
    chunk.chunk.sha256 = "0".repeat(64);
    assert_eq!(
        import_record_blob(&b.paths(), &sid(), &chunk).unwrap_err().code,
        StoreErrorCode::InvalidRecord
    );
    let writer = BlockingWriter::open(b.paths(), sid()).unwrap();
    let chunk = export_record_blob(&a.paths(), &sid(), &r, 0).unwrap();
    assert_eq!(
        import_record_blob(&b.paths(), &sid(), &chunk).unwrap_err().code,
        StoreErrorCode::Busy
    );
    drop(writer);
    assert!(import_record_blob(&b.paths(), &sid(), &chunk).unwrap().status.complete);
    let mut forged = serde_json::to_value(&r).unwrap();
    forged["encoding"] = json!("payload");
    assert!(serde_json::from_value::<CanonicalRecordRef>(forged).is_err());
}

#[test]
fn record_transfer_namespace_refuses_a_symlink_escape() {
    let (a, wire, r) = mixed();
    let b = Home::new();
    seed(&b, &[root()]);
    let before = b.bytes();
    let outside = b.0.join("outside");
    fs::DirBuilder::new().mode(0o700).create(&outside).unwrap();
    let path = b.paths().session_dir(&sid()).unwrap().join(".record-transfer");
    std::os::unix::fs::symlink(&outside, &path).unwrap();
    let chunk = export_record_blob(&a.paths(), &sid(), &r, 0).unwrap();
    assert_eq!(
        import_record_blob(&b.paths(), &sid(), &chunk).unwrap_err().code,
        StoreErrorCode::UnsafePath
    );
    assert_eq!(
        import_events(&b.paths(), &wire).unwrap_err().code,
        StoreErrorCode::UnsafePath
    );
    assert_eq!(b.bytes(), before);
    assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
}
