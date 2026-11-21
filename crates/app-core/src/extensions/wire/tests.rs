//! Contracts of the bounded extension wire.
//!
//! Each test names one observable failure mode from the issue: a refused
//! handshake, a frame past the cap, an unterminated or non-UTF-8 frame, an
//! unregistered or duplicate answer id, a deadline, a cancellation and a
//! shutdown. Most drive a real byte stream against a peer task that reads and
//! writes frames exactly as a child would; the last one runs the same framing
//! over a real child process's stdin and stdout.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use horon_agent_extension_types::wire::{
    Capability, CapabilitySet, CommandInvokeParams, FrameError, HOST_PROTOCOL_RANGE, HandshakeError, HostExpectation,
    InitializeParams, InitializeResult, Negotiated, ObserverEvent, ObserverParams, PackageKind, ProtocolRange,
    ProtocolVersion, RpcError, ToolCallParams, WireFrame, WireMethod, WireNotification, WireRequest, WireResponse,
    parse_frame,
};
use serde_json::Value;
use tokio::io::DuplexStream;
use tokio::task::JoinHandle;

use super::*;

/// A declaration digest of the right shape. The wire only compares it.
const DIGEST: &str = "3f2c1a9d0b7e4c58a1d2e3f4051728394a5b6c7d8e9f00112233445566778899";

fn limits() -> WireLimits {
    WireLimits {
        max_frame_bytes: 4096,
        max_pending_requests: 4,
        max_queued_frames: 8,
        reply_deadline: Duration::from_millis(300),
        read_deadline: Duration::from_millis(300),
        write_deadline: Duration::from_millis(300),
    }
}

fn capabilities(selected: &[Capability]) -> CapabilitySet {
    CapabilitySet::new(selected.iter().copied())
}

fn every_capability() -> CapabilitySet {
    CapabilitySet::new(Capability::ALL)
}

/// The installation record every handshake is compared against. The kind is
/// part of the record because both package kinds travel the same handshake.
struct Record {
    kind: PackageKind,
    capabilities: CapabilitySet,
}

impl Record {
    fn extension(declared: &[Capability]) -> Self {
        Self {
            kind: PackageKind::Extension,
            capabilities: capabilities(declared),
        }
    }

    fn plugin(declared: &[Capability]) -> Self {
        Self {
            kind: PackageKind::Plugin,
            capabilities: capabilities(declared),
        }
    }

    fn expectation(&self) -> HostExpectation<'_> {
        HostExpectation {
            // The range this host build implements, from the shared contract.
            protocol: HOST_PROTOCOL_RANGE,
            capabilities: &self.capabilities,
            declared: &self.capabilities,
            kind: self.kind,
            package_id: "sample",
            declaration_digest: DIGEST,
        }
    }
}

fn initialize_params(offered: &[Capability]) -> InitializeParams {
    initialize_params_for(PackageKind::Extension, offered)
}

fn initialize_params_for(kind: PackageKind, offered: &[Capability]) -> InitializeParams {
    InitializeParams {
        protocol: HOST_PROTOCOL_RANGE,
        capabilities: capabilities(offered),
        package_id: "sample".to_string(),
        kind,
    }
}

fn initialize_result(offered: &[Capability], protocol: ProtocolRange, digest: &str) -> InitializeResult {
    InitializeResult {
        protocol,
        capabilities: capabilities(offered),
        declaration_digest: digest.to_string(),
    }
}

fn accepted(offered: &[Capability]) -> InitializeResult {
    initialize_result(offered, ProtocolRange::exactly(ProtocolVersion::new(1, 0)), DIGEST)
}

/// What a peer does with one request.
#[derive(Debug)]
enum PeerAction {
    /// Write this frame back.
    Reply(WireFrame),
    /// Answer nothing and keep reading, which is how a child stalls a request.
    Silent,
    /// Stop reading; the stream ends.
    Stop,
}

/// Answer `initialize` with `body` and stay silent otherwise, so a follow-up
/// request waits until its deadline or cancellation.
fn answering(body: InitializeResult) -> impl FnMut(&WireRequest) -> PeerAction {
    move |request| match request.method {
        WireMethod::Initialize => PeerAction::Reply(WireFrame::Response(WireResponse::reply(
            request.id,
            serde_json::to_value(&body).expect("initialize result serializes"),
        ))),
        _ => PeerAction::Silent,
    }
}

/// Drive one peer over `stream`, writing each answer under the same deadline
/// the host uses.
fn spawn_peer(
    stream: DuplexStream,
    limits: WireLimits,
    mut answer: impl FnMut(&WireRequest) -> PeerAction,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(stream);
        let mut reader = FrameReader::new(read, limits);
        while let Ok(Some(frame)) = reader.next().await {
            let request = match frame {
                WireFrame::Request(request) => request,
                _ => break,
            };
            match answer(&request) {
                PeerAction::Reply(frame) => {
                    if write_frame(&mut write, &frame, limits.write_deadline).await.is_err() {
                        break;
                    }
                }
                PeerAction::Silent => {}
                PeerAction::Stop => break,
            }
        }
    })
}

