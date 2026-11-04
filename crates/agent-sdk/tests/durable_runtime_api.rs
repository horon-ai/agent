//! Durable-runtime surface of the Rust SDK (protocol minor 14).
//!
//! The invariant worth stating: a gated call against a server that did not
//! advertise the capability must fail before anything reaches the wire. A
//! legacy server would otherwise answer `unknown_request`, or act on a request
//! whose reply shape it cannot produce.

#![cfg(unix)]

use horon_agent_harness_api::{
    API_VERSION_MAJOR, ApiEvent, ApiRequest, AskSelection, ClientFrame, HistoryMessage, RuntimeControlOutcome,
    RuntimeControlReceipt, RuntimeJournalSummary, RuntimeQueueSummary, RuntimeRunSummary, RuntimeStateSummary,
    ServerFrame, SessionSnapshot, SnapshotMarkerInfo, read_frame, write_frame,
};
use horon_agent_sdk::{ConnectOptions, HoronAgentClient, Transport};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MODERN: &[&str] = &[
    "sessions",
    "durable_runtime_v1",
    "monitor_v1",
    "snapshot_marker_v1",
    "ask_receipts_v1",
];

const LEGACY: &[&str] = &["sessions", "streaming"];

struct PairTransport(UnixStream);

impl Transport for PairTransport {
    fn split(self: Box<Self>) -> horon_agent_sdk::Result<(Box<dyn BufRead + Send>, Box<dyn Write + Send>)> {
        let writer = self.0.try_clone().expect("socket pair must clone");
        Ok((Box::new(BufReader::new(self.0)), Box::new(writer)))
    }
}

fn harness(
    capabilities: &'static [&'static str],
    handle: impl Fn(&ClientFrame, &mut dyn Write) + Send + 'static,
) -> (HoronAgentClient, Arc<Mutex<Vec<ApiRequest>>>) {
    let log: Arc<Mutex<Vec<ApiRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&log);
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
            recorded.lock().expect("request log").push(frame.request.clone());
            handle(&frame, &mut writer);
        }
    });
    let client = HoronAgentClient::connect_with(
        Box::new(PairTransport(ours)),
        ConnectOptions {
            request_timeout: Some(Duration::from_secs(5)),
            ensure_runtime: false,
            ..Default::default()
        },
    )
    .expect("handshake must succeed");
    (client, log)
}

fn reply(frame: &ClientFrame, event: ApiEvent, writer: &mut dyn Write) {
    write_frame(
        &mut { writer },
        &ServerFrame {
            v: API_VERSION_MAJOR,
            reply_to: Some(frame.id),
            event,
        },
    )
    .expect("reply");
}

fn push(event: ApiEvent, writer: &mut dyn Write) {
    write_frame(
        &mut { writer },
        &ServerFrame {
            v: API_VERSION_MAJOR,
            reply_to: None,
            event,
        },
    )
    .expect("push");
}

fn summary() -> RuntimeStateSummary {
    RuntimeStateSummary {
        journal: RuntimeJournalSummary {
            exists: true,
            generation: 2,
            last_seq: 9,
            bytes: 512,
            records: 9,
            read_only: false,
        },
        runs: vec![RuntimeRunSummary {
            run_id: "run-1".into(),
            started_at: "2026-10-26T00:00:00Z".into(),
            input_id: Some("i1".into()),
        }],
        queue: RuntimeQueueSummary {
            live_inputs: 1,
            undetermined_inputs: 0,
            live_input_bytes: 12,
        },
        receipts: Vec::new(),
        monitors: Vec::new(),
        pending_wakes: 0,
        pending_controls: 1,
        last_stop: None,
    }
}

