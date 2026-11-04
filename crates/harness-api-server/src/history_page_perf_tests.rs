//! Full-history pages cost the page, and never the session.
//!
//! Each page is compared with the replay the index replaces, and each cost is
//! asserted from process counters rather than from wall time.

use super::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Instant;

const TEXT_WORDS: [&str; 8] = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel"];

struct HomeVar(Option<std::ffi::OsString>);

impl HomeVar {
    fn set(path: &Path) -> Self {
        let previous = std::env::var_os("HORON_AGENT_HOME");
        unsafe { std::env::set_var("HORON_AGENT_HOME", path) };
        Self(previous)
    }
}

impl Drop for HomeVar {
    fn drop(&mut self) {
        match self.0.take() {
            Some(value) => unsafe { std::env::set_var("HORON_AGENT_HOME", value) },
            None => unsafe { std::env::remove_var("HORON_AGENT_HOME") },
        }
    }
}

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Installed {
    paths: store::SessionPaths,
    id: store::SessionId,
    log: PathBuf,
}

impl Installed {
    fn events(&self) -> usize {
        store::full_scan(&self.paths, &self.id).expect("log scans").events.len()
    }
}

fn text_for(index: usize) -> String {
    let words: Vec<&str> = (0..6)
        .map(|offset| TEXT_WORDS[(index + offset) % TEXT_WORDS.len()])
        .collect();
    format!("fixture message {index}: {}", words.join(" "))
}

fn install(home: &Path, id: &str, events: usize) -> Installed {
    let paths = store::SessionPaths::new(home.join("sessions"));
    let sid = store::SessionId::parse(id).expect("session id");
    let directory = paths.session_dir(&sid).expect("session dir");
    std::fs::create_dir_all(&directory).expect("create session dir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(home.join("sessions"), std::fs::Permissions::from_mode(0o700)).expect("private root");
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).expect("private session dir");
    }
    let mut bytes = Vec::new();
    let mut parent: Option<store::EventId> = None;
    for index in 0..events {
        let (kind, payload) = if index == 0 {
            (
                store::KnownEventKind::SessionCreated,
                json!(store::SessionCreatedPayload {
                    branch_root: false,
                    reset_provenance: None,
                    metadata: store::SessionMetadata::default(),
                }),
            )
        } else {
            let message: horon_agent_session_types::StoredMessage = serde_json::from_value(json!({
                "id": format!("m{index:06}"),
                "role": if index % 2 == 0 { "user" } else { "assistant" },
                "content": [{"type": "text", "text": text_for(index)}],
            }))
            .expect("stored message");
            (
                store::KnownEventKind::MessageAppended,
                json!(store::MessageAppendedPayload { message }),
            )
        };
        let sealed = store::EventDraft::new(
            sid.clone(),
            store::EventId::new_v4(),
            parent.clone(),
            store::OriginId::new_v4(),
            store::SessionMetadata::default().created_at + chrono::Duration::milliseconds(index as i64),
            store::EventKind::new(kind),
            payload,
        )
        .seal()
        .expect("seal event");
        bytes.extend(store::encode_record_line(sealed.envelope()).expect("encode record"));
        parent = Some(sealed.envelope().event_id.clone());
    }
    let log = directory.join("events.jsonl");
    std::fs::write(&log, &bytes).expect("write log");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o600)).expect("private log");
    }
    Installed { paths, id: sid, log }
}

