use super::helpers::*;
use horon_agent_session_store::AuxiliaryRecordedPayload;
use horon_agent_session_store::legacy::*;
use serde_json::{Value, json};
use std::fs;

#[test]
fn journal_replays_new_tail_when_snapshot_contains_stale_prefix() {
    // Given a duplicated snapshot and already-checkpointed journal prefix.
    let home = fixture();
    let mut raw = snapshot();
    raw["messages"].as_array_mut().unwrap().push(msg("two", "assistant"));
    put(&home, "sessions/old.json", &serde_json::to_vec(&raw).unwrap());
    let journal = [
        journal(vec![msg("two", "assistant")], "2020-01-01T00:00:00Z"),
        journal(vec![], "2020-01-01T00:00:00Z"),
        journal(
            vec![msg("three", "user"), msg("four", "assistant")],
            "2020-01-03T00:00:00Z",
        ),
        journal(vec![], "2019-01-01T00:00:00Z"),
    ]
    .join("\n");
    put(&home, "sessions/old.journal.jsonl", journal.as_bytes());
    let before = hashes(&home);
    // When the real public migrator replays it.
    let report = source(&home).migrate(&paths(&home)).unwrap();
    // Then first IDs win, newer-prefix transition makes later metadata apply even with older time.
    assert_eq!(
        view(&home)
            .messages()
            .iter()
            .map(|m| m.message.id.as_str())
            .collect::<Vec<_>>(),
        ["one", "two", "three", "four"]
    );
    assert_eq!(
        view(&home).metadata().updated_at.to_rfc3339(),
        "2019-01-01T00:00:00+00:00"
    );
    assert!(
        report
            .receipt
            .diagnostics
            .iter()
            .any(|d| matches!(d, LegacyDiagnostic::DuplicateMessages { count: 2 }))
    );
    preserved(&home, &before);
}

#[test]
fn glued_and_torn_lines_preserve_complete_entries_and_report_damage() {
    // Given torn bytes, two glued complete entries and a trailing torn append.
    let home = fixture();
    let data = format!(
        "{{\"meta\":broken{}{}\n{{torn",
        journal(vec![msg("four", "assistant")], "2020-01-03T00:00:00Z"),
        journal(vec![msg("five", "user")], "2020-01-04T00:00:00Z")
    );
    put(&home, "sessions/old.journal.jsonl", data.as_bytes());
    put(&home, "sessions/old.journal.corrupt.jsonl", b"diagnostic only");
    put(&home, "sessions/old.json.pre-wipe-1.bak", b"not a recovery candidate");
    let before = hashes(&home);
    // When replaying without invoking old healing routines.
    let report = source(&home).migrate(&paths(&home)).unwrap();
    // Then complete entries survive and damaged lines are visible.
    assert_eq!(view(&home).messages().len(), 5);
    assert!(
        report
            .receipt
            .diagnostics
            .contains(&LegacyDiagnostic::JournalDamage { line: 1, salvaged: 2 })
    );
    assert!(
        report
            .receipt
            .diagnostics
            .contains(&LegacyDiagnostic::JournalDamage { line: 2, salvaged: 0 })
    );
    preserved(&home, &before);
}

#[test]
fn ordinary_backup_is_read_in_memory_only_when_primary_is_corrupt() {
    // Given invalid primary and a valid ordinary backup.
    let home = fixture();
    put(
        &home,
        "sessions/old.bak",
        &fs::read(home.join("sessions/old.json")).unwrap(),
    );
    put(&home, "sessions/old.json", b"{broken");
    let before = hashes(&home);
    // When migrating.
    let report = source(&home).migrate(&paths(&home)).unwrap();
    // Then backup replay succeeds without copyback.
    assert!(report.receipt.diagnostics.contains(&LegacyDiagnostic::BackupRecovered));
    assert_eq!(view(&home).messages().len(), 3);
    preserved(&home, &before);
}

#[test]
fn primary_wins_and_fork_sidecars_are_preserved_when_backup_differs() {
    // Given a fork, all five todo suffixes, archive and exact statistics.
    let home = fixture();
    let mut backup = snapshot();
    backup["title"] = json!("wrong backup");
    put(&home, "sessions/old.bak", &serde_json::to_vec(&backup).unwrap());
    for suffix in ["", "-goals", "-plan", "-gate-observations", "-review-state"] {
        put(
            &home,
            &format!("todos/old{suffix}.json"),
            br#"{"unknown":18446744073709551615}"#,
        );
    }
    put(
        &home,
        "sdk-archive.json",
        br#"{"sessions":{"old":123},"archive_after_days":7}"#,
    );
    put(
        &home,
        "sessions/edit-stats/old.json",
        br#"{"added":17,"removed":2,"approximate":true}"#,
    );
    let before = hashes(&home);
    // When migrating.
    source(&home).migrate(&paths(&home)).unwrap();
    // Then title, parent, archive and every auxiliary value remain accessible.
    let view = view(&home);
    assert_eq!(view.metadata().title.as_deref(), Some("snapshot"));
    assert_eq!(view.metadata().parent_id.as_deref(), Some("parent"));
    assert!(view.archive().unwrap().archived);
    let data = serde_json::to_value(view.auxiliary()).unwrap();
    for suffix in ["", "-goals", "-plan", "-gate-observations", "-review-state"] {
        assert!(data.to_string().contains(&format!("todos/old{suffix}.json")));
    }
    preserved(&home, &before);
}