#[test]
fn the_new_methods_send_the_protocol_requests_and_read_typed_replies() {
    let state = summary();
    let reply_state = state.clone();
    let snapshot_state = state.clone();
    let (client, log) = harness(MODERN, move |frame, writer| match &frame.request {
        ApiRequest::GetRuntimeState { .. } => reply(
            frame,
            ApiEvent::RuntimeState {
                session_id: "s1".into(),
                state: reply_state.clone(),
            },
            writer,
        ),
        ApiRequest::ApplyControl {
            operation_id, action, ..
        } => reply(
            frame,
            ApiEvent::ControlReceipt {
                session_id: "s1".into(),
                receipt: RuntimeControlReceipt {
                    operation_id: operation_id.clone(),
                    action: action.clone(),
                    outcome: RuntimeControlOutcome::Terminal,
                    result: Some(serde_json::json!({"cancelled": "i1"})),
                    message: "applied".into(),
                },
            },
            writer,
        ),
        ApiRequest::GetControlReceipt { operation_id, .. } => reply(
            frame,
            ApiEvent::ControlReceipt {
                session_id: "s1".into(),
                receipt: RuntimeControlReceipt {
                    operation_id: operation_id.clone(),
                    action: "cancel_input".into(),
                    outcome: RuntimeControlOutcome::Terminal,
                    result: None,
                    message: "terminal".into(),
                },
            },
            writer,
        ),
        ApiRequest::GetSnapshot { tail, max_bytes, .. } => reply(
            frame,
            ApiEvent::Snapshot {
                session_id: "s1".into(),
                snapshot: SessionSnapshot {
                    generation: "gen-1".into(),
                    seq: 4,
                    marker_id: "marker-1".into(),
                    state: snapshot_state.clone(),
                    messages: vec![HistoryMessage {
                        response_stats: None,
                        role: "user".into(),
                        content: "hi".into(),
                    }],
                    next_history_cursor: None,
                    truncated: *tail == Some(1) || max_bytes.is_some(),
                },
            },
            writer,
        ),
        ApiRequest::AskResponse { answer_id, .. } => {
            assert_eq!(answer_id.as_deref(), Some("answer-1"), "the id must travel");
            reply(frame, ApiEvent::Ok, &mut *writer)
        }
        other => panic!("unexpected request: {other:?}"),
    });

    assert_eq!(client.get_runtime_state("s1").unwrap(), state);

    let applied = client
        .apply_control(
            "s1",
            "op-1",
            "cancel_input",
            Some(serde_json::json!({"input_id": "i1"})),
        )
        .unwrap();
    assert_eq!(applied.outcome, RuntimeControlOutcome::Terminal);
    assert_eq!(applied.operation_id, "op-1");

    let receipt = client.get_control_receipt("s1", "op-2").unwrap();
    assert_eq!(receipt.outcome, RuntimeControlOutcome::Terminal);
    assert_eq!(receipt.operation_id, "op-2");

    let snapshot = client.get_snapshot("s1", Some(20), None).unwrap();
    assert_eq!(snapshot.marker_id, "marker-1");
    assert_eq!(snapshot.messages.len(), 1);

    client
        .respond_to_ask_with_id(
            "s1",
            "ask-1",
            "answer-1",
            vec![AskSelection {
                question_id: "q1".into(),
                selected: vec!["Fast".into()],
            }],
        )
        .unwrap();

    let sent = log.lock().expect("request log").clone();
    assert_eq!(sent.len(), 5, "each call must send exactly one request: {sent:?}");
    assert!(matches!(
        &sent[1],
        ApiRequest::ApplyControl { operation_id, action, args: Some(_), .. }
            if operation_id == "op-1" && action == "cancel_input"
    ));
    assert!(matches!(
        &sent[3],
        ApiRequest::GetSnapshot {
            tail: Some(20),
            max_bytes: None,
            ..
        }
    ));
}

#[test]
fn the_legacy_answer_path_still_sends_no_answer_id() {
    let (client, log) = harness(MODERN, |frame, writer| reply(frame, ApiEvent::Ok, &mut *writer));
    client
        .respond_to_ask(
            "s1",
            "ask-1",
            vec![AskSelection {
                question_id: "q1".into(),
                selected: vec!["Fast".into()],
            }],
        )
        .unwrap();
    let sent = log.lock().expect("request log").clone();
    assert!(matches!(&sent[0], ApiRequest::AskResponse { answer_id: None, .. }));
}

#[test]
fn a_server_without_the_capability_is_refused_before_anything_is_sent() {
    let (client, log) = harness(LEGACY, |frame, writer| reply(frame, ApiEvent::Ok, &mut *writer));
    for capability in ["durable_runtime_v1", "snapshot_marker_v1", "ask_receipts_v1"] {
        assert!(!client.supports(capability), "{capability} was not advertised");
    }

    let error = client.get_runtime_state("s1").expect_err("must refuse");
    assert_eq!(error.code(), "unsupported");
    assert!(error.message.contains("durable_runtime_v1"), "{}", error.message);

    let error = client
        .apply_control("s1", "op-1", "cancel_input", None)
        .expect_err("must refuse");
    assert_eq!(error.code(), "unsupported");

    let error = client.get_control_receipt("s1", "op-1").expect_err("must refuse");
    assert_eq!(error.code(), "unsupported");

    let error = client.get_snapshot("s1", None, None).expect_err("must refuse");
    assert_eq!(error.code(), "unsupported");

    let error = client
        .respond_to_ask_with_id("s1", "ask-1", "answer-1", Vec::new())
        .expect_err("must refuse");
    assert_eq!(error.code(), "unsupported");

    assert!(
        log.lock().expect("request log").is_empty(),
        "a refused call must not reach the wire; a legacy server would answer unknown_request \
         or act on a request it does not understand"
    );
}