fn scratch(label: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let home = std::env::temp_dir().join(format!("horon-page-{label}-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(home.join("sessions")).expect("create home");
    home
}

fn send(value: Value) -> ApiEvent {
    let mut state = BridgeState::default();
    let outbound = state.api_request_to_legacy(&value);
    assert_eq!(outbound.len(), 1, "expected one reply, got {outbound:?}");
    match outbound.into_iter().next().expect("one reply") {
        Outbound::Reply(frame) => frame.event,
        other => panic!("expected an API reply, got {other:?}"),
    }
}

fn page_of(event: ApiEvent) -> horon_agent_harness_api::SessionHistoryPage {
    match event {
        ApiEvent::SessionHistoryPage {
            session_id,
            head,
            messages,
            next_cursor,
        } => horon_agent_harness_api::SessionHistoryPage {
            session_id,
            head,
            messages,
            next_cursor,
        },
        other => panic!("expected a history page, got {other:?}"),
    }
}

fn digest(messages: &[horon_agent_harness_api::HistoryMessage]) -> String {
    let mut bytes = Vec::new();
    for message in messages {
        bytes.extend(serde_json::to_vec(message).expect("history message serializes"));
        bytes.push(b'\n');
    }
    store::sha256_hex(&bytes)
}

/// The pre-change page: replay the whole projection, then slice and flatten.
fn oracle(installed: &Installed, before: Option<&str>, limit: usize) -> Vec<horon_agent_harness_api::HistoryMessage> {
    let graph = store::EventGraph::open(&installed.paths, &installed.id).expect("graph opens");
    let head = graph.heads()[0].clone();
    let projection = graph.replay(Some(&head)).expect("replay");
    let mut rows: Vec<(String, horon_agent_harness_api::HistoryMessage)> = Vec::new();
    for message in projection.messages() {
        let value = serde_json::to_value(&message.message).expect("message serializes");
        rows.push((
            message.event_id.as_str().to_owned(),
            horon_agent_harness_api::HistoryMessage {
                response_stats: None,
                role: value["role"].as_str().unwrap_or_default().to_owned(),
                content: crate::translate::flatten_content(&value["content"]),
            },
        ));
    }
    let end = match before {
        Some(cursor) => rows
            .iter()
            .position(|(id, _)| id == cursor)
            .expect("cursor is on the head"),
        None => rows.len(),
    };
    rows[end.saturating_sub(limit)..end]
        .iter()
        .map(|(_, message)| message.clone())
        .collect()
}

fn event_id_at(installed: &Installed, position: usize) -> String {
    let graph = store::EventGraph::open(&installed.paths, &installed.id).expect("graph opens");
    let head = graph.heads()[0].clone();
    let projection = graph.replay(Some(&head)).expect("replay");
    projection.messages()[position].event_id.as_str().to_owned()
}

struct Measured {
    page: horon_agent_harness_api::SessionHistoryPage,
    io_read_bytes: u64,
    blob_bytes_read: u64,
    replayed: u64,
    flattened: u64,
    index_builds: u64,
    wall_ms: f64,
}

fn measure(label: &str, id: &str, cursor: Option<&str>, limit: u32, events: usize) -> Measured {
    let mut request = json!({"req":"get_history_page","id":1,"session_id":id,"limit":limit});
    if let Some(cursor) = cursor {
        request["before_event_id"] = json!(cursor);
    }
    horon_agent_perf_counters::reset();
    let started = Instant::now();
    let event = send(request);
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let counters = horon_agent_perf_counters::snapshot();
    let page = page_of(event);
    println!(
        "HISTORY_PAGE {}",
        serde_json::to_string(&json!({
            "page": label,
            "events": events,
            "limit": limit,
            "messages": page.messages.len(),
            "wall_ms": wall_ms,
            "io_read_bytes": counters.io_read_bytes,
            "blob_bytes_read": counters.blob_bytes_read,
            "replayed_messages": counters.history_page_replayed,
            "flattened_messages": counters.history_page_flattened,
            "reply_bytes": counters.history_page_wire_bytes,
            "index_builds": counters.history_index_builds,
            "head": page.head,
            "next_cursor": page.next_cursor,
            "digest": digest(&page.messages),
            "first_content": page.messages.first().map(|m| m.content.clone()),
            "last_content": page.messages.last().map(|m| m.content.clone()),
        }))
        .expect("line serializes")
    );
    Measured {
        page,
        io_read_bytes: counters.io_read_bytes,
        blob_bytes_read: counters.blob_bytes_read,
        replayed: counters.history_page_replayed,
        flattened: counters.history_page_flattened,
        index_builds: counters.history_index_builds,
        wall_ms,
    }
}

#[test]
fn first_middle_and_last_page_match_the_pre_change_output_on_100k_events() {
    let _lock = crate::translate::horon_agent_home_test_lock();
    let home = scratch("parity");
    let _home = HomeVar::set(&home);
    let installed = install(&home, "perf-events-100000", 100_000);
    let _cleanup = Cleanup(home.clone());
    assert_eq!(installed.events(), 100_000);

    let middle_cursor = event_id_at(&installed, 49_999);
    let last_cursor = event_id_at(&installed, 20);
    for (label, cursor) in [
        ("first", None),
        ("middle", Some(middle_cursor.as_str())),
        ("last", Some(last_cursor.as_str())),
    ] {
        let measured = measure(label, "perf-events-100000", cursor, 20, 100_000);
        let expected = oracle(&installed, cursor, 20);
        assert_eq!(measured.page.messages.len(), 20);
        assert_eq!(
            digest(&measured.page.messages),
            digest(&expected),
            "{label} page differs from the replay it replaces"
        );
    }
    let before = measure_replay("first", &installed);
    let after_page = measure("first", "perf-events-100000", None, 20, 100_000);
    let after = after_page.io_read_bytes + after_page.blob_bytes_read;
    println!("HISTORY_PAGE_AFTER {after} HISTORY_PAGE_BEFORE_TOTAL {before}");
    assert!(
        after * 10 < before,
        "a page must read at least ten times less than the replay it replaces: {after} vs {before}"
    );
}

/// The pre-change cost of one page: open the graph, replay, flatten, slice.
fn measure_replay(label: &str, installed: &Installed) -> u64 {
    horon_agent_perf_counters::reset();
    let started = Instant::now();
    let rows = oracle(installed, None, 20);
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let counters = horon_agent_perf_counters::snapshot();
    println!(
        "HISTORY_PAGE_BEFORE {}",
        serde_json::to_string(&json!({
            "page": label,
            "messages": rows.len(),
            "wall_ms": wall_ms,
            "io_read_bytes": counters.io_read_bytes,
            "blob_bytes_read": counters.blob_bytes_read,
        }))
        .expect("line serializes")
    );
    counters.io_read_bytes + counters.blob_bytes_read
}

fn baseline(label: &str) -> Vec<horon_agent_harness_api::HistoryMessage> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("history-page-log-500-{label}-20.json"));
    let document: Value = serde_json::from_slice(&std::fs::read(&path).expect("baseline fixture reads"))
        .expect("baseline fixture parses");
    document["messages"]
        .as_array()
        .expect("baseline messages")
        .iter()
        .map(|message| horon_agent_harness_api::HistoryMessage {
            response_stats: None,
            role: message["role"].as_str().unwrap_or_default().to_owned(),
            content: crate::translate::flatten_content(&message["content"]),
        })
        .collect()
}

