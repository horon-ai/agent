use super::{apply_edits, commit, plan};
use crate::tool::undo::UndoTool;
use crate::tool::{Tool, ToolContext, ToolExecutionMode};
use horon_agent_lsp::{FileEdit, TextEdit, WorkspaceEdit};
use serde_json::json;
use std::path::{Path, PathBuf};

fn ctx(dir: &Path, call: &str) -> ToolContext {
    ToolContext {
        session_id: "lsp-edit-session".to_string(),
        message_id: "lsp-edit-message".to_string(),
        tool_call_id: call.to_string(),
        working_dir: Some(dir.to_path_buf()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

fn edit(start_line: u32, start_character: u32, end_line: u32, end_character: u32, new_text: &str) -> TextEdit {
    TextEdit {
        start_line,
        start_character,
        end_line,
        end_character,
        new_text: new_text.to_string(),
    }
}

fn edit_at(path: &Path, edits: Vec<TextEdit>) -> WorkspaceEdit {
    WorkspaceEdit {
        files: vec![FileEdit {
            path: path.to_path_buf(),
            edits,
        }],
    }
}

#[test]
fn apply_edits_replaces_a_span() {
    let after = apply_edits("fn alpha() {}\n", &[edit(0, 3, 0, 8, "beta")], "lib.rs").expect("one edit");
    assert_eq!(after, "fn beta() {}\n");
}

#[test]
fn apply_edits_counts_columns_in_utf16_units() {
    // The emoji is one character but two UTF-16 code units, which is the unit
    // the protocol counts in.
    let text = "\u{1f600}ab\n";
    let after = apply_edits(text, &[edit(0, 2, 0, 3, "A")], "emoji.rs").expect("one edit");
    assert_eq!(after, "\u{1f600}Ab\n");
}

#[test]
fn apply_edits_applies_from_the_end_so_earlier_edits_do_not_shift_later_ones() {
    let text = "one two three\n";
    let after = apply_edits(
        text,
        &[edit(0, 0, 0, 3, "1"), edit(0, 4, 0, 7, "2"), edit(0, 8, 0, 13, "3")],
        "words.txt",
    )
    .expect("three edits");
    assert_eq!(after, "1 2 3\n");
}

#[test]
fn apply_edits_refuses_overlapping_edits() {
    let error = apply_edits(
        "abcdef\n",
        &[edit(0, 0, 0, 4, "x"), edit(0, 2, 0, 6, "y")],
        "overlap.txt",
    )
    .expect_err("overlap must be refused");
    assert!(format!("{error:#}").contains("overlap"), "{error:#}");
}

#[test]
fn apply_edits_refuses_a_range_past_the_end() {
    let error = apply_edits("abc\n", &[edit(4, 0, 4, 1, "x")], "short.txt").expect_err("past the end");
    assert!(format!("{error:#}").contains("past the end"), "{error:#}");
}

#[test]
fn apply_edits_accepts_the_position_at_the_end_of_the_file() {
    let after = apply_edits("abc", &[edit(0, 3, 0, 3, "d")], "tail.txt").expect("insert at end");
    assert_eq!(after, "abcd");
}

#[tokio::test]
async fn lsp_rename_applies_every_file_as_one_undo_unit() {
    let dir = tempfile::tempdir().expect("temp dir");
    let one = dir.path().join("one.rs");
    let two = dir.path().join("two.rs");
    std::fs::write(&one, "alpha\n").expect("seed one");
    std::fs::write(&two, "beta\n").expect("seed two");
    let edit = WorkspaceEdit {
        files: vec![
            FileEdit {
                path: one.clone(),
                edits: vec![edit(0, 0, 0, 5, "ALPHA")],
            },
            FileEdit {
                path: two.clone(),
                edits: vec![edit(0, 0, 0, 4, "BETA")],
            },
        ],
    };

    let shared = ctx(dir.path(), "lsp-commit");
    let plans = plan(&edit, dir.path()).await.expect("plan");
    commit(&shared, &plans).await.expect("commit");
    assert_eq!(std::fs::read_to_string(&one).expect("one"), "ALPHA\n");
    assert_eq!(std::fs::read_to_string(&two).expect("two"), "BETA\n");

    UndoTool::new()
        .execute(json!({"force": false}), shared)
        .await
        .expect("undo");
    assert_eq!(std::fs::read_to_string(&one).expect("one"), "alpha\n");
    assert_eq!(std::fs::read_to_string(&two).expect("two"), "beta\n");
}

#[tokio::test]
async fn lsp_rename_refuses_a_file_changed_after_the_plan() {
    let dir = tempfile::tempdir().expect("temp dir");
    let changed = dir.path().join("changed.rs");
    let untouched = dir.path().join("untouched.rs");
    std::fs::write(&changed, "alpha\n").expect("seed changed");
    std::fs::write(&untouched, "beta\n").expect("seed untouched");
    let edit = WorkspaceEdit {
        files: vec![
            FileEdit {
                path: changed.clone(),
                edits: vec![edit(0, 0, 0, 5, "ALPHA")],
            },
            FileEdit {
                path: untouched.clone(),
                edits: vec![edit(0, 0, 0, 4, "BETA")],
            },
        ],
    };

    let shared = ctx(dir.path(), "lsp-stale");
    let plans = plan(&edit, dir.path()).await.expect("plan");
    std::fs::write(&changed, "alpha plus an outside edit\n").expect("external change");

    let error = commit(&shared, &plans)
        .await
        .expect_err("a changed file must refuse the call");
    assert!(format!("{error:#}").contains("changed since"), "{error:#}");
    assert_eq!(
        std::fs::read_to_string(&changed).expect("changed"),
        "alpha plus an outside edit\n",
        "the external content must survive"
    );
    assert_eq!(
        std::fs::read_to_string(&untouched).expect("untouched"),
        "beta\n",
        "nothing is written when one file is stale"
    );
}

#[tokio::test]
async fn plan_resolves_a_relative_path_against_the_working_directory() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("rel.rs"), "alpha\n").expect("seed");
    let edit = WorkspaceEdit {
        files: vec![FileEdit {
            path: PathBuf::from("rel.rs"),
            edits: vec![edit(0, 0, 0, 5, "ALPHA")],
        }],
    };

    let plans = plan(&edit, dir.path()).await.expect("plan");
    assert_eq!(plans[0].path, dir.path().join("rel.rs"));
    assert_eq!(plans[0].after, "ALPHA\n");
}

#[tokio::test]
async fn plan_refuses_a_missing_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let edit = edit_at(&dir.path().join("absent.rs"), vec![edit(0, 0, 0, 0, "x")]);
    let error = plan(&edit, dir.path())
        .await
        .expect_err("a missing file must be refused");
    assert!(format!("{error:#}").contains("does not exist"), "{error:#}");
}