fn connected(limits: WireLimits) -> (Arc<ExtensionWire>, DuplexStream) {
    let (host, peer) = tokio::io::duplex(64 * 1024);
    (Arc::new(ExtensionWire::new(host, limits)), peer)
}

async fn handshake(wire: &ExtensionWire, record: &Record, offered: &[Capability]) -> Result<Negotiated, WireError> {
    wire.initialize(initialize_params(offered), &record.expectation()).await
}

/// Mark one connection ready without a peer, for tests that only exercise the
/// correlation rules.
fn ready(wire: &ExtensionWire, declared: &CapabilitySet) {
    *wire.lock_lifecycle() = Lifecycle::Ready(Negotiated {
        protocol: ProtocolVersion::new(1, 0),
        capabilities: declared.clone(),
    });
}

/// A peer that records every request id it sees and answers the tool and
/// command methods.
fn echoing_peer(
    stream: DuplexStream,
    limits: WireLimits,
    seen: Arc<Mutex<Vec<u64>>>,
    declared: CapabilitySet,
) -> JoinHandle<()> {
    spawn_peer(stream, limits, move |request| {
        let mut seen = seen.lock().expect("request log");
        seen.push(request.id);
        drop(seen);
        let reply = match request.method {
            WireMethod::Initialize => {
                serde_json::to_value(accepted(&declared.iter().collect::<Vec<_>>())).expect("serializes")
            }
            WireMethod::ToolCall => {
                let params = request.parse_params::<ToolCallParams>().expect("tool call parameters");
                serde_json::json!({"tool": params.tool, "arguments": params.arguments})
            }
            WireMethod::CommandInvoke => {
                let params = request
                    .parse_params::<CommandInvokeParams>()
                    .expect("command invoke parameters");
                serde_json::json!({"command": params.command})
            }
            // A child exiting on `shutdown` without answering is allowed.
            WireMethod::Shutdown => return PeerAction::Stop,
            WireMethod::Cancel | WireMethod::ObserverNotify => {
                return PeerAction::Reply(WireFrame::Response(WireResponse::fail(
                    request.id,
                    RpcError::method_not_found("not answered here"),
                )));
            }
        };
        PeerAction::Reply(WireFrame::Response(WireResponse::reply(request.id, reply)))
    })
}

#[tokio::test]
async fn handshake_accepts_matching_versions_and_capabilities() {
    let limits = limits();
    let (wire, peer) = connected(limits);
    let record = Record::extension(&[Capability::ToolCall, Capability::CommandInvoke]);
    spawn_peer(
        peer,
        limits,
        answering(initialize_result(
            &[Capability::ToolCall],
            ProtocolRange::new(ProtocolVersion::new(0, 9), ProtocolVersion::new(1, 3)).expect("ordered range"),
            DIGEST,
        )),
    );
    let negotiated = handshake(&wire, &record, &[Capability::ToolCall, Capability::CommandInvoke])
        .await
        .expect("a matching handshake settles");
    assert_eq!(negotiated.protocol, ProtocolVersion::new(1, 0));
    assert_eq!(negotiated.capabilities, capabilities(&[Capability::ToolCall]));
    assert_eq!(wire.negotiated(), Some(negotiated));
}

#[tokio::test]
async fn handshake_rejects_a_different_declaration() {
    let limits = limits();
    let (wire, peer) = connected(limits);
    let record = Record::extension(&[Capability::ToolCall]);
    spawn_peer(
        peer,
        limits,
        answering(initialize_result(
            &[Capability::ToolCall],
            ProtocolRange::exactly(ProtocolVersion::new(1, 0)),
            &"0".repeat(64),
        )),
    );
    let error = handshake(&wire, &record, &[Capability::ToolCall])
        .await
        .expect_err("a child that loaded another declaration must be refused");
    assert_eq!(error, WireError::Handshake(HandshakeError::DigestMismatch));
    assert_eq!(wire.negotiated(), None, "a refused handshake grants nothing");
}

#[tokio::test]
async fn handshake_rejects_a_digest_that_is_not_sha256() {
    let limits = limits();
    let (wire, peer) = connected(limits);
    let record = Record::extension(&[Capability::ToolCall]);
    spawn_peer(
        peer,
        limits,
        answering(initialize_result(
            &[Capability::ToolCall],
            ProtocolRange::exactly(ProtocolVersion::new(1, 0)),
            "not-a-digest",
        )),
    );
    let error = handshake(&wire, &record, &[Capability::ToolCall])
        .await
        .expect_err("an unshaped digest must be refused");
    assert_eq!(error, WireError::Handshake(HandshakeError::InvalidDigest));
}

