use super::{HashlineEditTool, HashlineOp, HashlineOpKind};
use crate::tool::{ConcurrencyClass, Registry, Tool, ToolContext, ToolExecutionMode};
use horon_agent_edit_safety::tag_for;
use serde_json::{Value, json};
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

fn anchor(content: &str, line: usize) -> String {
    let text = content
        .lines()
        .nth(line - 1)
        .unwrap_or_else(|| panic!("no line {line}"));
    format!("{line}#{}", tag_for(text.as_bytes()))
}

fn op(start: &str, end: Option<&str>, new_text: &str) -> HashlineOp {
    HashlineOp {
        start_anchor: start.to_string(),
        end_anchor: end.map(str::to_string),
        new_text: new_text.to_string(),
        kind: HashlineOpKind::Replace,
        block: false,
    }
}

fn op_kind(start: &str, kind: HashlineOpKind, new_text: &str) -> HashlineOp {
    HashlineOp {
        start_anchor: start.to_string(),
        end_anchor: None,
        new_text: new_text.to_string(),
        kind,
        block: false,
    }
}

fn block_op(start: &str, new_text: &str) -> HashlineOp {
    HashlineOp {
        start_anchor: start.to_string(),
        end_anchor: None,
        new_text: new_text.to_string(),
        kind: HashlineOpKind::Replace,
        block: true,
    }
}

fn kind_name(kind: HashlineOpKind) -> &'static str {
    match kind {
        HashlineOpKind::Replace => "replace",
        HashlineOpKind::InsertBefore => "insert_before",
        HashlineOpKind::InsertAfter => "insert_after",
        HashlineOpKind::Delete => "delete",
    }
}

fn seed(dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, content).expect("seed file");
    path
}

fn edit_input(name: &str, ops: Vec<HashlineOp>) -> Value {
    let ops: Vec<Value> = ops
        .into_iter()
        .map(|op| {
            let mut value = json!({
                "start_anchor": op.start_anchor,
                "new_text": op.new_text,
                "kind": kind_name(op.kind),
                "block": op.block
            });
            if let Some(end) = op.end_anchor {
                value["end_anchor"] = Value::String(end);
            }
            value
        })
        .collect();
    json!({"file_path": name, "ops": ops})
}

#[test]
fn hashline_edit_is_ordered() {
    assert_eq!(HashlineEditTool::new().concurrency_class(), ConcurrencyClass::Ordered);
}

#[tokio::test]
async fn hashline_edit_applies_span() {
    let dir = tempfile::tempdir().expect("temp dir");
    let original = "alpha\nbeta\ngamma\ndelta\n";
    let file = seed(dir.path(), "notes.txt", original);
    let ops = vec![
        op(&anchor(original, 1), None, "ALPHA"),
        op(&anchor(original, 3), Some(&anchor(original, 4)), "GAMMA"),
    ];

    let ctx = context(dir.path(), "hashline-applies", "hashline-1");
    HashlineEditTool::new()
        .execute(edit_input("notes.txt", ops), ctx)
        .await
        .expect("anchored edit");

    assert_eq!(
        std::fs::read(&file).expect("read after edit"),
        b"ALPHA\nbeta\nGAMMA\n",
        "two ops replace one line and an inclusive two-line span"
    );
}

#[tokio::test]
async fn hashline_edit_keeps_surrounding_lines_byte_identical() {
    let dir = tempfile::tempdir().expect("temp dir");
    let original = "first\n  indented\nthird\nfourth\nfifth\n";
    let file = seed(dir.path(), "keep.txt", original);
    let ops = vec![op(&anchor(original, 3), None, "THIRD")];

    let ctx = context(dir.path(), "hashline-keeps", "hashline-1");
    HashlineEditTool::new()
        .execute(edit_input("keep.txt", ops), ctx)
        .await
        .expect("anchored edit");

    assert_eq!(
        std::fs::read_to_string(&file).expect("read after edit"),
        "first\n  indented\nTHIRD\nfourth\nfifth\n"
    );
}

