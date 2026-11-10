//! `skill_manage` action `install` (issue #79).
//!
//! The action exists so an agent that finds an endorsed skill missing can add
//! it instead of stopping to ask a person. These tests pin the parts that make
//! it safe: a clone lands in the managed directory with a lock entry, an
//! unknown endorsed name writes nothing, and a planning session never reaches
//! the network or the disk because an install is a write and network effect.
//!
//! Every test writes below one private `HORON_HOME`, so they share the storage
//! test lock instead of running in parallel over the real home.

use super::SkillTool;
use crate::skill::SkillRegistry;
use crate::tool::{Registry, Tool, ToolContext};
use serde_json::json;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use tokio::sync::RwLock;

/// A private agent home with `[tools] managed_skills = true`, restored on drop.
struct IsolatedHome {
    previous: Option<std::ffi::OsString>,
    _temp: tempfile::TempDir,
}

impl IsolatedHome {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("isolated agent home");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", temp.path());
        std::fs::write(temp.path().join("config.toml"), "[tools]\nmanaged_skills = true\n").unwrap();
        crate::config::Config::invalidate_cache();
        Self { previous, _temp: temp }
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        match &self.previous {
            Some(home) => crate::env::set_var("HORON_HOME", home),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

fn run_isolated(body: impl Future<Output = ()>) {
    let _guard = crate::storage::lock_test_env();
    tokio::runtime::Runtime::new().expect("runtime").block_on(body);
}

fn empty_tool() -> SkillTool {
    SkillTool::new(Arc::new(RwLock::new(SkillRegistry::default())))
}

fn context() -> ToolContext {
    ToolContext {
        session_id: "skill-install-session".to_string(),
        message_id: "skill-install-message".to_string(),
        tool_call_id: "skill-install-call".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    }
}

fn managed_dir() -> std::path::PathBuf {
    crate::managed_skills::dir().expect("managed skill directory")
}

/// A git repository holding one skill, committed so a clone has a HEAD.
fn skill_repo(root: &Path, name: &str) -> std::path::PathBuf {
    let repo = root.join(format!("{name}-repo"));
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(
        repo.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Fixture skill {name}\n---\n\nBody."),
    )
    .unwrap();
    git(&repo, &["init", "--quiet"]);
    git(&repo, &["add", "SKILL.md"]);
    git(
        &repo,
        &[
            "-c",
            "user.email=fixture@example.com",
            "-c",
            "user.name=Fixture",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    );
    repo
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn skill_install_of_a_git_source_creates_the_managed_skill_and_a_lock_entry() {
    run_isolated(skill_install_of_a_git_source_creates_the_managed_skill_and_a_lock_entry_inner());
}

async fn skill_install_of_a_git_source_creates_the_managed_skill_and_a_lock_entry_inner() {
    let _home = IsolatedHome::new();
    let workspace = tempfile::tempdir().expect("workspace");
    let repo = skill_repo(workspace.path(), "installed-notes");
    let tool = empty_tool();

    let output = tool
        .execute(
            json!({"action": "install", "source": repo.display().to_string()}),
            context(),
        )
        .await
        .expect("install")
        .output;
    assert!(output.contains("Installed skill 'installed-notes'"), "{output}");

    let file = managed_dir().join("installed-notes").join("SKILL.md");
    assert!(file.is_file(), "the cloned SKILL.md must land in the managed directory");
    let body = std::fs::read_to_string(&file).unwrap();
    assert!(body.contains("Fixture skill installed-notes"), "{body}");

    let lock = std::fs::read_to_string(managed_dir().join("install-lock.json")).expect("lock file");
    let lock: serde_json::Value = serde_json::from_str(&lock).expect("lock json");
    let entry = &lock["installs"][0];
    assert_eq!(entry["name"], "installed-notes");
    assert!(entry["source"].as_str().unwrap().ends_with("installed-notes-repo"));
    assert_eq!(
        entry["commit"].as_str().unwrap().len(),
        40,
        "the lock records the commit the clone resolved to"
    );
}

#[test]
fn skill_install_lists_the_skill_without_a_manual_reload() {
    run_isolated(skill_install_lists_the_skill_without_a_manual_reload_inner());
}

async fn skill_install_lists_the_skill_without_a_manual_reload_inner() {
    let _home = IsolatedHome::new();
    let workspace = tempfile::tempdir().expect("workspace");
    let repo = skill_repo(workspace.path(), "listed-notes");
    let tool = empty_tool();

    tool.execute(
        json!({"action": "install", "source": repo.display().to_string()}),
        context(),
    )
    .await
    .expect("install");

    let listed = tool
        .execute(json!({"action": "list"}), context())
        .await
        .expect("list")
        .output;
    assert!(
        listed.contains("listed-notes"),
        "the installed skill must appear in the list without a manual reload: {listed}"
    );
}

#[test]
fn skill_install_of_an_unknown_endorsed_name_writes_nothing() {
    run_isolated(skill_install_of_an_unknown_endorsed_name_writes_nothing_inner());
}

async fn skill_install_of_an_unknown_endorsed_name_writes_nothing_inner() {
    let _home = IsolatedHome::new();
    let tool = empty_tool();

    let error = tool
        .execute(
            json!({"action": "install", "name": "no-such-endorsed-skill"}),
            context(),
        )
        .await
        .expect_err("an unknown endorsed name must be refused");
    assert!(error.to_string().contains("No endorsed skill"), "{error:#}");
    let dir = managed_dir();
    assert!(
        !dir.exists() || std::fs::read_dir(&dir).unwrap().next().is_none(),
        "a refused install must leave the managed directory empty"
    );
}

#[test]
fn skill_install_refuses_a_name_that_already_exists() {
    run_isolated(skill_install_refuses_a_name_that_already_exists_inner());
}

async fn skill_install_refuses_a_name_that_already_exists_inner() {
    let _home = IsolatedHome::new();
    let workspace = tempfile::tempdir().expect("workspace");
    let repo = skill_repo(workspace.path(), "twice-notes");
    let tool = empty_tool();
    tool.execute(
        json!({"action": "install", "source": repo.display().to_string()}),
        context(),
    )
    .await
    .expect("first install");

    let error = tool
        .execute(
            json!({"action": "install", "source": repo.display().to_string()}),
            context(),
        )
        .await
        .expect_err("installing over an existing skill must be refused");
    assert!(error.to_string().contains("already exists"), "{error:#}");
}

#[test]
fn skill_install_is_offered_by_the_tool_schema() {
    let tool = empty_tool();
    let schema = tool.parameters_schema();
    let actions = schema["properties"]["action"]["enum"]
        .as_array()
        .expect("the action property is an enum");
    assert!(
        actions.iter().any(|action| action.as_str() == Some("install")),
        "the model can only send an action the schema lists, got: {actions:?}"
    );
    assert!(
        schema["properties"]["source"].is_object(),
        "install needs a source property: {schema}"
    );
}

#[test]
fn skill_install_is_refused_in_plan_mode() {
    run_isolated(skill_install_is_refused_in_plan_mode_inner());
}

async fn skill_install_is_refused_in_plan_mode_inner() {
    let _home = IsolatedHome::new();
    let workspace = tempfile::tempdir().expect("workspace");
    let repo = skill_repo(workspace.path(), "planned-install");

    let registry = Registry::empty();
    registry
        .register("skill_manage".to_string(), Arc::new(empty_tool()))
        .await;

    let session = "skill-install-plan-mode";
    crate::plan_mode::clear_state(session);
    crate::plan_mode::set_state(session, crate::plan_mode::PlanState::Planning);
    let refusal = registry
        .execute(
            "skill_manage",
            json!({"action": "install", "source": repo.display().to_string()}),
            ToolContext {
                session_id: session.to_string(),
                ..context()
            },
        )
        .await
        .expect_err("plan mode is read-only, so an install must be refused");
    crate::plan_mode::clear_state(session);

    assert_eq!(
        refusal.to_string(),
        crate::plan_mode::block_message("skill_manage"),
        "the refusal must be the plan-mode message, not a tool error"
    );
    assert!(
        !managed_dir().join("planned-install").exists(),
        "a refused install must write nothing"
    );
}
