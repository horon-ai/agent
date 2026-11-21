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
