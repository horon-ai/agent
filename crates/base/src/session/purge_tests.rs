//! Proof that a purge removes the whole session and only that session.

use super::{PurgeReport, is_store_session_id, purge_session_state};
use crate::storage;

/// A sandboxed `HORON_HOME`, so a test never touches the real store.
struct Sandbox {
    _dir: tempfile::TempDir,
    _guard: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
}

impl Sandbox {
    fn new() -> Self {
        let guard = storage::lock_test_env();
        let dir = tempfile::tempdir().expect("sandbox home");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", dir.path());
        Self {
            _dir: dir,
            _guard: guard,
            previous,
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(previous) => crate::env::set_var("HORON_HOME", previous),
            None => crate::env::remove_var("HORON_HOME"),
        }
    }
}

/// Every file keyed by `session_id` the purge is expected to collect.
fn seed(session_id: &str) -> Vec<std::path::PathBuf> {
    let home = storage::horon_agent_dir().expect("home");
    let mut seeded = Vec::new();
    let files = [
        format!("todos/{session_id}.json"),
        format!("todos/{session_id}-goals.json"),
        format!("todos/{session_id}-plan.json"),
        format!("todos/{session_id}-gate-observations.json"),
        format!("todos/{session_id}-review-state.json"),
        format!("goals/sessions/{session_id}.json"),
        format!("pending-soft-interrupts/{session_id}.json"),
        format!("session_leases/{session_id}.json"),
        format!("active_pids/{session_id}"),
        format!("streaming_pids/{session_id}"),
        format!("internal_pids/{session_id}"),
        format!("context-notes/{session_id}.md"),
        format!("failover-budget/{session_id}.json"),
        format!("sessions/edit-stats/{session_id}.json"),
        format!("sessions/edit-stats/{session_id}.lock"),
        format!("sessions/.catalog.dirty/{session_id}"),
        format!("managed-skills/keep.md"),
    ];
    for relative in files {
        let path = home.join(&relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(&path, b"state\n").expect("file");
        if !relative.starts_with("managed-skills") {
            seeded.push(path);
        }
    }
    let journal = home.join("durable-runtime").join(session_id);
    std::fs::create_dir_all(&journal).expect("journal dir");
    std::fs::write(journal.join("journal.jsonl"), b"{}\n").expect("journal");
    seeded.push(journal);
    seeded
}

#[test]
fn a_purge_removes_every_file_keyed_by_the_session() {
    let _sandbox = Sandbox::new();
    let session_id = "purge-exhaustive-1";
    let seeded = seed(session_id);
    assert!(!seeded.is_empty());

    let report: PurgeReport = purge_session_state(session_id).expect("purge");

    assert_eq!(
        report.removed_count(),
        seeded.len(),
        "every seeded path is reported removed: {:?}",
        report.removed
    );
    for path in &seeded {
        assert!(!path.exists(), "{} survived the purge", path.display());
    }
    assert!(
        storage::horon_agent_dir()
            .expect("home")
            .join("managed-skills/keep.md")
            .is_file(),
        "process-wide state is not a session's to delete"
    );
}

#[test]
fn a_second_purge_removes_nothing_and_succeeds() {
    let _sandbox = Sandbox::new();
    let session_id = "purge-idempotent-1";
    seed(session_id);
    purge_session_state(session_id).expect("first purge");

    let second = purge_session_state(session_id).expect("second purge");
    assert_eq!(
        second.removed_count(),
        0,
        "nothing left to remove: {:?}",
        second.removed
    );
}

#[test]
fn a_purge_leaves_another_session_alone() {
    let _sandbox = Sandbox::new();
    let doomed = "purge-target-2";
    let kept = "purge-bystander-2";
    seed(doomed);
    let survivors = seed(kept);

    purge_session_state(doomed).expect("purge");

    for path in &survivors {
        assert!(path.exists(), "{} belongs to another session", path.display());
    }
}

#[test]
fn only_a_store_id_counts_as_a_session_id() {
    // The CLI resolves a delete target with this, so it decides both what may
    // be deleted and what may reach the filesystem. It must accept the ids the
    // store writes and refuse every spelling that could name a path.
    for id in [
        "session_penguin_1794517814917_cfb64a732f642b3a",
        "purge-exhaustive-1",
        "A1",
    ] {
        assert!(is_store_session_id(id), "{id:?} is a store id");
    }
    for id in ["", "..", "a/b", ".hidden", "a b", "session/x"] {
        assert!(!is_store_session_id(id), "{id:?} must not be deletable");
    }
}

#[test]
fn an_unsafe_session_id_is_refused_before_any_path_is_built() {
    let _sandbox = Sandbox::new();
    for id in ["", "..", "a/b", ".hidden"] {
        assert!(
            purge_session_state(id).is_err(),
            "{id:?} must not reach a filesystem call"
        );
    }
}