fn committed_log() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/history-page-log-500/events.jsonl")
}

#[test]
fn pages_match_the_committed_pre_change_baseline() {
    let _lock = crate::translate::horon_agent_home_test_lock();
    let home = scratch("baseline");
    let _home = HomeVar::set(&home);
    let id = "perf-events-500";
    let installed = install_from(&home, id, &committed_log());
    let _cleanup = Cleanup(home.clone());
    assert_eq!(installed.events(), 500);

    let middle_cursor = event_id_at(&installed, 249);
    let last_cursor = event_id_at(&installed, 20);
    for (label, cursor) in [
        ("first", None),
        ("middle", Some(middle_cursor.as_str())),
        ("last", Some(last_cursor.as_str())),
    ] {
        let measured = measure(label, id, cursor, 20, 500);
        let expected = baseline(label);
        assert_eq!(expected.len(), 20, "the {label} baseline holds one page");
        assert_eq!(
            digest(&measured.page.messages),
            digest(&expected),
            "{label} page differs from the pre-change baseline captured by the fixture generator"
        );
        assert_eq!(measured.page.messages, expected, "{label} page content differs");
    }
}

fn install_from(home: &Path, id: &str, fixture: &Path) -> Installed {
    let paths = store::SessionPaths::new(home.join("sessions"));
    let sid = store::SessionId::parse(id).expect("session id");
    let directory = paths.session_dir(&sid).expect("session dir");
    std::fs::create_dir_all(&directory).expect("create session dir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(home.join("sessions"), std::fs::Permissions::from_mode(0o700)).expect("private root");
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).expect("private session dir");
    }
    let log = directory.join("events.jsonl");
    std::fs::copy(fixture, &log).expect("copy fixture log");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o600)).expect("private log");
    }
    Installed { paths, id: sid, log }
}