#[tokio::test]
async fn handshake_rejects_ranges_that_do_not_touch() {
    let limits = limits();
    let (wire, peer) = connected(limits);
    let record = Record::extension(&[Capability::ToolCall]);
    spawn_peer(
        peer,
        limits,
        answering(initialize_result(
            &[Capability::ToolCall],
            ProtocolRange::new(ProtocolVersion::new(1, 1), ProtocolVersion::new(2, 0)).expect("ordered range"),
            DIGEST,
        )),
    );
    let error = handshake(&wire, &record, &[Capability::ToolCall])
        .await
        .expect_err("ranges with no shared version must be refused");
    assert!(matches!(
        error,
        WireError::Handshake(HandshakeError::ProtocolIncompatible { .. })
    ));
}

#[tokio::test]
async fn handshake_rejects_another_package_identity() {
    let (wire, peer) = connected(limits());
    let record = Record::extension(&[Capability::ToolCall]);

    let mut wrong_id = initialize_params(&[Capability::ToolCall]);
    wrong_id.package_id = "another".to_string();
    let error = wire
        .initialize(wrong_id, &record.expectation())
        .await
        .expect_err("the id must match the installation record");
    assert!(matches!(error, WireError::Handshake(HandshakeError::PackageId { .. })));

    let mut wrong_kind = initialize_params(&[Capability::ToolCall]);
    wrong_kind.kind = PackageKind::Plugin;
    let error = wire
        .initialize(wrong_kind, &record.expectation())
        .await
        .expect_err("the kind must match the installation record");
    assert!(matches!(error, WireError::Handshake(HandshakeError::Kind { .. })));

    // Neither refusal reached the child: no frame was built, so nothing waits.
    drop(peer);
    assert_eq!(wire.pending_requests(), 0);
}

#[tokio::test]
async fn handshake_rejects_a_capability_the_manifest_never_declared() {
    let limits = limits();
    let (wire, peer) = connected(limits);
    let record = Record::extension(&[Capability::ToolCall]);
    spawn_peer(
        peer,
        limits,
        answering(initialize_result(
            &[Capability::ToolCall, Capability::CommandInvoke],
            ProtocolRange::exactly(ProtocolVersion::new(1, 0)),
            DIGEST,
        )),
    );
    let error = handshake(&wire, &record, &[Capability::ToolCall, Capability::CommandInvoke])
        .await
        .expect_err("a capability the manifest never declared must be refused");
    assert_eq!(
        error,
        WireError::Handshake(HandshakeError::CapabilityNotDeclared(Capability::CommandInvoke))
    );
}

#[tokio::test]
async fn a_second_handshake_is_refused() {
    let limits = limits();
    let (wire, peer) = connected(limits);
    let record = Record::extension(&[Capability::ToolCall]);
    spawn_peer(peer, limits, answering(accepted(&[Capability::ToolCall])));
    handshake(&wire, &record, &[Capability::ToolCall])
        .await
        .expect("first handshake settles");
    let error = handshake(&wire, &record, &[Capability::ToolCall])
        .await
        .expect_err("one connection negotiates once");
    assert_eq!(error, WireError::AlreadyInitialized);
}

#[tokio::test]
async fn dispatch_before_the_handshake_is_refused() {
    let (wire, peer) = connected(limits());
    let error = wire
        .request_value(WireMethod::ToolCall, None)
        .await
        .expect_err("nothing may be dispatched before the handshake");
    assert_eq!(error, WireError::NotInitialized);
    drop(peer);
}

#[tokio::test]
async fn a_negotiated_gap_refuses_the_method() {
    let limits = limits();
    let (wire, peer) = connected(limits);
    let record = Record::extension(&[Capability::ToolCall]);
    spawn_peer(peer, limits, answering(accepted(&[Capability::ToolCall])));
    handshake(&wire, &record, &[Capability::ToolCall])
        .await
        .expect("handshake settles with one capability");
    let error = wire
        .request_value(WireMethod::CommandInvoke, None)
        .await
        .expect_err("a capability neither side granted must not be dispatched");
    assert_eq!(error, WireError::CapabilityUnavailable(WireMethod::CommandInvoke));
    assert_eq!(
        wire.pending_requests(),
        0,
        "a refused method must not wait for an answer"
    );
}

