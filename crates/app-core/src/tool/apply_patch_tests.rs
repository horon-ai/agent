#![allow(clippy::await_holding_lock)]
use super::*;
use crate::protocol::AcpClientCapabilities;
use crate::tool::client_fs::test_support::{FakeClient, Reply};
use crate::tool::undo::UndoTool;
use serde_json::json;
use std::io::Write;
use std::path::Path;
use tempfile::NamedTempFile;

fn write_temp(content: &str) -> NamedTempFile {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f
}

fn local_ctx(file: &NamedTempFile) -> crate::tool::ToolContext {
    crate::tool::ToolContext {
        session_id: "apply-patch-update-chunks".to_string(),
        message_id: "apply-patch-update-chunks".to_string(),
        tool_call_id: "apply-patch-update-chunks".to_string(),
        working_dir: file.path().parent().map(std::path::Path::to_path_buf),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    }
}

#[test]
fn test_parse_add_file() {
    let patch = "*** Begin Patch\n*** Add File: hello.txt\n+Hello world\n+Second line\n*** End Patch";
    let hunks = parse_apply_patch(patch).unwrap();
    assert_eq!(hunks.len(), 1);
    match &hunks[0] {
        PatchHunk::AddFile { path, contents } => {
            assert_eq!(path, "hello.txt");
            assert_eq!(contents, "Hello world\nSecond line\n");
        }
        _ => panic!("Expected AddFile"),
    }
}

#[test]
fn test_parse_delete_file() {
    let patch = "*** Begin Patch\n*** Delete File: old.txt\n*** End Patch";
    let hunks = parse_apply_patch(patch).unwrap();
    assert_eq!(hunks.len(), 1);
    match &hunks[0] {
        PatchHunk::DeleteFile { path } => {
            assert_eq!(path, "old.txt");
        }
        _ => panic!("Expected DeleteFile"),
    }
}

#[test]
fn test_parse_update_file_simple() {
    let patch = "*** Begin Patch\n*** Update File: test.py\n@@\n foo\n-bar\n+baz\n*** End Patch";
    let hunks = parse_apply_patch(patch).unwrap();
    assert_eq!(hunks.len(), 1);
    match &hunks[0] {
        PatchHunk::UpdateFile { path, chunks, .. } => {
            assert_eq!(path, "test.py");
            assert_eq!(chunks.len(), 1);
            assert_eq!(chunks[0].old_lines, vec!["foo", "bar"]);
            assert_eq!(chunks[0].new_lines, vec!["foo", "baz"]);
        }
        _ => panic!("Expected UpdateFile"),
    }
}

#[test]
fn test_parse_update_with_context() {
    let patch =
        "*** Begin Patch\n*** Update File: test.py\n@@ def my_func():\n-    pass\n+    return 42\n*** End Patch";
    let hunks = parse_apply_patch(patch).unwrap();
    match &hunks[0] {
        PatchHunk::UpdateFile { chunks, .. } => {
            assert_eq!(chunks[0].change_context, Some("def my_func():".to_string()));
            assert_eq!(chunks[0].old_lines, vec!["    pass"]);
            assert_eq!(chunks[0].new_lines, vec!["    return 42"]);
        }
        _ => panic!("Expected UpdateFile"),
    }
}

#[test]
fn test_parse_update_with_move() {
    let patch =
        "*** Begin Patch\n*** Update File: old.py\n*** Move to: new.py\n@@\n-old_line\n+new_line\n*** End Patch";
    let hunks = parse_apply_patch(patch).unwrap();
    match &hunks[0] {
        PatchHunk::UpdateFile { path, move_to, chunks } => {
            assert_eq!(path, "old.py");
            assert_eq!(move_to, &Some("new.py".to_string()));
            assert_eq!(chunks.len(), 1);
        }
        _ => panic!("Expected UpdateFile"),
    }
}

