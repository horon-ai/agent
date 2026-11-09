//! Task 10: replication, head selection, and cursor-history API surface.
//!
//! These drive the same `api_request_to_legacy` boundary a real bridge uses,
//! with a real on-disk session store under an isolated HOME. The two-HOME
//! process/SDK surface lives in `tests/session_replication.rs`.

use super::*;
use horon_agent_harness_api::replication::{
    CanonicalRecordRef, MAX_REPLICATION_WIRE_BYTES, REPLICATION_PROTOCOL_VERSION,
};
use horon_agent_session_store as store;
use horon_agent_session_types::StoredMessage;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;

struct ScopedHome {
    path: PathBuf,
    previous: Option<OsString>,
    _guard: MutexGuard<'static, ()>,
}

fn home_lock() -> MutexGuard<'static, ()> {
    crate::translate::horon_agent_home_test_lock()
}

impl ScopedHome {
    fn new(label: &str) -> Self {
        let guard = home_lock();
        let previous = std::env::var_os("HORON_HOME");
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("horon-api-replication-{label}-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create isolated HORON_HOME");
        unsafe { std::env::set_var("HORON_HOME", &path) };
        Self {
            path,
            previous,
            _guard: guard,
        }
    }

    fn sessions(&self) -> store::SessionPaths {
        store::SessionPaths::new(self.path.join("sessions"))
    }
}

impl Drop for ScopedHome {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => unsafe { std::env::set_var("HORON_HOME", value) },
            None => unsafe { std::env::remove_var("HORON_HOME") },
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn text_message(id: &str, role: &str, text: &str) -> StoredMessage {
    serde_json::from_value(json!({
        "id": id,
        "role": role,
        "content": [{"type": "text", "text": text}]
    }))
    .expect("valid stored message")
}

struct Seeded {
    id: store::SessionId,
    head: store::EventId,
    event_ids: Vec<store::EventId>,
}

fn seed_session(paths: &store::SessionPaths, id: &str, messages: usize) -> Seeded {
    let sid = store::SessionId::parse(id).unwrap();
    let timestamp = store::SessionMetadata::default().created_at;
    let mut writer = store::BlockingWriter::open(paths.clone(), sid.clone()).unwrap();
    let root = store::EventDraft::new(
        sid.clone(),
        store::EventId::new_v4(),
        None,
        store::OriginId::new_v4(),
        timestamp,
        store::EventKind::new(store::KnownEventKind::SessionCreated),
        json!(store::SessionCreatedPayload {
            branch_root: false,
            reset_provenance: None,
            metadata: store::SessionMetadata::default(),
        }),
    )
    .seal()
    .unwrap();
    writer.append(&root).unwrap();
    let mut parent = root.envelope().event_id.clone();
    let mut event_ids = Vec::new();
    for index in 0..messages {
        let role = if index % 2 == 0 { "user" } else { "assistant" };
        let event = store::EventDraft::new(
            sid.clone(),
            store::EventId::new_v4(),
            Some(parent.clone()),
            store::OriginId::new_v4(),
            timestamp,
            store::EventKind::new(store::KnownEventKind::MessageAppended),
            json!(store::MessageAppendedPayload {
                message: text_message(&format!("m{index}"), role, &format!("message {index}")),
            }),
        )
        .seal()
        .unwrap();
        writer.append(&event).unwrap();
        parent = event.envelope().event_id.clone();
        event_ids.push(event.envelope().event_id.clone());
    }
    Seeded {
        id: sid,
        head: parent,
        event_ids,
    }
}

fn request(value: Value) -> ApiEvent {
    let mut state = BridgeState::default();
    let outbound = state.api_request_to_legacy(&value);
    assert_eq!(outbound.len(), 1, "expected one reply, got {outbound:?}");
    match outbound.into_iter().next().unwrap() {
        Outbound::Reply(frame) | Outbound::Serialized { frame, .. } => frame.event,
        other => panic!("expected API reply, got {other:?}"),
    }
}

fn error_of(event: ApiEvent) -> (ErrorCode, Option<horon_agent_harness_api::StoreErrorDetail>) {
    match event {
        ApiEvent::Error { code, details, .. } => (code, details),
        other => panic!("expected an error frame, got {other:?}"),
    }
}

fn heads_of(event: ApiEvent) -> horon_agent_harness_api::SessionHeads {
    match event {
        ApiEvent::SessionHeads {
            session_id,
            heads,
            selected_head,
        } => horon_agent_harness_api::SessionHeads {
            session_id,
            heads,
            selected_head,
        },
        other => panic!("expected session_heads, got {other:?}"),
    }
}

fn page_of(event: ApiEvent) -> horon_agent_harness_api::SessionEventPage {
    match event {
        ApiEvent::SessionEventPage {
            session_id,
            heads,
            events,
            next_cursor,
            blob_refs,
            record_refs,
        } => horon_agent_harness_api::SessionEventPage {
            session_id,
            heads,
            events,
            next_cursor,
            blob_refs,
            record_refs,
        },
        other => panic!("expected session_event_page, got {other:?}"),
    }
}

fn history_of(event: ApiEvent) -> horon_agent_harness_api::SessionHistoryPage {
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
        other => panic!("expected session_history_page, got {other:?}"),
    }
}

