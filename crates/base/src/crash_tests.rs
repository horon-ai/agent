use super::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

fn with_crash_bundles<F, T>(enabled: bool, f: F) -> T
where
    F: FnOnce(&Path) -> T,
{
    let _guard = crate::storage::lock_test_env();
    let previous = std::env::var_os("HORON_AGENT_HOME");
    let temp = tempfile::TempDir::new().expect("create temp agent home");
    crate::env::set_var("HORON_AGENT_HOME", temp.path());
    install_panic_hook_with_enabled(enabled);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(temp.path())));

    BUNDLES_ENABLED.store(false, Ordering::SeqCst);
    match previous {
        Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
        None => crate::env::remove_var("HORON_AGENT_HOME"),
    }
    result.unwrap_or_else(|payload| std::panic::resume_unwind(payload))
}

fn panic_on_thread(message: &'static str) {
    std::thread::Builder::new()
        .name("crash-bundle-test".to_string())
        .spawn(move || {
            let _ = std::panic::catch_unwind(|| -> () { panic!("{message}") });
        })
        .expect("spawn panicking thread")
        .join()
        .expect("panicking thread must not abort the test");
}

fn bundle_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root.join("crashes"))
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    dirs
}

fn dirs_containing(root: &Path, needle: &str) -> Vec<PathBuf> {
    bundle_dirs(root)
        .into_iter()
        .filter(|dir| {
            std::fs::read_to_string(dir.join("bundle.json"))
                .map(|text| text.contains(needle))
                .unwrap_or(false)
        })
        .collect()
}

fn only_bundle_for(root: &Path, needle: &str) -> PathBuf {
    let dirs = dirs_containing(root, needle);
    assert_eq!(dirs.len(), 1, "expected exactly one bundle for {needle:?}");
    dirs[0].clone()
}

#[test]
fn crash_writes_redacted_bundle() {
    with_crash_bundles(true, |root| {
        panic_on_thread("crash bundle test panic marker 8f2a1c");

        let dir = only_bundle_for(root, "crash bundle test panic marker 8f2a1c");
        assert!(dir.join("bundle.json").is_file(), "bundle.json must exist");
        assert!(dir.join("report.txt").is_file(), "report.txt must exist");

        let json = std::fs::read_to_string(dir.join("bundle.json")).expect("read bundle.json");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("bundle.json must be JSON");
        assert_eq!(parsed["panic"]["message"], "crash bundle test panic marker 8f2a1c");
        assert_eq!(parsed["panic"]["thread"], "crash-bundle-test");
        let location = parsed["panic"]["location"].as_str().expect("panic location recorded");
        assert!(
            location.contains("crash_tests.rs"),
            "location must name this file: {location}"
        );
        assert_eq!(parsed["pid"], std::process::id());

        let report = std::fs::read_to_string(dir.join("report.txt")).expect("read report.txt");
        assert!(report.contains("crash bundle test panic marker 8f2a1c"));
        assert!(report.contains("thread: crash-bundle-test"));
        assert!(report.contains("location: "));
        assert!(report.contains("nothing was transmitted"));
    });
}

#[test]
fn crash_redacts_token_shapes() {
    with_crash_bundles(true, |root| {
        let fake_key = "ghp_FAKEKEYFAKEKEYFAKEKEYFAKEKEY12";
        record_tool_operation(
            "bash",
            &serde_json::json!({ "command": format!("curl -H 'Authorization: {fake_key}'") }),
        );
        panic_on_thread("redaction panic marker 4d1e");

        let dir = only_bundle_for(root, "redaction panic marker 4d1e");
        let json = std::fs::read_to_string(dir.join("bundle.json")).expect("read bundle.json");
        let report = std::fs::read_to_string(dir.join("report.txt")).expect("read report.txt");

        assert!(!json.contains(fake_key), "bundle.json leaked the token: {json}");
        assert!(!report.contains(fake_key), "report.txt leaked the token: {report}");
        assert!(json.contains("[REDACTED_SECRET]"), "bundle.json must show redaction");
        assert!(report.contains("[REDACTED_SECRET]"), "report.txt must show redaction");

        let parsed: serde_json::Value = serde_json::from_str(&json).expect("bundle.json must be JSON");
        let operations = parsed["operations"].as_array().expect("operations recorded");
        let bash_operation = operations
            .iter()
            .find(|operation| {
                operation["tool"] == "bash" && operation["detail"].as_str().unwrap_or("").contains("curl")
            })
            .expect("the seeded bash operation must be in the bundle");
        assert_eq!(
            bash_operation["detail"],
            "{\"command\":\"curl -H 'Authorization: [REDACTED_SECRET]'\"}"
        );
    });
}

#[test]
fn crash_bundles_disabled_writes_nothing() {
    with_crash_bundles(false, |root| {
        record_tool_operation("bash", &serde_json::json!({ "command": "echo disabled" }));
        panic_on_thread("disabled panic marker 9c3f");

        let crashes = root.join("crashes");
        let entries: Vec<_> = std::fs::read_dir(&crashes)
            .map(|entries| entries.filter_map(|entry| entry.ok()).collect())
            .unwrap_or_default();
        assert!(
            entries.is_empty(),
            "disabled crash bundles must write nothing: {entries:?}"
        );
    });
}

#[test]
fn crash_bundles_do_not_overwrite_each_other() {
    with_crash_bundles(true, |root| {
        panic_on_thread("first bundle marker 1a2b");
        panic_on_thread("second bundle marker 3c4d");

        let first = only_bundle_for(root, "first bundle marker 1a2b");
        let second = only_bundle_for(root, "second bundle marker 3c4d");

        assert_ne!(first, second, "two panics must land in distinct bundle directories");
        for dir in [&first, &second] {
            assert!(dir.join("bundle.json").is_file());
            assert!(dir.join("report.txt").is_file());
        }
    });
}