#[tokio::test]
async fn both_package_kinds_settle_the_same_handshake() {
    let limits = limits();
    let (extension_wire, extension_peer) = connected(limits);
    let (plugin_wire, plugin_peer) = connected(limits);
    spawn_peer(extension_peer, limits, answering(accepted(&[Capability::ToolCall])));
    spawn_peer(plugin_peer, limits, answering(accepted(&[Capability::ToolCall])));

    let extension_record = Record::extension(&[Capability::ToolCall]);
    let plugin_record = Record::plugin(&[Capability::ToolCall]);
    let via_extension = handshake(&extension_wire, &extension_record, &[Capability::ToolCall])
        .await
        .expect("an extension negotiates");
    let via_plugin = plugin_wire
        .initialize(
            initialize_params_for(PackageKind::Plugin, &[Capability::ToolCall]),
            &plugin_record.expectation(),
        )
        .await
        .expect("a plugin negotiates the same way");
    assert_eq!(via_extension, via_plugin, "both kinds settle the identical handshake");
}

#[tokio::test]
async fn tool_calls_and_commands_answer_over_a_real_stream() {
    let limits = limits();
    let (wire, peer) = connected(limits);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let declared = every_capability();
    echoing_peer(peer, limits, Arc::clone(&seen), declared.clone());

    // Every capability the wire defines, so the test covers `Capability::ALL`.
    let record = Record::extension(&Capability::ALL);
    handshake(&wire, &record, &Capability::ALL)
        .await
        .expect("handshake settles");

    let called: Value = wire
        .request(
            WireMethod::ToolCall,
            Some(serde_json::json!({"tool": "sample__echo", "arguments": {"text": "hi"}})),
        )
        .await
        .expect("tool/call answers over the stream");
    assert_eq!(called["tool"], "sample__echo");
    assert_eq!(called["arguments"]["text"], "hi");

    let invoked: Value = wire
        .request(
            WireMethod::CommandInvoke,
            Some(serde_json::json!({"command": "sample:hello"})),
        )
        .await
        .expect("command/invoke answers over the stream");
    assert_eq!(invoked["command"], "sample:hello");

    let ids = seen.lock().expect("request log").clone();
    assert_eq!(ids.len(), 3, "initialize plus two calls");
    assert!(
        ids.windows(2).all(|pair| pair[0] < pair[1]),
        "ids only increase: {ids:?}"
    );
    wire.notify(ObserverParams {
        event: ObserverEvent::TurnEnd,
        metadata: Value::Null,
    })
    .expect("observer/notify only has to reach the wire");
}

#[tokio::test]
async fn concurrent_answers_stay_with_their_own_request() {
    let limits = limits();
    let (wire, peer) = connected(limits);
    echoing_peer(
        peer,
        limits,
        Arc::new(Mutex::new(Vec::new())),
        capabilities(&[Capability::ToolCall]),
    );
    let record = Record::extension(&[Capability::ToolCall]);
    handshake(&wire, &record, &[Capability::ToolCall])
        .await
        .expect("handshake settles");

    let first = {
        let wire = Arc::clone(&wire);
        tokio::spawn(async move {
            wire.request::<Value>(
                WireMethod::ToolCall,
                Some(serde_json::json!({"tool": "first", "arguments": {"n": 1}})),
            )
            .await
        })
    };
    let second = {
        let wire = Arc::clone(&wire);
        tokio::spawn(async move {
            wire.request::<Value>(
                WireMethod::ToolCall,
                Some(serde_json::json!({"tool": "second", "arguments": {"n": 2}})),
            )
            .await
        })
    };
    let first = first.await.expect("first request joined").expect("first answers");
    let second = second.await.expect("second request joined").expect("second answers");
    assert_eq!(first["tool"], "first");
    assert_eq!(first["arguments"]["n"], 1);
    assert_eq!(second["tool"], "second");
    assert_eq!(second["arguments"]["n"], 2);
}

#[tokio::test]
async fn a_frame_past_the_cap_is_refused_before_it_is_parsed() {
    let narrow = WireLimits {
        max_frame_bytes: 64,
        ..limits()
    };
    let mut reader = FrameReader::new(tokio::io::repeat(b'A'), narrow);
    let error = reader.next().await.expect_err("an endless frame must fail, not buffer");
    assert_eq!(error, WireError::Frame(FrameError::TooLarge { limit: 64 }));
}

#[tokio::test]
async fn a_clean_end_of_stream_returns_nothing() {
    let mut reader = FrameReader::new(&b""[..], limits());
    assert_eq!(reader.next().await.expect("an empty stream is not an error"), None);
}

#[tokio::test]
async fn a_stream_ending_inside_a_frame_is_refused() {
    let mut reader = FrameReader::new(&b"{\"jsonrpc\":\"2.0\"}"[..], limits());
    let error = reader.next().await.expect_err("a partial frame must be refused");
    assert_eq!(error, WireError::Frame(FrameError::Unterminated));
}

#[tokio::test]
async fn a_frame_that_is_not_utf8_is_refused() {
    let mut reader = FrameReader::new(&b"\xff\xfe\n"[..], limits());
    let error = reader
        .next()
        .await
        .expect_err("invalid UTF-8 must be refused, not guessed");
    assert_eq!(error, WireError::Frame(FrameError::NotUtf8));
}

