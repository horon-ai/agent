//! Real-process QA for the non-destructive session commands.
//!
//! Every case spawns the shipped `horon-agent` binary against an isolated HOME,
//! so a command that only works in memory fails here. Each adversarial case
//! captures the refusal text and proves the originals are byte-identical.
#![cfg(unix)]

use horon_agent::session::event_store_api as store;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BINARY: &str = env!("CARGO_BIN_EXE_horon-agent");

struct Sandbox {
    /// Kept alive so the directory outlives the case; `root` is its real path.
    _directory: tempfile::TempDir,
    /// Real path of the scratch root. The session store refuses paths with a
    /// symlinked component and macOS keeps temporary directories under `/var`
    /// (a symlink to `/private/var`), so every fixture path is resolved once.
    root: PathBuf,
}

impl Sandbox {
    fn new(label: &str) -> Self {
        let directory = tempfile::Builder::new()
            .prefix(&format!("horon-sessions-{label}-"))
            .tempdir()
            .expect("temp dir");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).expect("private root");
        let root = directory.path().canonicalize().expect("real scratch root");
        for name in ["run", "config", "cache", "tmp"] {
            fs::create_dir_all(root.join(name)).expect("support dir");
        }
        Self {
            _directory: directory,
            root,
        }
    }

    fn path(&self) -> &Path {
        &self.root
    }

    fn home(&self, name: &str) -> PathBuf {
        let home = self.path().join(name);
        fs::create_dir_all(&home).expect("home");
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).expect("private home");
        home
    }

    fn run(&self, home: &Path, args: &[&str]) -> Output {
        Command::new(BINARY)
            .current_dir(self.path())
            .env_clear()
            .env("HOME", self.path())
            .env("HORON_HOME", home)
            .env("HORON_AGENT_RUNTIME_DIR", self.path().join("run"))
            .env("XDG_RUNTIME_DIR", self.path().join("run"))
            .env("XDG_CONFIG_HOME", self.path().join("config"))
            .env("XDG_CACHE_HOME", self.path().join("cache"))
            .env("TMPDIR", self.path().join("tmp"))
            .env("PATH", "/usr/bin:/bin")
            .arg("--no-update")
            .args(args)
            .output()
            .expect("spawn horon-agent")
    }

    fn migrate(&self, home: &Path, id: &str) -> Output {
        self.run(home, &["sessions", "migrate", id, "--apply"])
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn stdout(output: &Output) -> String {
    text(&output.stdout)
}

fn stderr(output: &Output) -> String {
    text(&output.stderr)
}

fn assert_ok(output: &Output) {
    assert!(
        output.status.success(),
        "command failed with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        stdout(output),
        stderr(output)
    );
}

fn assert_refused(output: &Output, needle: &str) {
    assert!(
        !output.status.success(),
        "command unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        stdout(output),
        stderr(output)
    );
    let combined = format!("{}{}", stdout(output), stderr(output));
    assert!(
        combined.contains(needle),
        "expected refusal containing {needle:?}, got:\nstdout:\n{}\nstderr:\n{}",
        stdout(output),
        stderr(output)
    );
}

fn sha(path: &Path) -> String {
    store::sha256_hex(&fs::read(path).expect("read for hash"))
}

fn tree(root: &Path) -> BTreeMap<String, String> {
    fn visit(root: &Path, here: &Path, out: &mut BTreeMap<String, String>) {
        let Ok(entries) = fs::read_dir(here) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .expect("relative")
                .to_string_lossy()
                .into_owned();
            let kind = entry.file_type().expect("file type");
            if kind.is_dir() {
                out.insert(relative, "<dir>".into());
                visit(root, &path, out);
            } else if kind.is_file() {
                out.insert(relative, sha(&path));
            } else {
                out.insert(relative, "<other>".into());
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

fn message(id: &str, role: &str) -> Value {
    json!({"id":id,"role":role,"content":[{"type":"text","text":id}],
        "timestamp":"2020-01-01T01:00:00+01:00"})
}

fn write_legacy(home: &Path, id: &str) {
    let sessions = home.join("sessions");
    fs::create_dir_all(&sessions).expect("sessions");
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o700)).expect("private sessions");
    let snapshot = json!({
        "id": id,
        "created_at": "2020-01-01T00:00:00Z",
        "updated_at": "2020-01-02T00:00:00Z",
        "title": "snapshot",
        "provider_key": "openai",
        "model": "test",
        "route_api_method": "openai-api",
        "messages": [message("one","user"), message("two","assistant"), message("three","user")],
        "parent_id": "parent",
        "status": "completed",
        "last_pid": null
    });
    fs::write(
        sessions.join(format!("{id}.json")),
        serde_json::to_vec(&snapshot).expect("snapshot json"),
    )
    .expect("write legacy snapshot");
}

fn legacy_bytes(home: &Path, id: &str) -> Vec<u8> {
    fs::read(home.join("sessions").join(format!("{id}.json"))).expect("legacy original")
}

fn event_log(home: &Path, id: &str) -> PathBuf {
    home.join("sessions").join(id).join("events.jsonl")
}

fn records(bytes: &[u8]) -> usize {
    bytes.iter().filter(|byte| **byte == b'\n').count()
}

fn session_id(raw: &str) -> store::SessionId {
    store::SessionId::parse(raw).expect("session id")
}

fn paths(home: &Path) -> store::SessionPaths {
    store::SessionPaths::new(home.join("sessions"))
}

#[test]
fn dry_run_reports_the_plan_and_changes_nothing() {
    let sandbox = Sandbox::new("dry-run");
    let home = sandbox.home("a");
    write_legacy(&home, "old");
    let before = tree(&home.join("sessions"));

    let output = sandbox.run(&home, &["sessions", "migrate", "old"]);
    assert_ok(&output);
    assert!(
        stdout(&output).contains("dry-run"),
        "dry run must say so:\n{}",
        stdout(&output)
    );
    assert!(stdout(&output).contains("status=ready"));
    assert_eq!(tree(&home.join("sessions")), before);
    assert!(!event_log(&home, "old").exists());
    assert!(!home.join("sessions/old").exists());
}

#[test]
fn apply_is_idempotent_and_keeps_every_original_byte() {
    let sandbox = Sandbox::new("idempotent");
    let home = sandbox.home("a");
    write_legacy(&home, "old");
    let original = legacy_bytes(&home, "old");

    let first = sandbox.migrate(&home, "old");
    assert_ok(&first);
    assert!(stdout(&first).contains("status=published"));
    let log = fs::read(event_log(&home, "old")).expect("events log");
    assert!(!log.is_empty());

    let second = sandbox.migrate(&home, "old");
    assert_ok(&second);
    assert!(
        stdout(&second).contains("already-published"),
        "second apply must be a verified no-op:\n{}",
        stdout(&second)
    );
    assert_eq!(fs::read(event_log(&home, "old")).expect("events log"), log);
    assert_eq!(legacy_bytes(&home, "old"), original);
}

#[test]
fn a_fresh_home_can_be_exported_back_to_the_legacy_layout() {
    let sandbox = Sandbox::new("legacy-export");
    let home = sandbox.home("a");
    let target = sandbox.home("b");
    write_legacy(&home, "old");
    assert_ok(&sandbox.migrate(&home, "old"));
    let original = legacy_bytes(&home, "old");
    let target_arg = target.to_str().expect("utf-8 path");

    let output = sandbox.run(&home, &["sessions", "export-legacy", "old", "--output", target_arg]);
    assert_ok(&output);
    assert!(stdout(&output).contains("verified"));

    let read = store::legacy::LegacySource::new(&target, session_id("old"))
        .expect("legacy source")
        .read()
        .expect("read back the export");
    assert_eq!(read.messages.len(), 3);
    assert_eq!(read.metadata.title.as_deref(), Some("snapshot"));
    assert_eq!(legacy_bytes(&home, "old"), original);

    let again = sandbox.run(&home, &["sessions", "export-legacy", "old", "--output", target_arg]);
    assert_refused(&again, "refusing to overwrite");

    let over_home = sandbox.run(
        &home,
        &[
            "sessions",
            "export-legacy",
            "old",
            "--output",
            home.to_str().expect("utf-8 path"),
        ],
    );
    assert_refused(&over_home, "refusing to write the legacy export over the current HOME");
    assert_eq!(legacy_bytes(&home, "old"), original);

    // A symlinked alias of HOME must be refused as well: the guard compares the
    // real path, not the spelling the caller typed.
    let alias = sandbox.path().join("home-alias");
    std::os::unix::fs::symlink(&home, &alias).expect("home alias");
    let over_alias = sandbox.run(
        &home,
        &[
            "sessions",
            "export-legacy",
            "old",
            "--output",
            alias.to_str().expect("utf-8 path"),
        ],
    );
    assert_refused(&over_alias, "refusing to write the legacy export over the current HOME");
    assert_eq!(legacy_bytes(&home, "old"), original);

    // A relative output path is resolved against the current directory, the same
    // place the command writes it.
    let relative = sandbox.run(
        &home,
        &["sessions", "export-legacy", "old", "--output", "relative-export"],
    );
    assert_ok(&relative);
    assert!(sandbox.path().join("relative-export/sessions/old.json").is_file());
}

#[test]
fn archive_and_restore_append_events_and_delete_nothing() {
    let sandbox = Sandbox::new("archive");
    let home = sandbox.home("a");
    write_legacy(&home, "old");
    assert_ok(&sandbox.migrate(&home, "old"));
    let original = legacy_bytes(&home, "old");
    let before = fs::read(event_log(&home, "old")).expect("log");

    let archived = sandbox.run(&home, &["sessions", "archive", "old"]);
    assert_ok(&archived);
    let after_archive = fs::read(event_log(&home, "old")).expect("log");
    assert!(after_archive.starts_with(&before));
    assert_eq!(records(&after_archive), records(&before) + 1);
    assert!(text(&after_archive[before.len()..]).contains("archive_changed"));
    assert_eq!(
        store::archive::visibility(&paths(&home), &session_id("old")).expect("visibility"),
        Some(true)
    );

    let restored = sandbox.run(&home, &["sessions", "restore", "old"]);
    assert_ok(&restored);
    let after_restore = fs::read(event_log(&home, "old")).expect("log");
    assert!(after_restore.starts_with(&after_archive));
    assert_eq!(records(&after_restore), records(&before) + 2);
    assert_eq!(
        store::archive::visibility(&paths(&home), &session_id("old")).expect("visibility"),
        Some(false)
    );
    assert_eq!(legacy_bytes(&home, "old"), original);
}

#[test]
fn doctor_reports_damage_and_never_repairs_it() {
    let sandbox = Sandbox::new("doctor-damage");
    let home = sandbox.home("a");
    write_legacy(&home, "old");
    assert_ok(&sandbox.migrate(&home, "old"));
    let log = event_log(&home, "old");
    let mut bytes = fs::read(&log).expect("log");
    bytes.extend_from_slice(b"{\"not\":\"canonical\"}\n");
    fs::write(&log, &bytes).expect("append damage");

    let output = sandbox.run(&home, &["sessions", "doctor", "old", "--json"]);
    assert_refused(&output, "doctor found damage");
    let report: Value = serde_json::from_str(&stdout(&output)).expect("doctor json");
    assert_eq!(report["damaged"], json!(1));
    assert!(report["sessions"][0]["damage"]["code"].is_string());
    assert_eq!(
        fs::read(&log).expect("log"),
        bytes,
        "doctor reports damage but never repairs the original"
    );
}

#[test]
fn inspect_and_doctor_read_without_writing() {
    let sandbox = Sandbox::new("inspect");
    let home = sandbox.home("a");
    write_legacy(&home, "old");
    assert_ok(&sandbox.migrate(&home, "old"));
    let before = tree(&home.join("sessions"));

    let inspect = sandbox.run(&home, &["sessions", "inspect", "old", "--json"]);
    assert_ok(&inspect);
    let report: Value = serde_json::from_str(&stdout(&inspect)).expect("inspect json");
    assert_eq!(report["layout"], json!("canonical"));
    assert_eq!(report["messages"], json!(3));
    assert_eq!(report["title"], json!("snapshot"));
    assert_eq!(report["archived"], json!(false));
    assert_eq!(report["torn_tail_bytes"], json!(0));
    assert!(report["damage"].is_null());

    let doctor = sandbox.run(&home, &["sessions", "doctor"]);
    assert_ok(&doctor);
    assert!(stdout(&doctor).contains("0 damaged"));
    assert_eq!(tree(&home.join("sessions")), before);
}

#[test]
fn export_then_import_converges_on_the_same_events() {
    let sandbox = Sandbox::new("exchange");
    let home = sandbox.home("a");
    let other = sandbox.home("b");
    let bundle = sandbox.path().join("bundle");
    write_legacy(&home, "old");
    assert_ok(&sandbox.migrate(&home, "old"));
    let bundle_arg = bundle.to_str().expect("utf-8 path");

    let exported = sandbox.run(&home, &["sessions", "export", "old", "--output", bundle_arg]);
    assert_ok(&exported);
    let manifest: Value =
        serde_json::from_slice(&fs::read(bundle.join("manifest.json")).expect("manifest")).expect("manifest json");
    assert_eq!(manifest["kind"], json!("horon-agent-session-bundle"));

    let imported = sandbox.run(&other, &["sessions", "import", bundle_arg]);
    assert_ok(&imported);

    let id = session_id("old");
    let source = store::EventGraph::open(&paths(&home), &id).expect("source graph");
    let replica = store::EventGraph::open(&paths(&other), &id).expect("replica graph");
    assert_eq!(source.ordered_ids(), replica.ordered_ids());
    assert_eq!(source.heads(), replica.heads());
    let expected = fs::read(event_log(&home, "old")).expect("source log");
    assert_eq!(fs::read(event_log(&other, "old")).expect("replica log"), expected);

    let again = sandbox.run(&other, &["sessions", "import", bundle_arg]);
    assert_ok(&again);
    assert_eq!(fs::read(event_log(&other, "old")).expect("replica log"), expected);
}

#[test]
fn an_existing_output_path_is_refused_and_left_untouched() {
    let sandbox = Sandbox::new("existing-output");
    let home = sandbox.home("a");
    write_legacy(&home, "old");
    assert_ok(&sandbox.migrate(&home, "old"));
    let taken = sandbox.path().join("taken");
    fs::create_dir_all(&taken).expect("existing dir");
    fs::write(taken.join("keep.txt"), b"do not touch").expect("keep file");
    let before = tree(&taken);

    let output = sandbox.run(
        &home,
        &[
            "sessions",
            "export",
            "old",
            "--output",
            taken.to_str().expect("utf-8 path"),
        ],
    );
    assert_refused(&output, "already exists");
    assert_eq!(tree(&taken), before);
    assert!(!taken.join("manifest.json").exists());
}

#[test]
fn a_live_legacy_writer_blocks_migration_and_changes_nothing() {
    let sandbox = Sandbox::new("live-writer");
    let home = sandbox.home("a");
    write_legacy(&home, "old");
    let pids = home.join("active_pids");
    fs::create_dir_all(&pids).expect("pid dir");
    fs::write(pids.join("old"), std::process::id().to_string()).expect("live pid");
    let original = legacy_bytes(&home, "old");

    let output = sandbox.migrate(&home, "old");
    assert_refused(&output, "live legacy session owner");
    assert!(!event_log(&home, "old").exists());
    assert_eq!(legacy_bytes(&home, "old"), original);
}

#[test]
fn a_path_traversal_output_is_refused_before_any_write() {
    let sandbox = Sandbox::new("traversal");
    let home = sandbox.home("a");
    write_legacy(&home, "old");
    assert_ok(&sandbox.migrate(&home, "old"));
    let escape = format!("../t19-escape-{}", std::process::id());
    let escaped = sandbox
        .path()
        .parent()
        .expect("parent")
        .join(format!("t19-escape-{}", std::process::id()));

    let output = sandbox.run(&home, &["sessions", "export", "old", "--output", &escape]);
    assert_refused(&output, "contains '..'");
    assert!(!escaped.exists());
    assert_eq!(
        store::reader::layout(&paths(&home), &session_id("old")).expect("layout"),
        Some(store::reader::SourceLayout::Canonical)
    );
}

#[test]
fn recovery_refuses_to_write_over_the_originals() {
    let sandbox = Sandbox::new("recover");
    let home = sandbox.home("a");
    write_legacy(&home, "old");
    assert_ok(&sandbox.migrate(&home, "old"));
    let before = fs::read(event_log(&home, "old")).expect("log");
    let inside = home.join("sessions").join("recovered");

    let output = sandbox.run(
        &home,
        &[
            "sessions",
            "recover-to",
            "old",
            "--output",
            inside.to_str().expect("utf-8 path"),
        ],
    );
    assert_refused(&output, "would overwrite the originals");
    assert!(!inside.exists());
    assert_eq!(fs::read(event_log(&home, "old")).expect("log"), before);

    let at_root = sandbox.run(
        &home,
        &[
            "sessions",
            "recover-to",
            "old",
            "--output",
            home.join("sessions").to_str().expect("utf-8 path"),
        ],
    );
    assert_refused(&at_root, "requires a new destination root");
    assert_eq!(fs::read(event_log(&home, "old")).expect("log"), before);
}

#[cfg(not(feature = "legacy-session-index-import"))]
#[test]
fn a_build_without_the_import_feature_refuses_without_opening_the_database() {
    let sandbox = Sandbox::new("no-index-feature");
    let home = sandbox.home("a");
    write_legacy(&home, "old");
    let database = home.join("session-metadata-v1.sqlite3");
    fs::write(&database, b"not a database this build may open").expect("index placeholder");
    let before = fs::read(&database).expect("index bytes");
    fs::set_permissions(&database, fs::Permissions::from_mode(0o000)).expect("unreadable index");

    let output = sandbox.run(&home, &["sessions", "migrate", "old", "--apply", "--legacy-index"]);
    assert_refused(&output, "--legacy-index is not available in this build");
    assert!(format!("{}{}", stdout(&output), stderr(&output)).contains("legacy-session-index-import"));

    fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).expect("restore mode");
    assert_eq!(fs::read(&database).expect("index bytes"), before);
    assert!(!home.join("session-metadata-v1.sqlite3-wal").exists());
    assert!(!home.join("session-metadata-v1.sqlite3-shm").exists());
    assert!(!event_log(&home, "old").exists());
    assert_eq!(legacy_bytes(&home, "old").len(), legacy_bytes(&home, "old").len());
}

/// The flag must not be offered by a build that cannot act on it: a hidden flag
/// is still parsed, so this checks the help text rather than a parse failure.
#[cfg(not(feature = "legacy-session-index-import"))]
#[test]
fn a_build_without_the_import_feature_hides_the_flag_from_help() {
    let sandbox = Sandbox::new("no-index-help");
    let home = sandbox.home("a");
    let output = sandbox.run(&home, &["sessions", "migrate", "--help"]);
    assert_ok(&output);
    assert!(
        !stdout(&output).contains("--legacy-index"),
        "help must not offer a flag this build cannot honour:\n{}",
        stdout(&output)
    );
}

/// The other half: a build that can import the index must document the flag.
#[cfg(feature = "legacy-session-index-import")]
#[test]
fn a_build_with_the_import_feature_offers_the_flag_in_help() {
    let sandbox = Sandbox::new("index-help");
    let home = sandbox.home("a");
    let output = sandbox.run(&home, &["sessions", "migrate", "--help"]);
    assert_ok(&output);
    assert!(
        stdout(&output).contains("--legacy-index"),
        "a build that can import the index must document the flag:\n{}",
        stdout(&output)
    );
}

/// A dry run must name the remedy a released binary can follow, not the flag it
/// will refuse on the next hop.
#[cfg(not(feature = "legacy-session-index-import"))]
#[test]
fn a_dry_run_reports_the_remedy_a_released_build_can_follow() {
    let sandbox = Sandbox::new("no-index-dry-run");
    let home = sandbox.home("a");
    write_legacy(&home, "old");
    fs::write(home.join("session-metadata-v1.sqlite3"), b"index").expect("index placeholder");

    let output = sandbox.run(&home, &["sessions", "migrate", "old", "--dry-run", "--json"]);
    assert_ok(&output);
    let report: Value = serde_json::from_str(&stdout(&output)).expect("migrate json");
    assert_eq!(report["feature_enabled"], json!(false));
    assert_eq!(report["sessions"][0]["status"], json!("index-import-required"));
    assert_eq!(report["sessions"][0]["index_import_available"], json!(false));
    let remedy = report["sessions"][0]["remedy"].as_str().expect("remedy");
    assert!(
        remedy.contains("legacy-session-index-import"),
        "the remedy must name the build feature that resolves this: {remedy}"
    );
}

/// `inspect` on a session it cannot finish reading must say what to install.
#[cfg(not(feature = "legacy-session-index-import"))]
#[test]
fn inspect_names_the_remedy_for_an_unreadable_index() {
    let sandbox = Sandbox::new("no-index-inspect");
    let home = sandbox.home("a");
    write_legacy(&home, "old");
    fs::write(home.join("session-metadata-v1.sqlite3"), b"index").expect("index placeholder");

    let output = sandbox.run(&home, &["sessions", "inspect", "old", "--json"]);
    assert_ok(&output);
    let report: Value = serde_json::from_str(&stdout(&output)).expect("inspect json");
    assert_eq!(report["migration_required"], json!(true));
    assert_eq!(report["index_import_available"], json!(false));
    assert!(
        report["remedy"].as_str().is_some_and(|text| text.contains("legacy-session-index-import")),
        "inspect must name the build that can import it: {report}"
    );
}

#[cfg(feature = "legacy-session-index-import")]
#[test]
fn index_import_reads_a_staged_copy_and_leaves_the_database_untouched() {
    let sandbox = Sandbox::new("index-import");
    let home = sandbox.home("a");
    write_legacy(&home, "old");
    let database = home.join("session-metadata-v1.sqlite3");
    let connection = rusqlite::Connection::open(&database).expect("open index");
    connection
        .execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
            CREATE TABLE recent_sessions(session_id TEXT PRIMARY KEY, working_dir TEXT,
            generated_title TEXT, custom_title TEXT, todo_title TEXT, saved INTEGER,
            save_label TEXT, updated_at_ms INTEGER, last_active_at_ms INTEGER);
            PRAGMA wal_checkpoint(TRUNCATE);
            INSERT INTO recent_sessions VALUES('old','/legacy/project','generated','db title',
            'todo',1,'saved title',1600000000000,1600000001234);",
        )
        .expect("index schema");
    let files = [
        "session-metadata-v1.sqlite3",
        "session-metadata-v1.sqlite3-wal",
        "session-metadata-v1.sqlite3-shm",
    ];
    let before: Vec<(String, Vec<u8>)> = files
        .iter()
        .map(|name| (name.to_string(), fs::read(home.join(name)).expect("index bytes")))
        .collect();

    let output = sandbox.run(&home, &["sessions", "migrate", "old", "--apply", "--legacy-index"]);
    assert_ok(&output);

    for (name, bytes) in &before {
        assert_eq!(
            &fs::read(home.join(name)).expect("index bytes"),
            bytes,
            "{name} changed during the offline import"
        );
    }
    let inspect = sandbox.run(&home, &["sessions", "inspect", "old", "--json"]);
    assert_ok(&inspect);
    let report: Value = serde_json::from_str(&stdout(&inspect)).expect("inspect json");
    assert_eq!(report["custom_title"], json!("db title"));
    assert_eq!(report["saved"], json!(true));
    drop(connection);
}

