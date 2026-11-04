use super::helpers::*;
use horon_agent_session_store::legacy::*;
use horon_agent_session_store::*;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn every_publication_sync_error_refuses_ack_and_can_resume() {
    // Given real filesystem I/O with one failed sync boundary per fixture.
    for point in [
        MigrationPoint::OriginalsSync,
        MigrationPoint::ManifestSync,
        MigrationPoint::StageDirectorySync,
        MigrationPoint::ParentSync,
        MigrationPoint::ParentSynced,
        MigrationPoint::ReceiptSync,
        MigrationPoint::ReceiptDirectorySync,
    ] {
        let home = fixture();
        let before = hashes(&home);
        // When the selected boundary reports ENOSPC.
        let error = source(&home)
            .migrate_with_hook(
                &paths(&home),
                Box::new(move |seen| {
                    if seen == point {
                        return Err(std::io::Error::from_raw_os_error(28));
                    }
                    Ok(())
                }),
            )
            .unwrap_err();
        // Then no successful report, originals unchanged, and explicit retry completes.
        assert_eq!(error.code, StoreErrorCode::Io);
        preserved(&home, &before);
        let resumed = source(&home).migrate(&paths(&home)).unwrap();
        assert_eq!(
            read_receipt(&paths(&home), &SessionId::parse("old").unwrap()).unwrap(),
            resumed.receipt
        );
        assert_eq!(view(&home).messages().len(), 3);
        println!("SYNC_FAILURE {point:?} no-ACK resumed {}", home.display());
    }
}

#[test]
fn migration_child() {
    let Ok(home) = std::env::var("LEGACY_CHILD_HOME") else {
        return;
    };
    let point = std::env::var("LEGACY_CHILD_POINT").unwrap();
    let home = std::path::PathBuf::from(home);
    source(&home)
        .migrate_with_hook(
            &paths(&home),
            Box::new(move |seen| {
                if format!("{seen:?}") == point {
                    println!("MIGRATION_READY");
                    std::io::stdout().flush()?;
                    let mut byte = [0];
                    std::io::stdin().read_exact(&mut byte)?;
                }
                Ok(())
            }),
        )
        .unwrap();
    println!("MIGRATION_ACK");
}

fn interrupt(home: &std::path::Path, point: MigrationPoint) {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "interruptions::migration_child", "--nocapture"])
        .env("LEGACY_CHILD_HOME", home)
        .env("LEGACY_CHILD_POINT", format!("{point:?}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    // Register the exact ready signal before the child can unblock; no timing sleep.
    let (send, receive) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut transcript = String::new();
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if line == "MIGRATION_READY" {
                send.send(()).unwrap();
            }
            transcript.push_str(&line);
            transcript.push('\n');
        }
        transcript
    });
    let ready = receive.recv_timeout(Duration::from_secs(30));
    child.kill().unwrap();
    let result = child.wait_with_output().unwrap();
    let transcript = reader.join().unwrap();
    put(home, &format!("process-{point:?}.stdout"), transcript.as_bytes());
    put(home, &format!("process-{point:?}.stderr"), &result.stderr);
    assert!(ready.is_ok(), "child readiness: {ready:?}; {transcript}");
    assert!(!transcript.contains("MIGRATION_ACK"));
    assert!(!result.status.success());
    println!("PROCESS_KILLED {point:?} {} {}", home.display(), result.status);
}

#[test]
fn interrupted_staging_and_renames_never_count_as_completed_migration() {
    // Given independent real child processes interrupted at durable publication boundaries.
    for point in [
        MigrationPoint::SourceCopied,
        MigrationPoint::EventsStaged,
        MigrationPoint::Renamed,
        MigrationPoint::ParentSync,
        MigrationPoint::ParentSynced,
    ] {
        let home = fixture();
        let original = fs::read(home.join("sessions/old.json")).unwrap();
        // When killed after its explicit ready signal.
        interrupt(&home, point);
        // Then no receipt authorizes runtime until a complete retry.
        assert!(read_receipt(&paths(&home), &SessionId::parse("old").unwrap()).is_err());
        source(&home).migrate(&paths(&home)).unwrap();
        assert_eq!(view(&home).messages().len(), 3);
        assert_eq!(fs::read(home.join("sessions/old.json")).unwrap(), original);
    }
}

#[test]
fn repeated_interruptions_resume_one_deterministic_history() {
    // Given the same home across repeated interrupted attempts.
    let home = fixture();
    let original = fs::read(home.join("sessions/old.json")).unwrap();
    // When each attempt reaches a later observable boundary.
    for point in [
        MigrationPoint::SourceCopied,
        MigrationPoint::BeforePublish,
        MigrationPoint::Renamed,
        MigrationPoint::ParentSync,
        MigrationPoint::ParentSynced,
    ] {
        interrupt(&home, point);
    }
    let report = source(&home).migrate(&paths(&home)).unwrap();
    // Then only one original event set exists and raw source remains identical.
    assert_eq!(report.receipt.events.len(), 5);
    assert_eq!(
        full_scan(&paths(&home), &SessionId::parse("old").unwrap())
            .unwrap()
            .events
            .len(),
        5
    );
    assert_eq!(fs::read(home.join("sessions/old.json")).unwrap(), original);
}