fn import_of(event: ApiEvent) -> horon_agent_harness_api::SessionImportResult {
    match event {
        ApiEvent::SessionImportResult {
            session_id,
            accepted_ids,
            heads,
            retry_cursor,
            conflict_heads,
        } => horon_agent_harness_api::SessionImportResult {
            session_id,
            accepted_ids,
            heads,
            retry_cursor,
            conflict_heads,
        },
        other => panic!("expected session_import_result, got {other:?}"),
    }
}

#[test]
fn replication_requests_are_answered_rather_than_unknown() {
    let home = ScopedHome::new("answered");
    let seeded = seed_session(&home.sessions(), "repl_answered", 2);
    let id = seeded.id.as_str();
    for value in [
        json!({"req":"list_session_heads","id":1,"session_id":id}),
        json!({"req":"select_session_head","id":2,"session_id":id,"head_event_id":seeded.head.as_str()}),
        json!({"req":"get_history_page","id":3,"session_id":id,"limit":1}),
        json!({"req":"export_session_events","id":4,"session_id":id}),
        json!({"req":"get_session_blob_status","id":5,"session_id":id,"sha256":"a".repeat(64),"total_size":1}),
        json!({"req":"get_session_record_blob_status","id":6,"session_id":id,"reference":{
            "position":0,"event_id":seeded.event_ids[0],"content_hash":"b".repeat(64),
            "sha256":"c".repeat(64),"size":1}}),
    ] {
        let event = request(value);
        if let ApiEvent::Error { code, ref message, .. } = event {
            assert_ne!(
                code,
                ErrorCode::UnknownRequest,
                "request must be implemented: {message}"
            );
        }
    }
}

#[test]
fn heads_and_history_page_follow_a_fixed_head_and_opaque_cursor() {
    let home = ScopedHome::new("heads-page");
    let seeded = seed_session(&home.sessions(), "repl_page", 5);
    let id = seeded.id.as_str();

    let heads = heads_of(request(json!({"req":"list_session_heads","id":1,"session_id":id})));
    assert_eq!(heads.session_id, id);
    assert_eq!(heads.heads, vec![seeded.head.as_str().to_string()]);
    assert_eq!(heads.selected_head.as_deref(), Some(seeded.head.as_str()));

    let first = history_of(request(json!({
        "req":"get_history_page","id":2,"session_id":id,"limit":2
    })));
    assert_eq!(first.head, seeded.head.as_str());
    assert_eq!(first.messages.len(), 2);
    assert_eq!(first.messages[0].content, "message 3");
    assert_eq!(first.messages[1].content, "message 4");
    let cursor = first.next_cursor.expect("older messages remain");
    assert_eq!(cursor, seeded.event_ids[3].as_str());

    let second = history_of(request(json!({
        "req":"get_history_page","id":3,"session_id":id,"head_event_id":seeded.head.as_str(),
        "before_event_id":cursor,"limit":10
    })));
    assert_eq!(second.messages.len(), 3);
    assert_eq!(second.messages[0].content, "message 0");
    assert_eq!(second.messages[2].content, "message 2");
    assert!(second.next_cursor.is_none());

    let (code, details) = error_of(request(json!({
        "req":"get_history_page","id":4,"session_id":id,
        "before_event_id":"00000000-0000-4000-8000-000000000000"
    })));
    assert_eq!(code, ErrorCode::InvalidRequest);
    assert_eq!(details.unwrap().store_code, "invalid_cursor");
}