/// `horon-agent export <id> --out x.md` writes exactly the bytes the request
/// returns, because both call one renderer.
#[test]
fn export_writes_the_same_markdown_the_api_reply_carries() {
    let sandbox = Sandbox::new("export-markdown");
    let home = sandbox.home("a");
    write_legacy(&home, "old");
    assert_ok(&sandbox.migrate(&home, "old"));
    let out = home.join("transcript.md");
    let out_arg = out.to_str().expect("utf-8 path");

    let output = sandbox.run(&home, &["export", "old", "--out", out_arg]);
    assert_ok(&output);
    assert!(
        stdout(&output).is_empty(),
        "the file form must not also print the transcript"
    );

    let written = fs::read(&out).expect("written transcript");
    let markdown = String::from_utf8(written).expect("utf-8 transcript");
    assert!(markdown.contains("### User"), "each user turn is a heading");
    assert!(markdown.contains("### Assistant"), "each assistant turn is a heading");

    // The same store root rendered in-process through the shared renderer the
    // harness API reply uses. One lock keeps the env swap from racing a sibling.
    let expected = {
        let _guard = EXPORT_HOME_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var_os("HORON_HOME");
        unsafe { std::env::set_var("HORON_HOME", &home) };
        let rendered = horon_agent::session::Session::load("old")
            .and_then(|session| horon_agent::session_export::export_session_markdown(&session));
        match previous {
            Some(value) => unsafe { std::env::set_var("HORON_HOME", value) },
            None => unsafe { std::env::remove_var("HORON_HOME") },
        }
        rendered.expect("render the same session")
    };
    assert_eq!(
        markdown, expected,
        "the file and the API reply must be one renderer's output"
    );

    let plain = sandbox.run(&home, &["export", "old"]);
    assert_ok(&plain);
    assert_eq!(stdout(&plain), markdown, "stdout form writes the same bytes");
}

static EXPORT_HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