#[tokio::test]
async fn hashline_edit_keeps_crlf_line_endings() {
    let dir = tempfile::tempdir().expect("temp dir");
    let original = "alpha\r\nbeta\r\ngamma\r\n";
    let file = seed(dir.path(), "crlf.txt", original);
    let ops = vec![op(&anchor(original, 2), None, "BETA")];

    let ctx = context(dir.path(), "hashline-crlf", "hashline-1");
    HashlineEditTool::new()
        .execute(edit_input("crlf.txt", ops), ctx)
        .await
        .expect("anchored edit");

    assert_eq!(
        std::fs::read_to_string(&file).expect("after edit"),
        "alpha\r\nBETA\r\ngamma\r\n",
        "a replaced line keeps the file's CRLF endings"
    );
}

#[tokio::test]
async fn hashline_edit_rejects_stale_anchor() {
    let dir = tempfile::tempdir().expect("temp dir");
    let original = "alpha\nbeta\n";
    let file = seed(dir.path(), "stale.txt", original);
    let stale = anchor(original, 2);
    let stale_tag = stale.split('#').nth(1).expect("tag").to_string();
    std::fs::write(&file, "alpha\nBETA\n").expect("external revision");
    let actual_tag = tag_for(b"BETA").to_string();

    let ctx = context(dir.path(), "hashline-stale", "hashline-1");
    let error = HashlineEditTool::new()
        .execute(edit_input("stale.txt", vec![op(&stale, None, "gamma")]), ctx)
        .await
        .expect_err("a stale anchor must be refused");
    let message = format!("{error:#}");

    assert!(
        message.contains(&stale_tag),
        "refusal must name the expected tag {stale_tag}, got: {message}"
    );
    assert!(
        message.contains(&actual_tag),
        "refusal must name the actual tag {actual_tag}, got: {message}"
    );
    assert!(
        message.contains("No changes written to stale.txt"),
        "refusal must say nothing was written, got: {message}"
    );
    assert_eq!(
        std::fs::read(&file).expect("read after refusal"),
        b"alpha\nBETA\n",
        "a refused call must leave the file untouched"
    );
}

#[tokio::test]
async fn hashline_edit_validates_every_anchor_before_writing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let original = "one\ntwo\nthree\n";
    let file = seed(dir.path(), "partial.txt", original);
    let ops = vec![
        op(&anchor(original, 1), None, "ONE"),
        op(&anchor(original, 2), None, "TWO"),
        op("3#0000", None, "THREE"),
    ];

    let ctx = context(dir.path(), "hashline-partial", "hashline-1");
    let error = HashlineEditTool::new()
        .execute(edit_input("partial.txt", ops), ctx)
        .await
        .expect_err("one stale anchor must refuse the whole call");
    let message = format!("{error:#}");

    assert!(message.contains("1 of 3 anchored edits failed"), "{message}");
    assert_eq!(
        std::fs::read(&file).expect("read after refusal"),
        original.as_bytes(),
        "the two valid anchors must not be applied on their own"
    );
}

#[tokio::test]
async fn undo_reverses_a_hashline_edit() {
    let dir = tempfile::tempdir().expect("temp dir");
    let original = "alpha\nbeta\n";
    let file = seed(dir.path(), "undone.txt", original);
    let ctx = context(dir.path(), "hashline-undo", "hashline-1");

    HashlineEditTool::new()
        .execute(
            edit_input("undone.txt", vec![op(&anchor(original, 1), None, "ALPHA")]),
            ctx.clone(),
        )
        .await
        .expect("anchored edit");
    assert_eq!(std::fs::read(&file).expect("after edit"), b"ALPHA\nbeta\n");

    crate::tool::undo::UndoTool::new()
        .execute(json!({"force": false}), ctx)
        .await
        .expect("undo");

    assert_eq!(
        std::fs::read(&file).expect("after undo"),
        original.as_bytes(),
        "undo must reverse the anchored edit"
    );
}

