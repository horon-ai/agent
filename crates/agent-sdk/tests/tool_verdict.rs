//! The tool verdict on a turn result's tool calls (protocol minor 19).
//!
//! `tool_done` may carry what became of the call. The SDK passes that through
//! on `ToolCall::verdict` and never fills it in: a frame without the key gives
//! `None`, and so does a verdict name this build does not know, without ending
//! the connection. A client built before the field keeps reading the frame.

#![cfg(unix)]

use horon_agent_harness_api::{
    API_VERSION_MAJOR, ApiEvent, ApiRequest, ClientFrame, ServerFrame, read_frame, write_frame,
};
use horon_agent_sdk::{ConnectOptions, HoronAgentClient, ToolVerdict, Transport, TurnResult};
use serde::Deserialize;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

struct PairTransport(UnixStream);

impl Transport for PairTransport {
    fn split(self: Box<Self>) -> horon_agent_sdk::Result<(Box<dyn BufRead + Send>, Box<dyn Write + Send>)> {
        let writer = self.0.try_clone().expect("socket pair must clone");
        Ok((Box::new(BufReader::new(self.0)), Box::new(writer)))
    }
}

fn frame_line(event: &ApiEvent) -> String {
    serde_json::to_string(&ServerFrame {
        v: API_VERSION_MAJOR,
        reply_to: None,
        event: event.clone(),
    })
    .expect("frame must serialize")
}

fn send_line(writer: &mut dyn Write, line: &str) {
    writer.write_all(line.as_bytes()).expect("frame");
    writer.write_all(b"\n").expect("frame end");
    writer.flush().expect("flush");
}

/// Runs one turn against a harness that answers `send_message` with the given
/// `tool_done` lines, written to the socket exactly as given.
fn turn_with(tool_done_lines: Vec<String>) -> TurnResult {
    let (ours, theirs) = UnixStream::pair().expect("socket pair");
    std::thread::spawn(move || {
        let mut reader = BufReader::new(theirs.try_clone().expect("clone"));
        let mut writer = theirs;
        while let Ok(frame) = read_frame::<_, ClientFrame>(&mut reader) {
            match frame.request {
                ApiRequest::Hello { .. } => {
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
                }
                ApiRequest::SendMessage { session_id, .. } => {
                    let accepted = ApiEvent::MessageAccepted {
                        session_id: session_id.clone(),
                    };
                    send_line(&mut writer, &frame_line(&accepted));
                    for line in &tool_done_lines {
                        send_line(&mut writer, line);
                    }
                    let done = ApiEvent::TurnDone {
                        session_id,
                        pending_soft_interrupts: None,
                    };
                    send_line(&mut writer, &frame_line(&done));
                }
                _ => {}
            }
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
    client
        .run("s1", "hi", Default::default())
        .expect("the turn must complete")
}

fn failed_tool_done() -> ApiEvent {
    ApiEvent::ToolDone {
        session_id: "s1".into(),
        call_id: "c1".into(),
        name: "bash".into(),
        output: "exit status 2".into(),
        error: Some("exit status 2".into()),
        verdict: Some(ToolVerdict::Failed),
    }
}

#[test]
fn a_failed_verdict_reaches_the_tool_call() {
    let turn = turn_with(vec![frame_line(&failed_tool_done())]);

    assert_eq!(turn.tool_calls.len(), 1);
    assert_eq!(turn.tool_calls[0].verdict, Some(ToolVerdict::Failed));
    assert_eq!(
        turn.tool_calls[0].error.as_deref(),
        Some("exit status 2"),
        "the verdict is beside the error, not instead of it"
    );
}

#[test]
fn a_tool_done_without_the_key_has_no_verdict() {
    let line = format!(
        r#"{{"v":{API_VERSION_MAJOR},"ev":"tool_done","session_id":"s1","call_id":"c1","name":"bash","output":"ok"}}"#
    );
    let turn = turn_with(vec![line]);

    assert_eq!(turn.tool_calls.len(), 1);
    assert_eq!(turn.tool_calls[0].output, "ok");
    assert_eq!(
        turn.tool_calls[0].verdict, None,
        "an absent verdict is not a judgment and must not read as ok"
    );
}

#[test]
fn an_unfamiliar_verdict_name_reads_as_absent_and_the_turn_goes_on() {
    let newer = format!(
        r#"{{"v":{API_VERSION_MAJOR},"ev":"tool_done","session_id":"s1","call_id":"c1","name":"bash","output":"later","verdict":"deferred"}}"#
    );
    let turn = turn_with(vec![newer, frame_line(&failed_tool_done())]);

    let verdicts: Vec<Option<ToolVerdict>> = turn.tool_calls.iter().map(|call| call.verdict).collect();
    assert_eq!(
        verdicts,
        vec![None, Some(ToolVerdict::Failed)],
        "a name from a newer daemon must not end the reader: the call after it still arrives"
    );
    assert_eq!(turn.tool_calls[0].output, "later");
}

/// The `tool_done` shape of a client built before protocol 1.19, in the same
/// envelope the harness API uses.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(tag = "ev", rename_all = "snake_case")]
enum EventBeforeVerdict {
    ToolDone {
        session_id: String,
        call_id: String,
        name: String,
        output: String,
        #[serde(default)]
        error: Option<String>,
    },
}

#[derive(Debug, Deserialize, PartialEq)]
struct FrameBeforeVerdict {
    v: u32,
    #[serde(default)]
    reply_to: Option<u64>,
    #[serde(flatten)]
    event: EventBeforeVerdict,
}

#[test]
fn a_decoder_that_predates_the_verdict_still_reads_the_frame() {
    let line = frame_line(&failed_tool_done());
    assert!(
        line.contains(r#""verdict":"failed""#),
        "the frame under test must carry the verdict: {line}"
    );

    let older: FrameBeforeVerdict = serde_json::from_str(&line).expect("the older shape must accept the frame");
    assert_eq!(
        older,
        FrameBeforeVerdict {
            v: API_VERSION_MAJOR,
            reply_to: None,
            event: EventBeforeVerdict::ToolDone {
                session_id: "s1".into(),
                call_id: "c1".into(),
                name: "bash".into(),
                output: "exit status 2".into(),
                error: Some("exit status 2".into()),
            },
        }
    );
}
