use super::*;

struct TempHome {
    _dir: tempfile::TempDir,
    _guard: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
}

impl TempHome {
    fn new() -> Self {
        let guard = crate::storage::lock_test_env();
        let dir = tempfile::tempdir().expect("temp home");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", dir.path());
        Self {
            _dir: dir,
            _guard: guard,
            previous,
        }
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
    }
}

#[test]
fn save_allocates_increasing_ids_and_path_resolves_them() {
    let _home = TempHome::new();
    let first = save("session-a", "bash", "first output").expect("save first");
    let second = save("session-a", "bash", "second output").expect("save second");
    assert_eq!(first, 0);
    assert_eq!(second, 1);

    let first_path = path("session-a", first).expect("first path");
    assert_eq!(
        std::fs::read_to_string(first_path).expect("read artifact"),
        "first output"
    );
    assert!(path("session-a", 99).is_err());
}

#[test]
fn ids_continue_after_a_restart() {
    let _home = TempHome::new();
    assert_eq!(save("session-b", "read", "one").expect("save"), 0);
    assert_eq!(save("session-b", "read", "two").expect("save"), 1);
    // A fresh process would only see the files; the allocation is derived from
    // them rather than held in memory.
    assert_eq!(save("session-b", "read", "three").expect("save"), 2);
}

#[test]
fn session_ids_outside_the_safe_alphabet_are_refused() {
    let _home = TempHome::new();
    assert!(save("../escape", "bash", "text").is_err());
    assert!(path("", 0).is_err());
}

#[test]
fn spill_returns_short_text_unchanged() {
    let _home = TempHome::new();
    assert_eq!(spill("session-c", "bash", "short", 100), "short");
}

#[test]
fn spill_keeps_head_and_tail_and_points_at_the_artifact() {
    let _home = TempHome::new();
    let text = (1..=5000).map(|n| format!("line-{n}")).collect::<Vec<_>>().join("\n");
    let spilled = spill("session-c", "bash", &text, 400);
    assert!(spilled.len() < text.len());
    assert!(spilled.contains("elided…]"), "{spilled}");
    assert!(spilled.contains("line-1"), "head survives");
    assert!(spilled.contains("line-5000"), "tail survives");
    assert!(spilled.contains("[raw output: artifact://0]"), "{spilled}");

    let saved = std::fs::read_to_string(path("session-c", 0).expect("artifact path")).expect("read");
    assert_eq!(saved, text);
}

#[test]
fn oversized_artifacts_keep_the_first_and_last_half() {
    let _home = TempHome::new();
    let text = format!("{}\n{}", "a".repeat(9 * 1024 * 1024), "b".repeat(9 * 1024 * 1024));
    let id = save("session-d", "bash", &text).expect("save oversized");
    let saved = std::fs::read_to_string(path("session-d", id).expect("path")).expect("read");
    assert!(saved.len() < text.len());
    assert!(saved.contains("ARTIFACT TRUNCATED"));
    assert!(saved.starts_with("aaaa"));
    assert!(saved.ends_with("bbbb"));
    assert!(saved.contains("elided from the middle"));
}

#[test]
fn spill_reports_a_failed_save_without_losing_the_clipped_text() {
    let _home = TempHome::new();
    // An invalid session id makes the save fail; the caller still gets the
    // clipped output so the model can act on it.
    let text = (1..=5000).map(|n| format!("line-{n}")).collect::<Vec<_>>().join("\n");
    let spilled = spill("../bad", "bash", &text, 400);
    assert!(spilled.contains("Full output was not saved"), "{spilled}");
    assert!(spilled.contains("line-1"));
    assert!(spilled.contains("line-5000"));
}
