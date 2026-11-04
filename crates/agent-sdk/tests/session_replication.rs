//! Replication and cursor-history client behavior on a real socket pair.
//!
//! The canonical-bytes case is the point of this file: a client that parses
//! `canonical_json` into a number model and re-serializes it changes the
//! event's identity, and nothing else in the suite would notice.

#![cfg(unix)]

use horon_agent_harness_api::{
    API_VERSION_MAJOR, ApiEvent, ApiRequest, CanonicalEventRecord, CanonicalRecordRef, ClientFrame, ErrorCode,
    ServerFrame, SessionBlobChunk, StoreErrorDetail, read_frame, write_frame,
};
use horon_agent_sdk::{ConnectOptions, ErrorKind, HoronAgentClient, Transport};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

const CANONICAL: &str = r#"{"schema_version":1,"payload":{"big":18446744073709551615,"one":1.0,"negative_zero":-0.0}}"#;

struct PairTransport(UnixStream);

impl Transport for PairTransport {
    fn split(self: Box<Self>) -> horon_agent_sdk::Result<(Box<dyn BufRead + Send>, Box<dyn Write + Send>)> {
        let writer = self.0.try_clone().expect("socket pair must clone");
        Ok((Box::new(BufReader::new(self.0)), Box::new(writer)))
    }
}

fn fake_harness(
    capabilities: Vec<&'static str>,
    handle: impl Fn(&ClientFrame, &mut dyn Write) + Send + 'static,
) -> HoronAgentClient {
    let (ours, theirs) = UnixStream::pair().expect("socket pair");
    std::thread::spawn(move || {
        let mut reader = BufReader::new(theirs.try_clone().expect("clone"));
        let mut writer = theirs;
        while let Ok(frame) = read_frame::<_, ClientFrame>(&mut reader) {
            if let ApiRequest::Hello { .. } = frame.request {
                let reply = ServerFrame {
                    v: API_VERSION_MAJOR,
                    reply_to: Some(frame.id),
                    event: ApiEvent::HelloOk {
                        version: API_VERSION_MAJOR,
                        server: "fake-harness/1.0".to_string(),
                        capabilities: capabilities.iter().map(|cap| (*cap).to_string()).collect(),
                    },
                };
                write_frame(&mut writer, &reply).expect("hello reply");
                continue;
            }
            handle(&frame, &mut writer);
        }
    });
    HoronAgentClient::connect_with(
        Box::new(PairTransport(ours)),
        ConnectOptions {
            request_timeout: Some(Duration::from_secs(5)),
            ensure_runtime: false,
            ..Default::default()
        },
    )
    .expect("handshake must succeed")
}

fn reply(frame: &ClientFrame, event: ApiEvent, writer: &mut dyn Write) {
    let out = ServerFrame {
        v: API_VERSION_MAJOR,
        reply_to: Some(frame.id),
        event,
    };
    write_frame(&mut { writer }, &out).expect("reply");
}

const CAPABILITIES: [&str; 4] = [
    "sessions",
    "session_event_store_v1",
    "session_replication_v1",
    "session_history_cursor_v1",
];

