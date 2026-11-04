use horon_agent_session_store::*;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
// allow: SIZE_OK - the assigned scope permits one integration file; its process
// entrypoint and crash-handshake tests must share this self-executing test binary.

mod support;

fn fixture() -> (SessionPaths, SessionId) {
    (
        SessionPaths::new(support::temp_root("writer")),
        SessionId::parse("durable").unwrap(),
    )
}
fn event(parent: Option<EventId>, size: usize) -> SealedEvent {
    let kind = if parent.is_none() {
        KnownEventKind::SessionCreated
    } else {
        KnownEventKind::AuxiliaryRecorded
    };
    let payload = if parent.is_none() {
        serde_json::json!(SessionCreatedPayload {
            branch_root: false,
            reset_provenance: None,
            metadata: SessionMetadata::default()
        })
    } else {
        serde_json::json!({"aux_kind":"legacy","schema_version":1,"value":"x".repeat(size)})
    };
    EventDraft::new(
        SessionId::parse("durable").unwrap(),
        EventId::new_v4(),
        parent,
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::new(kind),
        payload,
    )
    .seal()
    .unwrap()
}
fn fail_once(point: IoPoint) -> IoHook {
    let mut armed = true;
    Box::new(move |seen| {
        if armed && seen == point {
            armed = false;
            return Err(std::io::Error::from_raw_os_error(28));
        }
        Ok(())
    })
}
fn seeded() -> (SessionPaths, SessionId, SealedEvent) {
    let (paths, id) = fixture();
    let first = event(None, 0);
    BlockingWriter::open(paths.clone(), id.clone())
        .unwrap()
        .append(&first)
        .unwrap();
    (paths, id, first)
}

