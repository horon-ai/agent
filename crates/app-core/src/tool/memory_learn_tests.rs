//! Issue #61: `memory` action `learn` records a durable lesson.
//!
//! Every test drives the real `MemoryTool` against an isolated `HORON_HOME`
//! and a temporary working directory, so what is asserted is what a caller
//! observes: the stored record, the scope it landed in, and the text of a
//! refusal. The managed-skill cases configure `[tools] managed_skills` in that
//! same private home, because a skill write is only possible when a person
//! turned authoring on.

use super::*;
use crate::tool::ToolExecutionMode;
use serde_json::json;

fn test_ctx(working_dir: std::path::PathBuf) -> ToolContext {
    ToolContext {
        session_id: "memory-learn-session".to_string(),
        message_id: "memory-learn-message".to_string(),
        tool_call_id: "memory-learn-call".to_string(),
        working_dir: Some(working_dir),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

/// Isolated agent home plus a lock on the process-wide env, restored on drop.
/// `tools` is the `[tools]` table body, empty when the test needs no switches.
struct IsolatedHome {
    home: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
}

impl IsolatedHome {
    fn new(tools: &str) -> Self {
        let lock = crate::storage::lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", home.path());
        if !tools.is_empty() {
            std::fs::write(home.path().join("config.toml"), format!("[tools]\n{tools}")).expect("config");
        }
        crate::config::invalidate_config_cache();
        Self {
            home,
            _lock: lock,
            previous,
        }
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::invalidate_config_cache();
    }
}

/// The `[id: ...]` tail `learn` prints, so a test can name the record.
fn learned_id(output: &str) -> String {
    output
        .rsplit_once("[id: ")
        .map(|(_, tail)| tail.trim_end_matches(']').to_string())
        .expect("learn prints the id")
}

fn project_records(tool: &MemoryTool, ctx: &ToolContext) -> Vec<crate::memory::MemoryEntry> {
    tool.scoped_manager(ctx)
        .load_project_graph()
        .expect("load project graph")
        .active_memories()
        .cloned()
        .collect()
}

fn global_records(tool: &MemoryTool, ctx: &ToolContext) -> Vec<crate::memory::MemoryEntry> {
    tool.scoped_manager(ctx)
        .load_global_graph()
        .expect("load global graph")
        .active_memories()
        .cloned()
        .collect()
}

#[tokio::test]
async fn memory_learn_stores_a_lesson_a_recall_can_find() {
    let _isolated = IsolatedHome::new("");
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());
    let tool = MemoryTool::new();

    let output = tool
        .execute(
            json!({
                "action": "learn",
                "memory": "cargo test on this crate needs HORON_HOME set, or it writes into the real home",
                "context": "the failure looks like a passing test run that leaves state behind"
            }),
            ctx.clone(),
        )
        .await
        .expect("learn should store the lesson")
        .output;
    assert!(output.contains("Learned lesson (project)"), "{output}");

    let stored = project_records(&tool, &ctx);
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0].category, MemoryCategory::Lesson);
    assert!(
        stored[0]
            .content
            .starts_with("cargo test on this crate needs HORON_HOME set"),
        "{}",
        stored[0].content
    );
    assert!(
        stored[0].content.contains("Context: the failure looks like"),
        "the optional context is stored with the lesson: {}",
        stored[0].content
    );
    assert!(
        output.starts_with("Learned lesson (project): \"cargo test on this crate needs HORON_HOME set"),
        "{output}"
    );
    assert!(output.ends_with(&format!("[id: {}]", stored[0].id)), "{output}");

    // The record is reachable through the tool's own read paths, and it reads
    // back as a lesson rather than as a fact.
    let searched = tool
        .execute(
            json!({"action": "search", "query": "HORON_HOME", "scope": "project"}),
            ctx.clone(),
        )
        .await
        .expect("search")
        .output;
    assert!(searched.contains("[lesson]"), "{searched}");
    let recent = tool
        .execute(json!({"action": "recall", "scope": "project"}), ctx.clone())
        .await
        .expect("recent recall")
        .output;
    assert!(recent.contains("HORON_HOME"), "{recent}");
    assert_eq!(
        learned_id(&output),
        stored[0].id,
        "the printed id must name the stored record"
    );
}

