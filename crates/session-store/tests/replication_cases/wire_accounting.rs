use super::*;
use horon_agent_session_store::replication::{reset_wire_work, wire_work};

/// Digests of the two pages `baseline_pages` exports, captured from the loop
/// that re-serialized the whole page after every append (`dev 9d283c8`). The
/// loop now carries its wire size forward, and these pin the bytes it emits.
///
/// Re-captured once when the retired canary session flag left the default
/// metadata payload: the first page carries the root record, so it shrank by
/// exactly that field (240882 -> 240862); the last page has no metadata and is
/// unchanged.
const BASELINE_FIRST_SHA256: &str = "e6be497bfb5f781c7d06f9ca1b95389a634c1727f1ae148d83bc7d9e41ac69c4";
const BASELINE_LAST_SHA256: &str = "4a4855d4ca11533655bb7417d94a47e334a09bc956bc27b638d7c07f124ac264";
const BASELINE_FIRST_LEN: usize = 240862;
const BASELINE_LAST_LEN: usize = 26979;

fn escaped(n: u128, parent: u128) -> SealedEvent {
    seal(
        n,
        Some(parent),
        "Future-Event",
        json!({"escaped":"\"\\\n\t\u{7}".repeat(4000)}),
    )
}

fn externalized(n: u128, parent: u128) -> SealedEvent {
    EventDraft::new(
        sid(),
        eid(n),
        Some(eid(parent)),
        OriginId::parse(uuid::Uuid::from_u128(999).to_string()).unwrap(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::parse("Future-Event"),
        json!({"bytes":"x".repeat(64 * 1024)}),
    )
    .seal_with_limit(1024)
    .unwrap()
}

/// One page shape that exercises every field the export loop accounts for:
/// escaped inline records, an externalized payload reference, a continuation
/// cursor, and a second page that ends the transfer.
fn baseline_pages() -> (SessionEventPage, SessionEventPage) {
    let a = Home::new();
    let mut events = vec![root(), escaped(2, 1), externalized(3, 2)];
    events.extend((4..=300).map(|n| unknown(n, n - 1)));
    seed(&a, &events);
    let first = export_events(&a.paths(), &request(&[300])).unwrap();
    let mut next = request(&[300]);
    next.cursor = first.next_cursor.clone();
    let last = export_events(&a.paths(), &next).unwrap();
    (first, last)
}

#[test]
fn exported_page_bytes_are_identical_to_the_pre_change_export() {
    // Given a page with escaped records, a blob reference and a continuation.
    let (first, last) = baseline_pages();
    assert!(!first.blob_refs.is_empty(), "the fixture must carry a blob reference");
    assert!(first.next_cursor.is_some(), "the fixture must need a second page");
    assert_eq!(first.events.len(), MAX_PAGE_EVENTS);
    // When both pages are serialized exactly as the wire sees them.
    let first_bytes = serde_json::to_vec(&first).unwrap();
    let last_bytes = serde_json::to_vec(&last).unwrap();
    println!(
        "PAGE_DIGEST first={} last={} first_len={} last_len={}",
        sha256_hex(&first_bytes),
        sha256_hex(&last_bytes),
        first_bytes.len(),
        last_bytes.len()
    );
    // Then the incremental accounting emits the same bytes as the loop that
    // re-serialized the whole page after every append.
    assert_eq!(sha256_hex(&first_bytes), BASELINE_FIRST_SHA256);
    assert_eq!(sha256_hex(&last_bytes), BASELINE_LAST_SHA256);
    assert_eq!(first_bytes.len(), BASELINE_FIRST_LEN);
    assert_eq!(last_bytes.len(), BASELINE_LAST_LEN);
}

#[test]
fn an_oversized_page_is_refused_with_the_same_error() {
    // Given records whose escaped wire form exceeds the page cap together.
    let mut events = vec![root()];
    for n in 2..=3 {
        let huge = seal(
            n,
            Some(n - 1),
            "Future-Event",
            json!({"escaped":"\"".repeat(3 * 1024 * 1024)}),
        );
        assert!(huge.envelope().payload_ref.is_none(), "the fixture must stay inline");
        events.push(huge);
    }
    let oversized = page(&events);
    assert!(serde_json::to_vec(&oversized).unwrap().len() > MAX_WIRE_BYTES);
    // When that page is imported.
    let b = Home::new();
    // Then the cap refuses it before any original is published.
    assert_eq!(
        import_events(&b.paths(), &oversized).unwrap_err().code,
        StoreErrorCode::RecordTooLarge
    );
    assert!(!b.0.join("sessions").exists(), "a refused page publishes nothing");
}

#[test]
fn a_page_never_mixes_events_appended_after_its_graph_snapshot() {
    // Given a first page taken over a fixed head.
    let a = Home::new();
    let mut events = vec![root()];
    events.extend((2..=20).map(|n| unknown(n, n - 1)));
    seed(&a, &events);
    let first = export_events(&a.paths(), &request(&[20])).unwrap();
    // When an event lands on a second branch after that page was built.
    let mut writer = BlockingWriter::open(a.paths(), sid()).unwrap();
    writer.append(&unknown(21, 1)).unwrap();
    let mut next = request(&[20]);
    next.cursor = first.next_cursor.clone();
    // Then the continuation carries the page's own closure, not the new head.
    let continuation = export_events(&a.paths(), &next).unwrap();
    assert_eq!(continuation.heads, vec![eid(20)]);
    assert!(
        !continuation.events.iter().any(|event| event.event_id == eid(21)),
        "a page built before the append must not carry the appended event"
    );
    assert_eq!(
        export_events(&a.paths(), &request(&[21])).unwrap().events.len(),
        2,
        "the new branch is reachable only through its own heads"
    );
}

fn linear_log(home: &Home, events: usize) {
    let mut writer = BlockingWriter::open(home.paths(), sid()).unwrap();
    writer.append(&root()).unwrap();
    for n in 2..=events as u128 {
        writer.append(&unknown(n, n - 1)).unwrap();
    }
}

#[test]
fn export_serialization_work_is_linear_in_the_page() {
    // Given a session long enough to fill a page and a quarter of one.
    let a = Home::new();
    linear_log(&a, MAX_PAGE_EVENTS);
    let measure = |head: u128| {
        reset_wire_work();
        let page = export_events(&a.paths(), &request(&[head])).unwrap();
        let (bytes, serializations) = wire_work();
        (page.events.len(), bytes, serializations)
    };
    // When both pages are exported through the real loop.
    let (small_events, small_bytes, small_calls) = measure(64);
    let (large_events, large_bytes, large_calls) = measure(MAX_PAGE_EVENTS as u128);
    let small_per_event = small_bytes as f64 / small_events as f64;
    let large_per_event = large_bytes as f64 / large_events as f64;
    println!(
        "EXPORT_WORK small={small_events} events {small_bytes} bytes {small_calls} serializations \
         {small_per_event:.1} bytes/event | large={large_events} events {large_bytes} bytes \
         {large_calls} serializations {large_per_event:.1} bytes/event"
    );
    let (small_reference, large_reference) = (
        reserialize_per_append(64, &a),
        reserialize_per_append(MAX_PAGE_EVENTS as u128, &a),
    );
    println!(
        "EXPORT_WORK reference (whole page re-serialized after each append) \
         small={small_reference} bytes large={large_reference} bytes ratio={:.1}x | carried ratio={:.1}x",
        large_reference as f64 / small_reference as f64,
        large_bytes as f64 / small_bytes as f64
    );
    // Then each record is serialized once and the work per event stays flat.
    assert!(
        small_calls <= 2 * small_events + 2,
        "one serialization per record and cursor"
    );
    assert!(
        large_calls <= 2 * large_events + 2,
        "one serialization per record and cursor"
    );
    assert!(
        large_per_event < small_per_event * 1.25,
        "serialized bytes per exported event must stay flat: {small_per_event:.1} then {large_per_event:.1}"
    );
    // The shape this loop replaced grew with the square of the page, so the
    // comparison itself has to widen: 4x the events must cost far more than 4x
    // the reference bytes, while the carried size stays near 4x.
    assert!(
        large_reference > 12 * small_reference,
        "re-serializing the whole page per append must show the quadratic cost"
    );
    assert!(
        large_bytes < 5 * small_bytes,
        "carrying the size forward must stay proportional to the page"
    );
}

/// Serialized bytes the loop spent before it carried its size forward: the whole
/// page, again, after every append.
fn reserialize_per_append(head: u128, home: &Home) -> usize {
    let page = export_events(&home.paths(), &request(&[head])).unwrap();
    let mut growing = SessionEventPage {
        events: Vec::new(),
        ..page.clone()
    };
    let mut total = 0;
    for record in &page.events {
        growing.events.push(record.clone());
        total += serde_json::to_vec(&growing).unwrap().len();
    }
    total
}

/// Exports recorded sessions and prints the serialization work and page digest,
/// so a benchmark run can compare the counts against the recorded event log.
///
/// Ignored by default: it needs `HORON_AGENT_EXPORT_FIXTURES` pointing at a
/// directory holding `event-log-<n>/events.jsonl` recordings.
#[test]
#[ignore = "reads recorded event logs named by HORON_AGENT_EXPORT_FIXTURES"]
fn recorded_session_export_work_and_digest() {
    let fixtures = std::env::var("HORON_AGENT_EXPORT_FIXTURES").expect("HORON_AGENT_EXPORT_FIXTURES");
    let root = std::env::temp_dir().join(format!("export-fixtures-{}", uuid::Uuid::new_v4()));
    for case in ["event-log-100", "event-log-1000", "event-log-10000"] {
        let log = std::fs::read_to_string(std::path::Path::new(&fixtures).join(case).join("events.jsonl")).unwrap();
        let first: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        let id = SessionId::parse(first["session_id"].as_str().unwrap()).unwrap();
        let paths = SessionPaths::new(root.join(case).join("sessions"));
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(paths.root())
            .unwrap();
        let session_dir = paths.session_dir(&id).unwrap();
        fs::DirBuilder::new().mode(0o700).create(&session_dir).unwrap();
        let log_path = paths.events_log(&id).unwrap();
        fs::write(&log_path, &log).unwrap();
        std::fs::set_permissions(&log_path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
        let logged = log.lines().count();
        let request = ExportRequest {
            protocol_version: REPLICATION_PROTOCOL_VERSION,
            session_id: id,
            heads: Vec::new(),
            known_ids: Vec::new(),
            cursor: None,
        };
        reset_wire_work();
        let started = std::time::Instant::now();
        let page = export_events(&paths, &request).unwrap();
        let elapsed = started.elapsed();
        let (bytes, serializations) = wire_work();
        let page_bytes = serde_json::to_vec(&page).unwrap();
        println!(
            "FIXTURE {case} logged={logged} exported={} wire_bytes={bytes} serializations={serializations} \
             bytes_per_event={:.1} page_len={} page_digest={} elapsed_ms={:.1}",
            page.events.len(),
            bytes as f64 / page.events.len() as f64,
            page_bytes.len(),
            sha256_hex(&page_bytes),
            elapsed.as_secs_f64() * 1000.0
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}