#[tokio::test]
async fn a_silent_peer_hits_the_read_deadline() {
    let narrow = WireLimits {
        read_deadline: Duration::from_millis(50),
        ..limits()
    };
    // The peer half stays open and writes nothing, so the read would wait
    // forever without the deadline.
    let (host, _peer) = tokio::io::duplex(64);
    let mut reader = FrameReader::new(host, narrow);
    assert_eq!(
        reader.next().await.expect_err("a silent peer must not hang"),
        WireError::ReadTimeout
    );
}

#[tokio::test]
async fn a_response_with_both_result_and_error_is_refused() {
    let frame = br#"{"jsonrpc":"2.0","id":7,"result":null,"error":{"code":-32603,"message":"both"}}"#;
    let error = parse_frame(frame).expect_err("exactly one of result and error is the answer");
    assert_eq!(error, FrameError::InvalidResponse);
}

#[tokio::test]
async fn a_duplicate_or_unregistered_answer_is_reported() {
    let (wire, peer) = connected(limits());
    ready(&wire, &capabilities(&[Capability::ToolCall]));
    let answer = |id: u64| WireFrame::Response(WireResponse::reply(id, serde_json::json!({"ok": true})));
    assert_eq!(
        wire.dispatch(answer(1)).expect_err("nothing waits for this id"),
        WireError::UnexpectedResponse(1)
    );
    // The receiver stays alive: a dropped one makes the first send fail, which
    // is a different case from the duplicate this test is about.
    let (outcome, delivered) = tokio::sync::oneshot::channel();
    wire.lock_pending().insert(5, PendingRequest { outcome });
    wire.dispatch(answer(5))
        .expect("the first answer for a waiting id is delivered");
    assert!(delivered.await.expect("the answer is delivered").is_exclusive());
    assert_eq!(
        wire.dispatch(answer(5))
            .expect_err("the second answer for the same id is stale"),
        WireError::UnexpectedResponse(5),
        "a duplicated answer must not be delivered twice"
    );
    drop(peer);
}

#[tokio::test]
async fn a_deadline_fails_the_request_and_keeps_its_id_burned() {
    let limits = limits();
    let (wire, peer) = connected(limits);
    let seen = Arc::new(Mutex::new(Vec::new()));
    // Only `initialize` is answered; `tool/call` is left hanging on purpose.
    let mut answered = answering(accepted(&[Capability::ToolCall]));
    let peer_seen = Arc::clone(&seen);
    spawn_peer(peer, limits, move |request| {
        peer_seen.lock().expect("request log").push(request.id);
        answered(request)
    });
    let record = Record::extension(&[Capability::ToolCall]);
    handshake(&wire, &record, &[Capability::ToolCall])
        .await
        .expect("handshake settles");

    let error = wire
        .request_value(WireMethod::ToolCall, None)
        .await
        .expect_err("the peer answers nothing");
    assert_eq!(error, WireError::ReadTimeout);
    assert_eq!(wire.pending_requests(), 0, "a timed-out request stops waiting");

    // The id the timeout burned cannot accept an answer afterwards.
    let timed_out = seen.lock().expect("request log")[1];
    let late = WireFrame::Response(WireResponse::reply(timed_out, serde_json::json!("late")));
    assert_eq!(
        wire.dispatch(late).expect_err("a late answer is unregistered"),
        WireError::UnexpectedResponse(timed_out)
    );
}

#[tokio::test]
async fn cancel_wakes_the_waiter_and_drops_the_late_answer() {
    let limits = limits();
    let (wire, peer) = connected(limits);
    spawn_peer(peer, limits, answering(accepted(&[Capability::ToolCall])));
    let record = Record::extension(&[Capability::ToolCall]);
    handshake(&wire, &record, &[Capability::ToolCall])
        .await
        .expect("handshake settles");

    let pending = {
        let wire = Arc::clone(&wire);
        tokio::spawn(async move { wire.request_value(WireMethod::ToolCall, None).await })
    };
    tokio::time::sleep(Duration::from_millis(30)).await;
    let id = wire.lock_pending().keys().copied().next().expect("the call is waiting");
    wire.cancel(id).expect("a waiting request can be cancelled");
    assert_eq!(
        pending.await.expect("the request task joined"),
        Err(WireError::Cancelled),
        "a cancelled request reports Cancelled, never a closed channel"
    );
    assert_eq!(wire.pending_requests(), 0, "a cancelled request stops waiting");
    assert_eq!(
        wire.dispatch(WireFrame::Response(WireResponse::reply(id, serde_json::json!("late"))))
            .expect_err("a cancelled id must not accept a result"),
        WireError::UnexpectedResponse(id)
    );
}

