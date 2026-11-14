use super::*;
use crate::auth::test_sandbox::AuthTestSandbox;
use crate::server::state::{EmitOutcome, PendingFailoverEvent, emit_session_event};

fn switch_event(chain_id: &str, from: &str, to: &str, attempt: u32) -> ServerEvent {
    ServerEvent::FailoverSwitched {
        chain_id: chain_id.to_string(),
        from: from.to_string(),
        to: to.to_string(),
        reason: "429 Too Many Requests".to_string(),
        attempt,
        max: 3,
    }
}

fn stored_switches(sandbox: &AuthTestSandbox) -> Vec<PendingFailoverEvent> {
    let path = sandbox.root().join("failover-events.json");
    if !path.exists() {
        return Vec::new();
    }
    crate::storage::read_json::<Vec<PendingFailoverEvent>>(&path).expect("the announcement file is valid JSON")
}

/// A second ack for the same key finds nothing left to clear. That is still
/// success: the entry is already gone and retrying cannot help, so the
/// client must not be pushed into an ack loop over it.
#[test]
fn a_duplicate_ack_still_succeeds_and_removes_nothing() {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    let session_id = "sess-ack-duplicate";
    assert_eq!(
        emit_session_event(session_id, switch_event("chain-1", "model-a", "model-b", 1)),
        EmitOutcome::NoSink
    );

    let (tx, mut rx) = mpsc::unbounded_channel();
    for id in [500, 501] {
        handle_failover_ack(
            id,
            session_id,
            "chain-1".to_string(),
            "model-a".to_string(),
            "model-b".to_string(),
            1,
            &tx,
        );
        assert!(
            matches!(rx.try_recv(), Ok(ServerEvent::Ack { id: replied }) if replied == id),
            "the first ack clears the entry and the duplicate ack is not an error"
        );
    }

    assert!(
        stored_switches(&sandbox).is_empty(),
        "the acknowledged entry is gone and the duplicate changed nothing"
    );
}

/// The lock is held for the whole retry budget, so the clear cannot run. The
/// handler must say so and the entry must survive; the lock is taken by this
/// process on a separate file descriptor, which is exactly what the
/// cross-process lock sees, so the contention is not a timing race.
#[test]
fn a_held_announcement_lock_is_an_error_not_an_ack() {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    let session_id = "sess-ack-lock-held";
    assert_eq!(
        emit_session_event(session_id, switch_event("chain-1", "model-a", "model-b", 1)),
        EmitOutcome::NoSink
    );
    assert_eq!(
        stored_switches(&sandbox).len(),
        1,
        "precondition: one pending announcement"
    );

    let lock_path = sandbox.root().join("failover-events.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open the announcement lock");
    lock.try_lock().expect("this test holds the announcement lock");

    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_failover_ack(
        400,
        session_id,
        "chain-1".to_string(),
        "model-a".to_string(),
        "model-b".to_string(),
        1,
        &tx,
    );

    assert!(
        matches!(rx.try_recv(), Ok(ServerEvent::Error { id: 400, .. })),
        "a clear that never ran must be reported as an error, not an ack"
    );
    assert_eq!(
        stored_switches(&sandbox).len(),
        1,
        "the entry a client was told was cleared is still on disk"
    );

    lock.unlock().expect("release the announcement lock");
}

/// The rewrite cannot land because the storage directory is read-only. The
/// entry survives, so the reply must be an error.
#[test]
#[cfg(unix)]
fn a_failed_announcement_rewrite_is_an_error_not_an_ack() {
    use std::os::unix::fs::PermissionsExt;

    struct RestorePermissions(std::path::PathBuf);
    impl Drop for RestorePermissions {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }

    let sandbox = AuthTestSandbox::new().expect("isolated home");
    let session_id = "sess-ack-read-only";
    assert_eq!(
        emit_session_event(session_id, switch_event("chain-1", "model-a", "model-b", 1)),
        EmitOutcome::NoSink
    );
    assert_eq!(
        stored_switches(&sandbox).len(),
        1,
        "precondition: one pending announcement"
    );

    let _restore = RestorePermissions(sandbox.root().to_path_buf());
    std::fs::set_permissions(sandbox.root(), std::fs::Permissions::from_mode(0o500))
        .expect("make the storage directory read-only");

    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_failover_ack(
        600,
        session_id,
        "chain-1".to_string(),
        "model-a".to_string(),
        "model-b".to_string(),
        1,
        &tx,
    );

    assert!(
        matches!(rx.try_recv(), Ok(ServerEvent::Error { id: 600, .. })),
        "a rewrite that failed must be reported as an error, not an ack"
    );
    assert_eq!(
        stored_switches(&sandbox).len(),
        1,
        "the entry a client was told was cleared is still on disk"
    );
}
