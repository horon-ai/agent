//! Issue #60: `read memory://` serves stored records through the read tool.
//!
//! Each test writes memories through the real `memory` tool, then reads them
//! through the real `read` tool, so the assertions cover the observable
//! contract: every field on a record, a newest-first listing, a not-found for an
//! unknown id, and a refusal for a record outside the caller's scopes.

use super::*;
use crate::tool::read::ReadTool;
use crate::tool::{Tool, ToolContext, ToolEffects, ToolExecutionMode};
use serde_json::json;

fn test_ctx(working_dir: std::path::PathBuf) -> ToolContext {
    ToolContext {
        session_id: "memory-url-session".to_string(),
        message_id: "memory-url-message".to_string(),
        tool_call_id: "memory-url-call".to_string(),
        working_dir: Some(working_dir),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

struct IsolatedHome {
    _home: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
}

impl IsolatedHome {
    fn new() -> Self {
        let lock = crate::storage::lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let previous = std::env::var_os("HORON_AGENT_HOME");
        crate::env::set_var("HORON_AGENT_HOME", home.path());
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
            Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
            None => crate::env::remove_var("HORON_AGENT_HOME"),
        }
        crate::config::invalidate_config_cache();
    }
}

fn remembered_id(output: &str) -> String {
    output
        .rsplit_once("[id: ")
        .map(|(_, tail)| tail.trim_end_matches(']').to_string())
        .expect("remember prints the id")
}

async fn remember(ctx: &ToolContext, content: &str, scope: &str, tags: serde_json::Value) -> String {
    let tool = crate::tool::memory::MemoryTool::new();
    let mut input = json!({"action": "remember", "content": content, "scope": scope});
    if !tags.is_null() {
        input["tags"] = tags;
    }
    remembered_id(&tool.execute(input, ctx.clone()).await.expect("remember").output)
}

async fn read(ctx: &ToolContext, file_path: &str) -> anyhow::Result<String> {
    ReadTool::new()
        .execute(json!({"file_path": file_path}), ctx.clone())
        .await
        .map(|output| output.output)
}

#[tokio::test]
async fn memory_url_record_returns_every_field() {
    let _isolated = IsolatedHome::new();
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());

    let id = remember(
        &ctx,
        "deploys run from the release tag",
        "project",
        json!(["ops", "deploy"]),
    )
    .await;
    let rendered = read(&ctx, &format!("memory://{id}")).await.expect("record read");

    assert!(rendered.contains(&format!("id: {id}")), "{rendered}");
    assert!(rendered.contains("category: fact"), "{rendered}");
    assert!(rendered.contains("scope: project"), "{rendered}");
    assert!(rendered.contains("tags: ops, deploy"), "{rendered}");
    assert!(rendered.contains("created: "), "{rendered}");
    assert!(rendered.contains("updated: "), "{rendered}");
    assert!(rendered.contains("links: "), "{rendered}");
    assert!(rendered.contains("deploys run from the release tag"), "{rendered}");
}

#[tokio::test]
async fn memory_url_listing_is_newest_first() {
    let _isolated = IsolatedHome::new();
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());

    let oldest = remember(&ctx, "first note about the cache", "project", serde_json::Value::Null).await;
    std::thread::sleep(std::time::Duration::from_millis(5));
    let middle = remember(&ctx, "second note about the cache", "project", serde_json::Value::Null).await;
    std::thread::sleep(std::time::Duration::from_millis(5));
    let newest = remember(&ctx, "third note about the cache", "global", serde_json::Value::Null).await;

    let rendered = read(&ctx, "memory://").await.expect("listing read");
    let newest_at = rendered.find(&newest).expect("newest id listed");
    let middle_at = rendered.find(&middle).expect("middle id listed");
    let oldest_at = rendered.find(&oldest).expect("oldest id listed");
    assert!(newest_at < middle_at, "newest must come first: {rendered}");
    assert!(middle_at < oldest_at, "listing must be newest first: {rendered}");

    // One numbered line per record, with a preview rather than the whole body.
    // `read` numbers every rendered line, so each record row is a numbered
    // line whose text follows the "- " marker.
    let rows: Vec<&str> = rendered.lines().filter(|line| line.contains("\t- ")).collect();
    assert_eq!(rows.len(), 3, "{rendered}");
    let first = rows[0];
    assert!(first.contains("third note about the cache"), "{rendered}");
    assert!(first.contains("global/"), "the store is visible per row: {rendered}");
}