#[test]
fn test_parse_multiple_chunks() {
    let patch = "*** Begin Patch\n*** Update File: test.py\n@@\n foo\n-bar\n+BAR\n@@\n baz\n-qux\n+QUX\n*** End Patch";
    let hunks = parse_apply_patch(patch).unwrap();
    match &hunks[0] {
        PatchHunk::UpdateFile { chunks, .. } => {
            assert_eq!(chunks.len(), 2);
            assert_eq!(chunks[0].old_lines, vec!["foo", "bar"]);
            assert_eq!(chunks[0].new_lines, vec!["foo", "BAR"]);
            assert_eq!(chunks[1].old_lines, vec!["baz", "qux"]);
            assert_eq!(chunks[1].new_lines, vec!["baz", "QUX"]);
        }
        _ => panic!("Expected UpdateFile"),
    }
}

#[test]
fn test_parse_end_of_file() {
    let patch =
        "*** Begin Patch\n*** Update File: test.py\n@@\n last_line\n+new_last_line\n*** End of File\n*** End Patch";
    let hunks = parse_apply_patch(patch).unwrap();
    match &hunks[0] {
        PatchHunk::UpdateFile { chunks, .. } => {
            assert!(chunks[0].is_end_of_file);
        }
        _ => panic!("Expected UpdateFile"),
    }
}

#[tokio::test]
async fn test_apply_update_simple() {
    let f = write_temp("foo\nbar\n");
    let chunks = vec![UpdateFileChunk {
        change_context: None,
        old_lines: vec!["foo".to_string(), "bar".to_string()],
        new_lines: vec!["foo".to_string(), "baz".to_string()],
        is_end_of_file: false,
    }];
    let (old_result, new_result) = apply_update_chunks(f.path(), &chunks, &local_ctx(&f)).await.unwrap();
    assert_eq!(old_result, "foo\nbar\n");
    assert_eq!(new_result, "foo\nbaz\n");
}

#[tokio::test]
async fn test_apply_update_multiple_chunks() {
    let f = write_temp("foo\nbar\nbaz\nqux\n");
    let chunks = vec![
        UpdateFileChunk {
            change_context: None,
            old_lines: vec!["foo".to_string(), "bar".to_string()],
            new_lines: vec!["foo".to_string(), "BAR".to_string()],
            is_end_of_file: false,
        },
        UpdateFileChunk {
            change_context: None,
            old_lines: vec!["baz".to_string(), "qux".to_string()],
            new_lines: vec!["baz".to_string(), "QUX".to_string()],
            is_end_of_file: false,
        },
    ];
    let (old_result, new_result) = apply_update_chunks(f.path(), &chunks, &local_ctx(&f)).await.unwrap();
    assert_eq!(old_result, "foo\nbar\nbaz\nqux\n");
    assert_eq!(new_result, "foo\nBAR\nbaz\nQUX\n");
}

#[tokio::test]
async fn test_apply_update_with_context_header() {
    let f = write_temp("class Foo:\n    def bar(self):\n        pass\n    def baz(self):\n        pass\n");
    let chunks = vec![UpdateFileChunk {
        change_context: Some("def baz(self):".to_string()),
        old_lines: vec!["        pass".to_string()],
        new_lines: vec!["        return 42".to_string()],
        is_end_of_file: false,
    }];
    let (_old_result, new_result) = apply_update_chunks(f.path(), &chunks, &local_ctx(&f)).await.unwrap();
    assert_eq!(
        new_result,
        "class Foo:\n    def bar(self):\n        pass\n    def baz(self):\n        return 42\n"
    );
}

#[tokio::test]
async fn test_apply_update_append_at_eof() {
    let f = write_temp("foo\nbar\nbaz\n");
    let chunks = vec![UpdateFileChunk {
        change_context: None,
        old_lines: vec![],
        new_lines: vec!["quux".to_string()],
        is_end_of_file: false,
    }];
    let (_old_result, new_result) = apply_update_chunks(f.path(), &chunks, &local_ctx(&f)).await.unwrap();
    assert_eq!(new_result, "foo\nbar\nbaz\nquux\n");
}

#[test]
fn test_generate_diff_summary_compact_format() {
    let old = "line one\nline two\nline three\n";
    let new = "line one\nchanged two\nline three\n";
    let diff = generate_diff_summary(old, new);

    assert!(diff.contains("2- line two"));
    assert!(diff.contains("2+ changed two"));
    assert!(!diff.contains("line one"));
}

#[test]
fn test_seek_sequence_exact() {
    let lines: Vec<String> = vec!["foo", "bar", "baz"].into_iter().map(String::from).collect();
    let pattern: Vec<String> = vec!["bar", "baz"].into_iter().map(String::from).collect();
    assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(1));
}

