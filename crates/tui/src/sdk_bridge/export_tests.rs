//! Tests for the `/export` writer.

use std::fs;

use super::write;

/// The typed destination receives the report, and its parent folders are made.
#[test]
fn export_writes_the_report_at_the_named_path() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("reports/session.html");
    let path = path.to_str().expect("utf-8 path");

    write("<!DOCTYPE html>\n", path).expect("write the report");

    assert_eq!(fs::read_to_string(path).expect("read back"), "<!DOCTYPE html>\n");
}

/// A destination that already holds a file is refused, and the file survives.
#[test]
fn export_refuses_to_replace_an_existing_file() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("kept.html");
    fs::write(&path, "an earlier report").expect("seed the file");

    let error = write("<!DOCTYPE html>\n", path.to_str().expect("utf-8 path")).expect_err("must refuse");

    assert_eq!(error.kind, horon_agent_sdk::ErrorKind::InvalidOption);
    assert_eq!(fs::read_to_string(&path).expect("read back"), "an earlier report");
}
