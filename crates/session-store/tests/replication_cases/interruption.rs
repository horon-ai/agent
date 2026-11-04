use super::*;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn append_and_sync_failures_return_only_acknowledged_prefix_and_resend() {
    // Given failures at every original-commit boundary on the second record.
    for point in [
        IoPoint::LogWrite,
        IoPoint::LogBodyWritten,
        IoPoint::LogDataSync,
        IoPoint::LogFileSync,
        IoPoint::LogDirectorySync,
        IoPoint::LogPublished,
    ] {
        let h = Home::new();
        let mut hits = 0;
        let mut writer = BlockingWriter::with_hook(
            h.paths(),
            sid(),
            Box::new(move |p| {
                if p == point {
                    hits += 1;
                    if hits == 2 {
                        return Err(std::io::ErrorKind::StorageFull.into());
                    }
                }
                Ok(())
            }),
        )
        .unwrap();
        let wire = page(&[root(), unknown(2, 1), unknown(3, 2)]);
        // When a real write/sync boundary fails and the identical page is retried.
        let error = writer.import_events(&wire).unwrap_err();
        assert_eq!(error.code, StoreErrorCode::Io);
        assert_eq!(error.details.accepted_ids, vec![eid(1).as_str()]);
        let prefix = full_scan(&h.paths(), &sid()).unwrap();
        assert!(prefix.damage.is_none());
        assert!(prefix.events.len() <= 2);
        assert_eq!(prefix.events[0], root().envelope().clone());
        writer
            .import_events_resuming(&wire, error.details.retry_cursor.as_deref())
            .unwrap();
        drop(writer);
        let bytes = h.bytes();
        import_events(&h.paths(), &wire).unwrap();
        // Then all IDs occur once, even when a completed unacknowledged line existed.
        assert_eq!(h.bytes(), bytes);
        assert_eq!(full_scan(&h.paths(), &sid()).unwrap().events.len(), 3);
        println!(
            "PREFIX_RETRY {:?} accepted=1 final=3 sha256={}",
            point,
            sha256_hex(&bytes)
        );
    }
}

#[test]
fn import_child_process() {
    let Ok(root_path) = std::env::var("REPLICATION_CHILD_ROOT") else {
        return;
    };
    let mode = std::env::var("REPLICATION_CHILD_MODE").unwrap();
    let paths = SessionPaths::new(PathBuf::from(root_path).join("sessions"));
    if mode == "lock" {
        let _writer = BlockingWriter::open(paths, sid()).unwrap();
        println!("READY");
        std::io::stdout().flush().unwrap();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
    } else {
        let mut count = 0;
        let mut writer = BlockingWriter::with_hook(
            paths,
            sid(),
            Box::new(move |point| {
                if point == IoPoint::LogBodyWritten {
                    count += 1;
                    if count == 2 {
                        println!("READY");
                        std::io::stdout().flush().unwrap();
                        let mut line = String::new();
                        std::io::stdin().read_line(&mut line).unwrap();
                    }
                }
                Ok(())
            }),
        )
        .unwrap();
        writer
            .import_events(&page(&[root(), unknown(2, 1), unknown(3, 2)]))
            .unwrap();
    }
}

fn ready_child(h: &Home, mode: &str) -> std::process::Child {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "interruption::import_child_process", "--nocapture"])
        .env("REPLICATION_CHILD_ROOT", &h.0)
        .env("REPLICATION_CHILD_MODE", mode)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if line == "READY" {
                tx.send(()).unwrap();
            }
        }
    });
    if let Err(error) = rx.recv_timeout(Duration::from_secs(30)) {
        child.kill().unwrap();
        child.wait().unwrap();
        panic!("child signal: {error}");
    }
    child
}

#[test]
fn real_process_guard_returns_busy_then_releases_after_exit() {
    // Given another process owns the stable writer inode.
    let h = Home::new();
    let mut child = ready_child(&h, "lock");
    // When import is attempted before and after orderly process exit.
    assert_eq!(
        import_events(&h.paths(), &page(&[root()])).unwrap_err().code,
        StoreErrorCode::Busy
    );
    child.stdin.take().unwrap().write_all(b"finish\n").unwrap();
    assert!(child.wait().unwrap().success());
    import_events(&h.paths(), &page(&[root()])).unwrap();
    // Then only the post-release import wrote an original.
    assert_eq!(full_scan(&h.paths(), &sid()).unwrap().events.len(), 1);
}

#[test]
fn process_kill_mid_record_recovers_prefix_on_retransmission() {
    // Given a child stopped by an explicit signal after writing a partial second line.
    let h = Home::new();
    let mut child = ready_child(&h, "partial");
    // When the process is killed and the exact page is retransmitted.
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    let interrupted = full_scan(&h.paths(), &sid()).unwrap();
    assert!(interrupted.damage.is_none());
    // The kill lands between the second record's body write and its newline, but
    // whether the OS has already published the whole body is a scheduling race.
    // Both results are legal crash states: one durable record plus a torn tail, or
    // two records. Pin the state under test so the recovery path always runs.
    assert_eq!(
        interrupted.events.len() + usize::from(!interrupted.torn_tail.is_empty()),
        2
    );
    if interrupted.torn_tail.is_empty() {
        let log = h.paths().events_log(&sid()).unwrap();
        let length = fs::metadata(&log).unwrap().len();
        fs::OpenOptions::new()
            .write(true)
            .open(&log)
            .unwrap()
            .set_len(length - 1)
            .unwrap();
    }
    let interrupted = full_scan(&h.paths(), &sid()).unwrap();
    assert_eq!(interrupted.events.len(), 1);
    assert!(!interrupted.torn_tail.is_empty());
    import_events(&h.paths(), &page(&[root(), unknown(2, 1), unknown(3, 2)])).unwrap();
    // Then recovery retained the durable root and appended each missing ID once.
    let final_report = full_scan(&h.paths(), &sid()).unwrap();
    assert!(final_report.damage.is_none());
    assert!(final_report.torn_tail.is_empty());
    assert_eq!(final_report.events.len(), 3);
    assert_eq!(final_report.events[0], interrupted.events[0]);
}