#[tokio::test]
async fn memory_learn_rejects_the_lesson_field_name() {
    let _isolated = IsolatedHome::new("");
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());
    let tool = MemoryTool::new();

    // The field is `memory`. A call that says `lesson` would otherwise store
    // nothing at all and still report success, because serde ignores unknown
    // fields.
    let error = tool
        .execute(json!({"action": "learn", "lesson": "prefer rg over grep"}), ctx.clone())
        .await
        .expect_err("the wrong field name must be refused");
    assert!(error.to_string().contains("`memory`"), "{error}");
    assert!(project_records(&tool, &ctx).is_empty());

    // Sending both is refused for the same reason: one of the two would be
    // silently ignored.
    let both = tool
        .execute(
            json!({"action": "learn", "memory": "keep it small", "lesson": "keep it small"}),
            ctx.clone(),
        )
        .await
        .expect_err("both spellings at once must be refused");
    assert!(both.to_string().contains("`lesson`"), "{both}");
    assert!(project_records(&tool, &ctx).is_empty());

    // A missing `memory` is its own refusal, not a stored empty lesson.
    let missing = tool
        .execute(json!({"action": "learn"}), ctx)
        .await
        .expect_err("learn without memory must be refused");
    assert!(missing.to_string().contains("memory required"), "{missing}");
}

#[tokio::test]
async fn memory_learn_stores_the_same_lesson_once() {
    let _isolated = IsolatedHome::new("");
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());
    let tool = MemoryTool::new();

    let first = tool
        .execute(
            json!({"action": "learn", "memory": "run scripts/ci/check_all.sh before pushing"}),
            ctx.clone(),
        )
        .await
        .expect("first learn")
        .output;
    let second = tool
        .execute(
            json!({"action": "learn", "memory": "  run scripts/ci/check_all.sh before pushing  "}),
            ctx.clone(),
        )
        .await
        .expect("second learn")
        .output;

    assert_eq!(
        learned_id(&first),
        learned_id(&second),
        "the same lesson must reinforce the existing record"
    );
    let stored = project_records(&tool, &ctx);
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0].strength, 2, "the repeat reinforces the record it found");
}

#[tokio::test]
async fn memory_learn_redacts_a_secret_in_the_lesson() {
    let _isolated = IsolatedHome::new("");
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());
    let tool = MemoryTool::new();
    let secret = "sk-or-v1-abcdefghijklmnopqrstuvwxyz0123456789";

    tool.execute(
        json!({
            "action": "learn",
            "memory": format!("this endpoint needs OPENROUTER_API_KEY={secret} in the environment"),
            "context": format!("curl reports 401 until the key is exported; leaked one was {secret}")
        }),
        ctx.clone(),
    )
    .await
    .expect("learn");

    let stored = project_records(&tool, &ctx);
    assert_eq!(stored.len(), 1);
    assert!(
        stored[0].content.contains("[REDACTED_SECRET]"),
        "the stored lesson must be redacted: {}",
        stored[0].content
    );
    assert!(
        !stored[0].content.contains(secret),
        "a secret must never reach the store: {}",
        stored[0].content
    );
}

#[tokio::test]
async fn memory_learn_scope_defaults_to_project_and_honors_global() {
    let _isolated = IsolatedHome::new("");
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());
    let tool = MemoryTool::new();

    tool.execute(
        json!({"action": "learn", "memory": "default-scope lesson"}),
        ctx.clone(),
    )
    .await
    .expect("learn with no scope");
    tool.execute(
        json!({"action": "learn", "memory": "global-scope lesson", "scope": "global"}),
        ctx.clone(),
    )
    .await
    .expect("learn into global");

    let stored_in_project = project_records(&tool, &ctx);
    let stored_globally = global_records(&tool, &ctx);
    assert_eq!(stored_in_project.len(), 1, "{stored_in_project:?}");
    assert!(stored_in_project[0].content.contains("default-scope lesson"));
    assert_eq!(stored_globally.len(), 1, "{stored_globally:?}");
    assert!(stored_globally[0].content.contains("global-scope lesson"));

    // The global store is the exact place `remember_global` writes, and a
    // different project cannot see it as its own record.
    tool.execute(
        json!({"action": "remember", "content": "remember-global neighbour", "scope": "global"}),
        ctx.clone(),
    )
    .await
    .expect("remember global");
    assert_eq!(global_records(&tool, &ctx).len(), 2);
    let other_project = tempfile::tempdir().expect("other project");
    let other = project_records(&tool, &test_ctx(other_project.path().to_path_buf()));
    assert!(other.is_empty(), "another project has its own store: {other:?}");

    // `all` is a read scope; a lesson must land in exactly one store.
    let error = tool
        .execute(
            json!({"action": "learn", "memory": "no scope to write", "scope": "all"}),
            ctx,
        )
        .await
        .expect_err("learn cannot write to the read-only `all` scope");
    assert!(error.to_string().contains("project or global"), "{error}");
}

