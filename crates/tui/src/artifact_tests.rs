//! The addresses `/artifact` accepts and the store files it resolves.
//!
//! Every fixture writes the `<id>.<tool>.log` names the harness store writes
//! (`crates/base/src/artifacts.rs`) into a temporary root, so the client's
//! read-only view is checked against that layout rather than against itself.

use std::path::{Path, PathBuf};

use super::artifact::{self, Refusal};

/// A temporary store root, standing in for `$HORON_HOME/artifacts`.
fn store() -> tempfile::TempDir {
    tempfile::tempdir().expect("an artifact root")
}

/// Write one artifact where the store keeps it, returning its path.
fn write(root: &Path, session: &str, name: &str, text: &str) -> PathBuf {
    let directory = root.join(session);
    std::fs::create_dir_all(&directory).expect("a session store");
    let path = directory.join(name);
    std::fs::write(&path, text).expect("an artifact");
    path
}

#[test]
fn a_retained_artifact_resolves_by_its_id() {
    let root = store();
    let path = write(root.path(), "session_a", "0.bash.log", "first output");
    write(root.path(), "session_a", "1.read.log", "second output");
    let resolved = artifact::resolve(root.path(), "session_a", 0, true).expect("the id resolves");
    assert_eq!(resolved, path);
    assert_eq!(
        std::fs::read_to_string(resolved).expect("the artifact is readable"),
        "first output"
    );
}

#[test]
fn the_bare_id_and_the_pointer_name_the_same_artifact() {
    assert_eq!(artifact::parse("7"), Ok(7));
    assert_eq!(artifact::parse(" artifact://7 "), Ok(7));
}

#[test]
fn an_id_that_only_another_session_holds_is_not_found_here() {
    let root = store();
    write(root.path(), "session_b", "5.bash.log", "not this session");
    // The store scanned is the attached session's, so the other file is never
    // reached: the answer is the same as for an id that is not there at all.
    assert_eq!(
        artifact::resolve(root.path(), "session_a", 5, true),
        Err(Refusal::Missing(5))
    );
    // A line that prefixes an id with a session is not an address this client
    // reads, so it never becomes a path.
    assert_eq!(
        artifact::parse("session_b:5"),
        Err(Refusal::NotAnId("session_b:5".to_string()))
    );
}

#[test]
fn a_remote_or_a_path_address_is_refused_before_any_file_is_read() {
    for address in [
        "https://example.com/output.log",
        "/etc/passwd",
        "../escape",
        "notes/output.log",
    ] {
        assert_eq!(
            artifact::parse(address),
            Err(Refusal::NotAnArtifact(address.to_string())),
            "{address} was not refused as an address"
        );
    }
    assert_eq!(
        artifact::parse("artifact://basename"),
        Err(Refusal::NotAnId("basename".to_string()))
    );
}

#[test]
fn a_stale_id_is_refused_without_a_path() {
    let root = store();
    write(root.path(), "session_a", "2.bash.log", "output");
    assert_eq!(
        artifact::resolve(root.path(), "session_a", 9, true),
        Err(Refusal::Missing(9))
    );
}

#[test]
fn a_store_root_that_is_not_a_directory_is_refused() {
    let parent = store();
    let root = parent.path().join("artifacts");
    std::fs::write(&root, "not a directory").expect("a file");
    assert_eq!(
        artifact::entries(&root, "session_a", true),
        Err(Refusal::NotADirectory(root.clone()))
    );
    assert_eq!(
        artifact::resolve(&root, "session_a", 0, true),
        Err(Refusal::NotADirectory(root))
    );
}

#[test]
fn the_listing_names_ids_in_order_and_leaves_out_names_the_store_did_not_write() {
    let root = store();
    write(root.path(), "session_a", "2.bash.log", "later");
    write(root.path(), "session_a", "0.read.log", "earlier");
    write(root.path(), "session_a", "not-an-id.log", "ignored");
    let entries = artifact::entries(root.path(), "session_a", true).expect("the store lists");
    assert_eq!(
        entries
            .iter()
            .map(|entry| (entry.id, entry.tool.as_str(), entry.bytes))
            .collect::<Vec<_>>(),
        vec![(0, "read", 7), (2, "bash", 5)]
    );
}

#[test]
fn an_unattached_or_unknown_session_names_no_store() {
    let root = store();
    assert_eq!(
        artifact::entries(root.path(), "session_a", true),
        Ok(Vec::new()),
        "a store with no artifacts is empty, not an error"
    );
    assert_eq!(
        artifact::resolve(root.path(), "../escape", 0, true),
        Err(Refusal::BadSession("../escape".to_string()))
    );
    assert_eq!(
        artifact::resolve(root.path(), "", 0, true),
        Err(Refusal::BadSession(String::new()))
    );
}

#[test]
fn a_session_on_another_endpoint_reads_no_local_store() {
    let root = store();
    // The remote session happens to report the same id this store holds under
    // the same name. The id cannot tell the two apart, so the connection fact
    // refuses the read.
    write(root.path(), "session_a", "3.bash.log", "a local session's output");
    assert_eq!(
        artifact::entries(root.path(), "session_a", false),
        Err(Refusal::RemoteSession)
    );
    assert_eq!(
        artifact::resolve(root.path(), "session_a", 3, false),
        Err(Refusal::RemoteSession)
    );
}
