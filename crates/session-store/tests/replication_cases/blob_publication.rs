use super::*;
use horon_agent_session_store::blob_exchange::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

#[test]
fn final_publish_interruptions_never_ack_and_identical_resend_cleans_staging() {
    // Given a one-chunk original and failures after final verification.
    for failure in [IoPoint::BlobRenamed, IoPoint::BlobDirectorySync, IoPoint::BlobPublished] {
        let h = Home::new();
        let mut verified = false;
        let mut failed = false;
        let mut writer = BlockingWriter::with_hook(
            h.paths(),
            sid(),
            Box::new(move |point| {
                if point == IoPoint::BlobVerified {
                    verified = true;
                }
                if verified && !failed && point == failure {
                    failed = true;
                    return Err(std::io::ErrorKind::StorageFull.into());
                }
                Ok(())
            }),
        )
        .unwrap();
        let chunk = SessionBlobChunk {
            sha256: sha256_hex(b"{}"),
            offset: 0,
            data_base64: "e30=".into(),
            next_offset: 2,
            total_size: 2,
            complete: true,
        };
        // When publication is interrupted and the same bytes arrive after reopening.
        assert_eq!(writer.import_blob(&chunk).unwrap_err().code, StoreErrorCode::Io);
        drop(writer);
        assert!(import_blob(&h.paths(), &sid(), &chunk).unwrap().complete);
        // Then the original is owner-private, hash-exact, and no transfer chunks remain.
        let original = h.paths().blob_file(&sid(), &chunk.sha256).unwrap();
        assert_eq!(fs::read(&original).unwrap(), b"{}");
        let meta = fs::metadata(original).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o600);
        assert_eq!(meta.uid(), fs::metadata(&h.0).unwrap().uid());
        assert!(
            !h.paths()
                .session_dir(&sid())
                .unwrap()
                .join(".blob-staging")
                .join(&chunk.sha256)
                .exists()
        );
        let mut wrong = chunk.clone();
        wrong.data_base64 = "W10=".into();
        assert_eq!(
            import_blob(&h.paths(), &sid(), &wrong).unwrap_err().code,
            StoreErrorCode::Conflict
        );
    }
}

#[test]
fn permissive_staging_directory_is_rejected_without_data_change() {
    // Given a shared-readable staging directory that this store never creates.
    let h = Home::new();
    seed(&h, &[root()]);
    let directory = h.paths().session_dir(&sid()).unwrap().join(".blob-staging");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
    let before = h.bytes();
    let chunk = SessionBlobChunk {
        sha256: sha256_hex(b"{}"),
        offset: 0,
        data_base64: "e30=".into(),
        next_offset: 2,
        total_size: 2,
        complete: true,
    };
    // When a transfer attempts to use the unsafe directory.
    assert_eq!(
        import_blob(&h.paths(), &sid(), &chunk).unwrap_err().code,
        StoreErrorCode::UnsafePath
    );
    // Then nothing has been published and original event bytes are untouched.
    assert_eq!(h.bytes(), before);
    assert_eq!(fs::read_dir(directory).unwrap().count(), 0);
}