#[tokio::test]
async fn hashline_edit_refuses_an_external_change_after_read() {
    let dir = tempfile::tempdir().expect("temp dir");
    let original = "alpha\nbeta\n";
    let file = seed(dir.path(), "drift.txt", original);
    let start = anchor(original, 1);
    let end = anchor(original, 2);

    std::fs::write(&file, "alpha\nBETA\n").expect("external rewrite of an anchored line");

    let ctx = context(dir.path(), "hashline-drift", "hashline-1");
    let error = HashlineEditTool::new()
        .execute(edit_input("drift.txt", vec![op(&start, Some(&end), "gone")]), ctx)
        .await
        .expect_err("a file changed between read and edit must be refused");
    let message = format!("{error:#}");

    assert!(message.contains("stale"), "{message}");
    assert_eq!(
        std::fs::read_to_string(&file).expect("after refusal"),
        "alpha\nBETA\n",
        "the externally changed file must not be clobbered"
    );
}

#[tokio::test]
async fn hashline_edit_still_applies_when_only_the_tail_changed() {
    let dir = tempfile::tempdir().expect("temp dir");
    let original = "alpha\nbeta\n";
    let file = seed(dir.path(), "tail.txt", original);
    let start = anchor(original, 1);

    std::fs::write(&file, "alpha\nbeta\nappended\n").expect("external append below the span");

    let ctx = context(dir.path(), "hashline-tail", "hashline-1");
    HashlineEditTool::new()
        .execute(edit_input("tail.txt", vec![op(&start, None, "ALPHA")]), ctx)
        .await
        .expect("anchors outside the changed tail still name their bytes");

    assert_eq!(
        std::fs::read_to_string(&file).expect("after edit"),
        "ALPHA\nbeta\nappended\n",
        "an edit must not disturb lines the anchors do not name"
    );
}

#[tokio::test]
async fn hashline_edit_rejects_inverted_spans() {
    let dir = tempfile::tempdir().expect("temp dir");
    let original = "one\ntwo\nthree\n";
    let file = seed(dir.path(), "inverted.txt", original);
    let ops = vec![op(&anchor(original, 3), Some(&anchor(original, 1)), "x")];

    let ctx = context(dir.path(), "hashline-inverted", "hashline-1");
    let error = HashlineEditTool::new()
        .execute(edit_input("inverted.txt", ops), ctx)
        .await
        .expect_err("start after end must be refused");
    let message = format!("{error:#}");

    assert!(message.contains("must run downward"), "{message}");
    assert_eq!(std::fs::read(&file).expect("after refusal"), original.as_bytes());
}

#[tokio::test]
async fn hashline_edit_rejects_an_empty_op_list() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = seed(dir.path(), "empty.txt", "one\n");

    let ctx = context(dir.path(), "hashline-empty-ops", "hashline-1");
    let error = HashlineEditTool::new()
        .execute(json!({"file_path": "empty.txt", "ops": []}), ctx)
        .await
        .expect_err("an empty op list must be refused");
    let message = format!("{error:#}");

    assert!(message.contains("at least one"), "{message}");
    assert_eq!(std::fs::read(&file).expect("after refusal"), b"one\n");
}

#[tokio::test]
async fn hashline_edit_rejects_a_non_hex_tag() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = seed(dir.path(), "hex.txt", "one\ntwo\n");

    let ctx = context(dir.path(), "hashline-hex", "hashline-1");
    let error = HashlineEditTool::new()
        .execute(edit_input("hex.txt", vec![op("1#zzzz", None, "x")]), ctx)
        .await
        .expect_err("a non-hex tag must be refused");
    let message = format!("{error:#}");

    assert!(message.contains("non-hex tag"), "{message}");
    assert_eq!(std::fs::read(&file).expect("after refusal"), b"one\ntwo\n");
}

