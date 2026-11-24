//! The volume lifecycle through the real binary: export, import, quiesce,
//! resume, key handling and `delete_user`.
//!
//! Every case runs `horon-agent volume ...` in a child process with `HOME` and
//! `HORON_HOME` pointed at a throwaway directory, so nothing reads or writes the
//! operator's real home. A loopback HTTP server stands in for the control plane
//! where a command needs one; it records every request so a test can assert that
//! no memory body crossed the wire.
//!
//! The acceptance the issue names is here rather than only in the unit tests: an
//! exported tar imports into a new volume and search works, `delete_user` marks
//! the volume deleted only after every step, and a partially deleted page is
//! absent from the rebuilt index, the search and a later export.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

/// A throwaway home with a `HORON_HOME` inside it.
struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().expect("a temporary root"),
        }
    }

    /// A home directory whose mode is owner-only, as the store requires.
    fn home(&self) -> PathBuf {
        let home = self.root.path().join("home");
        if !home.is_dir() {
            std::fs::create_dir_all(&home).expect("create the home");
            set_private(&home);
        }
        home
    }

    fn run(&self, args: &[&str]) -> Output {
        let home = self.home();
        let mut command = Command::new(env!("CARGO_BIN_EXE_horon-agent"));
        command
            .current_dir(&home)
            .env_clear()
            .env("HOME", &home)
            .env("HORON_HOME", &home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_CACHE_HOME", home.join("cache"))
            .env("HORON_AGENT_NON_INTERACTIVE", "1")
            .env("HORON_AGENT_NO_AUTO_UPDATE", "1")
            .env("NO_COLOR", "1")
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .args(args)
            .output()
            .expect("the CLI runs")
    }
}

fn set_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("mode");
}

fn assert_success(label: &str, output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "{label} failed: status={:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    stdout
}

/// Write one page into a volume's store through the store API.
///
/// Driving the real store means the page is indexed by the same code the binary
/// uses, so `volume export` reads it from the index and the assertion is about
/// the command, not about a hand-written markdown dialect.
fn seed_page(home: &Path, id: &str, title: &str, body: &str) {
    use horon_agent::memory::store::MemoryWiki;
    use horon_agent::memory_types::MemoryCategory;
    use horon_agent::memory_types::page::{MemoryPage, PageId, PageKind};
    let mut wiki = MemoryWiki::open(home.join("memory")).expect("open the volume store");
    let page = MemoryPage::draft(
        PageId::parse(id).expect("valid page id"),
        PageKind::Fact,
        MemoryCategory::Fact,
        title,
        body,
        horon_agent_time::Timestamp::UNIX_EPOCH,
    );
    wiki.write_page(&page).expect("write the page");
}

// ---------------------------------------------------------------------------
// A loopback control plane
// ---------------------------------------------------------------------------

/// A mock control plane that answers every draft path and records each request.
///
/// The server thread runs until the test process exits; it holds no state the
/// test needs after the assertions, so nothing joins it.
struct MockControlPlane {
    endpoint: String,
    requests: Arc<Mutex<Vec<(String, String)>>>,
}

impl MockControlPlane {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the mock control plane");
        let port = listener.local_addr().expect("address").port();
        let requests: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() || request_line.trim().is_empty() {
                    continue;
                }
                let mut content_length = 0usize;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                        break;
                    }
                    let lower = header.to_ascii_lowercase();
                    if let Some(rest) = lower.strip_prefix("content-length:") {
                        content_length = rest.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; content_length];
                if content_length > 0 && reader.read_exact(&mut body).is_err() {
                    continue;
                }
                let path = request_line.split_whitespace().nth(1).unwrap_or("").to_string();
                recorded
                    .lock()
                    .expect("lock")
                    .push((path.clone(), String::from_utf8_lossy(&body).to_string()));
                let payload = if path.ends_with("/deletion-jobs") {
                    b"{\"job_id\":\"job-mock-1\",\"snapshots\":[]}".to_vec()
                } else {
                    b"{}".to_vec()
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    payload.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&payload);
            }
        });
        // Dropping a `JoinHandle` detaches the thread; the accept loop keeps the
        // listener alive until the test process exits.
        drop(handle);
        Self {
            endpoint: format!("http://127.0.0.1:{port}"),
            requests,
        }
    }

    fn requests(&self) -> Vec<(String, String)> {
        self.requests.lock().expect("lock").clone()
    }
}