#[test]
fn acknowledged_record_survives_reopen() {
    // Given a reserved ID; when acknowledged; then exactly that record survives.
    let (paths, id, first) = seeded();
    assert_eq!(full_scan(&paths, &id).unwrap().events, vec![first.envelope().clone()]);
    assert!(paths.root().join(".catalog.dirty/durable").is_file());
}
#[test]
fn lifetime_lock_is_stable_and_exclusive() {
    // Given a live owner; when a second owner opens; then it is busy without replacing the inode.
    let (paths, id) = fixture();
    let writer = BlockingWriter::open(paths.clone(), id.clone()).unwrap();
    let lock = paths.session_dir(&id).unwrap().join(".writer.lock");
    let inode = fs::metadata(&lock).unwrap().ino();
    assert_eq!(
        BlockingWriter::open(paths.clone(), id.clone()).err().unwrap().code,
        StoreErrorCode::Busy
    );
    drop(writer);
    let _again = BlockingWriter::open(paths, id).unwrap();
    assert_eq!(fs::metadata(lock).unwrap().ino(), inode);
}
#[test]
fn retry_is_idempotent_and_conflicting_identity_is_rejected() {
    // Given committed bytes; when same ID is retried or changed; then no duplicate or overwrite occurs.
    let (paths, id, first) = seeded();
    let before = fs::read(paths.events_log(&id).unwrap()).unwrap();
    let mut writer = BlockingWriter::open(paths.clone(), id.clone()).unwrap();
    assert_eq!(writer.append(&first).unwrap(), AppendOutcome::AlreadyDurable);
    let mut other = first.envelope().clone();
    other.timestamp = chrono::DateTime::from_timestamp(1, 0).unwrap();
    other.content_hash = other.content_hash_of().unwrap();
    assert_eq!(
        writer.append(&SealedEvent::Inline(other)).unwrap_err().code,
        StoreErrorCode::HashConflict
    );
    assert_eq!(fs::read(paths.events_log(&id).unwrap()).unwrap(), before);
}
#[test]
fn injected_commit_failures_never_ack_and_retry_same_bytes() {
    // Given actual filesystem operations; when each durability stage fails; then retry commits once.
    for point in [
        IoPoint::DirtyFileSync,
        IoPoint::DirtyDirectorySync,
        IoPoint::LogWrite,
        IoPoint::LogBodyWritten,
        IoPoint::LogDataSync,
        IoPoint::LogFileSync,
        IoPoint::LogDirectorySync,
        IoPoint::LogPublished,
    ] {
        let (paths, id) = fixture();
        let first = event(None, 0);
        let mut writer = BlockingWriter::with_hook(paths.clone(), id.clone(), fail_once(point)).unwrap();
        assert!(writer.append(&first).is_err(), "{point:?} falsely acknowledged");
        writer.append(&first).unwrap();
        assert_eq!(
            fs::read(paths.events_log(&id).unwrap()).unwrap(),
            encode_record_line(first.envelope()).unwrap()
        );
        println!(
            "RECEIPT {point:?} error/no-ACK -> same-id retry exactly once root={} canonical={}",
            paths.root().display(),
            sha256_hex(&encode_record_line(first.envelope()).unwrap())
        );
    }
}
#[test]
fn oversized_blob_is_published_before_reference() {
    // Given an oversized payload; when published; then blob hash/size and syscall order agree.
    let (paths, id, first) = seeded();
    let oversized = event(Some(first.envelope().event_id.clone()), MAX_RECORD_BYTES);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let capture = seen.clone();
    let mut writer = BlockingWriter::with_hook(
        paths.clone(),
        id.clone(),
        Box::new(move |p| {
            capture.lock().unwrap().push(p);
            Ok(())
        }),
    )
    .unwrap();
    writer.append(&oversized).unwrap();
    let reference = oversized.envelope().payload_ref.as_ref().unwrap();
    let bytes = fs::read(paths.blob_file(&id, &reference.sha256).unwrap()).unwrap();
    assert_eq!(sha256_hex(&bytes), reference.sha256);
    assert_eq!(u64::try_from(bytes.len()).unwrap(), reference.size);
    let trace = seen.lock().unwrap();
    let position = |point| trace.iter().position(|p| *p == point).unwrap();
    let ordered = [
        IoPoint::DirtyFileSync,
        IoPoint::DirtyDirectorySync,
        IoPoint::BlobWrite,
        IoPoint::BlobFileSync,
        IoPoint::BlobVerified,
        IoPoint::BlobRenamed,
        IoPoint::BlobDirectorySync,
        IoPoint::BlobPublished,
        IoPoint::LogWrite,
        IoPoint::LogBodyWritten,
        IoPoint::LogDataSync,
        IoPoint::LogFileSync,
        IoPoint::LogDirectorySync,
        IoPoint::LogPublished,
    ];
    assert!(ordered.windows(2).all(|pair| position(pair[0]) < position(pair[1])));
    println!("BLOB {} {} ORDER {trace:?}", reference.sha256, reference.size);
}
#[test]
fn blob_failure_does_not_publish_a_reference() {
    // Given a valid prefix; when blob publication fails; then the prefix remains unchanged.
    for point in [
        IoPoint::BlobWrite,
        IoPoint::BlobFileSync,
        IoPoint::BlobVerified,
        IoPoint::BlobRenamed,
        IoPoint::BlobDirectorySync,
        IoPoint::BlobPublished,
    ] {
        let (paths, id, first) = seeded();
        let next = event(Some(first.envelope().event_id.clone()), MAX_RECORD_BYTES);
        let before = fs::read(paths.events_log(&id).unwrap()).unwrap();
        let mut writer = BlockingWriter::with_hook(paths.clone(), id.clone(), fail_once(point)).unwrap();
        assert!(writer.append(&next).is_err(), "{point:?}");
        assert_eq!(fs::read(paths.events_log(&id).unwrap()).unwrap(), before);
        writer.append(&next).unwrap();
        assert_eq!(full_scan(&paths, &id).unwrap().events.len(), 2);
        println!(
            "BLOB_FAILURE {point:?} no-ACK prefix={} root={}",
            sha256_hex(&before),
            paths.root().display()
        );
    }
}
#[test]
fn torn_tail_is_quarantined_before_truncation() {
    // Given a complete prefix and raw incomplete bytes; when recovered; then both are preserved.
    let (paths, id, _) = seeded();
    let log = paths.events_log(&id).unwrap();
    let prefix = fs::read(&log).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .unwrap()
        .write_all(b"torn-tail")
        .unwrap();
    assert_eq!(full_scan(&paths, &id).unwrap().events.len(), 1);
    for point in [
        IoPoint::QuarantineFileSync,
        IoPoint::QuarantineDirectorySync,
        IoPoint::QuarantinePublished,
    ] {
        assert!(BlockingWriter::with_hook(paths.clone(), id.clone(), fail_once(point)).is_err());
        assert_eq!(fs::read(&log).unwrap(), [prefix.as_slice(), b"torn-tail"].concat());
        println!(
            "QUARANTINE_FAILURE {point:?} original={} root={}",
            sha256_hex(&fs::read(&log).unwrap()),
            paths.root().display()
        );
    }
    drop(BlockingWriter::open(paths.clone(), id.clone()).unwrap());
    assert_eq!(fs::read(log).unwrap(), prefix);
    println!(
        "QUARANTINE_RECOVERED prefix={} root={}",
        sha256_hex(&prefix),
        paths.root().display()
    );
    let copies: Vec<_> = fs::read_dir(paths.session_dir(&id).unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(".quarantine-"))
        .collect();
    assert!(!copies.is_empty());
    for path in copies {
        assert_eq!(fs::read(path).unwrap(), b"torn-tail");
    }
}
#[test]
fn middle_damage_hash_and_missing_parent_are_read_only() {
    // Given damaged complete records; when opened; then no bytes are skipped or rewritten.
    for corruption in 0..3 {
        let (paths, id, first) = seeded();
        let next = event(Some(EventId::new_v4()), 1);
        let mut bytes = encode_record_line(first.envelope()).unwrap();
        match corruption {
            0 => bytes.extend_from_slice(b"not-json\n"),
            1 => {
                bytes[20] = if bytes[20] == b'a' { b'b' } else { b'a' };
            }
            2 => bytes.extend_from_slice(&encode_record_line(next.envelope()).unwrap()),
            _ => unreachable!(),
        }
        let log = paths.events_log(&id).unwrap();
        fs::write(&log, &bytes).unwrap();
        let report = full_scan(&paths, &id).unwrap();
        assert!(report.damage.is_some());
        assert!(BlockingWriter::open(paths.clone(), id.clone()).is_err());
        assert_eq!(fs::read(log).unwrap(), bytes);
    }
}
#[test]
fn unsafe_file_paths_and_permissions_are_rejected() {
    // Given an owned session; when a log is replaced by a symlink; then its target is untouched.
    let (paths, id, _) = seeded();
    let log = paths.events_log(&id).unwrap();
    let target = paths.root().join("untouched");
    fs::rename(&log, &target).unwrap();
    symlink(&target, &log).unwrap();
    let before = fs::read(&target).unwrap();
    assert!(BlockingWriter::open(paths.clone(), id.clone()).is_err());
    assert_eq!(fs::read(target).unwrap(), before);
    assert_eq!(
        fs::metadata(paths.session_dir(&id).unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}
#[test]
fn process_entry() {
    // This entry executes real writes both standalone and as an independently killed process.
    let Ok(root) = std::env::var("WRITER_CHILD_ROOT") else {
        acknowledged_record_survives_reopen();
        return;
    };
    let paths = SessionPaths::new(root);
    let id = SessionId::parse("durable").unwrap();
    let stop = std::env::var("WRITER_CHILD_POINT").unwrap();
    let mut writer = BlockingWriter::with_hook(
        paths.clone(),
        id.clone(),
        Box::new(move |point| {
            if format!("{point:?}") == stop {
                println!("STOP {point:?}");
                std::io::stdout().flush()?;
                let mut line = String::new();
                if std::io::stdin().read_line(&mut line)? == 0 {
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
            }
            Ok(())
        }),
    )
    .unwrap();
    println!("LOCKED");
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    assert!(std::io::stdin().read_line(&mut line).unwrap() > 0);
    let scan = full_scan(&paths, &id).unwrap();
    let parent = scan.events.first().map(|e| e.event_id.clone());
    let mut reserved = event(parent, MAX_RECORD_BYTES);
    match &mut reserved {
        SealedEvent::Inline(envelope) | SealedEvent::Blob { envelope, .. } => {
            envelope.event_id = EventId::parse("22222222-2222-4222-8222-222222222222").unwrap();
            envelope.origin_id = OriginId::parse("33333333-3333-4333-8333-333333333333").unwrap();
            envelope.content_hash = envelope.content_hash_of().unwrap();
        }
    }
    writer.append(&reserved).unwrap();
    println!("ACK");
    std::io::stdout().flush().unwrap();
    std::io::stdin().read_line(&mut line).unwrap();
}
#[test]
fn process_death_releases_lock_and_preserves_commit_boundaries() {
    // Given pipe-synchronized independent processes; when killed at publication boundaries; then reopening is safe.
    let (repeated_paths, repeated_id, _) = seeded();
    for (point, repeated) in [
        ("LOCKED", false),
        ("BlobRenamed", false),
        ("BlobDirectorySync", false),
        ("BlobPublished", false),
        ("LogCreated", false),
        ("LogBodyWritten", false),
        ("LogDirectorySync", false),
        ("LogPublished", false),
        ("ACK", false),
        ("LogBodyWritten", true),
        ("LogDirectorySync", true),
        ("ACK", true),
    ] {
        let (paths, id) = if repeated {
            (repeated_paths.clone(), repeated_id.clone())
        } else if point == "LogCreated" {
            let (paths, id) = fixture();
            drop(BlockingWriter::open(paths.clone(), id.clone()).unwrap());
            (paths, id)
        } else {
            let (paths, id, _) = seeded();
            (paths, id)
        };
        let lock = paths.session_dir(&id).unwrap().join(".writer.lock");
        let inode = fs::metadata(&lock).unwrap().ino();
        let before = fs::read(paths.events_log(&id).unwrap()).unwrap_or_default();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "process_entry", "--nocapture"])
            .env("WRITER_CHILD_ROOT", paths.root())
            .env("WRITER_CHILD_POINT", point)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        // Child::wait closes a stdin handle left inside Child. Keep the gate
        // open until wait confirms death, so EOF cannot resume the paused write.
        let mut input = child.stdin.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        loop {
            if rx.recv_timeout(Duration::from_secs(30)).unwrap() == "LOCKED" {
                break;
            }
        }
        assert_eq!(
            BlockingWriter::open(paths.clone(), id.clone()).err().unwrap().code,
            StoreErrorCode::Busy
        );
        if point != "LOCKED" {
            input.write_all(b"go\n").unwrap();
            loop {
                let line = rx.recv_timeout(Duration::from_secs(90)).unwrap();
                assert!(line != "ACK" || point == "ACK", "did not hit {point}");
                if line == format!("STOP {point}") || (line == "ACK" && point == "ACK") {
                    break;
                }
            }
        }
        child.kill().unwrap();
        child.wait().unwrap();
        drop(input);
        reader.join().unwrap();
        drop(BlockingWriter::open(paths.clone(), id.clone()).unwrap());
        let after = fs::read(paths.events_log(&id).unwrap()).unwrap();
        assert!(after.starts_with(&before));
        if point == "LogBodyWritten" || point == "LogCreated" {
            assert_eq!(after, before, "a killed incomplete record cannot become complete");
        }
        assert_eq!(fs::metadata(lock).unwrap().ino(), inode);
        let report = full_scan(&paths, &id).unwrap();
        assert!(report.damage.is_none());
        if point == "ACK" || point == "LogPublished" {
            assert_eq!(report.events.len(), 2);
        }
        println!(
            "KILL {point} repeated={repeated} root={} prefix={} after={} ack={}",
            paths.root().display(),
            sha256_hex(&before),
            sha256_hex(&after),
            point == "ACK"
        );
    }
}

#[test]
fn recovery_to_new_root_preserves_damaged_source() {
    // Given middle damage after a blob; when explicitly salvaged; then the source stays byte-identical.
    let (paths, id, first) = seeded();
    let next = event(Some(first.envelope().event_id.clone()), MAX_RECORD_BYTES);
    BlockingWriter::open(paths.clone(), id.clone())
        .unwrap()
        .append(&next)
        .unwrap();
    let log = paths.events_log(&id).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .unwrap()
        .write_all(b"damaged\n")
        .unwrap();
    let before = fs::read(&log).unwrap();
    let destination = SessionPaths::new(paths.root().join("salvage"));
    let report = recover_to(&paths, &id, &destination).unwrap();
    assert!(report.damage.is_some());
    assert_eq!(full_scan(&destination, &id).unwrap().events.len(), 2);
    assert_eq!(fs::read(log).unwrap(), before);
    assert!(recover_to(&paths, &id, &destination).is_err());
}

#[test]
fn dedicated_worker_survives_response_cancellation() {
    // Given a queued event; when its reply is dropped; then shutdown still persists the original.
    let (paths, id) = fixture();
    let first = event(None, 0);
    let (writer, ready, stopped) = WriterHandle::spawn(paths.clone(), id.clone()).unwrap();
    ready.recv_timeout(Duration::from_secs(30)).unwrap().unwrap();
    drop(writer.append(first.clone()).unwrap());
    drop(writer);
    stopped.recv_timeout(Duration::from_secs(30)).unwrap();
    assert_eq!(full_scan(&paths, &id).unwrap().events, vec![first.envelope().clone()]);
    drop(BlockingWriter::open(paths, id).unwrap());
}

#[test]
fn unsupported_sync_and_replaced_lock_fail_closed() {
    // Given unsupported durability or an externally replaced inode; when appending; then no ACK occurs.
    let (paths, id) = fixture();
    let mut writer = BlockingWriter::with_hook(
        paths.clone(),
        id.clone(),
        Box::new(|p| {
            if p == IoPoint::LogDirectorySync {
                return Err(std::io::ErrorKind::Unsupported.into());
            }
            Ok(())
        }),
    )
    .unwrap();
    let first = event(None, 0);
    assert_eq!(
        writer.append(&first).unwrap_err().code,
        StoreErrorCode::DurabilityUnsupported
    );
    let lock = paths.session_dir(&id).unwrap().join(".writer.lock");
    fs::rename(&lock, lock.with_extension("old")).unwrap();
    fs::write(&lock, b"replacement").unwrap();
    assert!(writer.append(&first).is_err());
}

#[test]
fn unknown_kind_remains_inert_and_missing_blob_is_damage() {
    // Given an unknown future kind with valid raw bytes; when stored; then preservation does not execute it.
    let (paths, id, first) = seeded();
    let mut next = event(Some(first.envelope().event_id.clone()), MAX_RECORD_BYTES);
    if let SealedEvent::Blob { envelope, .. } = &mut next {
        envelope.kind = EventKind::parse("Future-Command");
        envelope.content_hash = envelope.content_hash_of().unwrap();
    }
    BlockingWriter::open(paths.clone(), id.clone())
        .unwrap()
        .append(&next)
        .unwrap();
    let reference = next.envelope().payload_ref.as_ref().unwrap();
    let blob = paths.blob_file(&id, &reference.sha256).unwrap();
    fs::rename(&blob, blob.with_extension("preserved")).unwrap();
    assert!(full_scan(&paths, &id).unwrap().damage.is_some());
    assert!(BlockingWriter::open(paths, id).is_err());
}

#[test]
fn uncertain_id_rejects_different_hash_without_losing_retry() {
    // Given a complete unacknowledged write; when its ID is reused; then conflict preserves retry.
    let (paths, id) = fixture();
    let first = event(None, 0);
    let mut writer = BlockingWriter::with_hook(paths.clone(), id.clone(), fail_once(IoPoint::LogDataSync)).unwrap();
    assert!(writer.append(&first).is_err());
    let mut changed = first.envelope().clone();
    changed.timestamp = chrono::DateTime::from_timestamp(2, 0).unwrap();
    changed.content_hash = changed.content_hash_of().unwrap();
    assert_eq!(
        writer.append(&SealedEvent::Inline(changed)).unwrap_err().code,
        StoreErrorCode::HashConflict
    );
    assert_eq!(writer.append(&first).unwrap(), AppendOutcome::AlreadyDurable);
    assert_eq!(full_scan(&paths, &id).unwrap().events.len(), 1);
}

#[test]
fn hierarchy_creation_sync_failure_is_retryable_without_ack() {
    // Given missing nested directories; when their parent sync fails; then retry creates private storage.
    let (base, id) = fixture();
    let paths = SessionPaths::new(base.root().join("home/sessions"));
    assert!(BlockingWriter::with_hook(paths.clone(), id.clone(), fail_once(IoPoint::DirectorySync)).is_err());
    let first = event(None, 0);
    BlockingWriter::open(paths.clone(), id.clone())
        .unwrap()
        .append(&first)
        .unwrap();
    let mut pending = vec![base.root().to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path).unwrap();
        assert_eq!(
            metadata.permissions().mode() & 0o777,
            if metadata.is_dir() { 0o700 } else { 0o600 }
        );
        if metadata.is_dir() {
            pending.extend(fs::read_dir(path).unwrap().map(|entry| entry.unwrap().path()));
        }
    }
}

