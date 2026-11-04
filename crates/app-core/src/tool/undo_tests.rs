use super::{RedoTool, UndoTool};
use crate::tool::write::WriteTool;
use crate::tool::{Tool, ToolContext, ToolExecutionMode};
use std::path::Path;

fn context(dir: &Path, session: &str, call: &str) -> ToolContext {
    ToolContext {
        session_id: session.to_string(),
        message_id: "message".to_string(),
        tool_call_id: call.to_string(),
        working_dir: Some(dir.to_path_buf()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

fn write_input(path: &str, content: &str) -> serde_json::Value {
    serde_json::json!({"file_path": path, "content": content})
}

#[tokio::test]
async fn undo_restores_original_bytes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("notes.txt");
    std::fs::write(&file, b"original bytes\n").expect("seed file");

    let ctx = context(dir.path(), "undo-restores", "write-1");
    WriteTool::new()
        .execute(write_input("notes.txt", "replacement bytes\n"), ctx.clone())
        .await
        .expect("write");
    assert_eq!(std::fs::read(&file).expect("read after write"), b"replacement bytes\n");

    UndoTool::new()
        .execute(serde_json::json!({"force": false}), ctx)
        .await
        .expect("undo");

    assert_eq!(
        std::fs::read(&file).expect("read after undo"),
        b"original bytes\n",
        "undo must restore the exact bytes the write replaced"
    );
}

#[tokio::test]
async fn undo_refuses_after_external_change_and_force_overrides() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("drifted.txt");
    std::fs::write(&file, b"before\n").expect("seed file");

    let ctx = context(dir.path(), "undo-refuses", "write-1");
    WriteTool::new()
        .execute(write_input("drifted.txt", "written by tool\n"), ctx.clone())
        .await
        .expect("write");

    std::fs::write(&file, b"edited outside the tool\n").expect("external edit");

    let error = UndoTool::new()
        .execute(serde_json::json!({"force": false}), ctx.clone())
        .await
        .expect_err("undo must refuse a stale file");
    let message = format!("{error:#}");
    assert!(
        message.contains(&file.display().to_string()),
        "refusal must name the path, got: {message}"
    );
    assert!(
        message.contains("expected sha256"),
        "refusal must state the reason, got: {message}"
    );
    assert!(
        message.contains("force: true"),
        "refusal must say how to override, got: {message}"
    );
    assert_eq!(
        std::fs::read(&file).expect("read after refusal"),
        b"edited outside the tool\n",
        "a refused undo must not clobber the externally edited file"
    );

    UndoTool::new()
        .execute(serde_json::json!({"force": true}), ctx)
        .await
        .expect("forced undo");

    assert_eq!(
        std::fs::read(&file).expect("read after forced undo"),
        b"before\n",
        "force must restore the recorded before-state"
    );
}

#[tokio::test]
async fn undo_removes_a_file_the_tool_created() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("fresh.txt");

    let ctx = context(dir.path(), "undo-created", "write-1");
    WriteTool::new()
        .execute(write_input("fresh.txt", "created\n"), ctx.clone())
        .await
        .expect("write");
    assert!(file.exists());

    UndoTool::new().execute(serde_json::json!({}), ctx).await.expect("undo");

    assert!(!file.exists(), "undoing a create must remove the file");
}

#[tokio::test]
async fn redo_reapplies_the_undone_write() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("redo.txt");
    std::fs::write(&file, b"one\n").expect("seed file");

    let ctx = context(dir.path(), "redo-reapplies", "write-1");
    WriteTool::new()
        .execute(write_input("redo.txt", "two\n"), ctx.clone())
        .await
        .expect("write");
    UndoTool::new()
        .execute(serde_json::json!({}), ctx.clone())
        .await
        .expect("undo");
    assert_eq!(std::fs::read(&file).expect("after undo"), b"one\n");

    RedoTool::new().execute(serde_json::json!({}), ctx).await.expect("redo");

    assert_eq!(std::fs::read(&file).expect("after redo"), b"two\n");
}

#[tokio::test]
async fn undo_reports_an_empty_history() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ctx = context(dir.path(), "undo-empty", "undo-1");
    let error = UndoTool::new()
        .execute(serde_json::json!({}), ctx)
        .await
        .expect_err("nothing was recorded");
    assert!(format!("{error:#}").contains("Nothing to undo"));
}