#[test]
fn a_gated_event_never_reaches_a_connection_that_did_not_negotiate_it() {
    let delivered = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&delivered);
    let (client, _log) = harness(LEGACY, move |frame, writer| {
        if let ApiRequest::Ping = frame.request {
            push(
                ApiEvent::SnapshotMarker {
                    session_id: "s1".into(),
                    marker: SnapshotMarkerInfo {
                        generation: "gen-1".into(),
                        seq: 1,
                        marker_id: "marker-1".into(),
                    },
                },
                &mut *writer,
            );
            push(
                ApiEvent::SessionStatus {
                    session_id: "s1".into(),
                    status: "idle".into(),
                    pending_soft_interrupts: None,
                },
                &mut *writer,
            );
            reply(frame, ApiEvent::Pong, &mut *writer);
        }
    });

    let stream = client.events(Some("s1"));
    client.ping().expect("ping");
    let event = stream
        .next_timeout(Duration::from_secs(5))
        .expect("an ungated event must arrive");
    assert!(
        matches!(event, ApiEvent::SessionStatus { .. }),
        "the gated marker must be dropped, not delivered: {event:?}"
    );
    let _ = counter.fetch_add(1, Ordering::Relaxed);
    assert_eq!(delivered.load(Ordering::Relaxed), 1);
}

#[test]
fn a_gated_event_reaches_a_connection_that_negotiated_it() {
    let (client, _log) = harness(MODERN, |frame, writer| {
        if let ApiRequest::Ping = frame.request {
            push(
                ApiEvent::SnapshotMarker {
                    session_id: "s1".into(),
                    marker: SnapshotMarkerInfo {
                        generation: "gen-1".into(),
                        seq: 1,
                        marker_id: "marker-1".into(),
                    },
                },
                &mut *writer,
            );
            reply(frame, ApiEvent::Pong, &mut *writer);
        }
    });

    let stream = client.events(Some("s1"));
    client.ping().expect("ping");
    let event = stream
        .next_timeout(Duration::from_secs(5))
        .expect("the marker must arrive");
    match event {
        ApiEvent::SnapshotMarker { marker, .. } => {
            assert_eq!(marker.marker_id, "marker-1");
            assert_eq!(marker.seq, 1);
        }
        other => panic!("expected the marker, got {other:?}"),
    }
}

/// The closest harness to the production path: a path-bound listener, dialed by
/// the real client, with the same handshake and framing a bridge uses.
fn listen(
    capabilities: &'static [&'static str],
    handle: impl Fn(&ClientFrame, &mut dyn Write) + Send + Sync + 'static,
) -> (std::path::PathBuf, Arc<Mutex<Vec<ApiRequest>>>) {
    let dir = std::path::PathBuf::from("/tmp").join(format!(
        "horon-qa-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let socket = dir.join("api.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let log: Arc<Mutex<Vec<ApiRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&log);
    let handle = Arc::new(handle);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let recorded = Arc::clone(&recorded);
            let handle = Arc::clone(&handle);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut writer = stream;
                while let Ok(frame) = read_frame::<_, ClientFrame>(&mut reader) {
                    if let ApiRequest::Hello { .. } = frame.request {
                        let reply = ServerFrame {
                            v: API_VERSION_MAJOR,
                            reply_to: Some(frame.id),
                            event: ApiEvent::HelloOk {
                                version: API_VERSION_MAJOR,
                                server: "durable-qa/1.0".to_string(),
                                capabilities: capabilities.iter().map(|cap| (*cap).to_string()).collect(),
                            },
                        };
                        write_frame(&mut &writer, &reply).expect("hello reply");
                        continue;
                    }
                    recorded.lock().expect("request log").push(frame.request.clone());
                    handle(&frame, &mut writer);
                }
            });
        }
    });
    (socket, log)
}