#[tokio::test]
async fn hashline_edit_rejects_a_truncated_anchor() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = seed(dir.path(), "truncated.txt", "one\ntwo\n");

    let ctx = context(dir.path(), "hashline-truncated", "hashline-1");
    let error = HashlineEditTool::new()
        .execute(edit_input("truncated.txt", vec![op("1#----", None, "x")]), ctx)
        .await
        .expect_err("a ---- anchor names no content");
    let message = format!("{error:#}");

    assert!(message.contains("names no content"), "{message}");
    assert_eq!(std::fs::read(&file).expect("after refusal"), b"one\ntwo\n");
}

#[tokio::test]
async fn hashline_edit_rejects_overlapping_spans() {
    let dir = tempfile::tempdir().expect("temp dir");
    let original = "one\ntwo\nthree\n";
    let file = seed(dir.path(), "overlap.txt", original);
    let ops = vec![
        op(&anchor(original, 1), Some(&anchor(original, 2)), "a"),
        op(&anchor(original, 2), Some(&anchor(original, 3)), "b"),
    ];

    let ctx = context(dir.path(), "hashline-overlap", "hashline-1");
    let error = HashlineEditTool::new()
        .execute(edit_input("overlap.txt", ops), ctx)
        .await
        .expect_err("overlapping spans must be refused");
    let message = format!("{error:#}");

    assert!(message.contains("spans overlap"), "{message}");
    assert_eq!(std::fs::read(&file).expect("after refusal"), original.as_bytes());
}

#[tokio::test]
async fn hashline_edit_deletes_a_span_with_empty_text() {
    let dir = tempfile::tempdir().expect("temp dir");
    let original = "keep\nremove\nme\nkeep too\n";
    let file = seed(dir.path(), "delete.txt", original);
    let ops = vec![op(&anchor(original, 2), Some(&anchor(original, 3)), "")];

    let ctx = context(dir.path(), "hashline-delete", "hashline-1");
    HashlineEditTool::new()
        .execute(edit_input("delete.txt", ops), ctx)
        .await
        .expect("deleting a span");

    assert_eq!(
        std::fs::read_to_string(&file).expect("after delete"),
        "keep\nkeep too\n"
    );
}

struct HomeGuard {
    previous: Option<std::ffi::OsString>,
}

impl HomeGuard {
    fn new(path: &Path) -> Self {
        let previous = std::env::var_os("HORON_AGENT_HOME");
        crate::env::set_var("HORON_AGENT_HOME", path);
        crate::config::Config::invalidate_cache();
        Self { previous }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            crate::env::set_var("HORON_AGENT_HOME", previous);
        } else {
            crate::env::remove_var("HORON_AGENT_HOME");
        }
        crate::config::Config::invalidate_cache();
    }
}

struct MockProvider;