#[test]
fn one_page_cost_does_not_grow_with_the_session() {
    let _lock = crate::translate::horon_agent_home_test_lock();
    let mut sizes = Vec::new();
    for events in [1_000usize, 10_000, 100_000] {
        let home = scratch(&format!("growth-{events}"));
        let _home = HomeVar::set(&home);
        let installed = install(&home, &format!("perf-events-{events}"), events);
        let _cleanup = Cleanup(home.clone());
        let id = format!("perf-events-{events}");
        let cold = measure("cold", &id, None, 20, events);
        assert_eq!(cold.index_builds, 1, "the first request builds the index once");
        let warm = measure("warm", &id, None, 20, events);
        assert_eq!(warm.index_builds, 0);
        assert_eq!(warm.replayed, 0, "a warm page replays nothing at {events} events");
        assert_eq!(warm.flattened, 20);
        assert_eq!(warm.page.messages.len(), 20);
        let log_bytes = std::fs::metadata(&installed.log).expect("log metadata").len();
        let warm_read = warm.io_read_bytes + warm.blob_bytes_read;
        assert!(
            warm_read < log_bytes / 10,
            "a warm page must read far below the session: {warm_read} of {log_bytes}"
        );
        // Read by scripts/perf/check_structural.py: a warm page replays nothing,
        // flattens only the page, and its cost does not follow the session.
        println!(
            "STRUCTURAL_PERF section=page events={} page_limit={} warm_replayed={} warm_flattened={} warm_index_builds={} warm_read_bytes={} log_bytes={}",
            events, 20, warm.replayed, warm.flattened, warm.index_builds, warm_read, log_bytes
        );
        sizes.push((events, log_bytes, warm_read, warm.wall_ms));
    }

    let smallest = sizes.first().expect("one size").2;
    let largest = sizes.last().expect("one size").2;
    println!("PAGE_GROWTH {sizes:?}");
    assert!(
        largest < smallest * 2,
        "page bytes must not grow with the session: {largest} vs {smallest}"
    );
}

#[test]
fn a_cursor_from_a_replaced_head_is_refused_by_name() {
    let _lock = crate::translate::horon_agent_home_test_lock();
    let home = scratch("stale");
    let _home = HomeVar::set(&home);
    let installed = install(&home, "perf-stale-cursor", 60);
    let _cleanup = Cleanup(home.clone());
    let id = "perf-stale-cursor";

    let newest = page_of(send(json!({
        "req":"get_history_page","id":1,"session_id":id,"limit":5
    })));
    let cursor = newest.next_cursor.expect("older messages remain");
    let older = page_of(send(json!({
        "req":"get_history_page","id":2,"session_id":id,"before_event_id":cursor,"limit":5
    })));
    assert_eq!(older.messages.len(), 5);

    let rewind = event_id_at(&installed, 20);
    send(json!({
        "req":"select_session_head","id":3,"session_id":id,"head_event_id":rewind
    }));
    let stale = send(json!({
        "req":"get_history_page","id":4,"session_id":id,"before_event_id":cursor,"limit":5
    }));
    let (code, details) = match stale {
        ApiEvent::Error { code, details, .. } => (code, details),
        other => panic!("a cursor past the selected head must be refused, got {other:?}"),
    };
    assert_eq!(code, ErrorCode::InvalidRequest);
    assert_eq!(details.expect("a store code is reported").store_code, "invalid_cursor");

    let fresh = page_of(send(json!({
        "req":"get_history_page","id":5,"session_id":id,"limit":5
    })));
    assert_eq!(fresh.head, rewind, "the replaced head is served, not the stale one");
}
