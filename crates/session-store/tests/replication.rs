use horon_agent_session_store::replication::*;
use horon_agent_session_store::*;
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;

#[path = "replication_cases/r2_regressions.rs"]
mod r2_regressions;
#[path = "replication_cases/record_refs.rs"]
mod record_refs;

#[path = "replication_cases/blob_publication.rs"]
mod blob_publication;
#[path = "replication_cases/blobs.rs"]
mod blobs;
#[path = "replication_cases/cache.rs"]
mod cache;
#[path = "replication_cases/failures.rs"]
mod failures;
#[path = "replication_cases/interruption.rs"]
mod interruption;
#[path = "replication_cases/paging.rs"]
mod paging;
#[path = "replication_cases/references.rs"]
mod references;

#[path = "replication_cases/wire_accounting.rs"]
mod wire_accounting;

struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let path = root.join(format!("replication-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        println!("FIXTURE {}", path.display());
        Self(path)
    }
    fn paths(&self) -> SessionPaths {
        SessionPaths::new(self.0.join("sessions"))
    }
    fn bytes(&self) -> Vec<u8> {
        let path = self.paths().events_log(&sid()).unwrap();
        match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => panic!("{e}"),
        }
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
        println!("CLEANED {}", self.0.display());
    }
}
fn sid() -> SessionId {
    SessionId::parse("replica").unwrap()
}
fn eid(n: u128) -> EventId {
    EventId::parse(uuid::Uuid::from_u128(n).to_string()).unwrap()
}
fn seal(n: u128, parent: Option<u128>, kind: &str, payload: Value) -> SealedEvent {
    EventDraft::new(
        sid(),
        eid(n),
        parent.map(eid),
        OriginId::parse(uuid::Uuid::from_u128(999).to_string()).unwrap(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::parse(kind),
        payload,
    )
    .seal()
    .unwrap()
}
fn root() -> SealedEvent {
    seal(
        1,
        None,
        "session_created",
        json!({"metadata":SessionMetadata::default(),"branch_root":false}),
    )
}
fn unknown(n: u128, parent: u128) -> SealedEvent {
    seal(
        n,
        Some(parent),
        "Future-Event",
        json!({"large":u64::MAX,"real":1.0,"negative":-0.0}),
    )
}
fn seed(home: &Home, events: &[SealedEvent]) {
    let mut writer = BlockingWriter::open(home.paths(), sid()).unwrap();
    for event in events {
        writer.append(event).unwrap();
    }
}
fn request(heads: &[u128]) -> ExportRequest {
    ExportRequest {
        protocol_version: REPLICATION_PROTOCOL_VERSION,
        session_id: sid(),
        heads: heads.iter().copied().map(eid).collect(),
        known_ids: Vec::new(),
        cursor: None,
    }
}
fn page(events: &[SealedEvent]) -> SessionEventPage {
    SessionEventPage {
        protocol_version: REPLICATION_PROTOCOL_VERSION,
        session_id: sid(),
        heads: events
            .last()
            .map(|e| vec![e.envelope().event_id.clone()])
            .unwrap_or_default(),
        events: events
            .iter()
            .map(|e| e.envelope().canonical_record().unwrap())
            .collect(),
        next_cursor: None,
        blob_refs: events.iter().filter_map(|e| e.envelope().payload_ref.clone()).collect(),
        record_refs: Vec::new(),
    }
}

#[test]
fn canonical_bytes_converge_when_reordered_and_retransmitted() {
    // Given independently located homes and exact numeric/unknown event originals.
    let a = Home::new();
    let b = Home::new();
    seed(&a, &[root(), unknown(2, 1), unknown(3, 2)]);
    let original = a.bytes();
    let mut wire = export_events(&a.paths(), &request(&[3])).unwrap();
    assert!(wire.events[1].canonical_json.contains("18446744073709551615"));
    assert!(wire.events[1].canonical_json.contains("-0.0"));
    wire.events.reverse();
    // When importing reordered records twice and exporting in the opposite direction.
    import_events(&b.paths(), &wire).unwrap();
    let before = b.bytes();
    import_events(&b.paths(), &wire).unwrap();
    let back = export_events(&b.paths(), &request(&[3])).unwrap();
    import_events(&a.paths(), &back).unwrap();
    // Then both original logs and the opaque canonical records are unchanged.
    assert_eq!(original, a.bytes());
    assert_eq!(original, b.bytes());
    assert_eq!(before, b.bytes());
    assert_eq!(back.events, wire.events.into_iter().rev().collect::<Vec<_>>());
    assert_eq!(
        EventGraph::open(&b.paths(), &sid())
            .unwrap()
            .replay(None)
            .unwrap_err()
            .code,
        StoreErrorCode::UnsupportedEventKind
    );
    println!("ORIGINAL_SHA256 {}", sha256_hex(&original));
}

#[test]
fn independently_migrated_legacy_histories_exchange_without_new_originals() {
    // Given equivalent legacy sources in two homes.
    let a = Home::new();
    let b = Home::new();
    let legacy = json!({"id":"replica","created_at":"2020-01-01T00:00:00Z",
        "messages":[{"id":"one","role":"user","content":[{"type":"text","text":"hello"}]}],
        "unknown":{"number":18446744073709551615u64}});
    for home in [&a, &b] {
        fs::DirBuilder::new().mode(0o700).create(home.paths().root()).unwrap();
        fs::write(
            home.paths().root().join("replica.json"),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        legacy::LegacySource::new(&home.0, sid())
            .unwrap()
            .migrate(&home.paths())
            .unwrap();
    }
    let before_a = a.bytes();
    let before_b = b.bytes();
    assert_eq!(before_a, before_b);
    // When exchanging both ways through the public replication API.
    for (source, target) in [(&a, &b), (&b, &a)] {
        let graph = EventGraph::open(&source.paths(), &sid()).unwrap();
        let mut req = request(&[]);
        req.heads = graph.heads().to_vec();
        let wire = export_events(&source.paths(), &req).unwrap();
        import_events(&target.paths(), &wire).unwrap();
    }
    // Then migration identity/hash and original source bytes remain untouched.
    assert_eq!(before_a, a.bytes());
    assert_eq!(before_b, b.bytes());
    assert_eq!(
        fs::read(a.paths().root().join("replica.json")).unwrap(),
        fs::read(b.paths().root().join("replica.json")).unwrap()
    );
}
