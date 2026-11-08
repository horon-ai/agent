//! `post_tool_feedback` replacing, flagging and annotating a tool result.
//!
//! The hook decides what the model sees; the file on disk and the undo journal
//! are never part of that decision.
//!
//! Unix only: every case drives a real hook script.
#![cfg(unix)]

use super::*;

use crate::tool::ToolOutput;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

/// Environment overrides for one test, restored on drop.
struct HookEnv {
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl HookEnv {
    fn install(values: &[(&'static str, Option<String>)]) -> Self {
        let previous = values
            .iter()
            .map(|(key, _)| (*key, std::env::var_os(key)))
            .collect::<Vec<_>>();
        for (key, value) in values {
            match value {
                Some(value) => crate::env::set_var(key, value),
                None => crate::env::remove_var(key),
            }
        }
        crate::config::invalidate_config_cache();
        Self { previous }
    }
}

impl Drop for HookEnv {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..) {
            match value {
                Some(value) => crate::env::set_var(key, value),
                None => crate::env::remove_var(key),
            }
        }
        crate::config::invalidate_config_cache();
    }
}

/// A provider the read tool never reaches: these tests only run tools.
struct NoopProvider;

#[async_trait::async_trait]
impl crate::provider::Provider for NoopProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<crate::provider::EventStream> {
        Err(anyhow::anyhow!("the feedback tests never reach the provider"))
    }

    fn name(&self) -> &str {
        "feedback-replace-probe"
    }

    fn fork(&self) -> Arc<dyn crate::provider::Provider> {
        Arc::new(NoopProvider)
    }
}

fn write_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write hook script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod hook script");
    path
}

