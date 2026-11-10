use super::*;
use crate::tool::{ToolContext, ToolExecutionMode};
use serde_json::json;
use std::io::{Cursor, Write};

fn make_ctx(working_dir: std::path::PathBuf) -> ToolContext {
    ToolContext {
        session_id: "read-archive-test".to_string(),
        message_id: "test-message".to_string(),
        tool_call_id: "test-call".to_string(),
        working_dir: Some(working_dir),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

/// Builds a `.tar.gz` holding `entries` as regular files.
fn tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for (name, body) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, *body).expect("append tar entry");
    }
    builder.into_inner().expect("finish tar").finish().expect("finish gzip")
}

/// Builds a `.zip` holding `entries` as stored (uncompressed) files.
fn zip_archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, body) in entries {
        writer.start_file(*name, options).expect("start zip entry");
        writer.write_all(body).expect("write zip entry");
    }
    writer.finish().expect("finish zip").into_inner()
}

fn input(raw: &str) -> ReadInput {
    serde_json::from_value(json!({ "file_path": raw })).expect("read input")
}

/// Builds an uncompressed `.tar` holding `entries` as regular files.
fn tar_only(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, body) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, *body).expect("append tar entry");
    }
    builder.into_inner().expect("finish tar")
}

fn write_fixture(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("write fixture");
    path
}

const MEMBER_TEXT: &str = "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\neta\ntheta\niota\nkappa\n\
lambda\nmu\n";

#[test]
fn lists_tar_gz_members_with_sizes() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_fixture(
        temp.path(),
        "bundle.tar.gz",
        &tar_gz(&[("dir/file.txt", b"hello"), ("README.md", b"readme!")]),
    );

    let output = read_archive(
        &input("bundle.tar.gz"),
        &make_ctx(temp.path().to_path_buf()),
        temp.path(),
    )
    .expect("list members");
    assert!(output.output.contains("2 members"), "{}", output.output);
    assert!(output.output.contains("- dir/file.txt (5 bytes)"), "{}", output.output);
    assert!(output.output.contains("- README.md (7 bytes)"), "{}", output.output);
}

#[test]
fn returns_one_tar_member_text() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_fixture(
        temp.path(),
        "bundle.tar.gz",
        &tar_gz(&[("dir/file.txt", b"one\ntwo\n")]),
    );

    let output = read_archive(
        &input("bundle.tar.gz:dir/file.txt"),
        &make_ctx(temp.path().to_path_buf()),
        temp.path(),
    )
    .expect("read member");
    assert!(output.output.contains("one"), "{}", output.output);
    assert!(output.output.contains("two"), "{}", output.output);
}

#[test]
fn zip_member_read_honors_a_line_selector() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_fixture(
        temp.path(),
        "bundle.zip",
        &zip_archive(&[("notes.txt", MEMBER_TEXT.as_bytes())]),
    );

    let output = read_archive(
        &input("bundle.zip:notes.txt:10-12"),
        &make_ctx(temp.path().to_path_buf()),
        temp.path(),
    )
    .expect("read member page");
    assert!(output.output.contains("lambda"), "{}", output.output);
    assert!(output.output.contains("mu"), "{}", output.output);
    assert!(!output.output.contains("alpha"), "{}", output.output);
    assert!(!output.output.contains("theta"), "{}", output.output);
}

#[test]
fn refuses_a_member_path_that_escapes_the_archive() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_fixture(temp.path(), "bundle.tar.gz", &tar_gz(&[("file.txt", b"x")]));

    let error = read_archive(
        &input("bundle.tar.gz:../etc/passwd"),
        &make_ctx(temp.path().to_path_buf()),
        temp.path(),
    )
    .expect_err("escaping member must be refused");
    assert!(
        error.to_string().contains("escapes the archive"),
        "unexpected error: {error}"
    );
}

#[test]
fn lists_zip_members() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_fixture(
        temp.path(),
        "bundle.zip",
        &zip_archive(&[("a.txt", b"one"), ("nested/b.bin", b"two")]),
    );

    let output =
        read_archive(&input("bundle.zip"), &make_ctx(temp.path().to_path_buf()), temp.path()).expect("list zip");
    assert!(output.output.contains("- a.txt (3 bytes)"), "{}", output.output);
    assert!(output.output.contains("- nested/b.bin (3 bytes)"), "{}", output.output);
}

#[test]
fn a_plain_file_named_like_an_archive_is_not_resolved() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_fixture(temp.path(), "notes.zip", b"this is not a zip archive at all");

    assert!(
        resolve("notes.zip", temp.path()).expect("resolve is total").is_none(),
        "a text file with a .zip name must stay on the plain-file path"
    );
}

#[test]
fn detects_archive_kinds_from_extension_and_magic() {
    let temp = tempfile::tempdir().expect("tempdir");
    let tar = write_fixture(temp.path(), "plain.tar", &tar_only(&[("a", b"x")]));
    assert_eq!(ArchiveKind::detect(&tar), Some(ArchiveKind::Tar));
    let tar_gz_path = write_fixture(temp.path(), "x.tgz", &tar_gz(&[("a", b"x")]));
    assert_eq!(ArchiveKind::detect(&tar_gz_path), Some(ArchiveKind::TarGz));
    let zip_path = write_fixture(temp.path(), "x.zip", &zip_archive(&[("a", b"x")]));
    assert_eq!(ArchiveKind::detect(&zip_path), Some(ArchiveKind::Zip));

    // The magic bytes are what decide: a `.tar` holding gzip data is not a tar.
    let mismatched = write_fixture(temp.path(), "wrong.tar", &tar_gz(&[("a", b"x")]));
    assert_eq!(ArchiveKind::detect(&mismatched), None);
}

#[test]
fn rejects_a_member_that_is_not_present() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_fixture(temp.path(), "bundle.zip", &zip_archive(&[("a.txt", b"one")]));

    let error = read_archive(
        &input("bundle.zip:missing.txt"),
        &make_ctx(temp.path().to_path_buf()),
        temp.path(),
    )
    .expect_err("missing member must fail");
    assert!(
        error.to_string().contains("no member named"),
        "unexpected error: {error}"
    );
}

#[test]
fn member_fixture_helper_reads_zip_bytes() {
    let bytes = zip_archive(&[("a.txt", b"hello")]);
    let read = member_from_bytes(&bytes, ArchiveKind::Zip, "a.txt").expect("read member");
    assert_eq!(read, b"hello".to_vec());
}