#[tokio::test]
async fn two_concurrent_handshakes_put_one_frame_on_the_wire() {
    let limits = limits();
    let (host, peer) = tokio::io::duplex(64 * 1024);
    let wire = Arc::new(ExtensionWire::new(host, limits));
    let seen = Arc::new(Mutex::new(Vec::new()));

    // An inline peer that answers `initialize` only after a delay, so the
    // second caller arrives while the first handshake is genuinely in flight.
    let peer_seen = Arc::clone(&seen);
    tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(peer);
        let mut reader = FrameReader::new(read, limits);
        while let Ok(Some(WireFrame::Request(request))) = reader.next().await {
            peer_seen.lock().expect("request log").push(request.id);
            if request.method != WireMethod::Initialize {
                continue;
            }
            tokio::time::sleep(Duration::from_millis(80)).await;
            let frame = WireFrame::Response(WireResponse::reply(
                request.id,
                serde_json::to_value(accepted(&[Capability::ToolCall])).expect("serializes"),
            ));
            if write_frame(&mut write, &frame, limits.write_deadline).await.is_err() {
                break;
            }
        }
    });

    let first = {
        let wire = Arc::clone(&wire);
        let record = Record::extension(&[Capability::ToolCall]);
        tokio::spawn(async move { handshake(&wire, &record, &[Capability::ToolCall]).await })
    };
    // Wait until the first handshake has really sent its frame, so the second
    // caller meets the reserved slot rather than the finished one. The wait is
    // bounded: a reservation that never sends would fail here instead of
    // hanging the suite.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            // A `std::sync::MutexGuard` must not live across the await, so the
            // check and its drop happen in one statement.
            let logged = !seen.lock().expect("request log").is_empty();
            if logged {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the first handshake put its frame on the wire");
    let record = Record::extension(&[Capability::ToolCall]);
    assert_eq!(
        handshake(&wire, &record, &[Capability::ToolCall])
            .await
            .expect_err("the first handshake owns the slot"),
        WireError::AlreadyInitialized
    );
    first
        .await
        .expect("the first handshake joined")
        .expect("the first handshake settles");
    let ids = seen.lock().expect("request log").clone();
    assert_eq!(ids, vec![1], "exactly one initialize reached the child: {ids:?}");
}

#[tokio::test]
async fn a_raw_request_cannot_carry_a_control_method() {
    let limits = limits();
    let (wire, peer) = connected(limits);
    spawn_peer(peer, limits, answering(accepted(&[Capability::ToolCall])));
    let record = Record::extension(&[Capability::ToolCall]);
    handshake(&wire, &record, &[Capability::ToolCall])
        .await
        .expect("handshake settles");

    let seen = wire.pending_requests();
    for method in [WireMethod::Initialize, WireMethod::Cancel, WireMethod::Shutdown] {
        assert_eq!(
            wire.request_value(method, None)
                .await
                .expect_err("a control method needs its own entry point"),
            WireError::ControlMethod(method)
        );
    }
    assert_eq!(
        wire.pending_requests(),
        seen,
        "no control method ever waited for an answer"
    );
    // The slot is still open, so a later `initialize` is refused as a duplicate
    // rather than accepted through the raw path.
    assert_eq!(
        handshake(&wire, &record, &[Capability::ToolCall])
            .await
            .expect_err("the handshake already settled"),
        WireError::AlreadyInitialized
    );
}

#[tokio::test]
async fn cancelling_an_unknown_request_is_reported() {
    let (wire, peer) = connected(limits());
    assert_eq!(
        wire.cancel(11).expect_err("nothing waits for this id"),
        WireError::UnknownRequest(11)
    );
    drop(peer);
}

#[tokio::test]
async fn the_pending_cap_refuses_more_work() {
    let limits = WireLimits {
        max_pending_requests: 1,
        ..limits()
    };
    // `limits` here narrows the pending cap to one; a connection built with the
    // defaults would never reach it.
    let (wire, peer) = connected(limits);
    ready(&wire, &capabilities(&[Capability::ToolCall]));
    let first = {
        let wire = Arc::clone(&wire);
        tokio::spawn(async move { wire.request_value(WireMethod::ToolCall, None).await })
    };
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(wire.pending_requests(), 1);
    let error = wire
        .request_value(WireMethod::ToolCall, None)
        .await
        .expect_err("a second request must not be queued past the cap");
    assert_eq!(error, WireError::TooManyPending { limit: 1 });
    assert_eq!(first.await.expect("first request joined"), Err(WireError::ReadTimeout));
    drop(peer);
}

