//! Issue #63: `memory` action `update` rewrites one record in place.
//!
//! Each test drives the real `MemoryTool` against an isolated
//! `HORON_HOME` and a temporary working directory, so the assertions are
//! about what a caller observes, not about a helper's internals.

use super::*;
use crate::tool::ToolExecutionMode;
use serde_json::json;

fn test_ctx(working_dir: std::path::PathBuf) -> ToolContext {
    ToolContext {
        session_id: "memory-update-session".to_string(),
        message_id: "memory-update-message".to_string(),
        tool_call_id: "memory-update-call".to_string(),
        working_dir: Some(working_dir),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

/// Isolated agent home plus a lock on the process-wide env, restored on drop.
struct IsolatedHome {
    _home: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
}

impl IsolatedHome {
    fn new() -> Self {
        let lock = crate::storage::lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", home.path());
        crate::config::invalidate_config_cache();
        Self {
            _home: home,
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

/// The `[id: ...]` tail `remember` prints, so a test can name the record.
fn remembered_id(output: &str) -> String {
    output
        .rsplit_once("[id: ")
        .map(|(_, tail)| tail.trim_end_matches(']').to_string())
        .expect("remember prints the id")
}

async fn remember(tool: &MemoryTool, ctx: &ToolContext, content: &str, tags: serde_json::Value) -> String {
    let mut input = json!({"action": "remember", "content": content, "scope": "project"});
    if !tags.is_null() {
        input["tags"] = tags;
    }
    remembered_id(&tool.execute(input, ctx.clone()).await.expect("remember").output)
}

fn store_entry(tool: &MemoryTool, ctx: &ToolContext, id: &str) -> crate::memory::MemoryEntry {
    tool.scoped_manager(ctx)
        .load_project_graph()
        .expect("load graph")
        .get_memory(id)
        .cloned()
        .expect("record exists")
}

#[tokio::test]
async fn memory_update_keeps_id_links_and_created_at() {
    let _isolated = IsolatedHome::new();
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());
    let tool = MemoryTool::new();

    let first = remember(&tool, &ctx, "deploy notes live here", json!(["ops"])).await;
    let second = remember(&tool, &ctx, "rollback procedure", serde_json::Value::Null).await;
    tool.execute(
        json!({"action": "link", "from_id": first, "to_id": second, "weight": 0.9}),
        ctx.clone(),
    )
    .await
    .expect("link");

    let before = store_entry(&tool, &ctx, &first);
    // A stored timestamp is the observable identity of "when this was written";
    // the update must not rewrite it.
    std::thread::sleep(std::time::Duration::from_millis(5));

    let output = tool
        .execute(
            json!({"action": "update", "id": first, "content": "deploy notes changed"}),
            ctx.clone(),
        )
        .await
        .expect("update");
    assert!(output.output.starts_with("Updated "), "{}", output.output);
    assert!(output.output.contains(&format!("id: {first}")), "{}", output.output);

    let after = store_entry(&tool, &ctx, &first);
    assert_eq!(after.id, before.id, "the id must survive the update");
    assert_eq!(
        after.created_at, before.created_at,
        "created_at must survive the update"
    );
    assert!(after.updated_at > before.updated_at, "updated_at must move forward");
    assert_eq!(after.content, "deploy notes changed");
    assert_eq!(after.tags, before.tags, "untouched tags must survive");

    let graph = tool.scoped_manager(&ctx).load_project_graph().expect("load graph");
    let has_link = graph
        .edges
        .get(&first)
        .is_some_and(|edges| edges.iter().any(|edge| edge.target == second));
    assert!(has_link, "the existing link must survive a content update");
}

#[tokio::test]
async fn memory_update_redacts_secret_content() {
    let _isolated = IsolatedHome::new();
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());
    let tool = MemoryTool::new();

    let id = remember(&tool, &ctx, "harmless seed", serde_json::Value::Null).await;
    tool.execute(
        json!({
            "action": "update",
            "id": id,
            "content": "deploy with OPENROUTER_API_KEY=sk-or-v1-abcdefghijklmnopqrstuvwxyz0123456789"
        }),
        ctx.clone(),
    )
    .await
    .expect("update");

    let stored = store_entry(&tool, &ctx, &id);
    assert!(
        stored.content.contains("[REDACTED_SECRET]"),
        "stored content must be redacted, got: {}",
        stored.content
    );
    assert!(
        !stored.content.contains("sk-or-v1-abcdefghijklmnopqrstuvwxyz0123456789"),
        "the secret must never reach the store: {}",
        stored.content
    );
}

#[tokio::test]
async fn memory_update_unknown_id_returns_not_found() {
    let _isolated = IsolatedHome::new();
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());
    let tool = MemoryTool::new();

    let output = tool
        .execute(
            json!({"action": "update", "id": "mem_missing", "content": "nothing"}),
            ctx,
        )
        .await
        .expect("an unknown id is a reported miss, not a tool failure");
    assert_eq!(output.output, "Not found: mem_missing");
}

#[tokio::test]
async fn memory_update_without_fields_is_rejected() {
    let _isolated = IsolatedHome::new();
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());
    let tool = MemoryTool::new();
    let id = remember(&tool, &ctx, "unchanged", serde_json::Value::Null).await;

    let error = tool
        .execute(json!({"action": "update", "id": id}), ctx.clone())
        .await
        .expect_err("an update with no fields must be refused");
    assert!(
        error.to_string().contains("at least one of content, category, or tags"),
        "{error}"
    );

    let blank = tool
        .execute(json!({"action": "update", "id": id, "content": "   "}), ctx)
        .await
        .expect_err("a blank body must be refused");
    assert!(blank.to_string().contains("must not be empty"), "{blank}");
}

#[tokio::test]
async fn memory_update_changes_category_and_tags() {
    let _isolated = IsolatedHome::new();
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());
    let tool = MemoryTool::new();
    let id = remember(&tool, &ctx, "prefers tabs", json!(["style", "old"])).await;

    tool.execute(
        json!({"action": "update", "id": id, "category": "preference", "tags": ["style", "editor"]}),
        ctx.clone(),
    )
    .await
    .expect("update");

    let stored = store_entry(&tool, &ctx, &id);
    assert_eq!(stored.category, crate::memory::MemoryCategory::Preference);
    assert_eq!(stored.tags, vec!["style".to_string(), "editor".to_string()]);

    // The tag graph must agree with the record, not keep the dropped tag.
    let graph = tool.scoped_manager(&ctx).load_project_graph().expect("load graph");
    let tagged: Vec<&str> = graph
        .edges
        .get(&id)
        .map(|edges| edges.iter().map(|edge| edge.target.as_str()).collect())
        .unwrap_or_default();
    assert!(tagged.contains(&"tag:editor"), "{tagged:?}");
    assert!(!tagged.contains(&"tag:old"), "{tagged:?}");
}

#[tokio::test]
async fn memory_update_can_update_a_global_record() {
    let _isolated = IsolatedHome::new();
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());
    let tool = MemoryTool::new();

    let id = remembered_id(
        &tool
            .execute(
                json!({"action": "remember", "content": "global seed", "scope": "global"}),
                ctx.clone(),
            )
            .await
            .expect("remember")
            .output,
    );
    tool.execute(
        json!({"action": "update", "id": id, "content": "global rewritten"}),
        ctx.clone(),
    )
    .await
    .expect("update");

    let stored = tool
        .scoped_manager(&ctx)
        .load_global_graph()
        .expect("load graph")
        .get_memory(&id)
        .cloned()
        .expect("global record exists");
    assert_eq!(stored.content, "global rewritten");
}