fn feedback_env(scripts: &[std::path::PathBuf]) -> HookEnv {
    let commands = serde_json::to_string(
        &scripts
            .iter()
            .map(|script| script.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
    )
    .expect("serialize the hook commands");
    HookEnv::install(&[
        ("HORON_AGENT_HOOK_POST_TOOL_FEEDBACK", Some(commands)),
        (
            "HORON_AGENT_HOOK_POST_TOOL_FEEDBACK_TIMEOUT_MS",
            Some("5000".to_string()),
        ),
    ])
}

fn test_context(working_dir: &std::path::Path, tool_call_id: &str) -> ToolContext {
    ToolContext {
        session_id: "post-tool-feedback-replace".to_string(),
        message_id: "message".to_string(),
        tool_call_id: tool_call_id.to_string(),
        working_dir: Some(working_dir.to_path_buf()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

/// The blocks the model reads for one tool result.
fn blocks(tool_call_id: &str, output: ToolOutput) -> Vec<ContentBlock> {
    tool_output_to_content_blocks(tool_call_id.to_string(), output)
}

fn tool_result_text(block: &ContentBlock) -> String {
    match block {
        ContentBlock::ToolResult { content, is_error, .. } => format!("{content}\u{0}{is_error:?}"),
        ContentBlock::Text { text, .. } => format!("text:{text}"),
        other => format!("other:{other:?}"),
    }
}

async fn run_read(dir: &std::path::Path, file: &str, tool_call_id: &str) -> ToolOutput {
    let provider: Arc<dyn crate::provider::Provider> = Arc::new(NoopProvider);
    let registry = Registry::new(provider).await;
    // `Registry::execute` returns the full output: the metadata is where the
    // hook's error flag and note ride, so the tests read it, not just the text.
    registry
        .execute(
            "read",
            serde_json::json!({ "file_path": file }),
            test_context(dir, tool_call_id),
        )
        .await
        .expect("read succeeds")
}

#[tokio::test]
async fn post_tool_feedback_replace_content_replaces_what_the_model_sees() {
    let _guard = crate::storage::lock_test_env();
    let dir = tempfile::TempDir::new().expect("temp dir");
    std::fs::write(dir.path().join("secret.txt"), "token=abc123\n").expect("write target");
    let script = write_script(
        dir.path(),
        "redact.sh",
        "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"content\":\"redacted\"}'\n",
    );
    let _env = feedback_env(&[script]);

    let output = run_read(dir.path(), "secret.txt", "call_replace").await;
    let rendered = blocks("call_replace", output)
        .iter()
        .map(tool_result_text)
        .collect::<Vec<_>>();

    assert_eq!(
        rendered,
        vec!["redacted\u{0}None".to_string()],
        "the model sees the replacement and nothing else"
    );
}

#[tokio::test]
async fn post_tool_feedback_replace_is_error_marks_the_result() {
    let _guard = crate::storage::lock_test_env();
    let dir = tempfile::TempDir::new().expect("temp dir");
    std::fs::write(dir.path().join("notes.txt"), "all good\n").expect("write target");
    let script = write_script(
        dir.path(),
        "flag.sh",
        "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"is_error\":true}'\n",
    );
    let _env = feedback_env(&[script]);

    let output = run_read(dir.path(), "notes.txt", "call_error").await;
    let rendered = blocks("call_error", output)
        .iter()
        .map(tool_result_text)
        .collect::<Vec<_>>();

    assert_eq!(rendered.len(), 1, "one tool result and nothing else: {rendered:?}");
    assert!(
        rendered[0].contains("all good"),
        "the text is kept when only the flag is set: {rendered:?}"
    );
    assert!(
        rendered[0].ends_with("Some(true)"),
        "the result carries the error flag: {rendered:?}"
    );
}

#[tokio::test]
async fn post_tool_feedback_replace_plain_text_still_appends() {
    let _guard = crate::storage::lock_test_env();
    let dir = tempfile::TempDir::new().expect("temp dir");
    std::fs::write(dir.path().join("prose.md"), "hello\n").expect("write target");
    let script = write_script(
        dir.path(),
        "lint.sh",
        "#!/bin/sh\ncat >/dev/null\nprintf 'prose.md:1: style\\n'\n",
    );
    let _env = feedback_env(&[script]);

    let output = run_read(dir.path(), "prose.md", "call_plain").await;

    assert!(
        output.output.contains("hello"),
        "plain feedback keeps the result: {}",
        output.output
    );
    assert!(
        output.output.contains("[post_tool_feedback]\nprose.md:1: style"),
        "plain feedback keeps the existing header: {}",
        output.output
    );
    assert!(
        !crate::tool::post_tool_feedback::is_error(&output),
        "plain feedback does not flag the result"
    );
}

#[tokio::test]
async fn post_tool_feedback_replace_note_is_a_separate_reminder() {
    let _guard = crate::storage::lock_test_env();
    let dir = tempfile::TempDir::new().expect("temp dir");
    std::fs::write(dir.path().join("data.txt"), "payload\n").expect("write target");
    let script = write_script(
        dir.path(),
        "note.sh",
        "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"note\":\"this output is stale\"}'\n",
    );
    let _env = feedback_env(&[script]);

    let output = run_read(dir.path(), "data.txt", "call_note").await;
    let rendered = blocks("call_note", output)
        .iter()
        .map(tool_result_text)
        .collect::<Vec<_>>();

    assert_eq!(rendered.len(), 2, "the result plus one reminder: {rendered:?}");
    assert!(
        rendered[0].contains("payload"),
        "the result text is untouched: {rendered:?}"
    );
    assert_eq!(
        rendered[1], "text:<system-reminder>\nthis output is stale\n</system-reminder>",
        "the note is a separate system reminder after the result"
    );
}

#[tokio::test]
async fn post_tool_feedback_replace_hooks_apply_in_order() {
    let _guard = crate::storage::lock_test_env();
    let dir = tempfile::TempDir::new().expect("temp dir");
    std::fs::write(dir.path().join("order.txt"), "original\n").expect("write target");
    let first = write_script(
        dir.path(),
        "first.sh",
        "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"content\":\"first\"}'\n",
    );
    // The second hook only appends when it was handed the first hook's text.
    let second = write_script(
        dir.path(),
        "second.sh",
        "#!/bin/sh\ncat >/dev/null\ncase \"$HORON_AGENT_HOOK_OUTPUT\" in first) printf 'second' ;; *) printf 'missing-first' ;; esac\n",
    );
    let _env = feedback_env(&[first, second]);

    let output = run_read(dir.path(), "order.txt", "call_order").await;

    assert_eq!(
        output.output, "first\n\n[post_tool_feedback]\nsecond",
        "each hook sees the result after the previous one"
    );
}

#[tokio::test]
async fn post_tool_feedback_replace_leaves_the_file_untouched() {
    let _guard = crate::storage::lock_test_env();
    let dir = tempfile::TempDir::new().expect("temp dir");
    let target = dir.path().join("keep.txt");
    std::fs::write(&target, "keep this on disk\n").expect("write target");
    let script = write_script(
        dir.path(),
        "redact.sh",
        "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"content\":\"gone\"}'\n",
    );
    let _env = feedback_env(&[script]);

    let output = run_read(dir.path(), "keep.txt", "call_disk").await;

    assert_eq!(output.output, "gone", "the model sees the replacement");
    assert_eq!(
        std::fs::read_to_string(&target).expect("read the target"),
        "keep this on disk\n",
        "the hook changes the model's view, never the file"
    );
}

/// A redaction is not an edit: no journal, backup or spill file appears.
#[tokio::test]
async fn post_tool_feedback_replace_does_not_touch_the_undo_journal() {
    let _guard = crate::storage::lock_test_env();
    let dir = tempfile::TempDir::new().expect("temp dir");
    let target = dir.path().join("journal.txt");
    std::fs::write(&target, "journal stays empty\n").expect("write target");
    let script = write_script(
        dir.path(),
        "redact.sh",
        "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"content\":\"gone\",\"note\":\"redacted\"}'\n",
    );
    let _env = feedback_env(&[script]);

    let _ = run_read(dir.path(), "journal.txt", "call_journal").await;

    let mut entries = std::fs::read_dir(dir.path())
        .expect("read the working directory")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    entries.sort();
    assert_eq!(
        entries,
        vec!["journal.txt".to_string(), "redact.sh".to_string()],
        "the hook leaves no journal or backup files behind"
    );
    assert_eq!(
        std::fs::read_to_string(&target).expect("read the target"),
        "journal stays empty\n"
    );
}

/// A tool result carrying no feedback metadata keeps the pre-change shape.
#[test]
fn post_tool_feedback_replace_absent_metadata_keeps_the_result_plain() {
    let output = ToolOutput::new("plain result");
    let rendered = blocks("call_plain_meta", output)
        .iter()
        .map(tool_result_text)
        .collect::<Vec<_>>();
    assert_eq!(rendered, vec!["plain result\u{0}None".to_string()]);
}
