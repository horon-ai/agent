//! Mixed new/old HOME parity for every surface Task16 converted.
//!
//! One HOME holds a canonical session, a preserved legacy session, and a
//! migrated session whose legacy snapshot is still on disk. Session loading,
//! search, peek, edit stats, and startup listing must report the same thing they
//! report for a legacy-only HOME, and a corrupt session must not remove a
//! neighbour.
use crate::message::{ContentBlock, Role};
use crate::session::Session;
use crate::tool::{search_options_for_test, session_search_report_for_test};
use std::path::{Path, PathBuf};

struct Home {
    dir: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("isolated agent home");
        crate::storage::create_store_dir(&dir.path().join("sessions"), true).expect("sessions dir");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", dir.path());
        crate::config::Config::invalidate_cache();
        Self { dir, previous }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn sessions(&self) -> PathBuf {
        self.dir.path().join("sessions")
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        match &self.previous {
            Some(home) => crate::env::set_var("HORON_HOME", home),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

fn text(value: &str) -> ContentBlock {
    ContentBlock::Text {
        text: value.to_string(),
        cache_control: None,
    }
}

fn canonical(id: &str, body: &str) {
    let mut session = Session::create_with_id(id.to_string(), None, None);
    session.mark_closed();
    session.add_message(Role::User, vec![text(body)]);
    session.save().expect("save canonical session");
}

fn preserved_legacy(home: &Home, id: &str, body: &str) {
    let mut session = Session::create_with_id(id.to_string(), None, None);
    session.mark_closed();
    session.last_pid = None;
    session.add_message(Role::User, vec![text(body)]);
    let path = crate::session::session_path(id).expect("legacy path");
    crate::storage::write_json(&path, &session).expect("legacy fixture");
    assert!(path.exists());
    assert!(!home.sessions().join(id).exists());
}

fn migrated_legacy(home: &Home, id: &str, body: &str) {
    preserved_legacy(home, id, body);
    Session::load(id).expect("migrate on first load");
    assert!(
        home.sessions().join(id).join("events.jsonl").exists(),
        "the canonical log must exist after the first load"
    );
    assert!(
        crate::session::session_path(id).expect("legacy path").exists(),
        "the legacy snapshot must stay preserved"
    );
}

fn search_bodies(home: &Home, query: &str) -> Vec<String> {
    let options = search_options_for_test("current-session");
    let report = session_search_report_for_test(&home.sessions(), query, &options).expect("search succeeds");
    let mut bodies = report
        .results
        .into_iter()
        .map(|result| result.snippet)
        .collect::<Vec<_>>();
    bodies.sort();
    bodies
}

fn peek_body(id: &str) -> String {
    let session = Session::load(id).expect("peek loads the session");
    session
        .active_context()
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn stats(id: &str) -> Option<harness_stats::SessionEditStats> {
    harness_stats::estimate_session_edit_stats(&crate::storage::horon_agent_dir().ok()?.join("sessions"), id)
}

fn startup_ids() -> Vec<String> {
    let mut ids = crate::recent_session_index::recent(100)
        .expect("recent listing")
        .into_iter()
        .map(|entry| entry.session_id)
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

#[test]
fn mixed_new_and_old_home_matches_a_legacy_only_home_on_every_surface() {
    let _guard = crate::storage::lock_test_env();
    let home = Home::new();

    canonical("session_parity_canonical_1770000001000", "parity-canonical");
    preserved_legacy(&home, "session_parity_legacy_1770000002000", "parity-legacy");
    migrated_legacy(&home, "session_parity_migrated_1770000003000", "parity-migrated");

    for (id, body) in [
        ("session_parity_canonical_1770000001000", "parity-canonical"),
        ("session_parity_legacy_1770000002000", "parity-legacy"),
        ("session_parity_migrated_1770000003000", "parity-migrated"),
    ] {
        assert_eq!(peek_body(id), body, "peek must read every layout");
        assert_eq!(
            Session::load(id).expect("session load reads every layout").id,
            id,
            "session loading must read every layout"
        );
        assert_eq!(
            stats(id),
            Some(harness_stats::SessionEditStats::default()),
            "a readable session with no edits reports zero, never unknown"
        );
        let hits = search_bodies(&home, "parity");
        assert!(
            hits.iter().any(|hit| hit.contains(body)),
            "search must find {body}: {hits:?}"
        );
    }

    let listed = startup_ids();
    for id in [
        "session_parity_canonical_1770000001000",
        "session_parity_legacy_1770000002000",
        "session_parity_migrated_1770000003000",
    ] {
        assert!(listed.contains(&id.to_string()), "startup must list {id}: {listed:?}");
    }
    assert_eq!(
        listed.iter().filter(|id| id.starts_with("session_parity_")).count(),
        3,
        "a preserved snapshot beside a canonical log is not a second session: {listed:?}"
    );
}

#[test]
fn corrupt_session_and_stale_pid_never_erase_a_neighbours_listing() {
    let _guard = crate::storage::lock_test_env();
    let home = Home::new();

    canonical("session_survivor_canonical_1770000004000", "survivor-canonical");
    preserved_legacy(&home, "session_survivor_legacy_1770000005000", "survivor-legacy");

    let corrupt = "session_corrupt_1770000006000";
    let corrupt_dir = home.sessions().join(corrupt);
    crate::storage::create_store_dir(&corrupt_dir, false).expect("corrupt session dir");
    let corrupt_bytes: &[u8] = b"{not a record}\n";
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(corrupt_dir.join("events.jsonl"))
            .expect("corrupt log");
        file.write_all(corrupt_bytes).expect("corrupt bytes");
        drop(file);
        horon_agent_platform::fs::set_file_permissions_owner_only(&corrupt_dir.join("events.jsonl"))
            .expect("owner-only log");
    }

    let pid_dir = home.path().join("active_pids");
    crate::storage::create_store_dir(&pid_dir, false).expect("pid dir");
    std::fs::write(pid_dir.join(corrupt), "999999").expect("stale pid");

    assert!(
        Session::load(corrupt).is_err(),
        "a corrupt original must fail its own load"
    );

    let listed = startup_ids();
    assert!(
        listed.contains(&"session_survivor_canonical_1770000004000".to_string()),
        "{listed:?}"
    );
    assert!(
        listed.contains(&"session_survivor_legacy_1770000005000".to_string()),
        "{listed:?}"
    );
    assert_eq!(
        peek_body("session_survivor_canonical_1770000004000"),
        "survivor-canonical"
    );
    assert_eq!(peek_body("session_survivor_legacy_1770000005000"), "survivor-legacy");
    assert!(
        search_bodies(&home, "survivor").len() >= 2,
        "search must keep both neighbours"
    );
    assert_eq!(
        std::fs::read(corrupt_dir.join("events.jsonl")).expect("corrupt bytes"),
        corrupt_bytes,
        "no raw fallback may rewrite an unreadable original"
    );

    crate::session::recover_crashed_sessions().expect("crash recovery must not fail on a corrupt peer");
    assert!(
        home.sessions()
            .join("session_survivor_canonical_1770000004000")
            .exists()
    );
    assert!(
        crate::session::session_path("session_survivor_legacy_1770000005000")
            .unwrap()
            .exists()
    );
}

mod harness_stats {
    pub use horon_agent_harness_api::{SessionEditStats, estimate_session_edit_stats};
}
