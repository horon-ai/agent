//! The per-workspace overlay cache: unchanged workspaces read nothing, every
//! kind of change is visible on the next call, and only changed files are read.
//!
//! The cache and the read counters are process-wide, so every test here holds
//! `counter_lock()` and starts from an empty cache.

use super::overlay_cache;
use super::test_support;
use std::path::Path;
use std::sync::Arc;

fn write_skill(root: &Path, scope: &str, name: &str, description: &str) {
    let dir = root.join(scope).join("skills").join(name);
    std::fs::create_dir_all(&dir).expect("create skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n\nBody of {name}.\n"),
    )
    .expect("write skill");
}

/// Serialize with the other counter tests and start from a cold cache.
fn cold_cache() -> std::sync::MutexGuard<'static, ()> {
    let guard = test_support::counter_lock();
    overlay_cache::clear_for_tests();
    guard
}

fn description(overlay: &super::SkillRegistry, name: &str) -> Option<String> {
    overlay.get(name).map(|skill| skill.description.clone())
}

#[test]
fn unchanged_requests_read_no_skill_files() {
    let _guard = cold_cache();
    let workspace = tempfile::tempdir().expect("tempdir");
    for index in 0..50 {
        write_skill(
            workspace.path(),
            ".horon-agent",
            &format!("skill-{index:02}"),
            "Unchanged.",
        );
    }

    let warmup_before = test_support::file_reads();
    let first = overlay_cache::overlay(Some(workspace.path()));
    assert_eq!(first.list().len(), 50);
    assert_eq!(
        test_support::file_reads() - warmup_before,
        50,
        "the cold request reads every SKILL.md"
    );
    let scans_before = test_support::dir_scans();
    let reads_before = test_support::file_reads();
    let counters_before = horon_agent_perf_counters::snapshot();
    for _ in 0..9 {
        let again = overlay_cache::overlay(Some(workspace.path()));
        // The same `Arc` means no rebuild ran, so no SKILL.md was opened.
        assert!(
            Arc::ptr_eq(&first, &again),
            "an unchanged workspace must not be rebuilt"
        );
    }
    let scans = test_support::dir_scans() - scans_before;
    let reads = test_support::file_reads() - reads_before;
    let counters_after = horon_agent_perf_counters::snapshot();
    let file_reads = counters_after.skill_file_reads - counters_before.skill_file_reads;
    let dir_reads = counters_after.skill_dir_reads - counters_before.skill_dir_reads;
    println!(
        "unchanged requests: 9, this thread: directory scans {scans}, SKILL.md reads {reads}; \
         process-wide (exact only with --test-threads=1): skill_file_reads {file_reads}, \
         skill_dir_reads {dir_reads}, counters enabled {}",
        counters_after.enabled
    );
    assert_eq!(scans, 0, "an unchanged workspace is checked with stat only");
    assert_eq!(reads, 0, "no SKILL.md is read for an unchanged workspace");
}

#[test]
fn edit_add_delete_relink_visible_next_request() {
    let _guard = cold_cache();
    let workspace = tempfile::tempdir().expect("tempdir");
    write_skill(workspace.path(), ".horon-agent", "alpha", "First alpha.");
    write_skill(workspace.path(), ".horon-agent", "beta", "Steady beta.");
    let first = overlay_cache::overlay(Some(workspace.path()));
    assert_eq!(description(&first, "alpha").as_deref(), Some("First alpha."));

    write_skill(workspace.path(), ".horon-agent", "alpha", "Edited alpha.");
    let edited = overlay_cache::overlay(Some(workspace.path()));
    assert_eq!(description(&edited, "alpha").as_deref(), Some("Edited alpha."));
    // The untouched skill is taken from the previous build, not read again: its
    // body cache is the very same allocation.
    let beta_before = &first.get("beta").expect("beta").body;
    let beta_after = &edited.get("beta").expect("beta").body;
    assert!(Arc::ptr_eq(beta_before, beta_after), "an unchanged SKILL.md is reused");
    let alpha_before = &first.get("alpha").expect("alpha").body;
    let alpha_after = &edited.get("alpha").expect("alpha").body;
    assert!(
        !Arc::ptr_eq(alpha_before, alpha_after),
        "the edited SKILL.md is read again"
    );

    write_skill(workspace.path(), ".horon-agent", "gamma", "New gamma.");
    let added = overlay_cache::overlay(Some(workspace.path()));
    assert_eq!(description(&added, "gamma").as_deref(), Some("New gamma."));

    std::fs::remove_dir_all(workspace.path().join(".horon-agent/skills/beta")).expect("delete beta");
    let deleted = overlay_cache::overlay(Some(workspace.path()));
    assert!(
        deleted.get("beta").is_none(),
        "a deleted skill disappears on the next call"
    );
    assert!(deleted.get("alpha").is_some() && deleted.get("gamma").is_some());
}

#[test]
fn a_source_directory_created_later_is_seen() {
    let _guard = cold_cache();
    let workspace = tempfile::tempdir().expect("tempdir");
    let empty = overlay_cache::overlay(Some(workspace.path()));
    assert!(empty.list().is_empty());

    write_skill(workspace.path(), ".claude", "late", "Arrived later.");
    let later = overlay_cache::overlay(Some(workspace.path()));
    assert_eq!(description(&later, "late").as_deref(), Some("Arrived later."));
}