#[test]
fn a_modern_client_on_a_real_socket_gets_the_advertised_set_and_a_state_reply() {
    let state = summary();
    let reply_state = state.clone();
    let (socket, log) = listen(MODERN, move |frame, writer| match &frame.request {
        ApiRequest::GetRuntimeState { .. } => reply(
            frame,
            ApiEvent::RuntimeState {
                session_id: "s1".into(),
                state: reply_state.clone(),
            },
            writer,
        ),
        other => panic!("unexpected request: {other:?}"),
    });

    let client = HoronAgentClient::connect(ConnectOptions {
        socket_path: Some(socket.clone()),
        request_timeout: Some(Duration::from_secs(10)),
        ensure_runtime: false,
        ..Default::default()
    })
    .expect("dial the listener");

    for capability in MODERN {
        assert!(
            client.supports(capability),
            "the advertised set must include {capability}"
        );
    }
    let reported = client
        .get_runtime_state("s1")
        .expect("runtime state over a real socket");
    assert_eq!(reported, state);
    assert_eq!(log.lock().expect("request log").len(), 1);

    drop(client);
    let _ = std::fs::remove_dir_all(socket.parent().expect("parent"));
}

#[test]
fn a_legacy_client_on_a_real_socket_gets_no_new_event_and_an_unsupported_error() {
    let (socket, log) = listen(LEGACY, |frame, writer| match &frame.request {
        ApiRequest::Ping => {
            push(
                ApiEvent::SnapshotMarker {
                    session_id: "s1".into(),
                    marker: SnapshotMarkerInfo {
                        generation: "gen-1".into(),
                        seq: 1,
                        marker_id: "marker-1".into(),
                    },
                },
                writer,
            );
            push(
                ApiEvent::SessionStatus {
                    session_id: "s1".into(),
                    status: "idle".into(),
                    pending_soft_interrupts: None,
                },
                writer,
            );
            reply(frame, ApiEvent::Pong, writer);
        }
        ApiRequest::GetRuntimeState { .. } => reply(
            frame,
            ApiEvent::Error {
                code: horon_agent_harness_api::ErrorCode::Unsupported,
                message: "get_runtime_state requires the `durable_runtime_v1` capability".into(),
                details: None,
            },
            writer,
        ),
        other => panic!("unexpected request: {other:?}"),
    });

    let client = HoronAgentClient::connect(ConnectOptions {
        socket_path: Some(socket.clone()),
        request_timeout: Some(Duration::from_secs(10)),
        ensure_runtime: false,
        ..Default::default()
    })
    .expect("dial the listener");

    for capability in ["durable_runtime_v1", "snapshot_marker_v1", "ask_receipts_v1"] {
        assert!(!client.supports(capability), "{capability} must not be claimed");
    }

    let stream = client.events(Some("s1"));
    client.ping().expect("ping");
    let event = stream
        .next_timeout(Duration::from_secs(5))
        .expect("the ungated event must arrive");
    assert!(
        matches!(event, ApiEvent::SessionStatus { .. }),
        "no new event may reach a connection that did not negotiate it: {event:?}"
    );

    let error = client.get_runtime_state("s1").expect_err("a legacy server must refuse");
    assert_eq!(error.code(), "unsupported");
    let sent = log.lock().expect("request log").clone();
    assert!(
        !sent
            .iter()
            .any(|request| matches!(request, ApiRequest::GetRuntimeState { .. })),
        "the SDK must refuse before writing the request: {sent:?}"
    );

    let raw = HoronAgentClient::connect(ConnectOptions {
        socket_path: Some(socket.clone()),
        request_timeout: Some(Duration::from_secs(10)),
        ensure_runtime: false,
        ..Default::default()
    })
    .expect("dial the listener");
    let error = raw
        .request(ApiRequest::GetRuntimeState {
            session_id: "s1".into(),
        })
        .expect("a raw request is written");
    match error.event {
        ApiEvent::Error { code, message, .. } => {
            assert_eq!(code, horon_agent_harness_api::ErrorCode::Unsupported);
            assert!(message.contains("durable_runtime_v1"), "{message}");
        }
        other => panic!("a legacy server must answer unsupported, got {other:?}"),
    }

    drop(client);
    drop(raw);
    let _ = std::fs::remove_dir_all(socket.parent().expect("parent"));
}
