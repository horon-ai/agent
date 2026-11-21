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
