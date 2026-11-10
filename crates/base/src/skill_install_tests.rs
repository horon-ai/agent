//! Unit tests for the parts of the installer that do not need a git remote:
//! the skill lookup inside a clone and the lock file's shape.

use super::*;

fn write_skill(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Fixture {name}\n---\n\nBody."),
    )
    .unwrap();
}

#[test]
fn a_root_skill_is_taken_from_the_clone_root() {
    let clone = tempfile::tempdir().unwrap();
    write_skill(clone.path(), "root-skill");
    let (root, description) = locate_skill(clone.path(), None).expect("root skill");
    assert_eq!(root, clone.path());
    assert_eq!(description, "Fixture root-skill");
}

#[test]
fn a_single_subdirectory_skill_is_taken_without_a_hint() {
    let clone = tempfile::tempdir().unwrap();
    write_skill(&clone.path().join("only-skill"), "only-skill");
    std::fs::create_dir_all(clone.path().join("docs")).unwrap();
    let (root, _) = locate_skill(clone.path(), None).expect("subdirectory skill");
    assert_eq!(root, clone.path().join("only-skill"));
}

#[test]
fn several_skills_need_the_hint() {
    let clone = tempfile::tempdir().unwrap();
    write_skill(&clone.path().join("one"), "one");
    write_skill(&clone.path().join("two"), "two");
    let error = locate_skill(clone.path(), None).expect_err("ambiguous");
    assert!(error.to_string().contains("2 skills"), "{error}");
    let (root, _) = locate_skill(clone.path(), Some("two")).expect("hinted skill");
    assert_eq!(root, clone.path().join("two"));
}

#[test]
fn a_clone_without_a_skill_is_refused() {
    let clone = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(clone.path().join("src")).unwrap();
    let error = locate_skill(clone.path(), None).expect_err("no skill");
    assert!(error.to_string().contains("no SKILL.md"), "{error}");
}

#[test]
fn the_lock_file_keeps_one_row_per_name() {
    let dir = tempfile::tempdir().unwrap();
    write_lock(
        dir.path(),
        InstallRecord {
            name: "one".to_string(),
            source: "a".to_string(),
            git_ref: None,
            commit: "0".repeat(40),
        },
    )
    .expect("first row");
    write_lock(
        dir.path(),
        InstallRecord {
            name: "one".to_string(),
            source: "b".to_string(),
            git_ref: Some("v1".to_string()),
            commit: "1".repeat(40),
        },
    )
    .expect("replacement row");

    let text = std::fs::read_to_string(dir.path().join(LOCK_FILE)).unwrap();
    let lock: serde_json::Value = serde_json::from_str(&text).unwrap();
    let rows = lock["installs"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "one name keeps one row: {text}");
    assert_eq!(rows[0]["source"], "b");
    assert_eq!(rows[0]["ref"], "v1");
}

#[test]
fn a_broken_lock_file_is_reported_instead_of_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(LOCK_FILE), "not json").unwrap();
    let error = write_lock(
        dir.path(),
        InstallRecord {
            name: "one".to_string(),
            source: "a".to_string(),
            git_ref: None,
            commit: "0".repeat(40),
        },
    )
    .expect_err("a broken lock must not be replaced");
    assert!(error.to_string().contains("install lock"), "{error}");
    assert_eq!(std::fs::read_to_string(dir.path().join(LOCK_FILE)).unwrap(), "not json");
}
