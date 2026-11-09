use super::*;
use std::io::{BufRead, Write};
use std::process::{Command, Stdio};

#[test]
fn session_replication_process_entry() {
    let Ok(root) = std::env::var("TASK9_CHILD_HOME") else {
        return;
    };
    crate::env::set_var("HORON_HOME", root);
    let session = seed("replica_process");
    let clone = session.clone();
    println!("READY_LOCK_HELD");
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    assert!(std::io::stdin().read_line(&mut line).unwrap() > 0);
    drop(session);
    println!("ONE_ARC_LEFT");
    std::io::stdout().flush().unwrap();
    line.clear();
    assert!(std::io::stdin().read_line(&mut line).unwrap() > 0);
    drop(clone);
    println!("RELEASED");
    std::io::stdout().flush().unwrap();
}

#[test]
fn session_replication_process_lock_lasts_until_last_arc_and_reopens() {
    // Given an independently scheduled process holding the actual writer lock.
    let home = Home::new();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "server::session_replication::tests::processes::session_replication_process_entry",
            "--nocapture",
        ])
        .env("TASK9_CHILD_HOME", home.directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    println!("TASK9_CHILD_PID {}", child.id());
    let mut input = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let signal = |expected: &str| {
        loop {
            if rx.recv_timeout(Duration::from_secs(20)).unwrap() == expected {
                break;
            }
        }
    };
    signal("READY_LOCK_HELD");
    let session = Session::load("replica_process").unwrap();
    let wire = successor(&session, "parent import");
    assert_eq!(
        store::replication::import_events(&home.paths(), &wire)
            .unwrap_err()
            .code,
        store::StoreErrorCode::Busy
    );
    // When one reference is dropped the process still owns the lock.
    input.write_all(b"drop one\n").unwrap();
    signal("ONE_ARC_LEFT");
    assert_eq!(
        store::replication::import_events(&home.paths(), &wire)
            .unwrap_err()
            .code,
        store::StoreErrorCode::Busy
    );
    input.write_all(b"release\n").unwrap();
    signal("RELEASED");
    assert!(child.wait().unwrap().success());
    reader.join().unwrap();
    // Then the original actor has drained and an independent import can acquire it.
    store::replication::import_events(&home.paths(), &wire).unwrap();
    assert_eq!(
        Session::load("replica_process")
            .unwrap()
            .history_messages()
            .unwrap()
            .len(),
        2
    );
}