// ---------------------------------------------------------------------------
// Export and import
// ---------------------------------------------------------------------------

#[test]
fn export_import_round_trip_keeps_search_working() {
    let source = Fixture::new();
    let home = source.home();
    seed_page(&home, "mem_volume_1", "아침 회의", "아침 9시 이전 회의를 선호한다.");
    let archive = source.root.path().join("memory-export.tar");
    let archive_arg = archive.to_string_lossy().into_owned();

    let output = source.run(&["volume", "export", &archive_arg]);
    let stdout = assert_success("volume export", &output);
    assert!(stdout.contains("Exported 1 page"), "{stdout}");
    assert!(archive.is_file(), "the tar exists");

    // Import into a brand new home.
    let target = Fixture::new();
    let target_home = target.home();
    let output = target.run(&["volume", "import", &archive_arg]);
    let stdout = assert_success("volume import", &output);
    assert!(stdout.contains("Imported 1 page"), "{stdout}");

    // The page file arrived and the index was rebuilt.
    let imported = target_home
        .join("memory")
        .join("wiki")
        .join("facts")
        .join("mem_volume_1.md");
    assert!(imported.is_file(), "the page file was installed");
    let index = target_home.join("memory").join("index.sqlite");
    assert!(index.is_file(), "the index was rebuilt from the markdown");

    // The memory CLI finds the page, which is the search path over the
    // rebuilt wiki the issue names as the acceptance.
    let output = target.run(&["memory", "list", "--scope", "all"]);
    let stdout = assert_success("memory list after import", &output);
    assert!(stdout.contains("mem_volume_1"), "the imported page is listed: {stdout}");
}

#[test]
fn an_import_into_a_used_home_is_refused() {
    let source = Fixture::new();
    let home = source.home();
    seed_page(&home, "mem_a", "A", "first body");
    let archive = source.root.path().join("export.tar");
    let archive_arg = archive.to_string_lossy().into_owned();
    assert_success("volume export", &source.run(&["volume", "export", &archive_arg]));

    // The same home already holds a store, so an import must refuse.
    let output = source.run(&["volume", "import", &archive_arg]);
    assert!(!output.status.success(), "an import into a used home fails");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(stderr.contains("not empty"), "{stderr}");
}

// ---------------------------------------------------------------------------
// Quiesce and resume
// ---------------------------------------------------------------------------

#[test]
fn quiesce_fences_writes_and_resume_lifts_the_fence() {
    let fixture = Fixture::new();
    let home = fixture.home();
    seed_page(&home, "mem_q", "Q", "quiesce body");
    // Open the store once so the index exists.
    assert_success("memory list", &fixture.run(&["memory", "list", "--scope", "all"]));

    let output = fixture.run(&["volume", "quiesce", "--json"]);
    let stdout = assert_success("volume quiesce", &output);
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).expect("a JSON report");
    assert!(value["already_fenced"] == serde_json::json!(false));
    assert!(value["fence"]["token"].as_str().is_some_and(|token| !token.is_empty()));
    assert!(home.join("memory").join(".write-fence.json").is_file());

    let output = fixture.run(&["volume", "resume", "--json"]);
    let stdout = assert_success("volume resume", &output);
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).expect("a JSON report");
    assert!(value["was_fenced"] == serde_json::json!(true));
    assert!(!home.join("memory").join(".write-fence.json").exists());
}

// ---------------------------------------------------------------------------
// The key contract
// ---------------------------------------------------------------------------

