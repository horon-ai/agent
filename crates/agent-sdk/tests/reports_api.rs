//! The report and acknowledgement surface over the Rust SDK: `search_sessions`,
//! `list_artifacts`, `usage_report`, `ack_approval` and `ack_failover`.
//!
//! Each call is checked twice: the request the client wrote must be exactly the
//! one the wire contract names, including an absent `limit` and an absent
//! `session_id` staying absent rather than becoming a default, and the reply
//! must reach the caller as the typed payload. The two acks answer with a bare
//! `ok`, so a server that answers with the ack event instead is an error rather
//! than a success the caller cannot distinguish from a real acknowledgement.

#![cfg(unix)]

use horon_agent_harness_api::{
    API_VERSION_MAJOR, ApiEvent, ApiRequest, ArtifactEntry, ArtifactKind, ClientFrame, ErrorCode, ServerFrame,
    SessionSearchHit, UsageLimitInfo, UsageModel, UsageSession, UsageTotals, read_frame, write_frame,
};
use horon_agent_sdk::{ConnectOptions, ErrorKind, HoronAgentClient, Transport};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct PairTransport(UnixStream);

impl Transport for PairTransport {
    fn split(self: Box<Self>) -> horon_agent_sdk::Result<(Box<dyn BufRead + Send>, Box<dyn Write + Send>)> {
        let writer = self.0.try_clone().expect("socket pair must clone");
        Ok((Box::new(BufReader::new(self.0)), Box::new(writer)))
    }
}

fn harness(
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
                        server: "fake-harness/1.0".into(),
                        capabilities: Vec::new(),
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

fn reply(frame: &ClientFrame, event: ApiEvent, mut writer: &mut dyn Write) {
    write_frame(
        &mut writer,
        &ServerFrame {
            v: API_VERSION_MAJOR,
            reply_to: Some(frame.id),
            event,
        },
    )
    .expect("reply");
}

fn sent(log: &Arc<Mutex<Vec<ApiRequest>>>) -> Vec<ApiRequest> {
    log.lock().expect("request log").clone()
}

fn hit(session_id: &str, preview: &str) -> SessionSearchHit {
    SessionSearchHit {
        session_id: session_id.to_string(),
        title: None,
        role: "user".into(),
        preview: preview.to_string(),
        at: "2026-01-01T00:00:00Z".into(),
    }
}

fn artifact(path: &str) -> ArtifactEntry {
    ArtifactEntry {
        path: path.to_string(),
        kind: ArtifactKind::Html,
        bytes: 42,
        mtime_ms: 1_700_000_000_000,
    }
}

#[test]
fn search_sessions_sends_the_query_and_no_limit_by_default() {
    let (client, log) = harness(|frame, writer| match &frame.request {
        ApiRequest::SearchSessions { query, limit } => {
            assert_eq!(query, "fixture", "the query travels unchanged");
            assert_eq!(*limit, None, "an omitted limit must stay omitted");
            reply(
                frame,
                ApiEvent::SessionSearchResults {
                    query: query.clone(),
                    results: vec![hit("s1", "...fixture..."), hit("s2", "fixture again")],
                },
                writer,
            )
        }
        other => panic!("unexpected request: {other:?}"),
    });

    let hits = client.search_sessions("fixture", None).expect("the reply decodes");

    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].session_id, "s1");
    assert_eq!(hits[0].preview, "...fixture...");
    assert_eq!(hits[1].role, "user");
    assert_eq!(
        sent(&log),
        vec![ApiRequest::SearchSessions {
            query: "fixture".into(),
            limit: None,
        }]
    );
}

#[test]
fn search_sessions_forwards_an_explicit_limit() {
    let (client, log) = harness(|frame, writer| {
        reply(
            frame,
            ApiEvent::SessionSearchResults {
                query: "fixture".into(),
                results: Vec::new(),
            },
            writer,
        )
    });

    assert!(
        client
            .search_sessions("fixture", Some(50))
            .expect("empty is an answer")
            .is_empty()
    );
    assert_eq!(
        sent(&log),
        vec![ApiRequest::SearchSessions {
            query: "fixture".into(),
            limit: Some(50),
        }]
    );
}

#[test]
fn list_artifacts_returns_the_entries_and_names_the_session() {
    let (client, log) = harness(|frame, writer| match &frame.request {
        ApiRequest::ListArtifacts { session_id } => reply(
            frame,
            ApiEvent::Artifacts {
                session_id: session_id.clone(),
                artifacts: vec![artifact("report.html")],
            },
            writer,
        ),
        other => panic!("unexpected request: {other:?}"),
    });

    let artifacts = client.list_artifacts("s1").expect("the reply decodes");

    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].path, "report.html");
    assert_eq!(artifacts[0].kind, ArtifactKind::Html);
    assert_eq!(artifacts[0].bytes, 42);
    assert_eq!(
        sent(&log),
        vec![ApiRequest::ListArtifacts {
            session_id: "s1".into(),
        }]
    );
}

