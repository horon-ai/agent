//! One enumeration rule for canonical logs and preserved legacy snapshots.
use horon_agent_session_store::legacy::LegacySource;
use horon_agent_session_store::reader::{SourceLayout, layout, open, sources};
use horon_agent_session_store::*;
use serde_json::json;
use std::fs;
use std::os::unix::fs::DirBuilderExt;

mod support;

fn home() -> std::path::PathBuf {
    let home = support::temp_root("reader");
    fs::DirBuilder::new().mode(0o700).create(home.join("sessions")).unwrap();
    home
}

fn legacy_snapshot(home: &std::path::Path, id: &str, text: &str) -> Vec<u8> {
    let raw = serde_json::to_vec(&json!({
        "id": id,
        "created_at": "2020-01-01T00:00:00Z",
        "updated_at": "2020-01-02T00:00:00Z",
        "title": "legacy",
        "messages": [{"id": "m1", "role": "user", "content": [{"type": "text", "text": text}]}],
        "last_pid": null
    }))
    .unwrap();
    fs::write(home.join("sessions").join(format!("{id}.json")), &raw).unwrap();
    raw
}

fn canonical(home: &std::path::Path, id: &str, text: &str) -> SessionId {
    legacy_snapshot(home, id, text);
    let parsed = SessionId::parse(id).unwrap();
    let paths = SessionPaths::new(home.join("sessions"));
    LegacySource::new(home, parsed.clone())
        .unwrap()
        .migrate(&paths)
        .unwrap();
    parsed
}

#[test]
fn enumerates_both_layouts_and_lists_a_migrated_id_once() {
    // Given a canonical session whose preserved snapshot still sits beside its log,
    // and a legacy-only session that was never migrated.
    let home = home();
    canonical(&home, "canonical_session", "canonical body");
    legacy_snapshot(&home, "legacy_session", "legacy body");
    let paths = SessionPaths::new(home.join("sessions"));

    // When the shared reader enumerates the root.
    let found = sources(&paths).unwrap();
    let ids: Vec<&str> = found.iter().map(|s| s.session_id.as_str()).collect();

    // Then each session appears once, canonical first in preference.
    assert_eq!(found.len(), 2, "found {ids:?}");
    assert!(ids.contains(&"canonical_session"));
    assert!(ids.contains(&"legacy_session"));
    let canonical_source = found
        .iter()
        .find(|s| s.session_id.as_str() == "canonical_session")
        .unwrap();
    assert_eq!(canonical_source.layout, SourceLayout::Canonical);
    assert!(
        canonical_source.snapshot.ends_with("canonical_session/events.jsonl"),
        "{:?}",
        canonical_source.snapshot
    );
    let legacy_source = found
        .iter()
        .find(|s| s.session_id.as_str() == "legacy_session")
        .unwrap();
    assert_eq!(legacy_source.layout, SourceLayout::Legacy);
    assert!(legacy_source.journal.is_none());
}

#[test]
fn raw_bytes_are_the_canonical_log_or_the_legacy_snapshot_plus_journal() {
    // Given one canonical session and one legacy session with a journal tail.
    let home = home();
    let canonical_id = canonical(&home, "canonical_session", "canonical body");
    let snapshot = legacy_snapshot(&home, "legacy_session", "legacy body");
    let journal = b"{\"append_messages\":[]}\n";
    fs::write(home.join("sessions/legacy_session.journal.jsonl"), journal).unwrap();
    let paths = SessionPaths::new(home.join("sessions"));

    // When each source is read through the reader.
    let log = open(&paths, &canonical_id).unwrap().unwrap();
    let log_bytes = log.raw().unwrap();
    let legacy = open(&paths, &SessionId::parse("legacy_session").unwrap())
        .unwrap()
        .unwrap();
    let legacy_bytes = legacy.raw().unwrap();

    // Then the canonical source is the log itself and the legacy source is
    // snapshot plus the appended journal, exactly as the pre-store reader saw it.
    assert_eq!(
        log_bytes,
        fs::read(home.join("sessions/canonical_session/events.jsonl")).unwrap()
    );
    let mut expected = snapshot;
    expected.push(b'\n');
    expected.extend_from_slice(journal);
    assert_eq!(legacy_bytes, expected);
    assert_eq!(legacy.size, expected.len() as u64);
}

#[test]
fn reserved_entries_and_sidecars_are_not_sessions() {
    // Given a root holding store caches, edit counters, backups and temp files.
    let home = home();
    canonical(&home, "canonical_session", "body");
    legacy_snapshot(&home, "legacy_session", "body");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home.join("sessions/edit-stats"))
        .unwrap();
    fs::write(home.join("sessions/edit-stats/canonical_session.json"), b"{}").unwrap();
    fs::write(home.join("sessions/legacy_session.bak"), b"{}").unwrap();
    fs::write(home.join("sessions/legacy_session.json.tmp"), b"{}").unwrap();
    fs::write(home.join("sessions/.hidden.json"), b"{}").unwrap();
    fs::DirBuilder::new()
        .mode(0o700)
        .create(home.join("sessions/not_a_session_dir"))
        .unwrap();
    let paths = SessionPaths::new(home.join("sessions"));

    // When enumerated.
    let mut ids: Vec<String> = sources(&paths)
        .unwrap()
        .into_iter()
        .map(|s| s.session_id.as_str().to_string())
        .collect();
    ids.sort();

    // Then only the two real sessions are listed.
    assert_eq!(ids, ["canonical_session", "legacy_session"]);
    assert_eq!(
        layout(&paths, &SessionId::parse("canonical_session").unwrap()).unwrap(),
        Some(SourceLayout::Canonical)
    );
    assert_eq!(
        layout(&paths, &SessionId::parse("legacy_session").unwrap()).unwrap(),
        Some(SourceLayout::Legacy)
    );
    assert_eq!(
        layout(&paths, &SessionId::parse("absent_session").unwrap()).unwrap(),
        None
    );
}

#[test]
fn a_corrupt_log_is_still_enumerated_so_it_cannot_hide_other_sessions() {
    // Given a corrupt canonical log beside a healthy session.
    let home = home();
    canonical(&home, "healthy_session", "body");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(home.join("sessions/corrupt_session"))
        .unwrap();
    fs::write(home.join("sessions/corrupt_session/events.jsonl"), b"{not a record}\n").unwrap();
    let paths = SessionPaths::new(home.join("sessions"));

    // When enumerated.
    let found = sources(&paths).unwrap();

    // Then the corrupt session is still listed (its failure belongs to the
    // loader, not the listing) and the healthy session is unaffected.
    let ids: Vec<&str> = found.iter().map(|s| s.session_id.as_str()).collect();
    assert!(ids.contains(&"corrupt_session"), "found {ids:?}");
    assert!(ids.contains(&"healthy_session"), "found {ids:?}");
    let corrupt = open(&paths, &SessionId::parse("corrupt_session").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        corrupt.raw().unwrap(),
        fs::read(home.join("sessions/corrupt_session/events.jsonl")).unwrap()
    );
}