#[tokio::test]
async fn memory_url_unknown_id_is_not_found() {
    let _isolated = IsolatedHome::new();
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());

    let error = read(&ctx, "memory://nope").await.expect_err("an unknown id must fail");
    assert!(error.to_string().contains("not found"), "{error}");
}

#[tokio::test]
async fn memory_url_refuses_records_outside_the_session_scopes() {
    let _isolated = IsolatedHome::new();
    let other_project = tempfile::tempdir().expect("other project");
    let caller_project = tempfile::tempdir().expect("caller project");

    // Written from a different working directory: its project store is not the
    // caller's, so the record must be out of reach rather than served.
    let foreign_ctx = test_ctx(other_project.path().to_path_buf());
    let foreign = remember(
        &foreign_ctx,
        "another project's private note",
        "project",
        serde_json::Value::Null,
    )
    .await;

    let caller_ctx = test_ctx(caller_project.path().to_path_buf());
    let error = read(&caller_ctx, &format!("memory://{foreign}"))
        .await
        .expect_err("an out-of-scope record must not be served");
    assert!(error.to_string().contains("not found"), "{error}");

    let listing = read(&caller_ctx, "memory://").await.expect("listing read");
    assert!(
        !listing.contains(&foreign),
        "the foreign record must not be listed: {listing}"
    );
}

#[tokio::test]
async fn memory_url_is_paged_like_a_file_read() {
    let _isolated = IsolatedHome::new();
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());

    let id = remember(&ctx, "paged body line one", "project", serde_json::Value::Null).await;
    let mut input = json!({"file_path": format!("memory://{id}"), "limit": 2});
    let first_page = ReadTool::new()
        .execute(input.clone(), ctx.clone())
        .await
        .expect("paged read")
        .output;
    assert!(first_page.contains("id: "), "{first_page}");
    assert!(
        first_page.contains("more lines"),
        "a short page must offer a continuation: {first_page}"
    );

    // Line selectors work on the rendered record too, including the trailing
    // context a file read adds after an explicit range.
    let selected = read(&ctx, &format!("memory://{id}:1-1")).await.expect("selector read");
    let lines: Vec<&str> = selected.lines().collect();
    assert!(
        lines.len() >= 4,
        "an explicit range keeps its trailing context: {selected}"
    );
    assert!(lines[0].contains(&format!("id: {id}")), "{selected}");
    assert!(lines[1].contains("category: fact"), "{selected}");
    assert!(lines[3].contains("tags: (none)"), "{selected}");

    input["limit"] = json!(5000);
    let full = ReadTool::new()
        .execute(input, ctx.clone())
        .await
        .expect("full read")
        .output;
    assert!(full.contains("paged body line one"), "{full}");
    assert!(!full.contains("more lines"), "{full}");
}

#[tokio::test]
async fn memory_url_is_local_and_read_only() {
    let _isolated = IsolatedHome::new();
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx(project.path().to_path_buf());
    let id = remember(&ctx, "read only record", "project", serde_json::Value::Null).await;

    // A memory reference never claims the network and never becomes a file path.
    let tool = ReadTool::new();
    assert_eq!(
        tool.effects_for_input(&json!({"file_path": format!("memory://{id}")})),
        ToolEffects::read(),
        "memory:// is local data, not a network read"
    );
    assert!(!project.path().join("memory:").exists());
    assert!(
        project.path().read_dir().expect("read dir").next().is_none(),
        "a memory read must not create anything in the working directory"
    );
}
