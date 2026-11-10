use super::*;
use rusqlite::Connection;

fn seeded_db(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("fixture.sqlite");
    let conn = Connection::open(&path).expect("create fixture");
    conn.execute_batch(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, note TEXT);
         INSERT INTO users (name, note) VALUES ('ada', 'first'), ('grace', NULL);
         CREATE TABLE empty_table (id INTEGER PRIMARY KEY, payload BLOB);",
    )
    .expect("seed fixture");
    drop(conn);
    path
}

#[test]
fn lists_tables_with_row_counts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = seeded_db(temp.path());
    let db = ReadOnlyDb::open(&path).expect("open read-only");
    let tables = db.tables().expect("list tables");
    assert_eq!(
        tables,
        vec![
            TableInfo {
                name: "empty_table".to_string(),
                rows: 0
            },
            TableInfo {
                name: "users".to_string(),
                rows: 2
            },
        ]
    );
}

#[test]
fn returns_schema_and_paged_rows() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = seeded_db(temp.path());
    let db = ReadOnlyDb::open(&path).expect("open read-only");

    let schema = db.schema("users").expect("schema");
    assert!(
        schema
            .sql
            .as_deref()
            .is_some_and(|sql| sql.contains("CREATE TABLE users")),
        "schema SQL should be the CREATE statement, got {:?}",
        schema.sql
    );
    let columns: Vec<&str> = schema.columns.iter().map(|column| column.name.as_str()).collect();
    assert_eq!(columns, ["id", "name", "note"]);
    assert!(schema.columns[0].primary_key);
    assert!(schema.columns[1].not_null);

    let first = db.rows("users", 1, 0).expect("first page");
    assert_eq!(first, vec![vec!["1", "ada", "first"]]);
    let second = db.rows("users", 1, 1).expect("second page");
    assert_eq!(second, vec![vec!["2", "grace", "NULL"]]);
}

#[test]
fn read_only_open_leaves_the_file_unchanged() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = seeded_db(temp.path());
    let before = std::fs::read(&path).expect("read fixture bytes");
    {
        let db = ReadOnlyDb::open(&path).expect("open read-only");
        db.tables().expect("list tables");
        db.schema("users").expect("schema");
        db.rows("users", 10, 0).expect("rows");
    }
    let after = std::fs::read(&path).expect("read fixture bytes again");
    assert_eq!(before, after, "a read must not change the database bytes");
}

#[test]
fn rejects_a_file_that_is_not_a_database() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("not.sqlite");
    std::fs::write(&path, b"this is plain text, not a database").expect("write fixture");
    let error = ReadOnlyDb::open(&path).expect_err("plain text is not a database");
    let message = error.to_string();
    assert!(
        message.contains("not a readable SQLite database"),
        "unexpected error: {message}"
    );
}

#[test]
fn renders_cells_as_single_lines() {
    assert_eq!(render_value(ValueRef::Null), "NULL");
    assert_eq!(render_value(ValueRef::Integer(7)), "7");
    assert_eq!(render_value(ValueRef::Text(b"two\nlines")), "two lines");
    assert_eq!(render_value(ValueRef::Blob(&[0u8; 3])), "<blob 3 bytes>");

    let long = "x".repeat(MAX_CELL_CHARS + 10);
    let rendered = render_value(ValueRef::Text(long.as_bytes()));
    assert!(rendered.ends_with('…'));
    assert_eq!(rendered.chars().count(), MAX_CELL_CHARS + 1);
}

#[test]
fn detects_the_sqlite_header() {
    assert!(has_sqlite_magic(b"SQLite format 3\0rest"));
    assert!(!has_sqlite_magic(b"PK\x03\x04"));
    assert!(!has_sqlite_magic(b""));
}