#[test]
fn test_seek_sequence_whitespace_tolerant() {
    let lines: Vec<String> = vec!["foo   ", "bar\t"].into_iter().map(String::from).collect();
    let pattern: Vec<String> = vec!["foo", "bar"].into_iter().map(String::from).collect();
    assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
}

#[test]
fn test_seek_sequence_eof() {
    let lines: Vec<String> = vec!["a", "b", "c", "d"].into_iter().map(String::from).collect();
    let pattern: Vec<String> = vec!["c", "d"].into_iter().map(String::from).collect();
    assert_eq!(seek_sequence(&lines, &pattern, 0, true), Some(2));
}

#[test]
fn test_parse_no_begin() {
    let result = parse_apply_patch("random text");
    assert!(result.is_err());
}

#[test]
fn test_parse_heredoc_wrapper() {
    let patch = "<<'EOF'\n*** Begin Patch\n*** Add File: test.txt\n+hello\n*** End Patch\nEOF";
    let hunks = parse_apply_patch(patch).unwrap();
    assert_eq!(hunks.len(), 1);
}

#[test]
fn test_parse_update_without_explicit_at() {
    let patch = "*** Begin Patch\n*** Update File: file.py\n import foo\n+bar\n*** End Patch";
    let hunks = parse_apply_patch(patch).unwrap();
    match &hunks[0] {
        PatchHunk::UpdateFile { chunks, .. } => {
            assert_eq!(chunks.len(), 1);
            assert!(chunks[0].change_context.is_none());
        }
        _ => panic!("Expected UpdateFile"),
    }
}

// Issue #604: apply_patch can delete by absolute path, so it is a second route
// to the same damage the bash gate blocks. `ToolContext::resolve_path` passes
// absolute paths through unchanged, so nothing else bounds it.

#[tokio::test]
async fn apply_patch_refuses_to_delete_a_protected_path() {
    let temp = tempfile::tempdir().expect("temp home");
    let home = temp.path().to_path_buf();
    let previous = std::env::var("HOME").ok();
    // SAFETY: single-threaded test setup; restored below.
    unsafe { std::env::set_var("HOME", &home) };

    // A credential file inside the protected ~/.ssh directory.
    let ssh = home.join(".ssh");
    std::fs::create_dir_all(&ssh).expect("ssh dir");
    let key = ssh.join("id_ed25519");
    std::fs::write(&key, "PRIVATE KEY").expect("key");

    let patch = format!("*** Begin Patch\n*** Delete File: {}\n*** End Patch", key.display());
    let result = ApplyPatchTool
        .execute(
            serde_json::json!({ "patch_text": patch }),
            ToolContext {
                session_id: "patch-gate".to_string(),
                message_id: "m".to_string(),
                tool_call_id: "c".to_string(),
                working_dir: Some(std::path::PathBuf::from("/tmp")),
                stdin_request_tx: None,
                graceful_shutdown_signal: None,
                execution_mode: crate::tool::ToolExecutionMode::Direct,
            },
        )
        .await;

    match previous {
        Some(value) => unsafe { std::env::set_var("HOME", value) },
        None => unsafe { std::env::remove_var("HOME") },
    }

    let output = result.expect("the tool should report, not error out");
    assert!(
        format!("{output:?}").contains("refused"),
        "expected a refusal in the output: {output:?}"
    );
    assert!(key.exists(), "apply_patch must not delete a protected credential file");
}

#[tokio::test]
async fn apply_patch_still_deletes_ordinary_files() {
    // The guard must not break the tool's normal job.
    let temp = tempfile::tempdir().expect("temp dir");
    let target = temp.path().join("obsolete.rs");
    std::fs::write(&target, "fn old() {}\n").expect("file");

    let patch = format!("*** Begin Patch\n*** Delete File: {}\n*** End Patch", target.display());
    ApplyPatchTool
        .execute(
            serde_json::json!({ "patch_text": patch }),
            ToolContext {
                session_id: "patch-ok".to_string(),
                message_id: "m".to_string(),
                tool_call_id: "c".to_string(),
                working_dir: Some(temp.path().to_path_buf()),
                stdin_request_tx: None,
                graceful_shutdown_signal: None,
                execution_mode: crate::tool::ToolExecutionMode::Direct,
            },
        )
        .await
        .expect("ordinary delete should succeed");

    assert!(!target.exists(), "an ordinary file should still be deleted");
}

