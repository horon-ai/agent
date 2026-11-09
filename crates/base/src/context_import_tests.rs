use super::*;

use std::borrow::Cow;
use std::fs;

use tempfile::TempDir;

/// Point `HORON_HOME` at a fresh directory so the home import root is
/// this test's own sandbox, and restore the previous value on drop.
struct SandboxHome {
    previous: Option<std::ffi::OsString>,
    dir: TempDir,
}

impl SandboxHome {
    fn new() -> Self {
        let previous = std::env::var_os("HORON_HOME");
        let dir = TempDir::new().expect("sandbox home temp dir");
        crate::env::set_var("HORON_HOME", dir.path());
        Self { previous, dir }
    }

    /// `$HORON_HOME/external`, the directory `~/` imports resolve under.
    fn external(&self) -> std::path::PathBuf {
        let external = self.dir.path().join("external");
        fs::create_dir_all(&external).expect("create external dir");
        external
    }
}

impl Drop for SandboxHome {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(previous) => crate::env::set_var("HORON_HOME", previous),
            None => crate::env::remove_var("HORON_HOME"),
        }
    }
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dir");
    }
    fs::write(path, content).expect("write test file");
}

#[test]
fn context_import_inlines_relative_file() {
    let _guard = crate::storage::lock_test_env();
    let project = TempDir::new().expect("project temp dir");
    write(&project.path().join("rules/a.md"), "rule a body\n");
    let agents = "intro\n@rules/a.md\noutro\n";
    write(&project.path().join("AGENTS.md"), agents);

    let expanded = expand_context_imports(agents, &project.path().join("AGENTS.md"), project.path());

    assert_eq!(expanded, "intro\nrule a body\noutro\n");
}

#[test]
fn context_import_resolves_against_containing_file_not_project_root() {
    let _guard = crate::storage::lock_test_env();
    let project = TempDir::new().expect("project temp dir");
    let nested = project.path().join("docs");
    write(&nested.join("nested.md"), "nested body\n");
    write(&nested.join("local.md"), "local body\n");
    let content = "sibling\n@local.md\n";

    let expanded = expand_context_imports(content, &nested.join("nested.md"), project.path());

    assert_eq!(expanded, "sibling\nlocal body\n");
}

#[test]
fn context_import_without_at_line_is_borrowed_and_unchanged() {
    let _guard = crate::storage::lock_test_env();
    let content = "# Plain instructions\n\nNo imports here.\n";
    let expanded = expand_context_imports(content, Path::new("AGENTS.md"), Path::new("."));

    assert!(
        matches!(&expanded, Cow::Borrowed(_)),
        "no @ must take the borrow fast path"
    );
    assert_eq!(expanded, content);
}

#[test]
fn context_import_cycle_terminates_with_one_marker() {
    let _guard = crate::storage::lock_test_env();
    let project = TempDir::new().expect("project temp dir");
    let a = "start\n@b.md\na end\n";
    let b = "b start\n@AGENTS.md\nb end\n";
    write(&project.path().join("AGENTS.md"), a);
    write(&project.path().join("b.md"), b);

    let expanded = expand_context_imports(a, &project.path().join("AGENTS.md"), project.path());

    assert!(expanded.contains("b start"));
    assert!(expanded.contains("b end"));
    assert_eq!(
        expanded.matches(CYCLE_MARKER_PREFIX).count(),
        1,
        "cycle must replace the back-reference exactly once: {expanded}"
    );
    assert!(!expanded.contains("\n@AGENTS.md\n"));
}

#[test]
fn context_import_sixth_hop_stays_literal() {
    let _guard = crate::storage::lock_test_env();
    let project = TempDir::new().expect("project temp dir");
    let root = project.path();
    write(&root.join("f1.md"), "layer1\n@f2.md\n");
    write(&root.join("f2.md"), "layer2\n@f3.md\n");
    write(&root.join("f3.md"), "layer3\n@f4.md\n");
    write(&root.join("f4.md"), "layer4\n@f5.md\n");
    write(&root.join("f5.md"), "layer5\n@f6.md\n");
    write(&root.join("f6.md"), "layer6\n@f7.md\n");
    write(&root.join("f7.md"), "layer7\n");
    let agents = "@f1.md\n";
    write(&root.join("AGENTS.md"), agents);

    let expanded = expand_context_imports(agents, &root.join("AGENTS.md"), root);

    for layer in 1..=5 {
        assert!(expanded.contains(&format!("layer{layer}")), "hop {layer} must expand");
    }
    assert!(expanded.contains("@f6.md"), "sixth hop must stay literal: {expanded}");
    assert!(!expanded.contains("layer6"), "sixth hop must not inline: {expanded}");
    assert!(!expanded.contains("layer7"));
}