#[test]
fn skill_md_created_in_an_existing_directory_is_seen() {
    let _guard = cold_cache();
    let workspace = tempfile::tempdir().expect("tempdir");
    let dir = workspace.path().join(".agents/skills/pending");
    std::fs::create_dir_all(&dir).expect("create dir");
    let before = overlay_cache::overlay(Some(workspace.path()));
    assert!(before.get("pending").is_none());

    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: pending\ndescription: Now here.\n---\n\nBody.\n",
    )
    .expect("write skill");
    let after = overlay_cache::overlay(Some(workspace.path()));
    assert_eq!(description(&after, "pending").as_deref(), Some("Now here."));
}

#[cfg(unix)]
#[test]
fn same_size_restored_mtime_edit_detected() {
    let _guard = cold_cache();
    let workspace = tempfile::tempdir().expect("tempdir");
    write_skill(workspace.path(), ".horon-agent", "steady", "aaaa");
    let skill_md = workspace.path().join(".horon-agent/skills/steady/SKILL.md");
    let first = overlay_cache::overlay(Some(workspace.path()));
    assert_eq!(description(&first, "steady").as_deref(), Some("aaaa"));

    let modified = std::fs::metadata(&skill_md)
        .expect("metadata")
        .modified()
        .expect("mtime");
    write_skill(workspace.path(), ".horon-agent", "steady", "bbbb");
    std::fs::File::options()
        .write(true)
        .open(&skill_md)
        .and_then(|file| file.set_modified(modified))
        .expect("restore mtime");

    let second = overlay_cache::overlay(Some(workspace.path()));
    assert_eq!(
        description(&second, "steady").as_deref(),
        Some("bbbb"),
        "a same-size rewrite with a restored mtime is caught by the ctime"
    );
}

#[cfg(unix)]
#[test]
fn same_size_same_mtime_relink_detected() {
    let _guard = cold_cache();
    let workspace = tempfile::tempdir().expect("tempdir");
    let targets = tempfile::tempdir().expect("tempdir");
    for (name, text) in [("one", "1111"), ("two", "2222")] {
        let dir = targets.path().join(name);
        std::fs::create_dir_all(&dir).expect("target dir");
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: linked\ndescription: {text}\n---\n\nBody.\n"),
        )
        .expect("write target");
    }
    let skills = workspace.path().join(".horon-agent/skills");
    std::fs::create_dir_all(&skills).expect("skills dir");
    let link = skills.join("linked");
    std::os::unix::fs::symlink(targets.path().join("one"), &link).expect("link");
    let first = overlay_cache::overlay(Some(workspace.path()));
    assert_eq!(description(&first, "linked").as_deref(), Some("1111"));

    std::fs::remove_file(&link).expect("unlink");
    std::os::unix::fs::symlink(targets.path().join("two"), &link).expect("relink");
    let second = overlay_cache::overlay(Some(workspace.path()));
    assert_eq!(description(&second, "linked").as_deref(), Some("2222"));
}

#[test]
fn concurrent_revalidation_is_consistent() {
    let _guard = cold_cache();
    let workspace = tempfile::tempdir().expect("tempdir");
    for index in 0..20 {
        write_skill(
            workspace.path(),
            ".horon-agent",
            &format!("shared-{index:02}"),
            "Shared.",
        );
    }
    overlay_cache::overlay(Some(workspace.path()));
    write_skill(workspace.path(), ".horon-agent", "shared-00", "Changed under readers.");

    let path = workspace.path();
    let listings: Vec<Vec<(String, String)>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(move || {
                    let overlay = overlay_cache::overlay(Some(path));
                    let mut skills: Vec<(String, String)> = overlay
                        .list()
                        .into_iter()
                        .map(|skill| (skill.name.clone(), skill.description.clone()))
                        .collect();
                    skills.sort();
                    skills
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("thread"))
            .collect()
    });
    assert!(listings.windows(2).all(|pair| pair[0] == pair[1]));
    assert!(listings[0].contains(&("shared-00".to_string(), "Changed under readers.".to_string())));
}

#[test]
fn cache_is_bounded() {
    let _guard = cold_cache();
    let workspaces: Vec<_> = (0..66).map(|_| tempfile::tempdir().expect("tempdir")).collect();
    for workspace in &workspaces {
        overlay_cache::overlay(Some(workspace.path()));
    }
    assert!(
        !overlay_cache::contains_for_tests(workspaces[0].path()),
        "the oldest entry is evicted"
    );
    assert!(overlay_cache::contains_for_tests(workspaces[65].path()));
    assert_eq!(overlay_cache::len_for_tests(), 64);
}

#[test]
fn forget_rescans_the_workspace() {
    let _guard = cold_cache();
    let workspace = tempfile::tempdir().expect("tempdir");
    write_skill(workspace.path(), ".horon-agent", "kept", "Kept.");
    let first = overlay_cache::overlay(Some(workspace.path()));
    overlay_cache::forget(Some(workspace.path()));
    let second = overlay_cache::overlay(Some(workspace.path()));
    assert!(!Arc::ptr_eq(&first, &second), "a forgotten workspace is rebuilt");
    assert_eq!(description(&second, "kept").as_deref(), Some("Kept."));
}