// A non-ACP turn keeps the plain-disk behavior on a file the text path cannot
// read: `text_snapshot` is None for non-UTF-8 content, so the existence of the
// file has to come from the filesystem. Otherwise an overwrite of such a file
// is counted as a new file with exact counts instead of an approximate
// overwrite.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn add_file_over_a_non_utf8_file_records_an_approximate_edit() {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("temp home");
    struct Home(Option<std::ffi::OsString>);
    impl Drop for Home {
        fn drop(&mut self) {
            if let Some(old) = self.0.take() {
                crate::env::set_var("HORON_HOME", old);
            } else {
                crate::env::remove_var("HORON_HOME");
            }
        }
    }
    let _home = Home(std::env::var_os("HORON_HOME"));
    crate::env::set_var("HORON_HOME", home.path());

    let dir = tempfile::tempdir().expect("work dir");
    let target = dir.path().join("legacy.txt");
    // 0xFF is not valid UTF-8, so this file has no text snapshot.
    std::fs::write(&target, [0x6cu8, 0x69, 0x6e, 0x65, 0xff, 0x0a]).expect("non-UTF-8 file");

    let session = "apply-patch-addfile-nonutf8";
    let ctx = ToolContext {
        session_id: session.to_string(),
        message_id: "m".to_string(),
        tool_call_id: "c".to_string(),
        working_dir: Some(dir.path().to_path_buf()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    };
    let patch = format!(
        "*** Begin Patch\n*** Add File: {}\n+replacement\n*** End Patch",
        target.display()
    );
    ApplyPatchTool
        .execute(serde_json::json!({ "patch_text": patch }), ctx)
        .await
        .expect("the add-file lands on disk");

    assert_eq!(std::fs::read(&target).unwrap(), b"replacement\n");
    let stats: horon_agent_harness_api::SessionEditStats = serde_json::from_slice(
        &std::fs::read(home.path().join("sessions/edit-stats").join(format!("{session}.json")))
            .expect("the edit counter was written under the test home"),
    )
    .expect("the counter is JSON");
    assert!(
        stats.approximate,
        "overwriting a file whose text cannot be read is an approximate count: {stats:?}"
    );
    assert_eq!((stats.added, stats.removed), (1, 0), "{stats:?}");
}

// Issue #595: unified diffs used to splice by offset and count alone, so a
// stale patch replaced whatever happened to sit at that line. These tests go
// through the public `apply_patch` entry point, which routes `---`/`+++` text
// to the unified-diff applier.