#[test]
fn select_head_accepts_an_ancestor_and_reports_diverged_heads() {
    let home = ScopedHome::new("select");
    let seeded = seed_session(&home.sessions(), "repl_select", 3);
    let id = seeded.id.as_str();

    let selected = heads_of(request(json!({
        "req":"select_session_head","id":1,"session_id":id,
        "head_event_id":seeded.event_ids[0].as_str()
    })));
    assert_eq!(selected.selected_head.as_deref(), Some(seeded.event_ids[0].as_str()));

    let absent = request(json!({
        "req":"select_session_head","id":2,"session_id":id,
        "head_event_id":"00000000-0000-4000-8000-000000000000"
    }));
    let (code, _) = error_of(absent);
    assert_eq!(code, ErrorCode::InvalidRequest);

    let missing = request(json!({
        "req":"list_session_heads","id":3,"session_id":"does_not_exist"
    }));
    assert_eq!(error_of(missing).0, ErrorCode::UnknownSession);
}

#[test]
fn an_export_reply_carries_its_serialized_bytes_and_measures_the_raw_request() {
    let home = ScopedHome::new("serialized-once");
    let seeded = seed_session(&home.sessions(), "repl_serialized", 3);
    let id = seeded.id.as_str();
    let raw = serde_json::to_vec(&json!({
        "req":"export_session_events","id":7,"session_id":id,
        "protocol_version":REPLICATION_PROTOCOL_VERSION
    }))
    .unwrap();
    let request: Value = serde_json::from_slice(&raw).unwrap();
    let mut state = BridgeState {
        request_envelope_bytes: Some(raw.len()),
        ..BridgeState::default()
    };
    let outbound = state.api_request_to_legacy(&request);
    assert_eq!(outbound.len(), 1);
    let (frame, bytes) = match outbound.into_iter().next().unwrap() {
        Outbound::Serialized { frame, bytes } => (frame, bytes),
        other => panic!("an export reply must arrive already serialized: {other:?}"),
    };
    assert!(matches!(frame.event, ApiEvent::SessionEventPage { .. }));
    assert_eq!(bytes, crate::writer::frame_bytes(&frame).unwrap());
    assert!(bytes.ends_with(b"\n"), "a frame carries its own terminator");
    assert_eq!(frame.reply_to, Some(7));

    // A request whose raw envelope is over the cap is refused before the store
    // is touched, and the same request measured by serializing it agrees.
    let mut state = BridgeState {
        request_envelope_bytes: Some(MAX_REPLICATION_WIRE_BYTES + 1),
        ..BridgeState::default()
    };
    let oversized = state.api_request_to_legacy(&json!({
        "req":"export_session_events","id":8,"session_id":id,
        "protocol_version":REPLICATION_PROTOCOL_VERSION
    }));
    assert_eq!(oversized.len(), 1);
    let event = match oversized.into_iter().next().unwrap() {
        Outbound::Reply(frame) | Outbound::Serialized { frame, .. } => frame.event,
        other => panic!("expected a reply, got {other:?}"),
    };
    let (code, details) = error_of(event);
    assert_eq!(code, ErrorCode::InvalidRequest);
    assert_eq!(details.unwrap().store_code, "record_too_large");
}