#[tokio::test]
async fn memory_learn_refuses_text_over_the_length_caps() {
    let _isolated = IsolatedHome::new("");
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());
    let tool = MemoryTool::new();

    let at_cap = "a".repeat(MAX_LESSON_CHARS);
    tool.execute(json!({"action": "learn", "memory": at_cap}), ctx.clone())
        .await
        .expect("a lesson exactly at the cap is accepted");

    let over_cap = "a".repeat(MAX_LESSON_CHARS + 1);
    let error = tool
        .execute(json!({"action": "learn", "memory": over_cap}), ctx.clone())
        .await
        .expect_err("a lesson over the cap must be refused");
    assert!(error.to_string().contains("2000 characters"), "{error}");

    let context_at_cap = "c".repeat(MAX_LESSON_CONTEXT_CHARS);
    tool.execute(
        json!({"action": "learn", "memory": "context at cap", "context": context_at_cap}),
        ctx.clone(),
    )
    .await
    .expect("a context exactly at the cap is accepted");

    let context_over_cap = "c".repeat(MAX_LESSON_CONTEXT_CHARS + 1);
    let error = tool
        .execute(
            json!({"action": "learn", "memory": "context over cap", "context": context_over_cap}),
            ctx.clone(),
        )
        .await
        .expect_err("a context over the cap must be refused");
    assert!(error.to_string().contains("400 characters"), "{error}");

    // The refusals wrote nothing, and the accepted lessons are the only records.
    let stored = project_records(&tool, &ctx);
    assert_eq!(stored.len(), 2, "{stored:?}");
}

#[tokio::test]
async fn memory_learn_creates_and_updates_a_managed_skill() {
    let isolated = IsolatedHome::new("managed_skills = true\n");
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());
    let tool = MemoryTool::new();

    let output = tool
        .execute(
            json!({
                "action": "learn",
                "memory": "regenerate the client SDK after any protocol change",
                "skill": {
                    "action": "create",
                    "name": "regenerate-sdk",
                    "description": "Regenerate the client SDK",
                    "body": "# Steps\n\n1. Run the generator."
                }
            }),
            ctx.clone(),
        )
        .await
        .expect("learn with a skill")
        .output;
    assert!(output.contains("Learned lesson"), "{output}");
    assert!(output.contains("Created managed skill 'regenerate-sdk'"), "{output}");

    let skill_file = isolated.home.path().join("managed-skills/regenerate-sdk/SKILL.md");
    let content = std::fs::read_to_string(&skill_file).expect("the skill file exists");
    assert!(content.contains("description: Regenerate the client SDK"), "{content}");
    assert!(content.contains("1. Run the generator."), "{content}");

    let updated = tool
        .execute(
            json!({
                "action": "learn",
                "memory": "regenerate the client SDK, then run the formatter",
                "skill": {
                    "action": "update",
                    "name": "regenerate-sdk",
                    "description": "Regenerate the client SDK",
                    "body": "# Steps\n\n1. Run the generator.\n2. Run the formatter."
                }
            }),
            ctx.clone(),
        )
        .await
        .expect("learn with a skill update")
        .output;
    assert!(updated.contains("Updated managed skill 'regenerate-sdk'"), "{updated}");
    let content = std::fs::read_to_string(&skill_file).expect("the skill file still exists");
    assert!(content.contains("2. Run the formatter."), "{content}");

    // The skill is not loaded into this session: `learn` does not reload the
    // skills list, so a failed write cannot half-apply a prompt change.
    assert_eq!(project_records(&tool, &ctx).len(), 2);
}