#[test]
fn context_import_refuses_path_outside_allowed_roots() {
    let _guard = crate::storage::lock_test_env();
    let _home = SandboxHome::new();
    let project = TempDir::new().expect("project temp dir");
    let agents = "keep\n@../../etc/passwd\nnext\n";

    let expanded = expand_context_imports(agents, &project.path().join("AGENTS.md"), project.path());

    assert_eq!(expanded, agents);
}

#[test]
fn context_import_refuses_absolute_path_outside_allowed_roots() {
    let _guard = crate::storage::lock_test_env();
    let _home = SandboxHome::new();
    let project = TempDir::new().expect("project temp dir");
    let outside = TempDir::new().expect("outside temp dir");
    let secret = outside.path().join("secret.md");
    write(&secret, "outside secret\n");
    let agents = format!("keep\n@{}\n", secret.display());

    let expanded = expand_context_imports(&agents, &project.path().join("AGENTS.md"), project.path());

    assert_eq!(expanded, agents);
    assert!(!expanded.contains("outside secret"));
}

#[test]
fn context_import_fenced_block_stays_literal() {
    let _guard = crate::storage::lock_test_env();
    let project = TempDir::new().expect("project temp dir");
    write(&project.path().join("x.md"), "should not load\n");
    let agents = "before\n```\n@x.md\n```\nafter\n";
    write(&project.path().join("AGENTS.md"), agents);

    let expanded = expand_context_imports(agents, &project.path().join("AGENTS.md"), project.path());

    assert_eq!(expanded, agents);
    assert!(!expanded.contains("should not load"));
}

#[test]
fn context_import_tilde_fence_block_stays_literal() {
    let _guard = crate::storage::lock_test_env();
    let project = TempDir::new().expect("project temp dir");
    write(&project.path().join("x.md"), "should not load\n");
    let agents = "~~~md\n@x.md\n~~~\n";
    write(&project.path().join("AGENTS.md"), agents);

    let expanded = expand_context_imports(agents, &project.path().join("AGENTS.md"), project.path());

    assert_eq!(expanded, agents);
}

#[test]
fn context_import_missing_file_keeps_line() {
    let _guard = crate::storage::lock_test_env();
    let project = TempDir::new().expect("project temp dir");
    let agents = "before\n@missing.md\nafter\n";

    let expanded = expand_context_imports(agents, &project.path().join("AGENTS.md"), project.path());

    assert_eq!(expanded, agents);
}

#[test]
fn context_import_home_path_resolves_under_horon_home() {
    let _guard = crate::storage::lock_test_env();
    let home = SandboxHome::new();
    write(&home.external().join("shared.md"), "shared rule\n");

    let project = TempDir::new().expect("project temp dir");
    let agents = "top\n@~/shared.md\n";
    let expanded = expand_context_imports(agents, &project.path().join("AGENTS.md"), project.path());

    assert_eq!(expanded, "top\nshared rule\n");
}

#[test]
fn context_import_loader_counts_expanded_size() {
    let _guard = crate::storage::lock_test_env();
    let project = TempDir::new().expect("project temp dir");
    write(&project.path().join("rules/a.md"), "expanded rule body\n");
    let agents = "intro\n@rules/a.md\n";
    write(&project.path().join("AGENTS.md"), agents);
    let agents_md = project.path().join("AGENTS.md");

    let (content, info) = crate::prompt::load_agents_md_files_from_dirs(project.path(), Some(&agents_md));
    let content = content.expect("project instructions");

    assert!(content.contains("expanded rule body"));
    assert!(!content.contains("@rules/a.md"));
    assert!(info.has_project_agents_md);
    assert_eq!(info.project_agents_md_chars, "intro\nexpanded rule body\n".len());
}

#[test]
fn context_import_loader_keeps_outside_path_literal() {
    let _guard = crate::storage::lock_test_env();
    let project = TempDir::new().expect("project temp dir");
    let agents = "intro\n@../../etc/passwd\n";
    write(&project.path().join("AGENTS.md"), agents);
    let agents_md = project.path().join("AGENTS.md");

    let (content, info) = crate::prompt::load_agents_md_files_from_dirs(project.path(), Some(&agents_md));
    let content = content.expect("project instructions");

    assert!(content.contains("@../../etc/passwd"));
    assert!(!content.contains("root:"));
    assert_eq!(info.project_agents_md_chars, agents.len());
}

#[test]
fn context_import_at_without_import_line_stays_borrowed() {
    let _guard = crate::storage::lock_test_env();
    let project = TempDir::new().expect("project temp dir");
    let agents = "mail dev@example.com\n@not-a-path here\n";

    let expanded = expand_context_imports(agents, &project.path().join("AGENTS.md"), project.path());

    assert!(
        matches!(&expanded, Cow::Borrowed(_)),
        "no import line must not copy: {expanded}"
    );
    assert_eq!(expanded, agents);
}