#[test]
fn export_import_roundtrip_is_idempotent_and_preserves_canonical_bytes() {
    let source_home = ScopedHome::new("export-source");
    let seeded = seed_session(&source_home.sessions(), "repl_roundtrip", 4);
    let id = seeded.id.as_str();

    let page = page_of(request(json!({
        "req":"export_session_events","id":1,"session_id":id,
        "protocol_version":REPLICATION_PROTOCOL_VERSION
    })));
    assert_eq!(page.heads, vec![seeded.head.as_str().to_string()]);
    assert_eq!(page.events.len(), 5);
    assert!(page.next_cursor.is_none());
    let canonical: Vec<_> = page
        .events
        .iter()
        .map(|event| (event.event_id.clone(), event.canonical_json.clone()))
        .collect();

    let destination = scratch_dir("roundtrip-destination");
    let _scratch = ScratchGuard(destination.clone());
    let destination_paths = store::SessionPaths::new(destination.join("sessions"));
    let import_request = json!({
        "req":"import_session_events","id":2,"session_id":id,
        "protocol_version":REPLICATION_PROTOCOL_VERSION,
        "heads":page.heads,"events":page.events,
        "blob_refs":page.blob_refs,"record_refs":page.record_refs
    });
    let mut state = BridgeState::default();
    let previous = std::env::var_os("HORON_HOME");
    unsafe { std::env::set_var("HORON_HOME", &destination) };
    let outbound = state.api_request_to_legacy(&import_request);
    match previous {
        Some(value) => unsafe { std::env::set_var("HORON_HOME", value) },
        None => unsafe { std::env::remove_var("HORON_HOME") },
    }
    assert_eq!(outbound.len(), 1);
    let imported = match outbound.into_iter().next().unwrap() {
        Outbound::Reply(frame) | Outbound::Serialized { frame, .. } => import_of(frame.event),
        other => panic!("expected reply, got {other:?}"),
    };
    assert_eq!(imported.session_id, id);
    assert_eq!(imported.accepted_ids.len(), 5);
    assert_eq!(imported.heads, vec![seeded.head.as_str().to_string()]);

    let destination_page = store::replication::export_events(
        &destination_paths,
        &store::replication::ExportRequest {
            protocol_version: 1,
            session_id: seeded.id.clone(),
            heads: Vec::new(),
            known_ids: Vec::new(),
            cursor: None,
        },
    )
    .unwrap();
    let destination_canonical: Vec<_> = destination_page
        .events
        .iter()
        .map(|event| (event.event_id.as_str().to_owned(), event.canonical_json.clone()))
        .collect();
    assert_eq!(destination_canonical, canonical);
    assert_eq!(destination_page.events[0].canonical_json, page.events[0].canonical_json);

    let mut state = BridgeState::default();
    unsafe { std::env::set_var("HORON_HOME", &destination) };
    let outbound = state.api_request_to_legacy(&json!({
        "req":"import_session_events","id":3,"session_id":id,
        "protocol_version":REPLICATION_PROTOCOL_VERSION,
        "heads":page.heads,"events":page.events,
        "blob_refs":page.blob_refs,"record_refs":page.record_refs
    }));
    unsafe { std::env::remove_var("HORON_HOME") };
    let again = match outbound.into_iter().next().unwrap() {
        Outbound::Reply(frame) | Outbound::Serialized { frame, .. } => import_of(frame.event),
        other => panic!("expected reply, got {other:?}"),
    };
    assert_eq!(again.accepted_ids, imported.accepted_ids);
    let after = store::replication::export_events(
        &destination_paths,
        &store::replication::ExportRequest {
            protocol_version: 1,
            session_id: seeded.id.clone(),
            heads: Vec::new(),
            known_ids: Vec::new(),
            cursor: None,
        },
    )
    .unwrap();
    assert_eq!(after.events.len(), destination_page.events.len());
}

#[test]
fn import_conflict_and_invalid_cursor_carry_store_details() {
    let home = ScopedHome::new("conflict");
    let seeded = seed_session(&home.sessions(), "repl_conflict", 1);
    let id = seeded.id.as_str();

    let invalid = request(json!({
        "req":"export_session_events","id":1,"session_id":id,"cursor":"not-a-cursor"
    }));
    let (code, details) = error_of(invalid);
    assert_eq!(code, ErrorCode::InvalidRequest);
    assert_eq!(details.unwrap().store_code, "invalid_cursor");

    let page = page_of(request(json!({
        "req":"export_session_events","id":2,"session_id":id
    })));
    let mut forged = page.events[1].clone();
    let mut value: Value = serde_json::from_str(&forged.canonical_json).unwrap();
    value["payload"]["message"]["content"][0]["text"] = json!("forged text");
    forged.canonical_json = store::canonical_json(&value).unwrap();
    forged.content_hash = store::sha256_hex(b"different-hash");
    let conflict = request(json!({
        "req":"import_session_events","id":3,"session_id":id,
        "events":[page.events[0].clone(), forged]
    }));
    let (code, details) = error_of(conflict);
    assert_eq!(code, ErrorCode::InvalidRequest);
    assert_eq!(details.unwrap().store_code, "hash_conflict");
}