#[tokio::test]
async fn a_request_whose_deadline_passed_is_never_queued() {
    // A zero reply deadline means the deadline is already spent when the
    // request would be queued. Sending it anyway would run a side effect whose
    // answer nobody reads, which is exactly the ambiguous retry the protocol
    // forbids.
    let limits = WireLimits {
        reply_deadline: Duration::ZERO,
        ..limits()
    };
    let (host, mut peer) = tokio::io::duplex(64 * 1024);
    let wire = ExtensionWire::new(host, limits);
    ready(&wire, &capabilities(&[Capability::ToolCall]));

    assert_eq!(
        wire.request_value(WireMethod::ToolCall, None)
            .await
            .expect_err("a spent deadline must fail before the frame is built"),
        WireError::WriteTimeout
    );
    assert_eq!(wire.pending_requests(), 0, "nothing waited for an answer");
    let read = tokio::time::timeout(Duration::from_millis(150), async {
        let mut buffer = [0_u8; 64];
        tokio::io::AsyncReadExt::read(&mut peer, &mut buffer).await
    })
    .await;
    match read {
        Err(_elapsed) | Ok(Ok(0)) => {}
        Ok(Ok(count)) => panic!("a spent request reached the peer as {count} bytes"),
        Ok(Err(error)) => panic!("reading the peer failed: {error}"),
    }
}

#[tokio::test]
async fn the_observation_queue_is_bounded() {
    let limits = WireLimits {
        max_queued_frames: 1,
        ..limits()
    };
    let (wire, peer) = connected(limits);
    ready(&wire, &capabilities(&[Capability::ObserverNotify]));
    let observation = || {
        WireFrame::Notification(WireNotification::new(
            WireMethod::ObserverNotify,
            Some(serde_json::json!({"event": "turn.start", "metadata": null})),
        ))
    };
    wire.dispatch(observation()).expect("the first observation fits");
    assert_eq!(
        wire.dispatch(observation()).expect_err("the second exceeds the queue"),
        WireError::QueueFull
    );
    drop(peer);
}

#[tokio::test]
async fn shutdown_fails_waiting_requests_and_refuses_new_ones() {
    let limits = limits();
    let (wire, peer) = connected(limits);
    spawn_peer(peer, limits, answering(accepted(&[Capability::ToolCall])));
    let record = Record::extension(&[Capability::ToolCall]);
    handshake(&wire, &record, &[Capability::ToolCall])
        .await
        .expect("handshake settles");
    wire.shutdown(Some("host finished".to_string()))
        .await
        .expect("a child that never answers shutdown is still the expected end");
    assert!(wire.is_closed(), "shutdown closes the connection");
    let error = wire
        .request_value(WireMethod::ToolCall, None)
        .await
        .expect_err("nothing may be sent after shutdown");
    assert_eq!(error, WireError::Closed);
}

#[tokio::test]
async fn an_end_of_stream_fails_every_waiting_request() {
    let (wire, peer) = connected(limits());
    ready(&wire, &capabilities(&[Capability::ToolCall]));
    let waiting = {
        let wire = Arc::clone(&wire);
        tokio::spawn(async move { wire.request_value(WireMethod::ToolCall, None).await })
    };
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(wire.pending_requests(), 1);
    wire.handle_eof();
    assert_eq!(
        waiting.await.expect("the request joins on EOF"),
        Err(WireError::Closed),
        "a child that exits mid-request fails it instead of hanging"
    );
    drop(peer);
}

#[tokio::test]
async fn an_observation_before_the_handshake_is_refused() {
    let (wire, peer) = connected(limits());
    let params = serde_json::to_value(ObserverParams {
        event: ObserverEvent::SessionStart,
        metadata: Value::Null,
    })
    .expect("observer parameters serialize");
    let report = WireFrame::Notification(WireNotification::new(WireMethod::ObserverNotify, Some(params)));
    assert_eq!(
        wire.dispatch(report)
            .expect_err("a report before the handshake describes nothing"),
        WireError::NotInitialized
    );
    drop(peer);
}

#[tokio::test]
async fn observations_arrive_in_order_and_end_with_the_stream() {
    let (wire, peer) = connected(limits());
    ready(&wire, &capabilities(&[Capability::ObserverNotify]));
    for event in [ObserverEvent::SessionStart, ObserverEvent::TurnStart] {
        let params = serde_json::to_value(ObserverParams {
            event,
            metadata: Value::Null,
        })
        .expect("observer parameters serialize");
        wire.dispatch(WireFrame::Notification(WireNotification::new(
            WireMethod::ObserverNotify,
            Some(params),
        )))
        .expect("an observation is queued, not answered");
    }
    drop(peer);
    let first = wire.next_observation().await.expect("first observation");
    assert_eq!(first.method, WireMethod::ObserverNotify);
    assert_eq!(
        first
            .parse_params::<ObserverParams>()
            .expect("observer parameters parse")
            .event,
        ObserverEvent::SessionStart
    );
    let second = wire.next_observation().await.expect("second observation");
    assert_eq!(
        second
            .parse_params::<ObserverParams>()
            .expect("observer parameters parse")
            .event,
        ObserverEvent::TurnStart
    );
    wire.handle_eof();
    assert_eq!(wire.next_observation().await, None, "a closed queue ends the stream");
}