#[test]
fn replication_methods_refuse_a_server_without_the_capability() {
    let client = fake_harness(vec!["sessions"], |frame, writer| {
        reply(
            frame,
            ApiEvent::Error {
                code: ErrorCode::UnknownRequest,
                message: "should not be reached".into(),
                details: None,
            },
            writer,
        );
    });
    let error = client.list_session_heads("s1").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert!(error.message.contains("session_event_store_v1"));
    let error = client
        .export_session_events("s1", Vec::new(), Vec::new(), None)
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert!(error.message.contains("session_replication_v1"));
    let error = client.get_history_page("s1", None, None, Some(1)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert!(error.message.contains("session_history_cursor_v1"));
}

#[test]
fn export_preserves_canonical_bytes_and_import_is_idempotent() {
    let client = fake_harness(CAPABILITIES.to_vec(), |frame, writer| match frame.request {
        ApiRequest::ExportSessionEvents { .. } => reply(
            frame,
            ApiEvent::SessionEventPage {
                session_id: "s1".into(),
                heads: vec!["h1".into()],
                events: vec![CanonicalEventRecord {
                    event_id: "11111111-1111-4111-8111-111111111111".into(),
                    content_hash: "c".repeat(64),
                    canonical_json: CANONICAL.into(),
                }],
                next_cursor: Some("cursor".into()),
                blob_refs: Vec::new(),
                record_refs: vec![CanonicalRecordRef {
                    position: 0,
                    event_id: "22222222-2222-4222-8222-222222222222".into(),
                    content_hash: "d".repeat(64),
                    sha256: "e".repeat(64),
                    size: 4096,
                }],
            },
            writer,
        ),
        ApiRequest::ImportSessionEvents { ref events, .. } => {
            assert_eq!(events[0].canonical_json, CANONICAL);
            reply(
                frame,
                ApiEvent::SessionImportResult {
                    session_id: "s1".into(),
                    accepted_ids: vec![events[0].event_id.clone()],
                    heads: vec!["h1".into()],
                    retry_cursor: None,
                    conflict_heads: Vec::new(),
                },
                writer,
            )
        }
        _ => reply(
            frame,
            ApiEvent::Error {
                code: ErrorCode::UnknownRequest,
                message: "unexpected request".into(),
                details: None,
            },
            writer,
        ),
    });

    let page = client
        .export_session_events("s1", Vec::new(), Vec::new(), None)
        .expect("export");
    assert_eq!(page.events[0].canonical_json, CANONICAL);
    assert!(page.events[0].canonical_json.contains("18446744073709551615"));
    assert!(page.events[0].canonical_json.contains("-0.0"));
    assert_eq!(page.record_refs[0].size, 4096);
    assert_eq!(page.next_cursor.as_deref(), Some("cursor"));

    let imported = client.import_session_events(&page, None).expect("import");
    assert_eq!(
        imported.accepted_ids,
        vec!["11111111-1111-4111-8111-111111111111".to_string()]
    );
    let again = client.import_session_events(&page, None).expect("retransmit");
    assert_eq!(again.accepted_ids, imported.accepted_ids);
}

#[test]
fn heads_selection_and_history_page_round_trip() {
    let client = fake_harness(CAPABILITIES.to_vec(), |frame, writer| match frame.request {
        ApiRequest::ListSessionHeads { .. } => reply(
            frame,
            ApiEvent::SessionHeads {
                session_id: "s1".into(),
                heads: vec!["h1".into(), "h2".into()],
                selected_head: Some("h1".into()),
            },
            writer,
        ),
        ApiRequest::SelectSessionHead { ref head_event_id, .. } => reply(
            frame,
            ApiEvent::SessionHeads {
                session_id: "s1".into(),
                heads: vec!["h1".into(), "h2".into()],
                selected_head: Some(head_event_id.clone()),
            },
            writer,
        ),
        ApiRequest::GetHistoryPage {
            ref head_event_id,
            ref before_event_id,
            limit,
            ..
        } => reply(
            frame,
            ApiEvent::SessionHistoryPage {
                session_id: "s1".into(),
                head: head_event_id.clone().unwrap_or_else(|| "h1".into()),
                messages: vec![horon_agent_harness_api::HistoryMessage {
                    response_stats: None,
                    role: "user".into(),
                    content: format!("limit={limit:?} before={before_event_id:?}"),
                }],
                next_cursor: Some("e0".into()),
            },
            writer,
        ),
        _ => reply(
            frame,
            ApiEvent::Error {
                code: ErrorCode::UnknownRequest,
                message: "unexpected request".into(),
                details: None,
            },
            writer,
        ),
    });

    let heads = client.list_session_heads("s1").expect("heads");
    assert_eq!(heads.heads, vec!["h1".to_string(), "h2".to_string()]);
    assert_eq!(heads.selected_head.as_deref(), Some("h1"));
    let selected = client.select_session_head("s1", "h2").expect("select");
    assert_eq!(selected.selected_head.as_deref(), Some("h2"));
    let page = client
        .get_history_page("s1", Some("h1"), Some("e5"), Some(2))
        .expect("page");
    assert_eq!(page.head, "h1");
    assert_eq!(page.messages[0].content, r#"limit=Some(2) before=Some("e5")"#);
    assert_eq!(page.next_cursor.as_deref(), Some("e0"));
}

#[test]
fn typed_store_details_reach_the_caller() {
    let client = fake_harness(CAPABILITIES.to_vec(), |frame, writer| {
        reply(
            frame,
            ApiEvent::Error {
                code: ErrorCode::Internal,
                message: "session writer is already owned".into(),
                details: Some(StoreErrorDetail {
                    store_code: "busy".into(),
                    retryable: true,
                    accepted_ids: vec!["a1".into()],
                    retry_cursor: Some("retry".into()),
                    conflict_heads: vec!["h1".into(), "h2".into()],
                }),
            },
            writer,
        );
    });
    let error = client
        .import_session_events(
            &horon_agent_harness_api::SessionEventPage {
                session_id: "s1".into(),
                heads: Vec::new(),
                events: Vec::new(),
                next_cursor: None,
                blob_refs: Vec::new(),
                record_refs: Vec::new(),
            },
            None,
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Harness(ErrorCode::Internal));
    let details = error.details.expect("typed detail");
    assert_eq!(details.store_code, "busy");
    assert!(details.retryable);
    assert_eq!(details.accepted_ids, vec!["a1".to_string()]);
    assert_eq!(details.retry_cursor.as_deref(), Some("retry"));
    assert_eq!(details.conflict_heads, vec!["h1".to_string(), "h2".to_string()]);
}

#[test]
fn blob_and_record_transfers_round_trip() {
    let chunk = SessionBlobChunk {
        sha256: "b".repeat(64),
        offset: 0,
        data_base64: "AAAA".into(),
        next_offset: 3,
        total_size: 3,
        complete: true,
    };
    let client = fake_harness(CAPABILITIES.to_vec(), {
        let chunk = chunk.clone();
        move |frame, writer| match frame.request {
            ApiRequest::ExportSessionBlob { .. } => reply(
                frame,
                ApiEvent::SessionBlobChunk {
                    session_id: "s1".into(),
                    sha256: chunk.sha256.clone(),
                    offset: chunk.offset,
                    data_base64: chunk.data_base64.clone(),
                    next_offset: chunk.next_offset,
                    total_size: chunk.total_size,
                    complete: chunk.complete,
                },
                writer,
            ),
            ApiRequest::ExportSessionRecordBlob { ref reference, .. } => reply(
                frame,
                ApiEvent::SessionBlobChunk {
                    session_id: "s1".into(),
                    sha256: reference.sha256.clone(),
                    offset: 0,
                    data_base64: "AAAA".into(),
                    next_offset: reference.size,
                    total_size: reference.size,
                    complete: true,
                },
                writer,
            ),
            ApiRequest::ImportSessionBlob { .. } | ApiRequest::GetSessionBlobStatus { .. } => reply(
                frame,
                ApiEvent::SessionBlobStatus {
                    session_id: "s1".into(),
                    sha256: chunk.sha256.clone(),
                    received: 3,
                    total_size: 3,
                    complete: true,
                },
                writer,
            ),
            ApiRequest::ImportSessionRecordBlob { ref reference, .. }
            | ApiRequest::GetSessionRecordBlobStatus { ref reference, .. } => reply(
                frame,
                ApiEvent::SessionRecordBlobStatus {
                    session_id: "s1".into(),
                    reference: reference.clone(),
                    received: 3,
                    total_size: 3,
                    complete: true,
                },
                writer,
            ),
            _ => reply(
                frame,
                ApiEvent::Error {
                    code: ErrorCode::UnknownRequest,
                    message: "unexpected request".into(),
                    details: None,
                },
                writer,
            ),
        }
    });

    let exported = client
        .export_session_blob("s1", &chunk.sha256, 3, 0)
        .expect("export blob");
    assert_eq!(exported, chunk);
    let status = client.import_session_blob("s1", &chunk).expect("import blob");
    assert!(status.complete);
    let resumed = client.session_blob_status("s1", &chunk.sha256, 3).expect("blob status");
    assert_eq!(resumed.received, 3);

    let reference = CanonicalRecordRef {
        position: 0,
        event_id: "22222222-2222-4222-8222-222222222222".into(),
        content_hash: "d".repeat(64),
        sha256: "e".repeat(64),
        size: 3,
    };
    let record_chunk = client
        .export_session_record_blob("s1", &reference, 0)
        .expect("export record");
    assert_eq!(record_chunk.sha256, reference.sha256);
    let record_status = client
        .import_session_record_blob("s1", &reference, &record_chunk)
        .expect("import record");
    assert_eq!(record_status.reference, reference);
    let record_resumed = client
        .session_record_blob_status("s1", &reference)
        .expect("record status");
    assert_eq!(record_resumed.received, 3);
}
