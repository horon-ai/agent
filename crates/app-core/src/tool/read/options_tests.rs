use super::tests::{ConfigHome, make_ctx};
use super::*;

#[tokio::test]
async fn markdown_outline_keeps_source_ranges_and_explicit_reads_show_text() {
    let _lock = crate::storage::lock_test_env();
    let _config = ConfigHome::with_config("[tools]\nread_summary = true\n");
    let dir = tempfile::tempdir().unwrap();
    let text = format!(
        "# Guide\noriginal text\n```md\n## Code sample\n```\n{}\nSetup\n-----\ninstall command\n## Use\nlast line\n",
        "body\n".repeat(400),
    );
    std::fs::write(dir.path().join("guide.md"), text).unwrap();
    let tool = ReadTool::new();
    let read = |input| tool.execute(input, make_ctx(dir.path().to_path_buf()));

    let outline = read(json!({"file_path": "guide.md"})).await.unwrap().output;
    assert!(
        outline.contains("1-406\t# Guide\n407-409\t## Setup\n410-411\t## Use\n"),
        "{outline}"
    );
    assert!(!outline.contains("Code sample"));
    assert!(!outline.contains("original text"));

    let range = read(json!({"file_path": "guide.md", "start_line": 409, "end_line": 409}))
        .await
        .unwrap()
        .output;
    assert!(range.contains("409\tinstall command"), "{range}");
    let full = read(json!({"file_path": "guide.md", "full": true}))
        .await
        .unwrap()
        .output;
    assert!(full.contains("2\toriginal text") && full.contains("411\tlast line"));
}

#[tokio::test]
async fn default_page_size_config_preserves_explicit_ranges_and_next_cursor() {
    let _lock = crate::storage::lock_test_env();
    let dir = tempfile::tempdir().unwrap();
    let text: String = (1..=5001).map(|line| format!("row-{line}\n")).collect();
    std::fs::write(dir.path().join("rows.txt"), text).unwrap();
    let tool = ReadTool::new();
    let read = |input| tool.execute(input, make_ctx(dir.path().to_path_buf()));

    {
        let _config = ConfigHome::with_config("[tools]\nread_dedup = false\n");
        let page = read(json!({"file_path": "rows.txt"})).await.unwrap().output;
        assert!(page.contains("5000\trow-5000\n") && page.contains("offset=5000"));
        assert!(!page.contains("row-5001"));
    }

    let _config = ConfigHome::with_config("[tools]\nread_default_limit = 300\nread_dedup = false\n");
    let page = read(json!({"file_path": "rows.txt"})).await.unwrap().output;
    assert!(page.contains("300\trow-300\n") && page.contains("offset=300"));
    assert!(!page.contains("row-301"));
    let range = read(json!({"file_path":"rows.txt", "start_line":301, "end_line":650}))
        .await
        .unwrap()
        .output;
    assert!(range.contains("301\trow-301\n") && range.contains("650\trow-650\n"));
    assert!(!range.contains("row-651"));
}

#[tokio::test]
async fn seen_line_guard_requires_current_read_and_preserves_crlf() {
    let _lock = crate::storage::lock_test_env();
    let _config = ConfigHome::with_config("[tools]\nenforce_seen_line_guard = true\nread_dedup = false\n");
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    std::fs::write(&file, "first\r\nneedle\r\nlast\r\n").unwrap();
    let ctx = make_ctx(dir.path().to_path_buf());
    let editor = super::super::edit::EditTool::new();
    let reader = ReadTool::new();
    let edit = || {
        editor.execute(
            json!({"file_path":"sample.txt", "old_string":"needle", "new_string":"updated"}),
            ctx.clone(),
        )
    };
    let read_line = |line| {
        reader.execute(
            json!({"file_path":"sample.txt", "start_line":line, "end_line":line}),
            ctx.clone(),
        )
    };

    read_line(1).await.unwrap();
    let unread = edit().await.unwrap_err().to_string();
    assert!(unread.contains("lines 2-2: unread lines"), "{unread}");
    assert_eq!(std::fs::read(&file).unwrap(), b"first\r\nneedle\r\nlast\r\n");

    read_line(2).await.unwrap();
    std::fs::write(&file, "changed\r\nneedle\r\nlast\r\n").unwrap();
    let stale = edit().await.unwrap_err().to_string();
    assert!(stale.contains("lines 2-2: stale read"), "{stale}");
    assert_eq!(std::fs::read(&file).unwrap(), b"changed\r\nneedle\r\nlast\r\n");

    read_line(2).await.unwrap();
    edit().await.unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), b"changed\r\nupdated\r\nlast\r\n");
}

#[tokio::test]
async fn seen_line_guard_checks_every_replacement_and_tracks_batch_shifts() {
    let _lock = crate::storage::lock_test_env();
    let _config = ConfigHome::with_config("[tools]\nenforce_seen_line_guard = true\n");
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sample.txt");
    std::fs::write(&file, "needle\nseparator\nneedle\n").unwrap();
    let ctx = make_ctx(dir.path().to_path_buf());
    let editor = super::super::edit::EditTool::new();
    let reader = ReadTool::new();
    let read_line = |line| {
        reader.execute(
            json!({"file_path":"sample.txt", "start_line":line, "end_line":line}),
            ctx.clone(),
        )
    };
    let input = json!({"file_path":"sample.txt", "edits":[
        {"old_string":"needle", "new_string":"first\nsecond", "replace_all":true},
        {"old_string":"second", "new_string":"done", "replace_all":true}
    ]});
    read_line(1).await.unwrap();
    let error = editor
        .execute(input.clone(), ctx.clone())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("lines 3-3: unread lines"), "{error}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "needle\nseparator\nneedle\n");
    read_line(3).await.unwrap();
    editor.execute(input, ctx).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "first\ndone\nseparator\nfirst\ndone\n"
    );
}