#[async_trait::async_trait]
impl crate::provider::Provider for MockProvider {
    async fn complete(
        &self,
        _messages: &[crate::message::Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<crate::provider::EventStream> {
        Err(anyhow::anyhow!("mock provider does not stream"))
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn fork(&self) -> std::sync::Arc<dyn crate::provider::Provider> {
        std::sync::Arc::new(MockProvider)
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

#[test]
fn hashline_edit_stays_off_by_default_and_joins_no_core_profile() {
    let _env = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let _guard = HomeGuard::new(home.path());

    runtime().block_on(async {
        let registry =
            Registry::new(std::sync::Arc::new(MockProvider) as std::sync::Arc<dyn crate::provider::Provider>).await;
        assert!(
            !crate::config::ToolConfig::default().hashline_edit,
            "hashline_edit must default to off"
        );
        let names = registry.tool_names().await;
        assert!(
            !names.iter().any(|name| name == "hashline_edit"),
            "hashline_edit must stay unregistered by default, got {names:?}"
        );
        assert!(
            !crate::config::CORE_TOOLS.contains(&"hashline_edit"),
            "hashline_edit must never join CORE_TOOLS"
        );
    });
}

#[test]
fn hashline_edit_registers_when_the_gate_is_on() {
    let _env = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let _guard = HomeGuard::new(home.path());

    let path = crate::config::Config::path().expect("config path");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    std::fs::write(&path, "[tools]\nhashline_edit = true\n").expect("write config");
    crate::config::Config::invalidate_cache();

    runtime().block_on(async {
        let registry =
            Registry::new(std::sync::Arc::new(MockProvider) as std::sync::Arc<dyn crate::provider::Provider>).await;
        let names = registry.tool_names().await;
        assert!(
            names.iter().any(|name| name == "hashline_edit"),
            "[tools] hashline_edit = true must register the tool, got {names:?}"
        );
    });
}

#[tokio::test]
async fn hashline_edit_inserts_before_after_and_deletes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let original = "alpha\nbeta\ngamma\n";
    let file = seed(dir.path(), "kinds.txt", original);
    let ops = vec![
        op_kind(&anchor(original, 1), HashlineOpKind::InsertBefore, "head"),
        op_kind(&anchor(original, 2), HashlineOpKind::InsertAfter, "tail"),
        op_kind(&anchor(original, 3), HashlineOpKind::Delete, "ignored"),
    ];

    let ctx = context(dir.path(), "hashline-kinds", "hashline-1");
    HashlineEditTool::new()
        .execute(edit_input("kinds.txt", ops), ctx)
        .await
        .expect("kinded edits");

    assert_eq!(
        std::fs::read_to_string(&file).expect("after edit"),
        "head\nalpha\nbeta\ntail\n"
    );
}

#[tokio::test]
async fn hashline_edit_rebases_a_stale_anchor_within_twenty_lines() {
    let dir = tempfile::tempdir().expect("temp dir");
    let original = "alpha\nbeta\ngamma\n";
    let stale = anchor(original, 2);
    let shifted = format!("zero\n{original}");
    seed(dir.path(), "shifted.txt", &shifted);

    let ctx = context(dir.path(), "hashline-stale", "hashline-1");
    let output = HashlineEditTool::new()
        .execute(edit_input("shifted.txt", vec![op(&stale, None, "BETA")]), ctx)
        .await
        .expect("stale anchor should rebase when the tag is unique nearby");
    assert!(output.output.contains("rebased to line 3"), "{}", output.output);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("shifted.txt")).expect("read"),
        "zero\nalpha\nBETA\ngamma\n"
    );
}

#[tokio::test]
async fn hashline_edit_block_expands_to_the_enclosing_function() {
    let dir = tempfile::tempdir().expect("temp dir");
    let original = "fn outer() {\n    let a = 1;\n    let b = 2;\n}\n";
    let file = seed(dir.path(), "lib.rs", original);
    let ops = vec![block_op(&anchor(original, 1), "fn replaced() {}")];

    let ctx = context(dir.path(), "hashline-block", "hashline-1");
    HashlineEditTool::new()
        .execute(edit_input("lib.rs", ops), ctx)
        .await
        .expect("block edit");

    assert_eq!(
        std::fs::read_to_string(&file).expect("after edit"),
        "fn replaced() {}\n"
    );
}

#[tokio::test]
async fn hashline_edit_reports_a_noop_and_escalates_after_three() {
    let dir = tempfile::tempdir().expect("temp dir");
    let original = "alpha\nbeta\n";
    let file = seed(dir.path(), "noop.txt", original);

    let run = |call: &'static str| {
        let ctx = context(dir.path(), "hashline-noop", call);
        let ops = vec![op_kind(&anchor(original, 1), HashlineOpKind::InsertBefore, "")];
        async move { HashlineEditTool::new().execute(edit_input("noop.txt", ops), ctx).await }
    };

    let first = run("hashline-1").await.expect("first no-op");
    assert!(
        first.output.contains("No changes: the edits reproduce"),
        "{}",
        first.output
    );
    let _ = run("hashline-2").await.expect("second no-op");
    let third = run("hashline-3").await.expect("third no-op");
    assert!(third.output.starts_with("STOP."), "{}", third.output);
    assert_eq!(std::fs::read_to_string(&file).expect("unchanged"), original);
}
