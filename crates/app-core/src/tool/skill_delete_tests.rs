//! `skill_manage` action `delete` (issue #62).
//!
//! The action exists so a wrong or outdated skill the model wrote itself can
//! stop matching. These tests pin the boundaries that make it safe: the
//! removal reaches the managed directory only, a name that belongs to an
//! authored skill is refused with its source and its files stay, and a managed
//! entry that is a symlink loses the link while its target keeps every file.
//! Plan mode is checked through the registry gate, because a deletion is a
//! write effect and a planning session must never reach the disk.
//!
//! Every test here writes below one private `HORON_HOME`, so they share
//! the storage test lock instead of running in parallel over the real home.

use super::SkillTool;
use crate::skill::SkillRegistry;
use crate::tool::{Registry, Tool, ToolContext};
use serde_json::json;
use std::path::Path;
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

/// Run `body` on its own runtime while holding the process-wide storage test
/// lock, which is what keeps the `HORON_HOME` change of one test out of
/// another test's read.
fn run_isolated(body: impl Future<Output = ()>) {
    let _guard = crate::storage::lock_test_env();
    tokio::runtime::Runtime::new().expect("runtime").block_on(body);
}

fn empty_tool() -> SkillTool {
    SkillTool::new(Arc::new(RwLock::new(SkillRegistry::default())))
}

fn context() -> ToolContext {
    ToolContext {
        session_id: "skill-delete-session".to_string(),
        message_id: "skill-delete-message".to_string(),
        tool_call_id: "skill-delete-call".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    }
}

fn context_in(dir: &Path) -> ToolContext {
    ToolContext {
        working_dir: Some(dir.to_path_buf()),
        ..context()
    }
}

fn managed_dir() -> std::path::PathBuf {
    crate::managed_skills::dir().expect("managed skill directory")
}

/// A workspace skill under `.agents/skills`, the row the loader reads without a
/// switch, so the refusal is about authorship rather than about configuration.
fn write_project_skill(workspace: &Path, name: &str) -> std::path::PathBuf {
    let dir = workspace.join(".agents").join("skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("SKILL.md");
    std::fs::write(
        &file,
        format!("---\nname: {name}\ndescription: Project skill {name}\n---\n\nBody."),
    )
    .unwrap();
    file
}

#[test]
fn skill_delete_managed_skill_leaves_the_list() {
    run_isolated(skill_delete_managed_skill_leaves_the_list_inner());
}

async fn skill_delete_managed_skill_leaves_the_list_inner() {
    let _home = IsolatedHome::new();
    let tool = empty_tool();

    // Created through the tool, so the fixture has exactly the shape the agent
    // itself writes and the loader itself reads.
    tool.execute(
        json!({"action": "create", "name": "stale-notes", "description": "Old procedure", "body": "Body."}),
        context(),
    )
    .await
    .unwrap();
    let before = tool.execute(json!({"action": "list"}), context()).await.unwrap();
    assert!(before.output.contains("## /stale-notes"), "{}", before.output);

    let deleted = tool
        .execute(json!({"action": "delete", "name": "stale-notes"}), context())
        .await
        .unwrap();
    assert_eq!(
        deleted.title.as_deref(),
        Some("Skills: Deleted stale-notes"),
        "{}",
        deleted.output
    );
    assert!(
        deleted.output.contains("Deleted managed skill 'stale-notes'"),
        "{}",
        deleted.output
    );
    assert!(
        !managed_dir().join("stale-notes").exists(),
        "the managed skill directory must be gone"
    );

    // The index is reloaded, so the next turn neither lists nor loads it.
    let after = tool.execute(json!({"action": "list"}), context()).await.unwrap();
    assert!(!after.output.contains("stale-notes"), "{}", after.output);
    let read = tool
        .execute(json!({"action": "read", "name": "stale-notes"}), context())
        .await
        .unwrap();
    assert!(read.output.contains("not found"), "{}", read.output);
}

#[test]
fn skill_delete_refuses_a_project_skill_and_keeps_its_files() {
    run_isolated(skill_delete_refuses_a_project_skill_and_keeps_its_files_inner());
}

async fn skill_delete_refuses_a_project_skill_and_keeps_its_files_inner() {
    let _home = IsolatedHome::new();
    let workspace = tempfile::tempdir().unwrap();
    let skill_file = write_project_skill(workspace.path(), "project-proc");
    let tool = empty_tool();

    let error = tool
        .execute(
            json!({"action": "delete", "name": "project-proc"}),
            context_in(workspace.path()),
        )
        .await
        .expect_err("a project skill is not the agent's to delete")
        .to_string();

    assert!(
        error.contains("Skill 'project-proc' is a project skill from the agents source"),
        "the refusal must name the source that owns the name, got: {error}"
    );
    assert!(
        error.contains("Only skills in"),
        "the refusal must say what can be deleted, got: {error}"
    );
    assert!(skill_file.exists(), "a refused delete must leave every file in place");
}

#[test]
fn skill_delete_refuses_a_user_skill_and_keeps_its_files() {
    run_isolated(skill_delete_refuses_a_user_skill_and_keeps_its_files_inner());
}

async fn skill_delete_refuses_a_user_skill_and_keeps_its_files_inner() {
    let _home = IsolatedHome::new();
    let authored = crate::storage::horon_agent_dir()
        .unwrap()
        .join("skills")
        .join("user-proc");
    std::fs::create_dir_all(&authored).unwrap();
    let skill_file = authored.join("SKILL.md");
    std::fs::write(
        &skill_file,
        "---\nname: user-proc\ndescription: Authored by a person\n---\n\nBody.",
    )
    .unwrap();

    let tool = empty_tool();
    tool.registry
        .write()
        .await
        .reload_global()
        .expect("the authored skill loads");

    let error = tool
        .execute(json!({"action": "delete", "name": "user-proc"}), context())
        .await
        .expect_err("a user skill is not the agent's to delete")
        .to_string();

    assert!(
        error.contains("Skill 'user-proc' is a user skill from the horon-agent source"),
        "the refusal must name the source that owns the name, got: {error}"
    );
    assert!(skill_file.exists(), "a refused delete must leave every file in place");
}

/// A managed entry that points outside the managed directory is unlinked, and
/// the directory it points at is untouched. The loader already refuses to read
/// such an entry, so the name a delete reaches is the entry, never the target.
#[cfg(unix)]
#[test]
fn skill_delete_unlinks_a_symlink_and_keeps_its_target() {
    run_isolated(skill_delete_unlinks_a_symlink_and_keeps_its_target_inner());
}

#[cfg(unix)]
async fn skill_delete_unlinks_a_symlink_and_keeps_its_target_inner() {
    let _home = IsolatedHome::new();
    let borrowed = tempfile::tempdir().unwrap();
    let target = borrowed.path().join("borrowed");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(
        target.join("SKILL.md"),
        "---\nname: borrowed\ndescription: Someone else's work\n---\n\nKeep me.",
    )
    .unwrap();

    let managed = managed_dir();
    std::fs::create_dir_all(&managed).unwrap();
    let link = managed.join("borrowed");
    std::os::unix::fs::symlink(&target, &link).expect("symlink the managed entry");

    let tool = empty_tool();
    let deleted = tool
        .execute(json!({"action": "delete", "name": "borrowed"}), context())
        .await
        .unwrap();

    assert!(
        deleted.output.contains("Deleted managed skill 'borrowed'"),
        "{}",
        deleted.output
    );
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "the symlink entry must be gone"
    );
    assert!(
        target.join("SKILL.md").is_file(),
        "the target the link pointed at must keep every file"
    );
}