#[test]
fn identical_logical_sources_have_identical_events_across_homes() {
    // Given different raw JSON formatting and installation origins.
    let a = fixture();
    let b = fixture();
    put(&a, "installation-origin.json", br#"{"id":"installation-a"}"#);
    put(&b, "installation-origin.json", br#"{"id":"installation-b"}"#);
    put(
        &b,
        "sessions/old.json",
        &serde_json::to_vec_pretty(&snapshot()).unwrap(),
    );
    let before_a = hashes(&a);
    let before_b = hashes(&b);
    // When independently migrating both.
    let ra = source(&a).migrate(&paths(&a)).unwrap();
    let rb = source(&b).migrate(&paths(&b)).unwrap();
    // Then local raw receipts differ but replicated identities, parent order and bytes do not.
    assert_ne!(ra.receipt.source_digest, rb.receipt.source_digest);
    assert_eq!(ra.receipt.logical_source_digest, rb.receipt.logical_source_digest);
    assert_eq!(ra.receipt.events, rb.receipt.events);
    assert_eq!(
        fs::read(a.join("sessions/old/events.jsonl")).unwrap(),
        fs::read(b.join("sessions/old/events.jsonl")).unwrap()
    );
    assert_eq!(
        source(&a).migrate(&paths(&a)).unwrap().outcome,
        MigrationOutcome::AlreadyPublished
    );
    assert_eq!(
        source(&b).migrate(&paths(&b)).unwrap().outcome,
        MigrationOutcome::AlreadyPublished
    );
    preserved(&a, &before_a);
    preserved(&b, &before_b);
}

#[test]
fn native_window_and_legacy_compaction_counts_survive_when_boundary_is_valid() {
    // Given one covered user/assistant pair and a retained latest user.
    let home = fixture();
    let mut raw = snapshot();
    let window = json!([{"type":"message","content":"retained"},
        {"type":"compaction","encrypted_content":"opaque","integer":u64::MAX},
        {"type":"other","value":-0.0}]);
    raw["compaction"] = json!({"summary_text":"","covers_up_to_turn":2,
        "original_turn_count":2,"compacted_count":2,"native_window":window});
    put(&home, "sessions/old.json", &serde_json::to_vec(&raw).unwrap());
    let before = hashes(&home);
    // When migrating into source-bound compaction.
    source(&home).migrate(&paths(&home)).unwrap();
    // Then every native item and the original count artifact survive.
    let view = view(&home);
    assert_eq!(
        view.compaction().unwrap().1.native_window,
        window.as_array().unwrap().clone()
    );
    assert_eq!(view.active_messages().len(), 1);
    assert_eq!(view.messages().len(), 3);
    assert_eq!(view.metadata().legacy_extensions["unknown"]["large"], json!(u64::MAX));
    preserved(&home, &before);
}

#[test]
fn read_only_legacy_surface_creates_nothing_when_database_is_absent() {
    // Given JSON only.
    let home = fixture();
    let before = hashes(&home);
    // When reading, not applying migration.
    let read = source(&home).read().unwrap();
    // Then no store or DB is created.
    assert_eq!(read.messages.len(), 3);
    assert!(!home.join("sessions/old").exists());
    preserved(&home, &before);
}
#[test]
fn removed_panel_events_in_a_legacy_journal_load_as_untouched_data() {
    // Given a journal whose replay events include event types this build no
    // longer defines. The fixture lives outside `.rs` sources so the removal
    // grep stays clean while the bytes stay realistic.
    let home = fixture();
    put(
        &home,
        "sessions/old.journal.jsonl",
        include_bytes!("../fixtures/removed_panel_journal.jsonl"),
    );
    let before = hashes(&home);
    let expected: Value = serde_json::from_slice(include_bytes!("../fixtures/removed_panel_journal.jsonl")).unwrap();
    // When the real public migrator replays it.
    let report = source(&home).migrate(&paths(&home)).unwrap();
    // Then nothing rejects the unknown event types and the bytes survive.
    assert_eq!(report.outcome, MigrationOutcome::Published);
    let projection = view(&home);
    assert_eq!(projection.messages().len(), 3);
    let AuxiliaryRecordedPayload::Legacy { value, .. } = &projection.auxiliary()[0].1 else {
        panic!("legacy auxiliary");
    };
    assert_eq!(value["replay_events"], expected["append_replay_events"]);
    preserved(&home, &before);
}
