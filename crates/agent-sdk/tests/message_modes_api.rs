//! `get_message_modes` over the Rust SDK (protocol minor 18).
//!
//! The request carries the session it asks about, and the reply's three strings
//! reach the caller unchanged. No capability gates the pair, so a server that
//! advertises nothing still answers it; what a server that predates the request
//! sends instead is a structured error, which the SDK must surface rather than
//! turn into a silent default.

#![cfg(unix)]

use horon_agent_harness_api::{
    API_VERSION_MAJOR, ApiEvent, ApiRequest, ClientFrame, ErrorCode, MessageModes, ServerFrame, read_frame, write_frame,
};
use horon_agent_sdk::{ConnectOptions, HoronAgentClient, Transport};
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

#[test]
fn the_modes_come_back_unchanged_and_the_request_names_the_session() {
    let (client, log) = harness(|frame, writer| match &frame.request {
        ApiRequest::GetMessageModes { session_id } => {
            assert_eq!(session_id, "s1", "the request must name the session it asks about");
            reply(
                frame,
                ApiEvent::MessageModes {
                    session_id: session_id.clone(),
                    modes: MessageModes {
                        follow_up_mode: "all".into(),
                        steering_mode: "one-at-a-time".into(),
                        interrupt_mode: "wait".into(),
                    },
                },
                writer,
            )
        }
        other => panic!("unexpected request: {other:?}"),
    });

    let modes = client.get_message_modes("s1").expect("the reply decodes");

    assert_eq!(modes.follow_up_mode, "all");
    assert_eq!(modes.steering_mode, "one-at-a-time");
    assert_eq!(modes.interrupt_mode, "wait");
    let sent = log.lock().expect("request log").clone();
    assert_eq!(sent.len(), 1, "one call is one request: {sent:?}");
    assert!(
        matches!(&sent[0], ApiRequest::GetMessageModes { session_id } if session_id == "s1"),
        "the call must not be rewritten into another request: {sent:?}"
    );
}

#[test]
fn the_call_needs_no_capability() {
    let (client, _log) = harness(|frame, writer| {
        reply(
            frame,
            ApiEvent::MessageModes {
                session_id: "s1".into(),
                modes: MessageModes {
                    follow_up_mode: "all".into(),
                    steering_mode: "all".into(),
                    interrupt_mode: "immediate".into(),
                },
            },
            writer,
        )
    });

    assert!(
        !client.supports("message_modes"),
        "the pair is additive, so it gates on no capability string"
    );
    assert_eq!(
        client
            .get_message_modes("s1")
            .expect("an unadvertised capability must not block")
            .interrupt_mode,
        "immediate"
    );
}

#[test]
fn an_older_server_surfaces_its_error_instead_of_inventing_defaults() {
    let (client, _log) = harness(|frame, writer| {
        reply(
            frame,
            ApiEvent::Error {
                code: ErrorCode::UnknownRequest,
                message: "unknown request: get_message_modes".into(),
                details: None,
            },
            writer,
        )
    });

    let error = client
        .get_message_modes("s1")
        .expect_err("a server that predates the request has no modes to report");

    assert_eq!(error.code(), "unknown_request");
}

#[test]
fn a_reply_of_another_kind_is_an_error_rather_than_a_default() {
    let (client, _log) = harness(|frame, writer| reply(frame, ApiEvent::Ok, writer));

    let error = client
        .get_message_modes("s1")
        .expect_err("an `ok` reply carries no modes");

    assert!(
        error.message.contains("message_modes"),
        "the error must name what was expected: {}",
        error.message
    );
}