#[tokio::test]
async fn a_child_request_is_refused_rather_than_answered() {
    let (wire, peer) = connected(limits());
    let request = WireRequest::new(1, WireMethod::ToolCall, None);
    assert_eq!(
        wire.dispatch(WireFrame::Request(request))
            .expect_err("a child never issues host methods"),
        WireError::UnexpectedRequest(WireMethod::ToolCall)
    );
    drop(peer);
}

#[tokio::test]
async fn a_refusal_never_quotes_the_frame() {
    // A log line built from an error must not carry tool output or a secret the
    // child sent; the message names the shape, never the bytes.
    let secret = "sk-live-should-never-be-logged";
    let frame = format!(r#"{{"jsonrpc":"2.0","id":1,"result":{{"text":"{secret}"}}}}"#);
    let (wire, peer) = connected(limits());
    let parsed = parse_frame(frame.as_bytes()).expect("a well-shaped response parses");
    let error = wire.dispatch(parsed).expect_err("no request waits for this id");
    assert!(
        !error.to_string().contains(secret),
        "the error must not quote the frame: {error}"
    );
    let malformed = parse_frame(secret.as_bytes()).expect_err("plain text is not a frame");
    assert!(
        !malformed.to_string().contains(secret),
        "the parse error must not quote the frame"
    );
    let wrong_type = format!(r#"{{"jsonrpc":"2.0","id":"{secret}","result":null}}"#);
    let error = parse_frame(wrong_type.as_bytes()).expect_err("a request id must be an integer");
    assert_eq!(error.to_string(), "malformed frame");
    drop(peer);
}

/// Env marker for the child role below. Only the spawned process sees it.
const CHILD_ENV: &str = "HORON_EXTENSION_WIRE_CHILD";

/// The one test whose name the child role re-enters. Passing it as an exact
/// filter keeps the child from running anything else.
const CHILD_FILTER: &str = "extensions::wire::tests::the_framing_survives_a_real_child_process";

#[tokio::test]
async fn the_framing_survives_a_real_child_process() {
    // Both roles run this one test. As the parent it starts this very test
    // executable as a real child process, writes one request to the child's
    // stdin and reads the answer from the child's stderr, so framing, flushing
    // and the deadlines cross real OS pipes. The child answers on stderr
    // because libtest owns the child's stdout: writing frames there would mix
    // the harness output into the stream. No production shim exists for either
    // role.
    if std::env::var_os(CHILD_ENV).is_some() {
        child_answer_one_frame().await;
        return;
    }
    let limits = limits();
    let executable = std::env::current_exe().expect("the test executable has a path");
    let mut child = tokio::process::Command::new(executable)
        .args(["--exact", CHILD_FILTER, "--nocapture"])
        .env(CHILD_ENV, "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("the test executable starts");
    let mut stdin = child.stdin.take().expect("child stdin");
    let stderr = child.stderr.take().expect("child stderr");
    let request = WireRequest::new(1, WireMethod::Initialize, Some(serde_json::json!({"probe": true})));
    write_frame(&mut stdin, &request, limits.write_deadline)
        .await
        .expect("the request reaches the child");
    let mut reader = FrameReader::new(stderr, limits);
    let frame = reader.next().await.expect("reading from the child succeeds");
    let response = match frame {
        Some(WireFrame::Response(response)) => response,
        other => panic!("expected one response over the pipe, got {other:?}"),
    };
    let body = response.into_outcome().expect("the child sent one answer");
    assert_eq!(
        body.expect("the child answers initialize"),
        serde_json::json!({"ok": true})
    );
    let status = child.wait().await.expect("the child exits");
    assert!(status.success(), "the child role exits cleanly");
}

/// The child role: answer exactly one request, then return.
async fn child_answer_one_frame() {
    let limits = limits();
    let mut stdin = tokio::io::stdin();
    // libtest writes its report to stdout, so the frame goes to stderr. The
    // child runs with `--nocapture`, which keeps stderr on the real pipe.
    let mut stderr = tokio::io::stderr();
    let mut reader = FrameReader::new(&mut stdin, limits);
    let request = match reader.next().await.expect("the parent's request arrives") {
        Some(WireFrame::Request(request)) => request,
        other => panic!("expected one request from the parent, got {other:?}"),
    };
    assert_eq!(request.method, WireMethod::Initialize);
    let answer = WireResponse::reply(request.id, serde_json::json!({"ok": true}));
    write_frame(&mut stderr, &answer, limits.write_deadline)
        .await
        .expect("the answer reaches the parent");
}