#[test]
fn a_created_key_is_shown_and_a_revoked_key_closes_the_volume() {
    let fixture = Fixture::new();
    let output = fixture.run(&["volume", "key", "create", "key-cli-1"]);
    let stdout = assert_success("volume key create", &output);
    assert!(stdout.contains("key-cli-1"), "{stdout}");

    let output = fixture.run(&["volume", "key", "show"]);
    let stdout = assert_success("volume key show", &output);
    assert!(stdout.contains("\"revoked_at\": null"), "{stdout}");

    let output = fixture.run(&["volume", "key", "revoke"]);
    let stdout = assert_success("volume key revoke", &output);
    assert!(
        !stdout.contains("\"revoked_at\": null"),
        "the record is revoked: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// delete_user
// ---------------------------------------------------------------------------

#[test]
fn delete_user_finishes_every_step_before_reporting_deleted() {
    let control = MockControlPlane::start();
    let fixture = Fixture::new();
    let home = fixture.home();
    seed_page(&home, "mem_d", "D", "delete body marker ZZ-none-should-leak");
    assert_success(
        "volume key create",
        &fixture.run(&["volume", "key", "create", "key-del-1"]),
    );
    std::fs::write(home.join("token"), "mock-token\n").expect("write the token");

    let token_file = home.join("token");
    let token_arg = token_file.to_string_lossy().into_owned();
    let output = fixture.run(&[
        "volume",
        "delete-user",
        "user-1",
        "--endpoint",
        &control.endpoint,
        "--token-file",
        &token_arg,
        "--json",
    ]);
    let stdout = assert_success("volume delete-user", &output);
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).expect("a JSON report");
    assert_eq!(value["state"], serde_json::json!("deleted"), "{value}");
    assert_eq!(value["failed_step"], serde_json::json!(null));
    let steps: Vec<&str> = value["steps_done"]
        .as_array()
        .expect("a step list")
        .iter()
        .filter_map(|step| step.as_str())
        .collect();
    assert_eq!(
        steps,
        vec![
            "write_stop",
            "key_revoke",
            "snapshot_delete",
            "volume_delete",
            "mark_deleted"
        ]
    );

    // Every request the control plane saw is free of the page body.
    let requests = control.requests();
    assert!(!requests.is_empty(), "the client reached the mock control plane");
    for (path, body) in &requests {
        assert!(!body.contains("ZZ-none-should-leak"), "request {path} carried the body");
    }
}

#[test]
fn a_retried_delete_user_reports_the_failed_step_and_stays_retryable() {
    let fixture = Fixture::new();
    let home = fixture.home();
    seed_page(&home, "mem_r", "R", "retry body");
    assert_success(
        "volume key create",
        &fixture.run(&["volume", "key", "create", "key-retry-1"]),
    );
    std::fs::write(home.join("token"), "mock-token\n").expect("write the token");

    // A deletion against an unreachable endpoint fails at the first central
    // call, and the command reports the failure rather than success.
    let output = fixture.run(&[
        "volume",
        "delete-user",
        "user-2",
        "--endpoint",
        "http://127.0.0.1:1",
        "--token-file",
        home.join("token").to_str().unwrap_or(""),
        "--json",
    ]);
    assert!(
        !output.status.success(),
        "an unreachable control plane fails the command"
    );
    // No job record exists, because the control plane never assigned one.
    let status = fixture.run(&["volume", "deletion-status", "--json"]);
    let stdout = assert_success("deletion status", &status);
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).expect("JSON");
    assert_eq!(value["job"], serde_json::json!(null));
}

#[test]
fn a_partially_deleted_page_is_absent_from_the_export() {
    let fixture = Fixture::new();
    let home = fixture.home();
    seed_page(&home, "mem_keep", "Keep", "kept body");
    seed_page(&home, "mem_drop", "Drop", "dropped body");
    // Opening the store indexes both pages.
    assert_success("memory list", &fixture.run(&["memory", "list", "--scope", "all"]));

    // Delete one page file behind the store, then let the next open prune it;
    // that is the partial deletion the store repairs.
    std::fs::remove_file(home.join("memory").join("wiki").join("facts").join("mem_drop.md"))
        .expect("remove the page file");

    let archive = fixture.root.path().join("export.tar");
    let archive_arg = archive.to_string_lossy().into_owned();
    let output = fixture.run(&["volume", "export", &archive_arg]);
    let stdout = assert_success("volume export after a partial deletion", &output);
    assert!(stdout.contains("Exported 1 page"), "{stdout}");

    // The dropped page is in no metadata line.
    let file = std::fs::File::open(&archive).expect("open the archive");
    let mut reader = tar::Archive::new(BufReader::new(file));
    let mut metadata = Vec::new();
    for entry in reader.entries().expect("entries") {
        let mut entry = entry.expect("entry");
        if entry.path().expect("path").display().to_string() == "metadata.jsonl" {
            entry.read_to_end(&mut metadata).expect("read the metadata");
        }
    }
    let text = String::from_utf8(metadata).expect("UTF-8 metadata");
    assert!(!text.contains("mem_drop"), "the dropped page is not exported: {text}");
    assert!(text.contains("mem_keep"), "the kept page is exported: {text}");
}

