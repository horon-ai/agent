//! Tests for the collision rules: identical copies collapse, different bodies
//! keep both under a namespace, a path separator is refused, and a managed
//! skill never takes a namespace.

use super::collision::{self, Candidate};
use crate::skill::test_support::test_skill;

fn candidate(skill: super::Skill, namespace: &str, managed: bool) -> Candidate {
    Candidate {
        skill,
        namespace: namespace.to_string(),
        managed,
    }
}

fn names(resolution: &collision::Resolution) -> Vec<String> {
    resolution.skills.iter().map(|skill| skill.name.clone()).collect()
}

#[test]
fn different_bodies_keep_both() {
    let resolution = collision::resolve(vec![
        candidate(test_skill("shared", "Mine", "authored body"), "skills", false),
        candidate(test_skill("shared", "Theirs", "plugin body"), "plugin", false),
    ]);

    assert_eq!(names(&resolution), vec!["plugin/shared", "shared"]);
    assert_eq!(
        resolution.warnings.len(),
        1,
        "one collision must produce one warning, got {:?}",
        resolution.warnings
    );
    let namespaced = resolution
        .skills
        .iter()
        .find(|skill| skill.name == "plugin/shared")
        .unwrap();
    let bare = resolution.skills.iter().find(|skill| skill.name == "shared").unwrap();
    assert_eq!(
        &resolution.warnings[0],
        &format!(
            "Skills: name collision: \"shared\" from {} differs from {}; available as \"plugin/shared\"",
            namespaced.path.display(),
            bare.path.display()
        )
    );
}

#[test]
fn identical_copies_collapse() {
    let resolution = collision::resolve(vec![
        candidate(test_skill("copy", "Same", "same body"), "skills", false),
        candidate(test_skill("copy", "Same", "same body"), "plugin", false),
    ]);

    assert_eq!(names(&resolution), vec!["copy"]);
    assert!(
        resolution.warnings.is_empty(),
        "an identical copy is not a collision: {:?}",
        resolution.warnings
    );
}

#[test]
fn same_body_with_a_different_frontmatter_is_kept() {
    let resolution = collision::resolve(vec![
        candidate(test_skill("copy", "One description", "same body"), "skills", false),
        candidate(test_skill("copy", "Another description", "same body"), "plugin", false),
    ]);

    assert_eq!(names(&resolution), vec!["copy", "plugin/copy"]);
    assert_eq!(resolution.warnings.len(), 1);
}

#[test]
fn slash_in_frontmatter_rejected() {
    let resolution = collision::resolve(vec![
        candidate(test_skill("plain", "Plain", "body"), "skills", false),
        candidate(test_skill("plug/in", "Sneaky", "body"), "plugin", false),
    ]);

    assert_eq!(names(&resolution), vec!["plain"]);
    assert_eq!(resolution.warnings.len(), 1);
    assert!(
        resolution.warnings[0].contains("contains a path separator; skipping"),
        "unexpected warning: {}",
        resolution.warnings[0]
    );
}

#[test]
fn backslash_in_frontmatter_rejected() {
    let resolution = collision::resolve(vec![candidate(
        test_skill("plug\\in", "Sneaky", "body"),
        "plugin",
        false,
    )]);

    assert!(resolution.skills.is_empty());
    assert_eq!(resolution.warnings.len(), 1);
}

#[test]
fn managed_hidden_when_authored_exists() {
    let resolution = collision::resolve(vec![
        candidate(test_skill("shared", "Authored", "authored body"), "skills", false),
        candidate(test_skill("shared", "Managed", "managed body"), "managed", true),
    ]);

    assert_eq!(names(&resolution), vec!["shared"]);
    assert_eq!(resolution.warnings.len(), 1);
    let warning = &resolution.warnings[0];
    assert!(
        warning.contains("is hidden by") && warning.contains("never take a namespace"),
        "unexpected warning: {warning}"
    );
}

#[test]
fn a_third_identical_copy_collapses_even_after_the_second_was_renamed() {
    // The second copy is stored as `claude/copy`, so its frontmatter name is no
    // longer its registry key; comparing keys instead of the declared name
    // would keep a third identical file as a new skill with a bogus warning.
    let resolution = collision::resolve(vec![
        candidate(test_skill("copy", "One", "first body"), "skills", false),
        candidate(test_skill("copy", "Two", "second body"), "claude", false),
        candidate(test_skill("copy", "Two", "second body"), "agents", false),
    ]);

    assert_eq!(names(&resolution), vec!["claude/copy", "copy"]);
    assert_eq!(resolution.warnings.len(), 1, "{:?}", resolution.warnings);
}

#[test]
fn third_copy_gets_tilde_suffix() {
    let resolution = collision::resolve(vec![
        candidate(test_skill("shared", "One", "body one"), "skills", false),
        candidate(test_skill("shared", "Two", "body two"), "plugin", false),
        candidate(test_skill("shared", "Three", "body three"), "plugin", false),
        candidate(test_skill("shared", "Four", "body four"), "other", false),
    ]);

    assert_eq!(
        names(&resolution),
        vec!["other/shared", "plugin/shared", "plugin/shared~2", "shared"]
    );
    assert_eq!(resolution.warnings.len(), 3);
}

#[test]
fn namespace_falls_back_to_the_source_id() {
    for (path, expected) in [
        ("/home/u/.claude/skills/foo/SKILL.md", "claude"),
        ("/home/u/.claude/skills/SKILL.md", "claude"),
        ("/home/u/pkg/skills/foo/SKILL.md", "pkg"),
        (
            "/home/u/.claude/plugins/cache/market/plug/1.0.0/skills/foo/SKILL.md",
            "plug",
        ),
        (
            "/home/u/.claude/plugins/cache/market___plug___1.0.0/skills/foo/SKILL.md",
            "plug",
        ),
        ("/home/u/pkg/bad name/skills/foo/SKILL.md", "bad-name"),
    ] {
        assert_eq!(
            collision::namespace_for(std::path::Path::new(path), "claude"),
            expected,
            "unexpected namespace for {path}"
        );
    }
}
