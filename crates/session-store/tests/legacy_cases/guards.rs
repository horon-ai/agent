use super::helpers::*;
use horon_agent_session_store::legacy::*;
use horon_agent_session_store::*;
use std::fs;

#[test]
fn corrupt_primary_and_backup_fail_without_healing() {
    // Given two corrupt candidates and a valid diagnostic pre-wipe file.
    let home = fixture();
    put(&home, "sessions/old.json", b"{broken");
    put(&home, "sessions/old.bak", b"{also broken");
    put(
        &home,
        "sessions/old.json.pre-wipe-1.bak",
        &serde_json::to_vec(&snapshot()).unwrap(),
    );
    let before = hashes(&home);
    // When migration is attempted.
    let error = source(&home).migrate(&paths(&home)).unwrap_err();
    // Then corruption is explicit and every byte is retained.
    assert_eq!(error.code, StoreErrorCode::CorruptLegacy);
    assert!(!home.join("sessions/old").exists());
    preserved(&home, &before);
}

#[test]
fn absent_primary_does_not_promote_backup_or_diagnostic_history() {
    // Given only ordinary backup, matching old read_json caller's missing-primary contract.
    let home = fixture();
    fs::rename(home.join("sessions/old.json"), home.join("sessions/old.bak")).unwrap();
    let before = hashes(&home);
    // When migration is attempted.
    let error = source(&home).migrate(&paths(&home)).unwrap_err();
    // Then missing primary is not invented from a backup.
    assert_eq!(error.code, StoreErrorCode::UnknownSession);
    preserved(&home, &before);
}

#[test]
fn live_pid_is_refused_before_source_staging() {
    // Given the current real process owns the legacy session.
    let home = fixture();
    put(&home, "active_pids/old", std::process::id().to_string().as_bytes());
    let before = hashes(&home);
    // When read and migration are requested.
    let results = [
        source(&home).read().unwrap_err(),
        source(&home).migrate(&paths(&home)).unwrap_err(),
    ];
    // Then both return Busy with no staging or source mutations.
    assert!(results.iter().all(|e| e.code == StoreErrorCode::Busy));
    assert!(
        fs::read_dir(home.join("sessions")).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with('.'))
    );
    preserved(&home, &before);
}

#[test]
fn away_or_stale_lease_is_refused_instead_of_silent_success() {
    // Given a session lease owned elsewhere or newer than this copy.
    for away in [serde_json::json!("remote"), serde_json::Value::Null] {
        let home = fixture();
        put(
            &home,
            "session_leases/old.json",
            &serde_json::to_vec(&serde_json::json!({
                "session_id":"old","epoch":3,"away_host":away,"updated_at":"old"
            }))
            .unwrap(),
        );
        let before = hashes(&home);
        // When migration is attempted.
        let error = source(&home).migrate(&paths(&home)).unwrap_err();
        // Then the lease is respected without mutating it.
        assert_eq!(error.code, StoreErrorCode::Busy);
        preserved(&home, &before);
    }
}

#[test]
fn source_change_at_each_guard_prevents_publication() {
    // Given a deterministic mutation at each observable copy/publication boundary.
    for point in [
        MigrationPoint::SourceCaptured,
        MigrationPoint::SourceCopied,
        MigrationPoint::BeforePublish,
    ] {
        let home = fixture();
        let changed = home.join("sessions/old.json");
        let raw = b"{changed by external writer}".to_vec();
        let bytes = raw.clone();
        // When an external writer changes bytes before the next guard.
        let error = source(&home)
            .migrate_with_hook(
                &paths(&home),
                Box::new(move |seen| {
                    if seen == point {
                        fs::write(&changed, &bytes)?;
                    }
                    Ok(())
                }),
            )
            .unwrap_err();
        // Then Conflict and no published directory, with the external bytes untouched.
        assert_eq!(error.code, StoreErrorCode::SourceConflict);
        assert!(!home.join("sessions/old").exists());
        assert_eq!(fs::read(home.join("sessions/old.json")).unwrap(), raw);
        println!("SOURCE_RACE {point:?} {}", home.display());
    }
}

#[test]
fn changed_raw_source_conflicts_even_when_logical_json_is_identical() {
    // Given a completed migration and whitespace-only source change.
    let home = fixture();
    source(&home).migrate(&paths(&home)).unwrap();
    let log = fs::read(home.join("sessions/old/events.jsonl")).unwrap();
    put(
        &home,
        "sessions/old.json",
        &serde_json::to_vec_pretty(&snapshot()).unwrap(),
    );
    let before = hashes(&home);
    // When repeated.
    let error = source(&home).migrate(&paths(&home)).unwrap_err();
    // Then no new origin branch or overwrite is allowed.
    assert_eq!(error.code, StoreErrorCode::SourceConflict);
    assert_eq!(fs::read(home.join("sessions/old/events.jsonl")).unwrap(), log);
    preserved(&home, &before);
}

#[cfg(not(feature = "legacy-session-index-import"))]
#[test]
fn index_requires_opt_in_without_opening_or_creating_database() {
    // Given a file that cannot be SQLite and must not be opened as a DB.
    let home = fixture();
    put(&home, "session-metadata-v1.sqlite3", b"not SQLite; must stay opaque");
    let before = hashes(&home);
    // When the default build attempts migration.
    let error = source(&home).migrate(&paths(&home)).unwrap_err();
    // Then explicit opt-in is required and no WAL/SHM appears.
    assert_eq!(error.code, StoreErrorCode::MigrationRequired);
    assert!(!home.join("session-metadata-v1.sqlite3-wal").exists());
    assert!(!home.join("session-metadata-v1.sqlite3-shm").exists());
    preserved(&home, &before);
}

#[test]
fn source_symlink_is_rejected_without_touching_target() {
    // Given a snapshot symlink to an unrelated fixture file.
    let home = fixture();
    fs::rename(home.join("sessions/old.json"), home.join("protected.json")).unwrap();
    std::os::unix::fs::symlink(home.join("protected.json"), home.join("sessions/old.json")).unwrap();
    let protected = fs::read(home.join("protected.json")).unwrap();
    // When migration is requested.
    let error = source(&home).migrate(&paths(&home)).unwrap_err();
    // Then no target writes occur.
    assert_eq!(error.code, StoreErrorCode::UnsafePath);
    assert_eq!(fs::read(home.join("protected.json")).unwrap(), protected);
}