fn usage_event(session_id: Option<String>) -> ApiEvent {
    ApiEvent::UsageReport {
        session_id: session_id.clone(),
        sessions: vec![UsageSession {
            session_id: "s1".into(),
            model: Some("fixture-model".into()),
            input: Some(10),
            output: Some(4),
            cache_read_input: None,
            cache_creation_input: None,
            requests: 2,
        }],
        totals: UsageTotals {
            input: Some(10),
            output: Some(4),
            cache_read_input: None,
            cache_creation_input: None,
            requests: 2,
            sessions: 1,
        },
        models: vec![UsageModel {
            model: "fixture-model".into(),
            input: Some(10),
            output: Some(4),
            cache_read_input: None,
            cache_creation_input: None,
            requests: 2,
            sessions: 1,
        }],
        context_window: Some(200_000),
        limits: vec![UsageLimitInfo {
            name: "subscription_spend".into(),
            used: Some(1.5),
            limit: Some(20.0),
            unit: "usd".into(),
            resets_at: None,
        }],
        pricing: "unknown".into(),
        estimated_cost: None,
    }
}

#[test]
fn usage_report_without_a_session_sends_no_session_id() {
    let (client, log) = harness(|frame, writer| reply(frame, usage_event(None), writer));

    let report = client.usage_report(None).expect("the reply decodes");

    assert_eq!(report.session_id, None);
    assert_eq!(report.totals.requests, 2);
    assert_eq!(report.models.len(), 1);
    assert_eq!(report.context_window, Some(200_000));
    assert_eq!(report.limits[0].unit, "usd");
    assert_eq!(report.pricing, "unknown");
    assert_eq!(report.estimated_cost, None);
    assert_eq!(sent(&log), vec![ApiRequest::UsageReport { session_id: None }]);
}

#[test]
fn usage_report_with_a_session_names_it() {
    let (client, log) = harness(|frame, writer| reply(frame, usage_event(Some("s1".into())), writer));

    let report = client.usage_report(Some("s1")).expect("the reply decodes");

    assert_eq!(report.session_id.as_deref(), Some("s1"));
    assert_eq!(report.sessions[0].model.as_deref(), Some("fixture-model"));
    assert_eq!(
        sent(&log),
        vec![ApiRequest::UsageReport {
            session_id: Some("s1".into()),
        }]
    );
}

#[test]
fn ack_approval_takes_a_bare_ok() {
    let (client, log) = harness(|frame, writer| reply(frame, ApiEvent::Ok, writer));

    client.ack_approval("s1", "req-1").expect("a bare ok is success");
    assert_eq!(
        sent(&log),
        vec![ApiRequest::ApprovalAck {
            session_id: "s1".into(),
            request_id: "req-1".into(),
        }]
    );
}

#[test]
fn ack_failover_takes_a_bare_ok() {
    let (client, log) = harness(|frame, writer| reply(frame, ApiEvent::Ok, writer));

    client
        .ack_failover("s1", "chain-1", "a", "b", 3)
        .expect("a bare ok is success");
    assert_eq!(
        sent(&log),
        vec![ApiRequest::FailoverAck {
            session_id: "s1".into(),
            chain_id: "chain-1".into(),
            from: "a".into(),
            to: "b".into(),
            attempt: 3,
        }]
    );
}

#[test]
fn a_structured_error_keeps_its_code() {
    let (client, log) = harness(|frame, writer| match &frame.request {
        ApiRequest::SearchSessions { query, .. } => {
            assert_eq!(query, "", "the refusal comes from the server, not the client");
            reply(
                frame,
                ApiEvent::Error {
                    code: ErrorCode::InvalidRequest,
                    message: "query must not be empty".into(),
                    details: None,
                },
                writer,
            )
        }
        other => panic!("unexpected request: {other:?}"),
    });

    let error = client.search_sessions("", None).expect_err("a blank query is refused");

    assert_eq!(error.code(), "invalid_request");
    assert!(
        error.message.contains("query must not be empty"),
        "the server's message must survive: {}",
        error.message
    );
    assert_eq!(sent(&log).len(), 1);
}

#[test]
fn an_event_reply_is_an_error_not_a_silent_ack() {
    let (client, _log) = harness(|frame, writer| {
        reply(
            frame,
            ApiEvent::ApprovalAck {
                request_id: "req-1".into(),
            },
            writer,
        )
    });

    let error = client
        .ack_approval("s1", "req-1")
        .expect_err("an ack event is not the reply to the ack; only `ok` is");

    assert_eq!(
        error.kind,
        ErrorKind::UnexpectedReply,
        "an approval ack answered by an event must be a typed unexpected reply: {error:?}"
    );
}
