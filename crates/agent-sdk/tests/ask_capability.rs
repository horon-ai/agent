//! The `ask` capability the client advertises in its hello frame.
//!
//! The daemon parks a turn on the person only for a connection that said it can
//! render the `ask` tool's questions, so the flag is the whole reason a client
//! without a question screen never sees one. This target exists on its own
//! because `client_behavior.rs` carries the correlation suite and is at its
//! test-size budget: the case needs a scripted server on a socket pair, which is
//! all this file has to build.

#![cfg(unix)]

use horon_agent_harness_api::{
    API_VERSION_MAJOR, ApiEvent, ApiRequest, ClientFrame, ServerFrame, read_frame, write_frame,
};
use horon_agent_sdk::{ConnectOptions, HoronAgentClient, Transport};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::channel;
use std::time::Duration;

/// A socket-pair transport, so the test drives the client over the same code
/// path a real connection uses.
struct PairTransport(UnixStream);

impl Transport for PairTransport {
    fn split(self: Box<Self>) -> horon_agent_sdk::Result<(Box<dyn BufRead + Send>, Box<dyn Write + Send>)> {
        let writer = self.0.try_clone().expect("socket pair must clone");
        Ok((Box::new(BufReader::new(self.0)), Box::new(writer)))
    }
}

/// The `ask` capability is opt-in and travels in the handshake.
///
/// The daemon parks a turn on the person only for a connection that said it can
/// render the `ask` tool's questions, so the flag is the whole reason a client
/// without a question screen never sees one. The Rust SDK sent a hardcoded
/// `false` while the TypeScript and Python clients already had the option; this
/// pins the field, its default and the frame together.
#[test]
fn the_ask_capability_is_sent_only_when_the_client_opts_in() {
    assert!(!ConnectOptions::default().supports_ask_requests);
    for enabled in [false, true] {
        let (seen_tx, seen_rx) = channel();
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        std::thread::spawn(move || {
            let mut reader = BufReader::new(theirs.try_clone().expect("clone"));
            let mut writer = theirs;
            let frame: ClientFrame = read_frame(&mut reader).expect("hello frame");
            let ApiRequest::Hello {
                supports_ask_requests, ..
            } = frame.request
            else {
                panic!("first frame must be Hello")
            };
            seen_tx.send(supports_ask_requests).expect("report hello");
            write_frame(
                &mut writer,
                &ServerFrame {
                    v: API_VERSION_MAJOR,
                    reply_to: Some(frame.id),
                    event: ApiEvent::HelloOk {
                        version: API_VERSION_MAJOR,
                        server: "ask-capability/1.0".to_string(),
                        capabilities: vec![],
                    },
                },
            )
            .expect("hello reply");
            // Hold the connection open so the client's reader does not see a
            // close and report a disconnect the test did not ask for.
            while read_frame::<_, ClientFrame>(&mut reader).is_ok() {}
        });
        let client = HoronAgentClient::connect_with(
            Box::new(PairTransport(ours)),
            ConnectOptions {
                supports_ask_requests: enabled,
                ensure_runtime: false,
                request_timeout: Some(Duration::from_secs(5)),
                ..Default::default()
            },
        )
        .expect("handshake must succeed");
        assert_eq!(client.server, "ask-capability/1.0");
        assert_eq!(
            seen_rx.recv_timeout(Duration::from_secs(5)).expect("hello frame"),
            enabled
        );
    }
}