#[test]
fn same_id_requires_current_blob_verification() {
    // Given a previously acknowledged blob; when its file changes; then an idempotent retry cannot ACK.
    let (paths, id, first) = seeded();
    let next = event(Some(first.envelope().event_id.clone()), MAX_RECORD_BYTES);
    let mut writer = BlockingWriter::open(paths.clone(), id.clone()).unwrap();
    writer.append(&next).unwrap();
    let path = paths
        .blob_file(&id, &next.envelope().payload_ref.as_ref().unwrap().sha256)
        .unwrap();
    fs::write(path, b"{}").unwrap();
    assert!(writer.append(&next).is_err());
    assert!(full_scan(&paths, &id).unwrap().damage.is_some());
}

#[test]
fn future_schema_and_oversized_complete_record_are_read_only() {
    // Given unsupported or oversized complete source lines; when inspected; then original bytes remain intact.
    for oversized in [false, true] {
        let (paths, id, first) = seeded();
        let log = paths.events_log(&id).unwrap();
        let bytes = if oversized {
            [vec![b'x'; MAX_RECORD_BYTES + 1], vec![b'\n']].concat()
        } else {
            let mut value: serde_json::Value =
                serde_json::from_slice(&encode_record_line(first.envelope()).unwrap()).unwrap();
            value["schema_version"] = serde_json::json!(2);
            format!("{}\n", canonical_json(&value).unwrap()).into_bytes()
        };
        fs::write(&log, &bytes).unwrap();
        assert!(full_scan(&paths, &id).unwrap().damage.is_some());
        assert!(BlockingWriter::open(paths.clone(), id.clone()).is_err());
        assert_eq!(fs::read(log).unwrap(), bytes);
    }
}