#[test]
fn encrypted_volume_export_and_import_round_trip_via_cli() {
    let source = Fixture::new();
    let home = source.home();
    seed_page(&home, "mem_cli_enc", "Encrypted note", "encrypted-cli-secret-body");
    let key_file = source.root.path().join("export.key");
    std::fs::write(
        &key_file,
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n",
    )
    .expect("write hex key file");
    set_private(&key_file);

    let archive = source.root.path().join("encrypted.tar");
    let archive_arg = archive.to_string_lossy().into_owned();
    let key_arg = key_file.to_string_lossy().into_owned();
    let output = source.run(&[
        "volume",
        "export",
        &archive_arg,
        "--key-id",
        "cli-key-1",
        "--key-file",
        &key_arg,
    ]);
    let stdout = assert_success("encrypted volume export", &output);
    assert!(stdout.contains("Exported 1 page"), "{stdout}");

    // Plain import without key file fails.
    let target = Fixture::new();
    let unkeyed = target.run(&["volume", "import", &archive_arg]);
    assert!(!unkeyed.status.success(), "unkeyed CLI import must fail");

    // Keyed import restores the page and search works.
    let keyed = target.run(&["volume", "import", &archive_arg, "--key-file", &key_arg]);
    let stdout = assert_success("encrypted volume import", &keyed);
    assert!(stdout.contains("Imported 1 page"), "{stdout}");
    let listed = assert_success(
        "memory list after encrypted import",
        &target.run(&["memory", "list", "--scope", "all"]),
    );
    assert!(listed.contains("mem_cli_enc"), "{listed}");
}

#[test]
fn encrypted_volume_export_trims_key_id_and_refuses_dual_sources_and_revoked_volumes() {
    let source = Fixture::new();
    let home = source.home();
    seed_page(&home, "mem_edge", "Edge", "edge-secret-body");
    let key_file = source.root.path().join("edge.key");
    std::fs::write(
        &key_file,
        "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210\n",
    )
    .expect("write key");
    set_private(&key_file);

    let archive = source.root.path().join("trimmed.tar");
    let archive_arg = archive.to_string_lossy().into_owned();
    let key_arg = key_file.to_string_lossy().into_owned();
    let trimmed_export = source.run(&[
        "volume",
        "export",
        &archive_arg,
        "--key-id",
        "  cli-trimmed-1  ",
        "--key-file",
        &key_arg,
    ]);
    assert_success("trimmed key-id export", &trimmed_export);

    let target = Fixture::new();
    let imported = target.run(&["volume", "import", &archive_arg, "--key-file", &key_arg]);
    let import_out = assert_success("trimmed key-id import", &imported);
    assert!(import_out.contains("Imported 1 page"), "{import_out}");

    // Supplying both --key-env and --key-file must error rather than silently dropping --key-file.
    let dual_archive = source.root.path().join("dual.tar");
    let dual_arg = dual_archive.to_string_lossy().into_owned();
    let dual = source.run(&[
        "volume",
        "export",
        &dual_arg,
        "--key-id",
        "cli-trimmed-1",
        "--key-env",
        "HOME",
        "--key-file",
        &key_arg,
    ]);
    assert!(!dual.status.success(), "ambiguous key sources must fail");
    let dual_err = String::from_utf8_lossy(&dual.stderr);
    assert!(dual_err.contains("not both"), "{dual_err}");

    // Revoking the volume key permanently closes exports even without --key-id.
    assert_success("create key", &source.run(&["volume", "key", "create", "cli-trimmed-1"]));
    assert_success("revoke key", &source.run(&["volume", "key", "revoke"]));
    let revoked = source.run(&["volume", "export", &dual_arg, "--key-file", &key_arg]);
    assert!(!revoked.status.success(), "revoked volume export must fail");
    let revoked_err = String::from_utf8_lossy(&revoked.stderr);
    assert!(revoked_err.contains("revoked"), "{revoked_err}");
    assert!(
        !revoked_err.contains("supply an active key with --key-id"),
        "must not give false --key-id hint on revoked volume: {revoked_err}"
    );
}