#[test]
fn payload_blob_roundtrip_and_status_resume() {
    let home = ScopedHome::new("blob");
    let paths = home.sessions();
    let seeded = seed_session(&paths, "repl_blob", 1);
    let id = seeded.id.as_str();
    let bytes = vec![b'{'; 4096];
    let sha = store::sha256_hex(&bytes);
    let blob_path = paths.blob_file(&seeded.id, &sha).unwrap();
    std::fs::create_dir_all(blob_path.parent().unwrap()).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(blob_path.parent().unwrap(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(&blob_path, &bytes).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&blob_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let chunk = match request(json!({
        "req":"export_session_blob","id":1,"session_id":id,"sha256":sha,"total_size":bytes.len(),"offset":0
    })) {
        ApiEvent::SessionBlobChunk {
            session_id,
            sha256,
            offset,
            data_base64,
            next_offset,
            total_size,
            complete,
        } => {
            assert_eq!(session_id, id);
            assert_eq!(sha256, sha);
            assert_eq!(offset, 0);
            assert_eq!(next_offset, bytes.len() as u64);
            assert_eq!(total_size, bytes.len() as u64);
            assert!(complete);
            horon_agent_harness_api::SessionBlobChunk {
                sha256,
                offset,
                data_base64,
                next_offset,
                total_size,
                complete,
            }
        }
        other => panic!("expected session_blob_chunk, got {other:?}"),
    };

    let destination = scratch_dir("blob-destination");
    let _scratch = ScratchGuard(destination.clone());
    let destination_paths = store::SessionPaths::new(destination.join("sessions"));
    seed_session(&destination_paths, "repl_blob", 1);
    let previous = std::env::var_os("HORON_HOME");
    unsafe { std::env::set_var("HORON_HOME", &destination) };
    let status = match request(json!({
        "req":"import_session_blob","id":2,"session_id":id,"chunk":chunk
    })) {
        ApiEvent::SessionBlobStatus {
            session_id,
            sha256,
            received,
            total_size,
            complete,
        } => {
            assert_eq!(session_id, id);
            assert_eq!(sha256, sha);
            assert_eq!(received, bytes.len() as u64);
            assert_eq!(total_size, bytes.len() as u64);
            assert!(complete);
            horon_agent_harness_api::SessionBlobStatus {
                session_id,
                sha256,
                received,
                total_size,
                complete,
            }
        }
        other => panic!("expected session_blob_status, got {other:?}"),
    };
    assert!(status.complete);
    let resumed = match request(json!({
        "req":"get_session_blob_status","id":3,"session_id":id,"sha256":status.sha256,"total_size":status.total_size
    })) {
        ApiEvent::SessionBlobStatus { received, complete, .. } => (received, complete),
        other => panic!("expected session_blob_status, got {other:?}"),
    };
    unsafe {
        match previous {
            Some(value) => std::env::set_var("HORON_HOME", value),
            None => std::env::remove_var("HORON_HOME"),
        }
    }
    assert_eq!(resumed, (bytes.len() as u64, true));
    assert_eq!(
        std::fs::read(destination_paths.blob_file(&seeded.id, &sha).unwrap()).unwrap(),
        bytes
    );
}

#[test]
fn oversized_replication_envelope_is_rejected_before_store() {
    let home = ScopedHome::new("oversize");
    let seeded = seed_session(&home.sessions(), "repl_oversize", 1);
    let id = seeded.id.as_str();
    let event = horon_agent_harness_api::CanonicalEventRecord {
        event_id: "11111111-1111-4111-8111-111111111111".into(),
        content_hash: "a".repeat(64),
        canonical_json: "x".repeat(MAX_REPLICATION_WIRE_BYTES + 1),
    };
    let (code, details) = error_of(request(json!({
        "req":"import_session_events","id":1,"session_id":id,"events":[event]
    })));
    assert_eq!(code, ErrorCode::InvalidRequest);
    assert_eq!(details.unwrap().store_code, "record_too_large");
}

#[test]
fn get_history_still_forwards_unchanged() {
    let mut state = BridgeState {
        session_id: Some("s1".into()),
        ..BridgeState::default()
    };
    let outbound = state.api_request_to_legacy(&json!({"req":"get_history","id":9,"session_id":"s1"}));
    assert_eq!(outbound.len(), 1);
    match &outbound[0] {
        Outbound::Legacy(value) => assert_eq!(value["type"], "get_history"),
        other => panic!("get_history must still reach the daemon: {other:?}"),
    }
}

#[test]
fn canonical_record_reference_roundtrips_whole_record_bytes() {
    let home = ScopedHome::new("record-ref");
    let paths = home.sessions();
    let seeded = seed_session(&paths, "repl_record", 1);
    let id = seeded.id.as_str();
    let graph = store::EventGraph::open(&paths, &seeded.id).unwrap();
    let record = graph.event(&seeded.event_ids[0]).unwrap().canonical_record().unwrap();
    let reference = CanonicalRecordRef {
        position: 0,
        event_id: record.event_id.as_str().to_owned(),
        content_hash: record.content_hash.clone(),
        sha256: store::sha256_hex(record.canonical_json.as_bytes()),
        size: record.canonical_json.len() as u64,
    };
    let chunk = match request(json!({
        "req":"export_session_record_blob","id":1,"session_id":id,
        "reference":reference,"offset":0
    })) {
        ApiEvent::SessionBlobChunk {
            sha256,
            data_base64,
            next_offset,
            total_size,
            complete,
            ..
        } => {
            assert_eq!(sha256, reference.sha256);
            assert_eq!(next_offset, reference.size);
            assert_eq!(total_size, reference.size);
            assert!(complete);
            horon_agent_harness_api::SessionBlobChunk {
                sha256,
                offset: 0,
                data_base64,
                next_offset,
                total_size,
                complete,
            }
        }
        other => panic!("expected session_blob_chunk, got {other:?}"),
    };

    let destination = scratch_dir("record-destination");
    let _scratch = ScratchGuard(destination.clone());
    let destination_paths = store::SessionPaths::new(destination.join("sessions"));
    seed_session(&destination_paths, "repl_record", 1);
    let previous = std::env::var_os("HORON_HOME");
    unsafe { std::env::set_var("HORON_HOME", &destination) };
    let status = match request(json!({
        "req":"import_session_record_blob","id":2,"session_id":id,
        "reference":reference,"chunk":chunk
    })) {
        ApiEvent::SessionRecordBlobStatus {
            received,
            total_size,
            complete,
            ..
        } => (received, total_size, complete),
        other => panic!("expected session_record_blob_status, got {other:?}"),
    };
    let resumed = match request(json!({
        "req":"get_session_record_blob_status","id":3,"session_id":id,"reference":reference
    })) {
        ApiEvent::SessionRecordBlobStatus {
            received,
            total_size,
            complete,
            ..
        } => (received, total_size, complete),
        other => panic!("expected session_record_blob_status, got {other:?}"),
    };
    unsafe {
        match previous {
            Some(value) => std::env::set_var("HORON_HOME", value),
            None => std::env::remove_var("HORON_HOME"),
        }
    }
    assert_eq!(status, (reference.size, reference.size, true));
    assert_eq!(resumed, status);
    let transfer = destination_paths
        .session_dir(&seeded.id)
        .unwrap()
        .join(".record-transfer")
        .join(seeded.id.as_str())
        .join("blobs")
        .join(format!("{}.json", reference.sha256));
    let published = std::fs::read(&transfer).unwrap();
    assert_eq!(published, record.canonical_json.as_bytes());
}

fn _unused(_: &Path) {}

fn scratch_dir(label: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("horon-api-replication-{label}-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(&path).expect("create scratch home");
    path
}

struct ScratchGuard(PathBuf);

impl Drop for ScratchGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
