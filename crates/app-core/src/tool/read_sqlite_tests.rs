use super::*;
use crate::tool::ToolExecutionMode;
use serde_json::json;

fn make_ctx(working_dir: std::path::PathBuf) -> ToolContext {
    ToolContext {
        session_id: "read-sqlite-test".to_string(),
        message_id: "test-message".to_string(),
        tool_call_id: "test-call".to_string(),
        working_dir: Some(working_dir),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

fn input(raw: &str) -> ReadInput {
    serde_json::from_value(json!({ "file_path": raw })).expect("read input")
}

/// Creates `fixture.sqlite` with two tables and returns its path.
fn seeded_db(dir: &Path) -> PathBuf {
    let path = dir.join("fixture.sqlite");
    let conn = rusqlite::Connection::open(&path).expect("create fixture");
    conn.execute_batch(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, note TEXT);
         INSERT INTO users (name, note) VALUES ('ada', 'first'), ('grace', NULL), ('linus', 'third');
         CREATE TABLE logs (id INTEGER PRIMARY KEY, body TEXT);",
    )
    .expect("seed fixture");
    drop(conn);
    path
}

#[test]
fn lists_tables_with_row_counts() {
    let temp = tempfile::tempdir().expect("tempdir");
    seeded_db(temp.path());

    let output = read_sqlite(
        &input("fixture.sqlite"),
        &make_ctx(temp.path().to_path_buf()),
        temp.path(),
    )
    .expect("list tables");
    assert!(output.output.contains("2 tables"), "{}", output.output);
    assert!(output.output.contains("- users (3 rows)"), "{}", output.output);
    assert!(output.output.contains("- logs (0 rows)"), "{}", output.output);
}

#[test]
fn returns_schema_and_paged_rows() {
    let temp = tempfile::tempdir().expect("tempdir");
    seeded_db(temp.path());

    let output = read_sqlite(
        &input("fixture.sqlite:users?limit=1&offset=1"),
        &make_ctx(temp.path().to_path_buf()),
        temp.path(),
    )
    .expect("read table page");
    assert!(output.output.contains("CREATE TABLE users"), "{}", output.output);
    assert!(output.output.contains("id INTEGER (primary key)"), "{}", output.output);
    assert!(output.output.contains("name TEXT (not null)"), "{}", output.output);
    assert!(output.output.contains("grace"), "{}", output.output);
    assert!(!output.output.contains("ada"), "{}", output.output);
    assert!(output.output.contains("?limit=1&offset=2"), "{}", output.output);
}

#[test]
fn leaves_the_database_bytes_unchanged() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = seeded_db(temp.path());
    let before = std::fs::read(&path).expect("read bytes");

    let ctx = make_ctx(temp.path().to_path_buf());
    read_sqlite(&input("fixture.sqlite"), &ctx, temp.path()).expect("list");
    read_sqlite(&input("fixture.sqlite:users"), &ctx, temp.path()).expect("table");

    let after = std::fs::read(&path).expect("read bytes again");
    assert_eq!(before, after, "a read must not change the database bytes");
}

#[test]
fn reports_an_unknown_table_with_the_available_names() {
    let temp = tempfile::tempdir().expect("tempdir");
    seeded_db(temp.path());

    let error = read_sqlite(
        &input("fixture.sqlite:absent"),
        &make_ctx(temp.path().to_path_buf()),
        temp.path(),
    )
    .expect_err("unknown table must fail");
    let message = error.to_string();
    assert!(message.contains("no table named"), "unexpected error: {message}");
    assert!(message.contains("users"), "unexpected error: {message}");
}

#[test]
fn a_plain_file_named_like_a_database_is_not_resolved() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("notes.db"), b"plain text, no SQLite header").expect("write fixture");
    assert!(
        resolve("notes.db", temp.path()).is_none(),
        "a text file named .db must stay on the plain-file path"
    );
}

#[test]
fn a_database_without_an_extension_is_not_resolved_by_magic_alone() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = seeded_db(temp.path());
    let renamed = temp.path().join("no_extension");
    std::fs::rename(&path, &renamed).expect("rename fixture");
    // Detection is by extension plus magic per the issue: the extension is
    // what tells `read` a selector may follow, so an extensionless database is
    // read as a plain file.
    assert!(resolve("no_extension", temp.path()).is_none());
}

#[test]
fn parses_the_page_query() {
    assert_eq!(parse_page(None), (DEFAULT_ROW_LIMIT, 0));
    assert_eq!(parse_page(Some("limit=10")), (10, 0));
    assert_eq!(parse_page(Some("offset=5")), (DEFAULT_ROW_LIMIT, 5));
    assert_eq!(parse_page(Some("limit=2&offset=4")), (2, 4));
    assert_eq!(parse_page(Some("nonsense=1&limit=3")), (3, 0));
    assert_eq!(
        parse_page(Some("limit=0")),
        (DEFAULT_ROW_LIMIT, 0),
        "zero is not a page size"
    );
    assert_eq!(parse_page(Some("limit=abc")), (DEFAULT_ROW_LIMIT, 0));
}

#[test]
fn an_existing_file_name_containing_a_colon_wins() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = seeded_db(temp.path());
    let odd = temp.path().join("with:colon.sqlite");
    std::fs::rename(&path, &odd).expect("rename fixture");

    let resolved = resolve("with:colon.sqlite", temp.path()).expect("resolve whole path");
    assert_eq!(resolved.path, odd);
    assert!(
        resolved.table.is_none(),
        "the whole existing file wins over a table split"
    );
}
