#![allow(clippy::await_holding_lock)]
use super::*;

fn context(session: &str) -> ToolContext {
    ToolContext {
        session_id: session.into(),
        message_id: "msg".into(),
        tool_call_id: "call".into(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    }
}

struct Home(Option<std::ffi::OsString>, #[allow(dead_code)] tempfile::TempDir);

impl Drop for Home {
    fn drop(&mut self) {
        match &self.0 {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
    }
}

fn temp_home() -> Home {
    let temp = tempfile::tempdir().unwrap();
    let previous = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", temp.path());
    Home(previous, temp)
}

#[tokio::test]
async fn read_write_and_delete_round_trip() {
    let _guard = crate::storage::lock_test_env();
    let home = temp_home();
    let tool = ContextNotesTool::new();
    let ctx = context("notes-session");

    let empty = tool.execute(json!({}), ctx.clone()).await.unwrap();
    assert_eq!(empty.output, "No context notes are stored for this session.");

    let stored = tool
        .execute(
            json!({"text": "- goal: finish parse_x\n- next: add tests"}),
            ctx.clone(),
        )
        .await
        .unwrap();
    assert!(stored.output.contains("Stored"), "{}", stored.output);
    assert_eq!(
        std::fs::read_to_string(crate::context_notes::notes_path("notes-session").unwrap()).unwrap(),
        "- goal: finish parse_x\n- next: add tests"
    );

    let read_back = tool.execute(json!({}), ctx.clone()).await.unwrap();
    assert_eq!(read_back.output, "- goal: finish parse_x\n- next: add tests");

    let cleared = tool.execute(json!({"text": ""}), ctx.clone()).await.unwrap();
    assert_eq!(cleared.output, "Context notes cleared.");
    assert!(!crate::context_notes::notes_path("notes-session").unwrap().exists());
    assert!(home.1.path().exists());
}

#[tokio::test]
async fn notes_over_the_limit_are_refused_with_the_byte_count() {
    let _guard = crate::storage::lock_test_env();
    let _home = temp_home();
    let tool = ContextNotesTool::new();
    let text = "x".repeat(MAX_NOTE_BYTES + 1);

    let error = tool
        .execute(json!({ "text": text }), context("oversize-notes"))
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(
        error,
        format!(
            "Notes are {} bytes; the limit is 16384. Keep a compact index and read history://current for details.",
            MAX_NOTE_BYTES + 1
        )
    );
    assert!(!crate::context_notes::notes_path("oversize-notes").unwrap().exists());
}

#[tokio::test]
async fn a_session_id_that_could_escape_the_notes_directory_is_refused() {
    let _guard = crate::storage::lock_test_env();
    let _home = temp_home();
    let tool = ContextNotesTool::new();

    let error = tool
        .execute(json!({"text": "x"}), context("../escape"))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("Invalid session id for context notes"), "{error}");
}
