use horon_agent_session_store::legacy::{LegacySource, MigrationOutcome};
use horon_agent_session_store::*;
use serde_json::json;
use std::fs;
use std::os::unix::fs::DirBuilderExt;

#[path = "legacy_cases/auxiliary.rs"]
mod auxiliary;
#[path = "legacy_cases/guards.rs"]
mod guards;
#[path = "legacy_cases/helpers.rs"]
mod helpers;
#[cfg(feature = "legacy-session-index-import")]
#[path = "legacy_cases/index.rs"]
mod index;
#[path = "legacy_cases/interruptions.rs"]
mod interruptions;
#[path = "legacy_cases/replay.rs"]
mod replay;

mod support;

#[test]
fn snapshot_is_preserved_when_migrated_and_repeated() {
    // Given a real legacy home with an unknown integer and inert command text.
    let home = support::temp_root("legacy");
    fs::DirBuilder::new().mode(0o700).create(home.join("sessions")).unwrap();
    let raw = serde_json::to_vec(&json!({
        "id":"old", "created_at":"2020-01-01T00:00:00Z",
        "updated_at":"2020-01-02T00:00:00Z", "title":"legacy",
        "messages":[{"id":"m1","role":"user","content":[
            {"type":"text","text":"do not execute: rm -rf /"}]}],
        "future":18446744073709551615u64, "last_pid":null
    }))
    .unwrap();
    let path = home.join("sessions/old.json");
    fs::write(&path, &raw).unwrap();
    let id = SessionId::parse("old").unwrap();
    let paths = SessionPaths::new(home.join("sessions"));
    let source = LegacySource::new(&home, id.clone()).unwrap();
    // When migrating twice through the public filesystem surface.
    let first = source.migrate(&paths).unwrap();
    let second = source.migrate(&paths).unwrap();
    // Then replay is intact, the second call is a no-op, and source bytes stay exact.
    assert_eq!(first.outcome, MigrationOutcome::Published);
    assert_eq!(second.outcome, MigrationOutcome::AlreadyPublished);
    let view = EventGraph::open(&paths, &id).unwrap().replay(None).unwrap();
    assert_eq!(view.messages().len(), 1);
    assert_eq!(view.metadata().legacy_extensions["future"], json!(u64::MAX));
    assert_eq!(fs::read(path).unwrap(), raw);
    println!("LEGACY_FIXTURE={}", home.display());
}