#[tokio::test]
async fn memory_learn_keeps_the_lesson_when_authoring_is_off() {
    let _isolated = IsolatedHome::new("");
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());
    let tool = MemoryTool::new();

    // `[tools] managed_skills` is off by default, so authoring is refused. The
    // lesson is still the point of the call and must survive.
    let output = tool
        .execute(
            json!({
                "action": "learn",
                "memory": "always check the exit code of a piped command",
                "skill": {
                    "action": "create",
                    "name": "check-exit-code",
                    "description": "Check exit codes",
                    "body": "# Steps\n\n1. Look at $?."
                }
            }),
            ctx.clone(),
        )
        .await
        .expect("a refused skill must not fail the lesson")
        .output;
    assert!(output.contains("Learned lesson"), "{output}");
    assert!(output.contains("Skill not written"), "{output}");
    assert!(output.contains("managed_skills"), "{output}");
    assert_eq!(project_records(&tool, &ctx).len(), 1);
}

#[tokio::test]
async fn memory_learn_reports_an_invalid_skill_name_and_keeps_the_lesson() {
    let isolated = IsolatedHome::new("managed_skills = true\n");
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());
    let tool = MemoryTool::new();

    let output = tool
        .execute(
            json!({
                "action": "learn",
                "memory": "name a skill in lowercase with hyphens",
                "skill": {"action": "create", "name": "Not A Name", "description": "d", "body": "b"}
            }),
            ctx.clone(),
        )
        .await
        .expect("an invalid skill name must not fail the lesson")
        .output;
    assert!(output.contains("Learned lesson"), "{output}");
    assert!(output.contains("Invalid skill name"), "{output}");
    assert_eq!(project_records(&tool, &ctx).len(), 1);
    assert!(
        !isolated.home.path().join("managed-skills").exists(),
        "nothing is written for a refused skill"
    );
}

#[tokio::test]
async fn memory_learn_reports_a_skill_without_a_name_and_keeps_the_lesson() {
    let isolated = IsolatedHome::new("managed_skills = true\n");
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());
    let tool = MemoryTool::new();

    // The schema requires `skill.name`, but a caller that sends only `action`
    // must be told what is missing instead of writing a nameless skill.
    let output = tool
        .execute(
            json!({
                "action": "learn",
                "memory": "name the skill you want to write",
                "skill": {"action": "create"}
            }),
            ctx.clone(),
        )
        .await
        .expect("a skill object without a name must not fail the lesson")
        .output;
    assert!(output.contains("Learned lesson"), "{output}");
    assert!(output.contains("`skill.name` is required"), "{output}");
    assert_eq!(project_records(&tool, &ctx).len(), 1);
    assert!(
        !isolated.home.path().join("managed-skills").exists(),
        "nothing is written without a name"
    );
}

#[tokio::test]
async fn memory_learn_ranks_ahead_of_a_fact_of_the_same_age() {
    let _isolated = IsolatedHome::new("");
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());
    let tool = MemoryTool::new();

    let stamp = chrono::Utc::now();
    for (category, content) in [
        (MemoryCategory::Fact, "the build script lives in scripts/ci"),
        (
            MemoryCategory::Lesson,
            "run the build script with an isolated HORON_HOME",
        ),
    ] {
        let mut entry = MemoryEntry::new(category, content);
        entry.created_at = stamp;
        entry.updated_at = stamp;
        tool.scoped_manager(&ctx)
            .remember_project(entry)
            .expect("store the record");
    }

    // Same timestamp, same trust, never recalled: relevance ties, so what is
    // left to separate them is the category. The rendered recall puts the
    // Lessons section ahead of Facts.
    let rendered = tool
        .execute(json!({"action": "recall", "scope": "project", "limit": 5}), ctx.clone())
        .await
        .expect("recent recall")
        .output;
    let lesson_at = rendered.find("## Lessons").expect("a Lessons section");
    let fact_at = rendered.find("## Facts").expect("a Facts section");
    assert!(lesson_at < fact_at, "{rendered}");

    // The score a store ranks by follows the same order, which is what keeps
    // the section order from being a display-only rule.
    let lesson = MemoryEntry::new(MemoryCategory::Lesson, "same age");
    let fact = MemoryEntry::new(MemoryCategory::Fact, "same age");
    assert!(
        crate::memory_types::memory_score(&lesson) > crate::memory_types::memory_score(&fact),
        "a lesson must outrank a fact"
    );
}
