//! Durable `agent://` results: the write-through, the cold-cache fallback, the
//! redaction, and the deletion that follows the owning session.

use super::*;
use serde_json::json;
use std::ffi::OsString;
use std::sync::MutexGuard;

/// An isolated store home, restored on drop.
struct StoreSandbox {
    dir: tempfile::TempDir,
    _guard: MutexGuard<'static, ()>,
    previous: Option<OsString>,
}

impl StoreSandbox {
    fn new() -> Self {
        let guard = crate::storage::lock_test_env();
        let dir = tempfile::TempDir::new().expect("temp home");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", dir.path());
        crate::storage::create_store_dir(&dir.path().join("sessions"), false).expect("sessions dir");
        Self {
            dir,
            _guard: guard,
            previous,
        }
    }

    fn parent(&self, session_id: &str) {
        crate::session::Session::create_with_id(session_id.to_string(), None, None)
            .save()
            .expect("save the parent session");
    }

    fn stored(&self, parent_id: &str, child_id: &str, extension: &str) -> std::path::PathBuf {
        self.dir
            .path()
            .join("sessions")
            .join(parent_id)
            .join(STORED_DIR)
            .join(format!("{child_id}.{extension}"))
    }
}

impl Drop for StoreSandbox {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(previous) => crate::env::set_var("HORON_HOME", previous),
            None => crate::env::remove_var("HORON_HOME"),
        }
    }
}

#[test]
fn a_cold_cache_still_serves_the_stored_result() {
    let sandbox = StoreSandbox::new();
    sandbox.parent("parent_cold");
    publish_completed(
        "child_cold",
        "parent_cold",
        "the final answer",
        Some(json!({"status": "success", "files": ["a.rs"]})),
    )
    .expect("publish the completed result");
    assert!(
        sandbox.stored("parent_cold", "child_cold", "md").exists(),
        "the completed result must be written under the owning session"
    );

    reset_for_test();
    assert_eq!(
        read("child_cold", "parent_cold").expect("a restarted process must still read the result"),
        "the final answer"
    );
    assert_eq!(
        read("child_cold/files/0", "parent_cold").expect("the stored details keep their projection"),
        "a.rs"
    );
    assert!(
        read("child_cold", "parent_elsewhere").is_err(),
        "another session's directory must not answer for this child"
    );
}

#[test]
fn an_evicted_result_reads_back_from_the_stored_copy() {
    let sandbox = StoreSandbox::new();
    sandbox.parent("parent_evict");
    publish_completed("child_oldest", "parent_evict", "the first answer", None).expect("publish");
    for index in 0..=MAX_RESULTS {
        publish_completed(&format!("child_{index}"), "parent_evict", "later", None).expect("publish");
    }

    assert_eq!(
        read("child_oldest", "parent_evict").expect("an evicted result is still on disk"),
        "the first answer"
    );
}

#[test]
fn the_stored_copy_is_redacted() {
    let sandbox = StoreSandbox::new();
    sandbox.parent("parent_secret");
    let secret = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123";
    publish_completed(
        "child_secret",
        "parent_secret",
        &format!("the token is {secret}"),
        Some(json!({"token": secret})),
    )
    .expect("publish");

    let text = std::fs::read_to_string(sandbox.stored("parent_secret", "child_secret", "md")).expect("stored text");
    assert!(!text.contains(secret), "the stored text must not carry the secret");
    let details =
        std::fs::read_to_string(sandbox.stored("parent_secret", "child_secret", "json")).expect("stored details");
    assert!(
        !details.contains(secret),
        "the stored details must not carry the secret"
    );

    reset_for_test();
    assert!(!read("child_secret", "parent_secret").expect("read").contains(secret));
}

#[test]
fn forgetting_a_session_removes_its_stored_results() {
    let sandbox = StoreSandbox::new();
    sandbox.parent("parent_forget");
    publish_completed("child_forget", "parent_forget", "gone soon", None).expect("publish");
    let stored = sandbox.stored("parent_forget", "child_forget", "md");
    assert!(stored.exists());

    forget_stored("parent_forget");
    assert!(!stored.exists());
    reset_for_test();
    assert!(read("child_forget", "parent_forget").is_err());
}

#[test]
fn a_child_id_cannot_escape_the_result_directory() {
    assert!(!is_path_safe(""));
    assert!(!is_path_safe("."));
    assert!(!is_path_safe(".."));
    assert!(!is_path_safe("../escape"));
    assert!(!is_path_safe("nested/id"));
    assert!(is_path_safe("child-123_a.b"));
}
