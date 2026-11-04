use super::*;
use horon_agent_session_store::blob_exchange::*;

#[test]
fn blobs_resume_durably_before_referenced_events_are_accepted() {
    // Given an externalized payload and two homes.
    let a = Home::new();
    let b = Home::new();
    let large = seal(2, Some(1), "Future-Event", json!({"body":"x".repeat(9*1024*1024)}));
    let reference = large.envelope().payload_ref.clone().unwrap();
    seed(&a, &[root(), large.clone()]);
    let wire = page(&[root(), large]);
    assert_eq!(
        import_events(&b.paths(), &wire).unwrap_err().code,
        StoreErrorCode::MissingDependency
    );
    assert!(b.bytes().is_empty());
    // When chunks are sent, a new call resumes from disk and retransmits the first chunk.
    let first = export_blob(&a.paths(), &sid(), &reference, 0).unwrap();
    assert_eq!(first.next_offset, 1024 * 1024);
    import_blob(&b.paths(), &sid(), &first).unwrap();
    import_blob(&b.paths(), &sid(), &first).unwrap();
    let mut offset = blob_status(&b.paths(), &sid(), &reference).unwrap().received;
    while offset < reference.size {
        let chunk = export_blob(&a.paths(), &sid(), &reference, offset).unwrap();
        offset = import_blob(&b.paths(), &sid(), &chunk).unwrap().received;
    }
    // Then complete SHA/size verification permits event publication with identical originals.
    assert!(blob_status(&b.paths(), &sid(), &reference).unwrap().complete);
    import_events(&b.paths(), &wire).unwrap();
    assert_eq!(a.bytes(), b.bytes());
    assert_eq!(
        fs::read(a.paths().blob_file(&sid(), &reference.sha256).unwrap()).unwrap(),
        fs::read(b.paths().blob_file(&sid(), &reference.sha256).unwrap()).unwrap()
    );
}

fn small_blob() -> SealedEvent {
    EventDraft::new(
        sid(),
        eid(2),
        Some(eid(1)),
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::parse("Future-Event"),
        json!({"bytes":"x".repeat(2*1024*1024+20)}),
    )
    .seal_with_limit(1024)
    .unwrap()
}

#[test]
fn missing_offsets_conflicting_bytes_and_sizes_never_advance_status() {
    // Given a three-chunk original.
    let a = Home::new();
    let b = Home::new();
    let original = small_blob();
    let r = original.envelope().payload_ref.clone().unwrap();
    seed(&a, &[root(), original]);
    let first = export_blob(&a.paths(), &sid(), &r, 0).unwrap();
    let second = export_blob(&a.paths(), &sid(), &r, BLOB_CHUNK_BYTES).unwrap();
    // When chunks skip an offset, change bytes or change the fixed total size.
    assert_eq!(
        import_blob(&b.paths(), &sid(), &second).unwrap_err().code,
        StoreErrorCode::MissingOffset
    );
    assert_eq!(blob_status(&b.paths(), &sid(), &r).unwrap().received, 0);
    import_blob(&b.paths(), &sid(), &first).unwrap();
    let mut changed = first.clone();
    changed.data_base64.replace_range(0..1, "A");
    assert_eq!(
        import_blob(&b.paths(), &sid(), &changed).unwrap_err().code,
        StoreErrorCode::Conflict
    );
    changed = first.clone();
    changed.total_size += 1;
    assert_eq!(
        import_blob(&b.paths(), &sid(), &changed).unwrap_err().code,
        StoreErrorCode::Conflict
    );
    // Then the durable prefix remains exactly one chunk.
    assert_eq!(blob_status(&b.paths(), &sid(), &r).unwrap().received, BLOB_CHUNK_BYTES);
    assert!(!b.paths().blob_file(&sid(), &r.sha256).unwrap().exists());
}

#[test]
fn final_hash_mismatch_and_oversize_chunks_cannot_publish() {
    // Given one short complete chunk falsely claiming another hash.
    let b = Home::new();
    let chunk = SessionBlobChunk {
        sha256: sha256_hex(b"y"),
        offset: 0,
        data_base64: "eA==".into(),
        next_offset: 1,
        total_size: 1,
        complete: true,
    };
    // When final verification fails or a decoded chunk exceeds one MiB.
    assert_eq!(
        import_blob(&b.paths(), &sid(), &chunk).unwrap_err().code,
        StoreErrorCode::PayloadDigestMismatch
    );
    assert!(!b.paths().blob_file(&sid(), &chunk.sha256).unwrap().exists());
    let mut oversized = chunk;
    oversized.data_base64 = "AAAA".repeat(400_000);
    assert_eq!(
        import_blob(&b.paths(), &sid(), &oversized).unwrap_err().code,
        StoreErrorCode::InvalidRecord
    );
    // Then no event or original blob is falsely acknowledged.
    assert!(b.bytes().is_empty());
}

#[test]
fn symlink_staging_and_blob_destinations_never_escape_the_owned_session() {
    // Given an outside sentinel and a symlink planted at a transfer path.
    use std::os::unix::fs::symlink;
    let h = Home::new();
    seed(&h, &[root()]);
    let outside = h.0.join("sentinel");
    fs::write(&outside, b"untouched").unwrap();
    let chunk = SessionBlobChunk {
        sha256: sha256_hex(b"x"),
        offset: 0,
        data_base64: "eA==".into(),
        next_offset: 1,
        total_size: 1,
        complete: true,
    };
    let stage = h.paths().session_dir(&sid()).unwrap().join(".blob-staging");
    symlink(&h.0, &stage).unwrap();
    // When the staging path escapes, then the original destination escapes.
    assert_eq!(
        import_blob(&h.paths(), &sid(), &chunk).unwrap_err().code,
        StoreErrorCode::UnsafePath
    );
    fs::remove_file(&stage).unwrap();
    let blobs = h.paths().blobs_dir(&sid()).unwrap();
    fs::DirBuilder::new().mode(0o700).create(&blobs).unwrap();
    symlink(&outside, h.paths().blob_file(&sid(), &chunk.sha256).unwrap()).unwrap();
    assert!(import_blob(&h.paths(), &sid(), &chunk).is_err());
    // Then external data is unchanged.
    assert_eq!(fs::read(outside).unwrap(), b"untouched");
}

#[test]
fn chunk_sync_interruption_resumes_from_published_offsets_only() {
    // Given a real file-sync failure before a staged chunk can be published.
    let a = Home::new();
    let b = Home::new();
    let original = small_blob();
    let r = original.envelope().payload_ref.clone().unwrap();
    seed(&a, &[root(), original]);
    let chunk = export_blob(&a.paths(), &sid(), &r, 0).unwrap();
    let mut fired = false;
    let mut writer = BlockingWriter::with_hook(
        b.paths(),
        sid(),
        Box::new(move |p| {
            if !fired && p == IoPoint::BlobFileSync {
                fired = true;
                return Err(std::io::ErrorKind::StorageFull.into());
            }
            Ok(())
        }),
    )
    .unwrap();
    // When the write fails, the writer closes, and a new writer resumes.
    assert_eq!(writer.import_blob(&chunk).unwrap_err().code, StoreErrorCode::Io);
    drop(writer);
    assert_eq!(blob_status(&b.paths(), &sid(), &r).unwrap().received, 0);
    import_blob(&b.paths(), &sid(), &chunk).unwrap();
    // Then only a fully synced renamed chunk contributes to progress.
    assert_eq!(blob_status(&b.paths(), &sid(), &r).unwrap().received, BLOB_CHUNK_BYTES);
}
