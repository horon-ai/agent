//! The three failures a caller must tell apart without reading prose.
//!
//! A daemon that is not running, a daemon built for a different protocol
//! version, and a session id the daemon does not have all used to arrive as an
//! `Error` whose sentence was the only signal. These tests pin the typed
//! variants and the identifying fields that ride on them, so a change that
//! folds them back into one string fails here.

#![cfg(unix)]

use horon_agent_harness_api::{
    API_VERSION_MAJOR, ApiEvent, ApiRequest, ClientFrame, ServerFrame, StoreErrorDetail, read_frame, write_frame,
};
use horon_agent_sdk::{ConnectOptions, ErrorKind, HoronAgentClient, Transport};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// A socket-pair transport, so the client runs the same code path a real
/// connection runs.
struct PairTransport(UnixStream);

impl Transport for PairTransport {
    fn split(self: Box<Self>) -> horon_agent_sdk::Result<(Box<dyn BufRead + Send>, Box<dyn Write + Send>)> {
        let writer = self.0.try_clone().expect("socket pair must clone");
        Ok((Box::new(BufReader::new(self.0)), Box::new(writer)))
    }
}

/// A fake harness whose handshake reply is scripted per test.
///
/// The hello reply is where a version mismatch is decided, so it cannot be the
/// fixed `HelloOk` a generic scaffold would use.
fn fake_harness(
    hello: impl FnOnce() -> ApiEvent + Send + 'static,
    handle: impl Fn(&ClientFrame, &mut dyn Write) + Send + 'static,
) -> horon_agent_sdk::Result<HoronAgentClient> {
    let (ours, theirs) = UnixStream::pair().expect("socket pair");
    std::thread::spawn(move || {
        let mut reader = BufReader::new(theirs.try_clone().expect("clone"));
        let mut writer = theirs;
        let mut hello = Some(hello);
        while let Ok(frame) = read_frame::<_, ClientFrame>(&mut reader) {
            if let ApiRequest::Hello { .. } = frame.request {
                let event = hello.take().map_or_else(
                    || ApiEvent::HelloOk {
                        version: API_VERSION_MAJOR,
                        server: "fake-harness/1.0".to_string(),
                        capabilities: vec!["sessions".to_string()],
                    },
                    |script| script(),
                );
                let reply = ServerFrame {
                    v: API_VERSION_MAJOR,
                    reply_to: Some(frame.id),
                    event,
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
}

fn reply(frame: &ClientFrame, event: ApiEvent, writer: &mut dyn Write) {
    let out = ServerFrame {
        v: API_VERSION_MAJOR,
        reply_to: Some(frame.id),
        event,
    };
    write_frame(&mut { writer }, &out).expect("reply");
}

/// A dial that finds no socket is a connection failure that names the path and
/// keeps the OS cause, so a caller can print one and log the other.
#[test]
fn a_missing_socket_is_a_connection_failure_with_its_path_and_cause() {
    let socket = std::env::temp_dir().join("horon-sdk-typed-errors-missing.sock");
    let error = HoronAgentClient::connect(ConnectOptions {
        socket_path: Some(socket.clone()),
        ensure_runtime: false,
        request_timeout: Some(Duration::from_millis(200)),
        ..Default::default()
    })
    .err()
    .expect("a missing socket must not connect");
    assert_eq!(error.code(), "connect_failed");
    match &error.kind {
        ErrorKind::ConnectFailed { path, cause } => {
            assert_eq!(
                path.as_deref(),
                Some(socket.as_path()),
                "the failed dial names its socket"
            );
            assert!(!cause.is_empty(), "the OS cause is kept");
        }
        other => panic!("expected a typed connection failure, got {other:?}"),
    }
    assert_eq!(error.failed_path(), Some(socket.as_path()));
    assert!(!error.is_version_mismatch());
    assert!(!error.is_unknown_session());
}

/// A refused handshake is a version mismatch that carries both majors: the one
/// this client asked for and the one the server reported.
#[test]
fn a_refused_handshake_is_a_version_mismatch_with_both_majors() {
    let unsupported = API_VERSION_MAJOR + 1;
    let error = fake_harness(
        move || ApiEvent::Error {
            code: horon_agent_harness_api::ErrorCode::UnsupportedVersion,
            message: format!(
                "bridge speaks API v{unsupported}; this client asked for v{API_VERSION_MAJOR}..=v{API_VERSION_MAJOR}"
            ),
            details: None,
        },
        |_, _| {},
    )
    .err()
    .expect("a refused handshake must fail");
    assert_eq!(error.code(), "unsupported_version");
    assert!(error.is_version_mismatch());
    match &error.kind {
        ErrorKind::UnsupportedVersion {
            client_major,
            server_major,
        } => {
            assert_eq!(*client_major, API_VERSION_MAJOR);
            assert_eq!(
                *server_major,
                Some(unsupported),
                "the server's major is read from the refusal"
            );
        }
        other => panic!("expected a typed version mismatch, got {other:?}"),
    }
}

/// A handshake the server does not answer with `hello_ok` or an error frame is
/// a distinct failure: nothing about the versions was ever agreed.
#[test]
fn an_unexpected_handshake_reply_is_not_reported_as_a_version_mismatch() {
    let error = fake_harness(|| ApiEvent::Pong, |_, _| {})
        .err()
        .expect("an unexpected handshake reply must fail");
    assert_eq!(error.code(), "handshake_failed");
    assert!(!error.is_version_mismatch(), "no version was refused; none was agreed");
}

/// An error frame for a session the server does not have becomes the typed
/// variant and echoes the id the client sent, so a typo is visible without
/// re-reading the request.
#[test]
fn an_unknown_session_carries_the_id_the_client_sent() {
    let client = fake_harness(
        || ApiEvent::HelloOk {
            version: API_VERSION_MAJOR,
            server: "fake-harness/1.0".to_string(),
            capabilities: vec!["sessions".to_string()],
        },
        |frame, writer| {
            reply(
                frame,
                ApiEvent::Error {
                    code: horon_agent_harness_api::ErrorCode::UnknownSession,
                    message: "no such session".to_string(),
                    details: None,
                },
                writer,
            );
        },
    )
    .expect("handshake must succeed");
    let error = client.attach_session("gone-session-42").expect_err("attach must fail");
    assert_eq!(error.code(), "unknown_session");
    assert!(error.is_unknown_session());
    assert!(!error.is_version_mismatch());
    assert_eq!(
        error.kind,
        ErrorKind::UnknownSession {
            session_id: "gone-session-42".to_string()
        }
    );
    assert!(error.message.contains("no such session"));
}

/// Any other error frame keeps its wire code and message instead of collapsing
/// into one catch-all, so the typed variants did not cost information.
#[test]
fn other_error_frames_keep_their_wire_code_and_details() {
    let client = fake_harness(
        || ApiEvent::HelloOk {
            version: API_VERSION_MAJOR,
            server: "fake-harness/1.0".to_string(),
            capabilities: vec!["sessions".to_string()],
        },
        |frame, writer| {
            reply(
                frame,
                ApiEvent::Error {
                    code: horon_agent_harness_api::ErrorCode::InvalidRequest,
                    message: "provider credential is missing".to_string(),
                    details: Some(StoreErrorDetail {
                        store_code: "missing_credential".to_string(),
                        retryable: false,
                        accepted_ids: Vec::new(),
                        retry_cursor: None,
                        conflict_heads: Vec::new(),
                    }),
                },
                writer,
            );
        },
    )
    .expect("handshake must succeed");
    let error = client
        .set_model("s1", "unavailable")
        .expect_err("the refusal must surface");
    assert_eq!(error.code(), "invalid_request");
    assert_eq!(
        error.kind,
        ErrorKind::Harness(horon_agent_harness_api::ErrorCode::InvalidRequest)
    );
    assert_eq!(
        error.details.expect("typed detail").store_code,
        "missing_credential",
        "the server's finer cause survives the typed change"
    );
    assert!(error.to_string().starts_with("invalid_request: "), "{error}");
}

/// The session id riding on an error comes from the request, so the accessor
/// has to name the right one for both a required and an optional id.
#[test]
fn requests_expose_the_session_id_they_act_on() {
    assert_eq!(
        ApiRequest::AttachSession {
            session_id: "s1".to_string()
        }
        .session_id(),
        Some("s1")
    );
    assert_eq!(
        ApiRequest::ListSessions {
            include_archived: false,
            limit: None
        }
        .session_id(),
        None
    );
    assert_eq!(ApiRequest::UsageReport { session_id: None }.session_id(), None);
}