struct TestEnv {
    _home: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl TestEnv {
    fn new() -> Self {
        let lock = crate::storage::lock_test_env();
        let home = tempfile::tempdir().expect("temp home");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", home.path());
        Self {
            _home: home,
            previous,
            _lock: lock,
        }
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
    }
}

fn ctx(dir: &Path, session: &str) -> crate::tool::ToolContext {
    crate::tool::ToolContext {
        session_id: session.to_string(),
        message_id: "unified-patch".to_string(),
        tool_call_id: "unified-patch".to_string(),
        working_dir: Some(dir.to_path_buf()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    }
}

fn body(output: &crate::tool::ToolOutput) -> String {
    output.output.clone()
}

async fn run_patch(dir: &Path, session: &str, patch: &str) -> crate::tool::ToolOutput {
    ApplyPatchTool
        .execute(json!({ "patch_text": patch }), ctx(dir, session))
        .await
        .expect("the tool reports in its output")
}

#[tokio::test]
async fn unified_patch_applies_declared_hunk_and_undo_restores_bytes() {
    let _env = TestEnv::new();
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("sample.txt");
    std::fs::write(&file, "header\nkeep this\nold title\n").expect("seed");
    let session = "unified-patch-happy-undo";

    let output = run_patch(
        dir.path(),
        session,
        "--- a/sample.txt\n+++ b/sample.txt\n@@ -3,1 +3,1 @@\n-old title\n+new title\n",
    )
    .await;

    assert!(
        body(&output).contains("✓ sample.txt: modified (1 hunks)"),
        "{}",
        body(&output)
    );
    assert_eq!(output.verdict, Some(crate::tool::ToolVerdict::Ok));
    assert_eq!(
        std::fs::read(&file).expect("patched file"),
        b"header\nkeep this\nnew title\n"
    );

    UndoTool::new()
        .execute(json!({}), ctx(dir.path(), session))
        .await
        .expect("the local write is undoable");
    assert_eq!(
        std::fs::read(&file).expect("restored file"),
        b"header\nkeep this\nold title\n"
    );
}

#[tokio::test]
async fn unified_patch_rejects_stale_context_without_changing_bytes() {
    let _env = TestEnv::new();
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("sample.txt");
    std::fs::write(&file, "header\nkeep this\nold title\n").expect("seed");

    // The patch was cut from a file where "old title" was line 2. The current
    // file has "keep this" there, so applying by offset alone would overwrite
    // an unrelated line.
    let output = run_patch(
        dir.path(),
        "unified-patch-stale",
        "--- a/sample.txt\n+++ b/sample.txt\n@@ -2 +2 @@\n-old title\n+new title\n",
    )
    .await;
    let text = body(&output);

    assert!(text.starts_with("✗ sample.txt: "), "{text}");
    assert!(text.contains("does not match"), "{text}");
    assert!(text.contains("No files changed"), "{text}");
    assert!(!text.contains('✓'), "a failed preflight must not claim success: {text}");
    assert_eq!(output.verdict, Some(crate::tool::ToolVerdict::Failed));
    assert_eq!(
        std::fs::read(&file).expect("unchanged file"),
        b"header\nkeep this\nold title\n"
    );
}

#[tokio::test]
async fn unified_patch_preflights_every_file_before_writing() {
    let _env = TestEnv::new();
    let dir = tempfile::tempdir().expect("temp dir");
    let modified = dir.path().join("modify.txt");
    let deleted = dir.path().join("delete.txt");
    let failed = dir.path().join("fail.txt");
    std::fs::write(&modified, "one\ntwo\n").expect("modify");
    std::fs::write(&deleted, "gone\n").expect("delete");
    std::fs::write(&failed, "stable\n").expect("fail");
    let created = dir.path().join("created.txt");
    let session = "unified-patch-preflight";

    let patch = "--- a/modify.txt\n+++ b/modify.txt\n@@ -1,1 +1,1 @@\n-one\n+ONE\n\
--- /dev/null\n+++ b/created.txt\n@@ -0,0 +1,1 @@\n+created\n\
--- a/delete.txt\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-gone\n\
--- a/fail.txt\n+++ b/fail.txt\n@@ -1,1 +1,1 @@\n-stale\n+changed\n";
    let output = run_patch(dir.path(), session, &patch).await;
    let text = body(&output);

    assert!(text.contains("✗ fail.txt: "), "{text}");
    assert!(text.contains("No files changed"), "{text}");
    assert!(
        !text.contains('✓'),
        "no file may be written before preflight finishes: {text}"
    );
    assert_eq!(output.verdict, Some(crate::tool::ToolVerdict::Failed));
    assert_eq!(std::fs::read(&modified).expect("modify unchanged"), b"one\ntwo\n");
    assert_eq!(std::fs::read(&deleted).expect("delete unchanged"), b"gone\n");
    assert_eq!(std::fs::read(&failed).expect("failed unchanged"), b"stable\n");
    assert!(
        !created.exists(),
        "a planned creation must not land after a later file fails"
    );

    let error = UndoTool::new()
        .execute(json!({}), ctx(dir.path(), session))
        .await
        .expect_err("a failed preflight records no undo unit");
    assert!(error.to_string().contains("Nothing to undo"), "{error}");
}

#[tokio::test]
async fn unified_patch_rejects_a_hunk_whose_declared_count_is_wrong() {
    let _env = TestEnv::new();
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("counts.txt");
    std::fs::write(&file, "one\n").expect("seed");

    let error = ApplyPatchTool
        .execute(
            json!({
                "patch_text": "--- a/counts.txt\n+++ b/counts.txt\n@@ -1,3 +1,1 @@\n-one\n+ONE\n"
            }),
            ctx(dir.path(), "unified-patch-counts"),
        )
        .await
        .expect_err("a count that cannot be satisfied is a parse error");
    let message = format!("{error:#}");
    assert!(message.contains("declares"), "{message}");
    assert_eq!(std::fs::read(&file).expect("unchanged"), b"one\n");
}

#[tokio::test]
async fn unified_patch_rejects_malformed_hunk_headers_before_writing() {
    let _env = TestEnv::new();
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("range.txt");
    std::fs::write(&file, "one\n").expect("seed");

    let cases = [
        (
            "--- a/range.txt\n+++ b/range.txt\n@@ -1,x +1,1 @@\n-one\n+ONE\n",
            "hunk range",
        ),
        (
            "--- a/range.txt\n+++ b/range.txt\n@@ -1 +1\n-one\n+ONE\n",
            "closing '@@'",
        ),
        (
            "--- a/range.txt\n+++ b/range.txt\n@@ -1,1 +1,1 @@\n-one\n+ONE\n-extra\n",
            "more lines",
        ),
        (
            "--- a/range.txt\n+++ b/range.txt\n@@ -1,1 +0,1 @@\n-one\n+ONE\n",
            "starts at line 0",
        ),
    ];

    for (patch, expected) in cases {
        let error = ApplyPatchTool
            .execute(
                json!({ "patch_text": patch }),
                ctx(dir.path(), "unified-patch-malformed"),
            )
            .await
            .expect_err("a malformed hunk header is a parse error");
        let message = format!("{error:#}");
        assert!(message.contains(expected), "{expected}: {message}");
        assert_eq!(std::fs::read(&file).expect("unchanged"), b"one\n");
    }
}

#[tokio::test]
async fn unified_patch_applies_in_order_zero_length_insertions() {
    let _env = TestEnv::new();
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("insert.txt");
    std::fs::write(&file, "first\nthird\n").expect("seed");

    let output = run_patch(
        dir.path(),
        "unified-patch-insertions",
        "--- a/insert.txt\n+++ b/insert.txt\n@@ -0,0 +1,1 @@\n+top\n\
@@ -1,0 +3,1 @@\n+second\n\
@@ -2,0 +5,1 @@\n+end\n",
    )
    .await;

    assert!(!body(&output).contains('✗'), "{}", body(&output));
    assert_eq!(
        std::fs::read(&file).expect("inserted file"),
        b"top\nfirst\nsecond\nthird\nend\n"
    );

    // The declared new range is part of the contract: `+3` for a one-line
    // replacement at line 1 cannot describe this hunk.
    let bad = dir.path().join("bad-position.txt");
    std::fs::write(&bad, "one\n").expect("seed bad position");
    let output = run_patch(
        dir.path(),
        "unified-patch-bad-position",
        "--- a/bad-position.txt\n+++ b/bad-position.txt\n@@ -1,1 +3,1 @@\n-one\n+ONE\n",
    )
    .await;
    let text = body(&output);
    assert!(text.contains("declared new range"), "{text}");
    assert_eq!(std::fs::read(&bad).expect("unchanged bad position"), b"one\n");
}

#[tokio::test]
async fn unified_patch_rejects_out_of_order_hunks_before_writing() {
    let _env = TestEnv::new();
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("order.txt");
    std::fs::write(&file, "a\nb\n").expect("seed");

    let output = run_patch(
        dir.path(),
        "unified-patch-order",
        "--- a/order.txt\n+++ b/order.txt\n@@ -2,1 +2,1 @@\n-b\n+B\n@@ -1,1 +1,1 @@\n-a\n+A\n",
    )
    .await;
    let text = body(&output);

    assert!(text.contains("✗ order.txt: "), "{text}");
    assert!(text.contains("order"), "{text}");
    assert_eq!(std::fs::read(&file).expect("unchanged"), b"a\nb\n");
}

#[tokio::test]
async fn unified_patch_rejects_a_file_named_twice() {
    let _env = TestEnv::new();
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("duplicate.txt");
    std::fs::write(&file, "a\n").expect("seed");

    let output = run_patch(
        dir.path(),
        "unified-patch-duplicate",
        "--- a/duplicate.txt\n+++ b/duplicate.txt\n@@ -1,1 +1,1 @@\n-a\n+b\n\
--- a/duplicate.txt\n+++ b/duplicate.txt\n@@ -1,1 +1,1 @@\n-b\n+c\n",
    )
    .await;
    let text = body(&output);

    assert!(text.contains("✗ duplicate.txt: "), "{text}");
    assert!(text.contains("same file"), "{text}");
    assert_eq!(std::fs::read(&file).expect("unchanged"), b"a\n");
}

#[tokio::test]
async fn unified_patch_preserves_a_missing_final_newline() {
    let _env = TestEnv::new();
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("last.txt");
    std::fs::write(&file, "last").expect("seed");

    let output = run_patch(
        dir.path(),
        "unified-patch-no-newline",
        "--- a/last.txt\n+++ b/last.txt\n@@ -1,1 +1,1 @@\n-last\n\\ No newline at end of file\n\
+changed\n\\ No newline at end of file\n",
    )
    .await;

    assert!(!body(&output).contains('✗'), "{}", body(&output));
    assert_eq!(std::fs::read(&file).expect("patched file"), b"changed");

    // A marker on a line that is not the last of its side describes no ending.
    let invalid = dir.path().join("invalid.txt");
    std::fs::write(&invalid, "a\nb\n").expect("seed invalid");
    let error = ApplyPatchTool
        .execute(
            json!({
                "patch_text": "--- a/invalid.txt\n+++ b/invalid.txt\n@@ -1,2 +1,2 @@\n-a\n\\ No newline at end of file\n+A\n b\n"
            }),
            ctx(dir.path(), "unified-patch-marker-position"),
        )
        .await
        .expect_err("a marker after a non-final line is malformed");
    assert!(
        format!("{error:#}").contains("only valid on the last line"),
        "{error:#}"
    );
    assert_eq!(std::fs::read(&invalid).expect("unchanged invalid"), b"a\nb\n");

    // The old-side marker must describe the original file, not merely sit at
    // the end of the hunk.
    let newline = dir.path().join("has-newline.txt");
    std::fs::write(&newline, "last\n").expect("seed newline");
    let output = run_patch(
        dir.path(),
        "unified-patch-marker-original-newline",
        "--- a/has-newline.txt\n+++ b/has-newline.txt\n@@ -1,1 +1,1 @@\n-last\n\\ No newline at end of file\n\
+changed\n\\ No newline at end of file\n",
    )
    .await;
    let text = body(&output);
    assert!(text.contains("ends with one"), "{text}");
    assert_eq!(std::fs::read(&newline).expect("unchanged newline file"), b"last\n");

    // A new-side marker cannot change the ending when the hunk is not the tail
    // of the file.
    let tail = dir.path().join("has-tail.txt");
    std::fs::write(&tail, "a\nb\n").expect("seed tail");
    let output = run_patch(
        dir.path(),
        "unified-patch-marker-new-tail",
        "--- a/has-tail.txt\n+++ b/has-tail.txt\n@@ -1,1 +1,1 @@\n-a\n+A\n\\ No newline at end of file\n",
    )
    .await;
    let text = body(&output);
    assert!(text.contains("would change the ending"), "{text}");
    assert_eq!(std::fs::read(&tail).expect("unchanged tail file"), b"a\nb\n");
}

#[tokio::test]
async fn unified_patch_preserves_final_newline_and_crlf_style() {
    let _env = TestEnv::new();
    let dir = tempfile::tempdir().expect("temp dir");
    let appended = dir.path().join("append.txt");
    let crlf = dir.path().join("crlf.txt");
    std::fs::write(&appended, "first\nlast\n").expect("seed append");
    std::fs::write(&crlf, "one\r\ntwo\r\n").expect("seed crlf");

    let output = run_patch(
        dir.path(),
        "unified-patch-endings",
        "--- a/append.txt\n+++ b/append.txt\n@@ -2,1 +2,2 @@\n last\n+added\n\
--- a/crlf.txt\n+++ b/crlf.txt\n@@ -2,1 +2,1 @@\n-two\n+TWO\n",
    )
    .await;

    assert!(!body(&output).contains('✗'), "{}", body(&output));
    assert_eq!(
        std::fs::read(&appended).expect("appended file"),
        b"first\nlast\nadded\n"
    );
    assert_eq!(
        std::fs::read(&crlf).expect("crlf file"),
        b"one\r\nTWO\r\n",
        "a uniformly CRLF file keeps CRLF"
    );
}

#[tokio::test]
async fn unified_patch_keeps_a_dash_prefixed_body_line_inside_its_hunk() {
    let _env = TestEnv::new();
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("flags.txt");
    std::fs::write(&file, "alpha\n--flag\nomega\n").expect("seed");

    // `---flag` is a removed line whose content is `--flag`, not a file
    // header, because the hunk counts consume it before the file-boundary
    // check runs.
    let output = run_patch(
        dir.path(),
        "unified-patch-dash-body",
        "--- a/flags.txt\n+++ b/flags.txt\n@@ -2,1 +2,1 @@\n---flag\n+++flag\n",
    )
    .await;

    assert!(!body(&output).contains('✗'), "{}", body(&output));
    assert_eq!(std::fs::read(&file).expect("patched flags"), b"alpha\n++flag\nomega\n");
}

#[tokio::test]
async fn unified_patch_accepts_a_git_format_patch_envelope() {
    let _env = TestEnv::new();
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("notes.txt");
    std::fs::write(&file, "old\n").expect("seed");

    let output = run_patch(
        dir.path(),
        "unified-format-patch",
        concat!(
            "From 1234567890abcdef1234567890abcdef12345678 Mon Sep 17 00:00:00 2001\n",
            "Subject: [PATCH] Update notes\n\n---\n notes.txt | 2 +-\n\n",
            "diff --git a/notes.txt b/notes.txt\n--- a/notes.txt\n+++ b/notes.txt\n",
            "@@ -1 +1 @@\n-old\n+new\n-- \n2.53.0\n\n"
        ),
    )
    .await;

    assert!(body(&output).contains("✓ notes.txt: modified"), "{}", body(&output));
    assert_eq!(std::fs::read(&file).expect("patched notes"), b"new\n");
}

#[tokio::test]
async fn unified_patch_creates_a_file_from_a_new_file_hunk() {
    let _env = TestEnv::new();
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("new.txt");

    let output = run_patch(
        dir.path(),
        "unified-patch-create",
        "--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,2 @@\n+alpha\n+beta\n",
    )
    .await;

    assert!(body(&output).contains("✓ new.txt: created"), "{}", body(&output));
    assert_eq!(std::fs::read(&file).expect("created file"), b"alpha\nbeta\n");
}

#[tokio::test]
async fn unified_patch_matches_the_editor_buffer_not_the_stale_disk_copy() {
    let _env = TestEnv::new();
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("buffer.txt");
    std::fs::write(&file, "stale disk line\n").expect("seed");
    let client = FakeClient::install(
        "unified-patch-buffer",
        AcpClientCapabilities {
            fs_read_text_file: true,
            fs_write_text_file: true,
            terminal: false,
        },
        Reply::EditorBuffer,
    );
    client.set_buffer(&file, "editor buffer line\n");

    let patch = format!(
        "--- a/{0}\n+++ b/{0}\n@@ -1,1 +1,1 @@\n-stale disk line\n+patched line\n",
        file.display()
    );
    let output = ApplyPatchTool
        .execute(json!({ "patch_text": patch }), ctx(dir.path(), &client.session))
        .await
        .expect("the tool reports the mismatch");
    let text = body(&output);

    assert!(text.starts_with(&format!("✗ {}: ", file.display())), "{text}");
    assert!(text.contains("does not match"), "{text}");
    assert_eq!(output.verdict, Some(crate::tool::ToolVerdict::Failed));
    assert_eq!(
        client.buffer_of(&file).as_deref(),
        Some("editor buffer line\n"),
        "a rejected patch must not write the editor buffer"
    );
    assert_eq!(
        std::fs::read(&file).expect("disk untouched"),
        b"stale disk line\n",
        "a delegated read must not fall back to disk"
    );
    assert!(client.asked().iter().any(|method| method == "fs/read_text_file"));
    assert!(
        !client.asked().iter().any(|method| method == "fs/write_text_file"),
        "a rejected preflight must not reach the client write: {:?}",
        client.asked()
    );
}
