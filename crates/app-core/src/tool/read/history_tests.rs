//! `history://<session-id>`: the spawn-tree boundary applied to stored transcripts.

use super::*;
use crate::tool::ToolExecutionMode;

/// Restores `HORON_HOME` and the config cache on drop.
struct HomeGuard {
    previous: Option<std::ffi::OsString>,
}

impl HomeGuard {
    fn set(home: &std::path::Path) -> Self {
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", home);
        crate::config::invalidate_config_cache();
        Self { previous }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::invalidate_config_cache();
    }
}

fn context(session_id: &str, working_dir: std::path::PathBuf) -> ToolContext {
    ToolContext {
        session_id: session_id.to_string(),
        message_id: "history-id-message".to_string(),
        tool_call_id: "history-id-call".to_string(),
        working_dir: Some(working_dir),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

fn saved_session(parent: Option<String>, text: &str) -> crate::session::Session {
    let mut session = crate::session::Session::create(parent, None);
    session.add_message(
        crate::message::Role::Assistant,
        vec![crate::message::ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    );
    session.save().expect("save the fixture session");
    session
}

#[tokio::test]
async fn a_child_transcript_is_readable_through_history_url() {
    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(home.path());
    std::fs::write(home.path().join("config.toml"), "[tools]\ncontext_notes = true\n").unwrap();
    crate::config::invalidate_config_cache();

    let parent = saved_session(None, "the coordinator's own turn");
    let child = saved_session(Some(parent.id.clone()), "the child's finding");
    let stranger = saved_session(None, "someone else's turn");

    let tool = ReadTool::new();
    let dir = tempfile::tempdir().unwrap();
    let output = tool
        .execute(
            json!({"file_path": format!("history://{}", child.id)}),
            context(&parent.id, dir.path().to_path_buf()),
        )
        .await
        .unwrap()
        .output;
    assert!(output.contains("the child's finding"), "{output}");
    assert!(
        !output.contains("the coordinator's own turn"),
        "the child transcript must not borrow the caller's messages: {output}"
    );

    let refused = tool
        .execute(
            json!({"file_path": format!("history://{}", stranger.id)}),
            context(&parent.id, dir.path().to_path_buf()),
        )
        .await
        .expect_err("a session outside the caller's spawn tree must be refused");
    let message = refused.to_string();
    assert!(message.contains("spawn tree"), "{message}");
}

#[tokio::test]
async fn an_unknown_history_id_reports_no_transcript() {
    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(home.path());
    std::fs::write(home.path().join("config.toml"), "[tools]\ncontext_notes = true\n").unwrap();
    crate::config::invalidate_config_cache();

    let parent = saved_session(None, "the coordinator's own turn");
    let tool = ReadTool::new();
    let dir = tempfile::tempdir().unwrap();
    let refused = tool
        .execute(
            json!({"file_path": "history://session_that_never_existed"}),
            context(&parent.id, dir.path().to_path_buf()),
        )
        .await
        .expect_err("an absent session has no transcript");
    assert!(refused.to_string().contains("No stored history"), "{refused}");
}
