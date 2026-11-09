//! `skill://` reads: body, files, selectors, namespaces, and every way out of
//! the skill directory refused.

use super::super::ReadTool;
use crate::tool::{Tool, ToolContext, ToolExecutionMode};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A workspace with project skills under an isolated `HORON_HOME`, so
/// the global registry never reads the real `~/.horon`.
struct Fixture {
    _lock: std::sync::MutexGuard<'static, ()>,
    _home: tempfile::TempDir,
    workspace: tempfile::TempDir,
    previous_home: Option<std::ffi::OsString>,
}

impl Fixture {
    fn new() -> Self {
        let lock = crate::storage::lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let previous_home = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", home.path());
        crate::config::invalidate_config_cache();
        Self {
            _lock: lock,
            _home: home,
            workspace: tempfile::tempdir().expect("workspace"),
            previous_home,
        }
    }

    /// Write a project skill and return its directory.
    fn skill(&self, name: &str, body: &str) -> PathBuf {
        let dir = self.workspace.path().join(".horon/skills").join(name);
        std::fs::create_dir_all(&dir).expect("skill dir");
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Test skill {name}\n---\n\n{body}\n"),
        )
        .expect("write skill");
        dir
    }

    fn ctx(&self, session_id: &str) -> ToolContext {
        ToolContext {
            session_id: session_id.to_string(),
            message_id: "skill-message".to_string(),
            tool_call_id: "skill-call".to_string(),
            working_dir: Some(self.workspace.path().to_path_buf()),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
        }
    }

    async fn read(&self, file_path: &str) -> anyhow::Result<String> {
        let output = ReadTool::new()
            .execute(json!({ "file_path": file_path }), self.ctx("skill-read-session"))
            .await?;
        Ok(output.output)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        match self.previous_home.take() {
            Some(previous) => crate::env::set_var("HORON_HOME", previous),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::invalidate_config_cache();
    }
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    std::fs::write(path, text).expect("write");
}

#[tokio::test]
async fn reads_the_skill_body_under_a_header() {
    let fixture = Fixture::new();
    let dir = fixture.skill("qa-skill", "Line one.\nLine two.");
    let output = fixture.read("skill://qa-skill").await.expect("read skill");
    assert!(output.starts_with("## Skill: qa-skill\nBase directory: "), "{output}");
    assert!(output.contains(&dir.display().to_string()), "{output}");
    assert!(output.contains("Line one.") && output.contains("Line two."), "{output}");
    assert!(
        !output.contains("description: Test skill"),
        "the frontmatter is not the body: {output}"
    );
}

#[tokio::test]
async fn reads_a_file_inside_the_skill_with_a_selector() {
    let fixture = Fixture::new();
    let dir = fixture.skill("qa-skill", "Body.");
    let lines: String = (1..=40).map(|line| format!("reference line {line:02}\n")).collect();
    write(&dir.join("references/a.md"), &lines);
    let output = fixture
        .read("skill://qa-skill/references/a.md:20-21")
        .await
        .expect("read reference");
    assert!(
        output.contains("reference line 20") && output.contains("reference line 21"),
        "{output}"
    );
    assert!(
        !output.contains("reference line 01") && !output.contains("reference line 40"),
        "the selector pages the file: {output}"
    );
}

#[tokio::test]
async fn a_namespaced_name_takes_the_longest_registered_prefix() {
    let fixture = Fixture::new();
    // A skill named `claude`, plus a `review` in two sources with different
    // bodies: the `.claude` one is kept as `claude/review`. The reference
    // `claude/review/notes.md` must name that skill's `notes.md`, never a path
    // `review/notes.md` under the skill `claude`.
    let outer = fixture.skill("claude", "Outer.");
    write(&outer.join("review/notes.md"), "outer's nested file\n");
    fixture.skill("review", "Review from horon-agent.");
    let namespaced = fixture.workspace.path().join(".claude/skills/review");
    write(
        &namespaced.join("SKILL.md"),
        "---\nname: review\ndescription: Review\n---\n\nReview from claude.\n",
    );
    write(&namespaced.join("notes.md"), "namespaced notes\n");

    let registry = super::effective_registry(&fixture.ctx("skill-ns-session")).await;
    assert!(
        registry.get("claude/review").is_some(),
        "the colliding skill keeps a namespaced name: {:?}",
        registry.list().iter().map(|skill| &skill.name).collect::<Vec<_>>()
    );
    let (skill, relative) = super::split_reference(&registry, "claude/review/notes.md").expect("split");
    assert_eq!((skill.name.as_str(), relative), ("claude/review", Some("notes.md")));

    let output = fixture
        .read("skill://claude/review/notes.md")
        .await
        .expect("read notes");
    assert!(
        output.contains("namespaced notes") && !output.contains("outer's"),
        "{output}"
    );
    let body = fixture
        .read("skill://claude/review")
        .await
        .expect("read namespaced body");
    assert!(body.contains("Review from claude."), "{body}");
}

#[tokio::test]
async fn a_dot_dot_path_is_refused() {
    let fixture = Fixture::new();
    fixture.skill("qa-skill", "Body.");
    write(&fixture.workspace.path().join("secret.txt"), "top secret\n");
    let error = fixture
        .read("skill://qa-skill/../../../secret.txt")
        .await
        .expect_err("escape refused");
    assert!(
        error
            .to_string()
            .contains("skill:// path resolves outside the skill directory"),
        "{error}"
    );
    let error = fixture
        .read("skill://qa-skill/../../../../../../../../etc/passwd")
        .await
        .expect_err("absolute escape refused");
    assert!(!error.to_string().contains("root:"), "{error}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_out_of_the_skill_is_refused() {
    let fixture = Fixture::new();
    let dir = fixture.skill("qa-skill", "Body.");
    let outside = fixture._home.path().join("outside.txt");
    write(&outside, "outside text\n");
    std::os::unix::fs::symlink(&outside, dir.join("link.md")).expect("symlink");
    let error = fixture
        .read("skill://qa-skill/link.md")
        .await
        .expect_err("symlink refused");
    assert!(
        error
            .to_string()
            .contains("skill:// path resolves outside the skill directory"),
        "{error}"
    );
}

#[tokio::test]
async fn an_unknown_skill_is_an_error() {
    let fixture = Fixture::new();
    fixture.skill("qa-skill", "Body.");
    let error = fixture.read("skill://nope").await.expect_err("unknown skill");
    assert!(error.to_string().contains("names no installed skill: nope"), "{error}");
    let error = fixture.read("skill://").await.expect_err("empty reference");
    assert!(error.to_string().contains("needs a skill name"), "{error}");
}

#[tokio::test]
async fn the_deny_policy_judges_the_real_file() {
    let fixture = Fixture::new();
    let dir = fixture.skill("qa-skill", "Body.");
    write(&dir.join("private.env"), "TOKEN=1\n");
    let session = "skill-deny-session";
    let _policy = crate::tool::path_policy::register_session_path_policy(
        session,
        Arc::new(|path: &Path| {
            path.extension()
                .is_some_and(|ext| ext == "env")
                .then(|| "env files are private".to_string())
        }),
    );
    let error = ReadTool::new()
        .execute(
            json!({ "file_path": "skill://qa-skill/private.env" }),
            fixture.ctx(session),
        )
        .await
        .expect_err("denied");
    assert!(error.to_string().contains("env files are private"), "{error}");
    let allowed = ReadTool::new()
        .execute(json!({ "file_path": "skill://qa-skill" }), fixture.ctx(session))
        .await
        .expect("the body is not denied");
    assert!(allowed.output.contains("Body."));
}