#[test]
fn skill_delete_missing_skill_reports_not_found() {
    run_isolated(skill_delete_missing_skill_reports_not_found_inner());
}

async fn skill_delete_missing_skill_reports_not_found_inner() {
    let _home = IsolatedHome::new();
    let tool = empty_tool();
    let error = tool
        .execute(json!({"action": "delete", "name": "never-written"}), context())
        .await
        .expect_err("a missing skill cannot be deleted")
        .to_string();
    assert!(
        error.contains("Managed skill 'never-written' not found"),
        "a missing skill must read as not-found, got: {error}"
    );
}

#[test]
fn skill_delete_is_refused_in_plan_mode() {
    run_isolated(skill_delete_is_refused_in_plan_mode_inner());
}

async fn skill_delete_is_refused_in_plan_mode_inner() {
    let _home = IsolatedHome::new();
    let skill_file = managed_dir().join("planned-removal").join("SKILL.md");
    std::fs::create_dir_all(skill_file.parent().unwrap()).unwrap();
    std::fs::write(
        &skill_file,
        "---\nname: planned-removal\ndescription: Kept\n---\n\nBody.",
    )
    .unwrap();

    let registry = Registry::empty();
    registry
        .register("skill_manage".to_string(), Arc::new(empty_tool()))
        .await;

    let session = "skill-delete-plan-mode";
    crate::plan_mode::clear_state(session);
    crate::plan_mode::set_state(session, crate::plan_mode::PlanState::Planning);
    let refusal = registry
        .execute(
            "skill_manage",
            json!({"action": "delete", "name": "planned-removal"}),
            ToolContext {
                session_id: session.to_string(),
                ..context()
            },
        )
        .await
        .expect_err("plan mode is read-only, so a delete must be refused");
    crate::plan_mode::clear_state(session);

    assert_eq!(
        refusal.to_string(),
        crate::plan_mode::block_message("skill_manage"),
        "the refusal must be the plan-mode message, not a tool error"
    );
    assert!(skill_file.is_file(), "a refused delete must not touch the disk");
}

#[test]
fn skill_delete_is_offered_by_the_tool_schema() {
    let tool = empty_tool();
    let schema = tool.parameters_schema();
    let actions = schema["properties"]["action"]["enum"]
        .as_array()
        .expect("the action property is an enum");
    assert!(
        actions.iter().any(|action| action.as_str() == Some("delete")),
        "the model can only send an action the schema lists, got: {actions:?}"
    );
}
