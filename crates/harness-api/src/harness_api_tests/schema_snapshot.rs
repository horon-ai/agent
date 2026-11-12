//! Schema snapshot tests: fail if the wire shape changes accidentally.

use crate::*;

#[test]
fn api_version_minor_is_nineteen() {
    assert_eq!(API_VERSION_MAJOR, 1);
    assert_eq!(API_VERSION_MINOR, 19);
}

#[test]
fn minor_fourteen_requests_are_additive_and_keep_legacy_goldens() {
    let legacy_ask = r#"{"v":1,"id":4,"req":"ask_response","session_id":"s1","ask_id":"a1","answers":[{"question_id":"q1","selected":["Fast"]}]}"#;
    let decoded: ClientFrame = serde_json::from_str(legacy_ask).unwrap();
    assert!(
        matches!(&decoded.request, ApiRequest::AskResponse { answer_id: None, ask_id, .. } if ask_id == "a1"),
        "{decoded:?}"
    );
    assert_eq!(
        serde_json::to_string(&decoded).unwrap(),
        legacy_ask,
        "an answer without an id must serialize byte-identically to the legacy frame"
    );

    let hello = ClientFrame::new(
        1,
        ApiRequest::Hello {
            min_version: API_VERSION_MAJOR,
            max_version: API_VERSION_MAJOR,
            client: "legacy/1".into(),
            supports_user_response: false,
            supports_ask_requests: false,
        },
    );
    assert_eq!(
        serde_json::to_string(&hello).unwrap(),
        r#"{"v":1,"id":1,"req":"hello","min_version":1,"max_version":1,"client":"legacy/1"}"#,
        "the handshake must not gain fields a legacy server would reject"
    );

    let requests = [
        ApiRequest::GetRuntimeState {
            session_id: "s1".into(),
        },
        ApiRequest::ApplyControl {
            session_id: "s1".into(),
            operation_id: "op-1".into(),
            action: "cancel_input".into(),
            args: Some(serde_json::json!({"input_id": "i1"})),
        },
        ApiRequest::GetControlReceipt {
            session_id: "s1".into(),
            operation_id: "op-1".into(),
        },
        ApiRequest::GetSnapshot {
            session_id: "s1".into(),
            tail: Some(20),
            max_bytes: Some(262_144),
        },
        ApiRequest::AskResponse {
            session_id: "s1".into(),
            ask_id: "a1".into(),
            answers: vec![AskSelection {
                question_id: "q1".into(),
                selected: vec!["Fast".into()],
            }],
            answer_id: Some("answer-1".into()),
        },
    ];
    for request in requests {
        let frame = ClientFrame::new(9, request);
        let json = serde_json::to_string(&frame).unwrap();
        let back: ClientFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(frame, back, "round trip: {json}");
    }

    let minimal: ClientFrame =
        serde_json::from_str(r#"{"v":1,"id":9,"req":"get_snapshot","session_id":"s1"}"#).unwrap();
    assert_eq!(
        minimal.request,
        ApiRequest::GetSnapshot {
            session_id: "s1".into(),
            tail: None,
            max_bytes: None,
        },
        "an omitted tail keeps its default rather than a fabricated budget"
    );
}

/// Every gated request names its capability, and an unnegotiated one is
/// refused instead of served.
#[test]
fn gated_requests_name_their_capability_and_refuse_an_unnegotiated_peer() {
    let cases = [
        (
            ApiRequest::GetRuntimeState {
                session_id: "s1".into(),
            },
            CAPABILITY_DURABLE_RUNTIME_V1,
        ),
        (
            ApiRequest::ApplyControl {
                session_id: "s1".into(),
                operation_id: "op-1".into(),
                action: "cancel_input".into(),
                args: None,
            },
            CAPABILITY_DURABLE_RUNTIME_V1,
        ),
        (
            ApiRequest::GetControlReceipt {
                session_id: "s1".into(),
                operation_id: "op-1".into(),
            },
            CAPABILITY_DURABLE_RUNTIME_V1,
        ),
        (
            ApiRequest::GetSnapshot {
                session_id: "s1".into(),
                tail: None,
                max_bytes: None,
            },
            CAPABILITY_SNAPSHOT_MARKER_V1,
        ),
    ];
    for (request, capability) in cases {
        assert_eq!(request.required_capability(), Some(capability));
        assert_eq!(
            request.missing_capability(&[]),
            Some(capability),
            "a peer that advertises nothing must be refused"
        );
        assert_eq!(
            request.missing_capability(&[capability.to_string()]),
            None,
            "a peer that advertises the capability must be served"
        );
    }

    let plain_ask = ApiRequest::AskResponse {
        session_id: "s1".into(),
        ask_id: "a1".into(),
        answers: vec![],
        answer_id: None,
    };
    assert_eq!(
        plain_ask.required_capability(),
        None,
        "the legacy answer needs no negotiation"
    );

    let gated_ask = ApiRequest::AskResponse {
        session_id: "s1".into(),
        ask_id: "a1".into(),
        answers: vec![],
        answer_id: Some("answer-1".into()),
    };
    assert_eq!(gated_ask.required_capability(), Some(CAPABILITY_ASK_RECEIPTS_V1));

    let legacy_requests = [
        ApiRequest::Ping,
        ApiRequest::AttachSession {
            session_id: "s1".into(),
        },
        ApiRequest::GetHistory {
            session_id: "s1".into(),
        },
    ];
    for request in legacy_requests {
        assert_eq!(request.required_capability(), None, "{request:?} predates negotiation");
    }
}

/// The refusal is a structured error naming the capability, never a silent
/// success and never a version error an older client would misread.
#[test]
fn an_unsupported_request_is_a_structured_error() {
    let frame = unsupported_reply(7, CAPABILITY_DURABLE_RUNTIME_V1, "get_runtime_state");
    assert_eq!(frame.reply_to, Some(7));
    let wire = serde_json::to_value(&frame).unwrap();
    assert_eq!(wire["ev"], "error");
    assert_eq!(wire["code"], "unsupported");
    assert!(
        wire["message"]
            .as_str()
            .is_some_and(|message| message.contains(CAPABILITY_DURABLE_RUNTIME_V1)),
        "the message must name the missing capability: {wire}"
    );
    assert_eq!(serde_json::from_value::<ServerFrame>(wire).unwrap(), frame);
}

/// The new events round-trip, and the marker is a stream event rather than a
/// reply so a client cannot mistake it for an answer to its own request.
#[test]
fn minor_fourteen_events_roundtrip_and_the_marker_is_a_stream_event() {
    let state = RuntimeStateSummary {
        journal: RuntimeJournalSummary {
            exists: true,
            generation: 3,
            last_seq: 42,
            bytes: 4096,
            records: 42,
            read_only: false,
        },
        runs: vec![RuntimeRunSummary {
            run_id: "run-1".into(),
            started_at: "2026-10-26T00:00:00Z".into(),
            input_id: Some("i1".into()),
        }],
        queue: RuntimeQueueSummary {
            live_inputs: 2,
            undetermined_inputs: 1,
            live_input_bytes: 128,
        },
        receipts: vec![RuntimeReceiptSummary {
            input_id: "i0".into(),
            phase: "committed".into(),
            seq: 41,
            run_id: None,
        }],
        monitors: vec![RuntimeMonitorSummary {
            monitor_id: "m1".into(),
            kind: "file".into(),
            stopped: false,
            last_summary: Some("src/lib.rs changed".into()),
        }],
        pending_wakes: 1,
        pending_controls: 0,
        last_stop: Some(RuntimeStopSummary {
            run_id: "run-0".into(),
            cause: "completed".into(),
            at: "2026-10-26T00:00:00Z".into(),
        }),
    };

    let runtime_state = ServerFrame::reply(
        5,
        ApiEvent::RuntimeState {
            session_id: "s1".into(),
            state: state.clone(),
        },
    );
    let wire = serde_json::to_value(&runtime_state).unwrap();
    assert_eq!(wire["ev"], "runtime_state");
    assert_eq!(wire["state"]["journal"]["last_seq"], 42);
    assert_eq!(serde_json::from_value::<ServerFrame>(wire).unwrap(), runtime_state);

    let receipt = ServerFrame::reply(
        6,
        ApiEvent::ControlReceipt {
            session_id: "s1".into(),
            receipt: RuntimeControlReceipt {
                operation_id: "op-1".into(),
                action: "cancel_input".into(),
                outcome: RuntimeControlOutcome::Replay,
                result: Some(serde_json::json!({"cancelled": true})),
                message: "already applied".into(),
            },
        },
    );
    let wire = serde_json::to_value(&receipt).unwrap();
    assert_eq!(wire["receipt"]["outcome"], "replay");
    assert_eq!(serde_json::from_value::<ServerFrame>(wire).unwrap(), receipt);

    let marker = ServerFrame::event(ApiEvent::SnapshotMarker {
        session_id: "s1".into(),
        marker: SnapshotMarkerInfo {
            generation: "gen-1".into(),
            seq: 12,
            marker_id: "marker-1".into(),
        },
    });
    let wire = serde_json::to_value(&marker).unwrap();
    assert_eq!(wire["ev"], "snapshot_marker");
    assert!(
        wire.get("reply_to").is_none(),
        "the marker bounds a stream, it does not answer a request"
    );
    assert_eq!(serde_json::from_value::<ServerFrame>(wire).unwrap(), marker);

    let snapshot = ServerFrame::reply(
        7,
        ApiEvent::Snapshot {
            session_id: "s1".into(),
            snapshot: SessionSnapshot {
                generation: "gen-1".into(),
                seq: 12,
                marker_id: "marker-1".into(),
                state,
                messages: vec![HistoryMessage {
                    response_stats: None,
                    role: "user".into(),
                    content: "hi".into(),
                }],
                next_history_cursor: None,
                truncated: true,
            },
        },
    );
    let wire = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(wire["snapshot"]["marker_id"], "marker-1");
    assert_eq!(wire["snapshot"]["truncated"], true);
    assert_eq!(serde_json::from_value::<ServerFrame>(wire).unwrap(), snapshot);
}

/// A summary is all a client gets: nothing that could carry queued text, a
/// command line, or an environment variable may appear on this wire.
#[test]
fn the_runtime_summary_carries_no_content_or_command_environment() {
    let wire = serde_json::to_value(ApiEvent::RuntimeState {
        session_id: "s1".into(),
        state: RuntimeStateSummary::default(),
    })
    .unwrap();
    let text = serde_json::to_string(&wire).unwrap();
    for forbidden in ["content", "env", "command", "spec", "args"] {
        assert!(
            !text.contains(&format!("\"{forbidden}\"")),
            "the runtime summary must not expose `{forbidden}`: {text}"
        );
    }
}

/// Canonical event bytes are carried as opaque strings. These values are the
/// ones a JSON number round-trip destroys, so a client that parses and
/// re-serializes would silently change the event's identity.
#[test]
fn replication_wire_preserves_canonical_event_bytes() {
    let canonical = r#"{"big":18446744073709551615,"one":1.0,"negative_zero":-0.0,"text":"\u00e9"}"#;
    let record = CanonicalEventRecord {
        event_id: "11111111-1111-4111-8111-111111111111".into(),
        content_hash: "a".repeat(64),
        canonical_json: canonical.into(),
    };
    let wire = serde_json::to_string(&record).unwrap();
    assert!(wire.contains("18446744073709551615"));
    assert!(wire.contains("1.0"));
    assert!(wire.contains("-0.0"));
    let back: CanonicalEventRecord = serde_json::from_str(&wire).unwrap();
    assert_eq!(back.canonical_json, canonical);
    assert_eq!(back, record);
}

#[test]
fn replication_requests_roundtrip_with_defaults() {
    let page = SessionEventPage {
        session_id: "s1".into(),
        heads: vec!["h1".into()],
        events: vec![CanonicalEventRecord {
            event_id: "e1".into(),
            content_hash: "c".repeat(64),
            canonical_json: "{}".into(),
        }],
        next_cursor: Some("cursor".into()),
        blob_refs: vec![PayloadRef {
            sha256: "b".repeat(64),
            size: 7,
            encoding: "canonical_json".into(),
        }],
        record_refs: vec![CanonicalRecordRef {
            position: 0,
            event_id: "e1".into(),
            content_hash: "c".repeat(64),
            sha256: "d".repeat(64),
            size: 9,
        }],
    };
    let requests = [
        ApiRequest::ListSessionHeads {
            session_id: "s1".into(),
        },
        ApiRequest::SelectSessionHead {
            session_id: "s1".into(),
            head_event_id: "h1".into(),
        },
        ApiRequest::GetHistoryPage {
            session_id: "s1".into(),
            head_event_id: Some("h1".into()),
            before_event_id: Some("e1".into()),
            limit: Some(50),
        },
        ApiRequest::ExportSessionEvents {
            session_id: "s1".into(),
            protocol_version: REPLICATION_PROTOCOL_VERSION,
            heads: vec!["h1".into()],
            known_ids: vec!["e0".into()],
            cursor: Some("cursor".into()),
        },
        ApiRequest::ImportSessionEvents {
            session_id: "s1".into(),
            protocol_version: REPLICATION_PROTOCOL_VERSION,
            heads: page.heads.clone(),
            events: page.events.clone(),
            next_cursor: page.next_cursor.clone(),
            blob_refs: page.blob_refs.clone(),
            record_refs: page.record_refs.clone(),
            retry_cursor: Some("retry".into()),
        },
        ApiRequest::ExportSessionBlob {
            session_id: "s1".into(),
            sha256: "b".repeat(64),
            total_size: 7,
            offset: 0,
        },
        ApiRequest::ImportSessionBlob {
            session_id: "s1".into(),
            chunk: SessionBlobChunk {
                sha256: "b".repeat(64),
                offset: 0,
                data_base64: "AAAA".into(),
                next_offset: 3,
                total_size: 7,
                complete: false,
            },
        },
        ApiRequest::GetSessionBlobStatus {
            session_id: "s1".into(),
            sha256: "b".repeat(64),
            total_size: 7,
        },
        ApiRequest::ExportSessionRecordBlob {
            session_id: "s1".into(),
            reference: page.record_refs[0].clone(),
            offset: 0,
        },
        ApiRequest::ImportSessionRecordBlob {
            session_id: "s1".into(),
            reference: page.record_refs[0].clone(),
            chunk: SessionBlobChunk {
                sha256: "d".repeat(64),
                offset: 0,
                data_base64: "AAAA".into(),
                next_offset: 3,
                total_size: 9,
                complete: false,
            },
        },
        ApiRequest::GetSessionRecordBlobStatus {
            session_id: "s1".into(),
            reference: page.record_refs[0].clone(),
        },
    ];
    for request in requests {
        let frame = ClientFrame::new(1, request);
        let json = serde_json::to_string(&frame).unwrap();
        let back: ClientFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(frame, back);
    }

    let legacy: ClientFrame =
        serde_json::from_str(r#"{"v":1,"id":2,"req":"export_session_events","session_id":"s1"}"#).unwrap();
    assert!(matches!(
        legacy.request,
        ApiRequest::ExportSessionEvents {
            protocol_version: REPLICATION_PROTOCOL_VERSION,
            heads,
            known_ids,
            cursor: None,
            ..
        } if heads.is_empty() && known_ids.is_empty()
    ));
}

#[test]
fn replication_events_roundtrip() {
    let reference = CanonicalRecordRef {
        position: 1,
        event_id: "e1".into(),
        content_hash: "c".repeat(64),
        sha256: "d".repeat(64),
        size: 9,
    };
    let events = [
        ApiEvent::SessionHeads {
            session_id: "s1".into(),
            heads: vec!["h1".into(), "h2".into()],
            selected_head: None,
        },
        ApiEvent::SessionHistoryPage {
            session_id: "s1".into(),
            head: "h1".into(),
            messages: vec![HistoryMessage {
                response_stats: None,
                role: "user".into(),
                content: "hi".into(),
            }],
            next_cursor: Some("e1".into()),
        },
        ApiEvent::SessionEventPage {
            session_id: "s1".into(),
            heads: vec!["h1".into()],
            events: vec![CanonicalEventRecord {
                event_id: "e1".into(),
                content_hash: "c".repeat(64),
                canonical_json: "{}".into(),
            }],
            next_cursor: Some("cursor".into()),
            blob_refs: vec![],
            record_refs: vec![reference.clone()],
        },
        ApiEvent::SessionImportResult {
            session_id: "s1".into(),
            accepted_ids: vec!["e1".into()],
            heads: vec!["h1".into()],
            retry_cursor: Some("retry".into()),
            conflict_heads: vec!["h2".into()],
        },
        ApiEvent::SessionBlobChunk {
            session_id: "s1".into(),
            sha256: "b".repeat(64),
            offset: 0,
            data_base64: "AAAA".into(),
            next_offset: 3,
            total_size: 7,
            complete: false,
        },
        ApiEvent::SessionBlobStatus {
            session_id: "s1".into(),
            sha256: "b".repeat(64),
            received: 3,
            total_size: 7,
            complete: false,
        },
        ApiEvent::SessionRecordBlobStatus {
            session_id: "s1".into(),
            reference,
            received: 3,
            total_size: 9,
            complete: false,
        },
    ];
    for event in events {
        let frame = ServerFrame::event(event);
        let json = serde_json::to_string(&frame).unwrap();
        let back: ServerFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(frame, back);
    }
}

#[test]
fn error_details_are_additive_and_old_frames_still_decode() {
    let legacy = r#"{"v":1,"reply_to":3,"ev":"error","code":"invalid_request","message":"bad"}"#;
    let frame: ServerFrame = serde_json::from_str(legacy).unwrap();
    assert!(matches!(
        &frame.event,
        ApiEvent::Error {
            code: ErrorCode::InvalidRequest,
            details: None,
            ..
        }
    ));
    assert_eq!(serde_json::to_string(&frame).unwrap(), legacy);

    let details = StoreErrorDetail {
        store_code: "hash_conflict".into(),
        retryable: false,
        accepted_ids: vec!["e1".into()],
        retry_cursor: Some("retry".into()),
        conflict_heads: vec!["h2".into()],
    };
    let frame = ServerFrame::reply(
        4,
        ApiEvent::Error {
            code: ErrorCode::InvalidRequest,
            message: "conflict".into(),
            details: Some(details.clone()),
        },
    );
    let wire = serde_json::to_value(&frame).unwrap();
    assert_eq!(wire["details"]["store_code"], "hash_conflict");
    assert_eq!(wire["details"]["retry_cursor"], "retry");
    let back: ServerFrame = serde_json::from_value(wire).unwrap();
    assert_eq!(back, frame);
}

#[test]
fn create_session_system_prompt_roundtrip_and_legacy_default() {
    let legacy = serde_json::json!({"req": "create_session"});
    let decoded: ApiRequest = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), legacy);
    for prompt in ["Custom system instructions\nwith unicode: 世界", ""] {
        let request = ApiRequest::CreateSession {
            working_dir: None,
            system_prompt: Some(prompt.into()),
        };
        let wire = serde_json::to_value(&request).unwrap();
        assert_eq!(wire["system_prompt"], prompt);
        assert_eq!(serde_json::from_value::<ApiRequest>(wire).unwrap(), request);
    }
}

#[test]
fn token_usage_preserves_cache_creation_and_accepts_legacy_frames() {
    let legacy = r#"{"v":1,"ev":"token_usage","session_id":"s1","input":10,"output":5,"cache_read_input":2}"#;
    let legacy_frame: ServerFrame = serde_json::from_str(legacy).unwrap();
    assert!(matches!(
        &legacy_frame.event,
        ApiEvent::TokenUsage {
            cache_creation_input: None,
            ..
        }
    ));
    assert_eq!(serde_json::to_string(&legacy_frame).unwrap(), legacy);

    for cache_creation_input in [Some(0), Some(42)] {
        let frame = ServerFrame::event(ApiEvent::TokenUsage {
            session_id: "s1".into(),
            input: 10,
            output: 5,
            cache_read_input: Some(2),
            cache_creation_input,
        });
        let wire = serde_json::to_value(&frame).unwrap();
        assert_eq!(wire["cache_creation_input"], cache_creation_input.unwrap());
        let decoded: ServerFrame = serde_json::from_value(wire).unwrap();
        assert_eq!(decoded.event, frame.event);
    }
}

#[test]
fn client_frame_wire_shape() {
    let frame = ClientFrame::new(
        7,
        ApiRequest::SendMessage {
            session_id: "s1".into(),
            content: "hi".into(),
            images: vec![],
            system_reminder: None,
            no_reply: false,
        },
    );
    let json = serde_json::to_string(&frame).unwrap();
    assert_eq!(
        json,
        r#"{"v":1,"id":7,"req":"send_message","session_id":"s1","content":"hi"}"#
    );
}

#[test]
fn send_message_no_reply_wire_shape_and_legacy_default() {
    let frame = ClientFrame::new(
        8,
        ApiRequest::SendMessage {
            session_id: "s1".into(),
            content: "context".into(),
            images: vec![],
            system_reminder: None,
            no_reply: true,
        },
    );
    let json = serde_json::to_string(&frame).unwrap();
    assert_eq!(
        json,
        r#"{"v":1,"id":8,"req":"send_message","session_id":"s1","content":"context","no_reply":true}"#
    );

    let legacy: ClientFrame =
        serde_json::from_str(r#"{"v":1,"id":9,"req":"send_message","session_id":"s1","content":"old","images":[]}"#)
            .unwrap();
    assert!(matches!(
        legacy.request,
        ApiRequest::SendMessage {
            system_reminder: None,
            no_reply: false,
            ..
        }
    ));
}

#[test]
fn soft_interrupt_images_wire_shape_and_legacy_default() {
    let frame = ClientFrame::new(
        10,
        ApiRequest::SoftInterrupt {
            session_id: "s1".into(),
            content: "look".into(),
            images: vec![("image/png".into(), "aW1hZ2U=".into())],
            urgent: true,
            client_id: None,
        },
    );
    assert_eq!(
        serde_json::to_string(&frame).unwrap(),
        r#"{"v":1,"id":10,"req":"soft_interrupt","session_id":"s1","content":"look","images":[["image/png","aW1hZ2U="]],"urgent":true}"#
    );

    let legacy: ClientFrame =
        serde_json::from_str(r#"{"v":1,"id":11,"req":"soft_interrupt","session_id":"s1","content":"old"}"#).unwrap();
    assert!(matches!(
        legacy.request,
        ApiRequest::SoftInterrupt {
            images,
            urgent: false,
            ..
        } if images.is_empty()
    ));
}

#[test]
fn soft_interrupt_ids_requests_and_events_wire_shape() {
    let frame = ClientFrame::new(
        12,
        ApiRequest::SoftInterrupt {
            session_id: "s1".into(),
            content: "steer".into(),
            images: vec![],
            urgent: false,
            client_id: Some("f1".into()),
        },
    );
    assert_eq!(
        serde_json::to_string(&frame).unwrap(),
        r#"{"v":1,"id":12,"req":"soft_interrupt","session_id":"s1","content":"steer","urgent":false,"client_id":"f1"}"#
    );

    // Unfiltered cancel keeps the legacy shape; the filter is additive.
    let all: ClientFrame =
        serde_json::from_str(r#"{"v":1,"id":13,"req":"cancel_soft_interrupts","session_id":"s1"}"#).unwrap();
    assert_eq!(
        all.request,
        ApiRequest::CancelSoftInterrupts {
            session_id: "s1".into(),
            client_ids: None
        }
    );
    let some = ClientFrame::new(
        14,
        ApiRequest::CancelSoftInterrupts {
            session_id: "s1".into(),
            client_ids: Some(vec!["f1".into()]),
        },
    );
    assert_eq!(
        serde_json::to_string(&some).unwrap(),
        r#"{"v":1,"id":14,"req":"cancel_soft_interrupts","session_id":"s1","client_ids":["f1"]}"#
    );

    let injected = ServerFrame::event(ApiEvent::SoftInterruptInjected {
        session_id: "s1".into(),
        client_ids: vec!["f1".into(), "f2".into()],
        point: "D".into(),
        tools_skipped: None,
        display_role: None,
    });
    assert_eq!(
        serde_json::to_string(&injected).unwrap(),
        r#"{"v":1,"ev":"soft_interrupt_injected","session_id":"s1","client_ids":["f1","f2"],"point":"D"}"#
    );

    let cancelled = ServerFrame::reply(
        14,
        ApiEvent::SoftInterruptsCancelled {
            cancelled: vec!["f1".into()],
            not_queued: vec![],
        },
    );
    assert_eq!(
        serde_json::to_string(&cancelled).unwrap(),
        r#"{"v":1,"reply_to":14,"ev":"soft_interrupts_cancelled","cancelled":["f1"],"not_queued":[]}"#
    );

    let moved = ServerFrame::reply(
        15,
        ApiEvent::BackgroundToolResult {
            moved: true,
            tool_call_id: Some("call_1".into()),
            tool_name: Some("bash".into()),
        },
    );
    assert_eq!(
        serde_json::to_string(&moved).unwrap(),
        r#"{"v":1,"reply_to":15,"ev":"background_tool_result","moved":true,"tool_call_id":"call_1","tool_name":"bash"}"#
    );

    // A turn with nothing left queued keeps the legacy `turn_done` shape.
    let done = ServerFrame::event(ApiEvent::TurnDone {
        session_id: "s1".into(),
        pending_soft_interrupts: None,
    });
    assert_eq!(
        serde_json::to_string(&done).unwrap(),
        r#"{"v":1,"ev":"turn_done","session_id":"s1"}"#
    );
}

#[test]
fn server_frame_wire_shape() {
    let frame = ServerFrame::reply(
        3,
        ApiEvent::HelloOk {
            version: 1,
            server: "horon-agent/0.55.1".into(),
            capabilities: vec![],
        },
    );
    let json = serde_json::to_string(&frame).unwrap();
    assert_eq!(
        json,
        r#"{"v":1,"reply_to":3,"ev":"hello_ok","version":1,"server":"horon-agent/0.55.1"}"#
    );
}

#[test]
fn unknown_event_kind_is_skippable() {
    let json = r#"{"v":1,"ev":"some_future_event","payload":123}"#;
    let frame: ServerFrame = serde_json::from_str(json).unwrap();
    assert_eq!(frame.event, ApiEvent::Unknown);
}

#[test]
fn unknown_fields_are_ignored() {
    let json = r#"{"v":1,"ev":"turn_done","session_id":"s1","future_field":true}"#;
    let frame: ServerFrame = serde_json::from_str(json).unwrap();
    assert_eq!(
        frame.event,
        ApiEvent::TurnDone {
            session_id: "s1".into(),
            pending_soft_interrupts: None,
        }
    );
}

#[test]
fn request_roundtrip() {
    let reqs = [
        ApiRequest::Hello {
            min_version: 1,
            max_version: 1,
            client: "test/0".into(),
            supports_user_response: false,
            supports_ask_requests: false,
        },
        ApiRequest::ListSessions {
            include_archived: false,
            limit: None,
        },
        ApiRequest::ArchiveSession {
            session_id: "s1".into(),
        },
        ApiRequest::RestoreSession {
            session_id: "s1".into(),
        },
        ApiRequest::SetRetentionPolicy {
            archive_after_days: Some(30),
        },
        ApiRequest::CreateSession {
            working_dir: None,
            system_prompt: None,
        },
        ApiRequest::AttachSession {
            session_id: "s1".into(),
        },
        ApiRequest::Cancel {
            session_id: "s1".into(),
        },
        ApiRequest::PermissionResponse {
            session_id: "s1".into(),
            request_id: "p1".into(),
            decision: PermissionDecision::Allow,
        },
        ApiRequest::GetRuntimeInfo {
            session_id: "s1".into(),
        },
        ApiRequest::SetApiKey {
            provider: "gemini".into(),
            api_key: "secret".into(),
        },
        ApiRequest::ClearApiKey {
            provider: "gemini".into(),
        },
        ApiRequest::ReadFile {
            session_id: "s1".into(),
            path: "src/lib.rs".into(),
            max_bytes: Some(1024),
        },
        ApiRequest::FindFiles {
            session_id: "s1".into(),
            query: "lib".into(),
            limit: Some(10),
        },
        ApiRequest::SearchText {
            session_id: "s1".into(),
            query: "needle".into(),
            path: Some("src".into()),
            limit: Some(10),
        },
        ApiRequest::FileStatus {
            session_id: "s1".into(),
            path: "src/lib.rs".into(),
        },
        ApiRequest::GetTodoState {
            session_id: "s1".into(),
        },
        ApiRequest::AckTodoFollowUp {
            session_id: "s1".into(),
            effects: horon_agent_todo_policy::TodoEffects {
                clear_gate_observations: true,
                mark_long_session_review_delivered: false,
            },
        },
        ApiRequest::Ping,
    ];
    for req in reqs {
        let frame = ClientFrame::new(1, req);
        let json = serde_json::to_string(&frame).unwrap();
        let back: ClientFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(frame, back);
    }
}

#[test]
fn client_handshake_over_in_memory_pipe() {
    // Server side scripted: one hello_ok line.
    let reply = serde_json::to_string(&ServerFrame::reply(
        1,
        ApiEvent::HelloOk {
            version: 1,
            server: "horon-agent/test".into(),
            capabilities: vec!["sessions".into()],
        },
    ))
    .unwrap()
        + "\n";
    let mut out: Vec<u8> = Vec::new();
    let mut client = HarnessClient::new(std::io::BufReader::new(reply.as_bytes()), &mut out);
    let frame = client.hello("test-client/0.1").unwrap();
    match frame.event {
        ApiEvent::HelloOk { version, .. } => assert_eq!(version, 1),
        other => panic!("unexpected event: {other:?}"),
    }
    let sent = String::from_utf8(out).unwrap();
    assert!(sent.contains(r#""req":"hello""#), "sent: {sent}");
}

/// `read_line` grows its buffer until it finds a newline. A server, proxy, or
/// stuck peer that never sends one must not make the client allocate forever.
#[test]
fn read_frame_refuses_an_unterminated_frame_past_the_cap() {
    let mut reader = std::io::BufReader::new(std::io::repeat(b'A'));
    let error = read_frame::<_, ServerFrame>(&mut reader).expect_err("an endless frame must fail, not buffer");
    assert!(
        matches!(error, FrameError::FrameTooLarge { limit } if limit == MAX_FRAME_BYTES),
        "{error:?}"
    );
    assert!(error.to_string().contains("exceeds"), "error should say why: {error}");
}

/// The cap must not clip legitimate traffic: a frame just under it, images
/// included, has to pass intact, and the next frame after it must still read.
#[test]
fn read_frame_accepts_a_large_terminated_frame_and_continues() {
    let payload = "B".repeat(1024 * 1024);
    let large =
        serde_json::json!({"v": 1, "ev": "text_delta", "session_id": "s1", "message_id": "m1", "text": payload});
    let input = format!(
        "{large}\n{}\n",
        serde_json::to_string(&ServerFrame::reply(1, ApiEvent::Pong)).unwrap()
    );
    let mut reader = std::io::BufReader::new(input.as_bytes());
    let first: ServerFrame = read_frame(&mut reader).expect("a 1 MiB frame is legitimate");
    assert!(
        matches!(first.event, ApiEvent::TextDelta { ref text, .. } if text.len() == 1024 * 1024),
        "{first:?}"
    );
    let second: ServerFrame = read_frame(&mut reader).expect("the frame after a large one still reads");
    assert!(matches!(second.event, ApiEvent::Pong), "{second:?}");
}

/// The cap is one exact number on this side too, and the structural gate reads
/// it. The client refuses a frame the bridge would accept if the two constants
/// drift, so the boundary is measured here and printed for
/// `scripts/perf/check_structural.py` rather than trusted.
#[test]
fn read_frame_cap_is_exact_and_reported_to_the_structural_gate() {
    let cap = MAX_FRAME_BYTES as usize;
    let prefix = r#"{"v":1,"ev":"text_delta","session_id":"s1","message_id":"m1","text":""#;
    let suffix = "\"}\n";
    let mut accepted = String::with_capacity(cap);
    accepted.push_str(prefix);
    accepted.push_str(&"B".repeat(cap - prefix.len() - suffix.len()));
    accepted.push_str(suffix);
    assert_eq!(accepted.len(), cap, "the accepted frame must sit exactly at the cap");
    let mut reader = std::io::BufReader::new(accepted.as_bytes());
    let frame: ServerFrame = read_frame(&mut reader).expect("a frame of exactly the cap is legitimate");
    match frame.event {
        ApiEvent::TextDelta { ref text, .. } => assert_eq!(text.len(), cap - prefix.len() - suffix.len()),
        ref other => panic!("unexpected event: {other:?}"),
    }

    let mut rejected = String::with_capacity(cap + 1);
    rejected.push_str(&"B".repeat(cap));
    rejected.push('\n');
    assert_eq!(
        rejected.len(),
        cap + 1,
        "the refused frame must be one byte above the cap"
    );
    let mut reader = std::io::BufReader::new(rejected.as_bytes());
    let error = read_frame::<_, ServerFrame>(&mut reader).expect_err("one byte past the cap must be refused");
    assert!(
        matches!(error, FrameError::FrameTooLarge { limit } if limit == MAX_FRAME_BYTES),
        "{error:?}"
    );

    println!(
        "STRUCTURAL_PERF section=frame source=sdk max_frame_bytes={MAX_FRAME_BYTES} \
         accepted_frame_bytes={} rejected_frame_bytes={}",
        accepted.len(),
        rejected.len()
    );
}

/// The TypeScript SDK mirrors these enums by hand, so a variant added here
/// without a matching entry in `sdk/typescript/src/protocol.ts` silently
/// leaves every JS client unable to name the new frame. Checking from the
/// Rust side means the guard runs in the normal `cargo test` suite, where
/// the change is actually being made, rather than only in the SDK's own
/// Node tests which a Rust-only contributor never runs.
#[test]
fn typescript_sdk_lists_every_variant() {
    let Some(sdk) = sdk_protocol_source() else {
        // Absent in vendored/packaged builds: nothing to check.
        return;
    };
    for (file, enum_name) in [("requests.rs", "ApiRequest"), ("events.rs", "ApiEvent")] {
        for variant in enum_variants(file, enum_name) {
            assert!(
                sdk.contains(&format!("\"{variant}\"")),
                "{enum_name}::{variant} is missing from sdk/typescript/src/protocol.ts; \
                 add it to the union and to KNOWN_*_KINDS"
            );
        }
    }
}

fn sdk_protocol_source() -> Option<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/typescript/src/protocol.ts");
    std::fs::read_to_string(path).ok()
}

/// Tag parity is not enough: a variant can keep its name and gain a field,
/// and every JS client would then be unable to read the new data while all
/// existing tests stay green. Renaming a field is worse, because the SDK
/// keeps compiling against a name the wire no longer carries. Check the
/// payload shape too, from the same side the change is made on.
#[test]
fn typescript_sdk_lists_every_field() {
    let Some(sdk) = sdk_protocol_source() else {
        return;
    };
    for (file, enum_name, tag) in [("requests.rs", "ApiRequest", "req"), ("events.rs", "ApiEvent", "ev")] {
        for (variant, fields) in enum_variant_fields(file, enum_name) {
            let Some(block) = sdk_variant_block(&sdk, tag, &variant) else {
                panic!(
                    "{enum_name}::{variant} has no `{tag}: \"{variant}\"` member in \
                     sdk/typescript/src/protocol.ts"
                );
            };
            for field in fields {
                assert!(
                    block.contains(&format!("{field}:")) || block.contains(&format!("{field}?:")),
                    "{enum_name}::{variant}.{field} is missing from its TypeScript member \
                     in sdk/typescript/src/protocol.ts:\n{block}"
                );
            }
        }
    }
}

/// The `{ ev: "tag"; ... }` object literal for one variant, braces balanced.
fn sdk_variant_block(sdk: &str, tag: &str, variant: &str) -> Option<String> {
    let needle = format!("{tag}: \"{variant}\"");
    let hit = sdk.find(&needle)?;
    let open = sdk[..hit].rfind('{')?;
    let mut depth = 0usize;
    for (offset, ch) in sdk[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(sdk[open..open + offset + 1].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Snake-cased variant names paired with their serialized field names.
fn enum_variant_fields(file: &str, enum_name: &str) -> Vec<(String, Vec<String>)> {
    let body = enum_body(file, enum_name);
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for line in body.lines() {
        let Some(rest) = line.strip_prefix("    ") else {
            continue;
        };
        if rest.starts_with(' ') {
            // A field line inside the variant currently being collected.
            if let Some((_, fields)) = out.last_mut()
                && let Some(name) = field_name(rest)
            {
                fields.push(name);
            }
            continue;
        }
        let name: String = rest.chars().take_while(|ch| ch.is_ascii_alphanumeric()).collect();
        let Some(first) = name.chars().next() else {
            continue;
        };
        if !first.is_ascii_uppercase() || name == "Unknown" {
            continue;
        }
        // Single-line variants carry their fields inline: `Foo { a: T, b: U },`.
        let inline = rest
            .split_once('{')
            .and_then(|(_, tail)| tail.rsplit_once('}'))
            .map(|(inner, _)| {
                inner
                    .split(',')
                    .filter_map(|part| field_name(part.trim()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        out.push((snake_case(&name), inline));
    }
    out
}

/// `name` from a `name: Type` field line, skipping attributes and comments.
fn field_name(line: &str) -> Option<String> {
    let line = line.trim();
    if line.starts_with('#') || line.starts_with("//") || line.is_empty() {
        return None;
    }
    let (name, _) = line.split_once(':')?;
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return None;
    }
    Some(name.to_string())
}

fn enum_body(file: &str, enum_name: &str) -> String {
    let source = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file))
        .expect("read API source");
    let start = source.find(&format!("pub enum {enum_name} {{")).expect("enum present");
    let body = &source[start..];
    let end = body.find("\n}").unwrap_or(body.len());
    body[..end].to_string()
}

/// Snake-cased variant names of `enum_name`, excluding the `Unknown` catch-all.
fn enum_variants(file: &str, enum_name: &str) -> Vec<String> {
    let source = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file))
        .expect("read API source");
    let start = source.find(&format!("pub enum {enum_name} {{")).expect("enum present");
    let body = &source[start..];
    let end = body.find("\n}").unwrap_or(body.len());
    body[..end]
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("    ")?;
            if rest.starts_with(' ') {
                return None;
            }
            let name: String = rest.chars().take_while(|ch| ch.is_ascii_alphanumeric()).collect();
            let mut chars = name.chars();
            let first = chars.next()?;
            if !first.is_ascii_uppercase() || name == "Unknown" {
                return None;
            }
            Some(snake_case(&name))
        })
        .collect()
}

fn snake_case(name: &str) -> String {
    let mut out = String::new();
    for (index, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[test]
fn session_recovery_wire_shape_roundtrips_optional_notice() {
    for reconnect_notice in [None, Some("reconnected".into())] {
        let frame = ServerFrame::event(ApiEvent::SessionRecovery {
            session_id: "s1".into(),
            continuation_message: "continue task".into(),
            reconnect_notice: reconnect_notice.clone(),
        });
        let wire = serde_json::to_value(&frame).unwrap();
        assert_eq!(wire["ev"], "session_recovery");
        assert_eq!(wire["session_id"], "s1");
        assert_eq!(wire["continuation_message"], "continue task");
        assert_eq!(wire.get("reconnect_notice").is_some(), reconnect_notice.is_some());
        assert!(wire.get("reply_to").is_none());
        assert_eq!(serde_json::from_value::<ServerFrame>(wire).unwrap(), frame);
    }
}

#[test]
fn hidden_system_reminder_wire_shape_and_legacy_default() {
    let frame = ClientFrame::new(
        12,
        ApiRequest::SendMessage {
            session_id: "s1".into(),
            content: String::new(),
            system_reminder: Some("continue task".into()),
            images: vec![],
            no_reply: false,
        },
    );
    let wire = serde_json::to_value(&frame).unwrap();
    assert_eq!(wire["content"], "");
    assert_eq!(wire["system_reminder"], "continue task");
    assert!(wire.get("no_reply").is_none());
    assert_eq!(serde_json::from_value::<ClientFrame>(wire).unwrap(), frame);
    let legacy: ClientFrame =
        serde_json::from_str(r#"{"v":1,"id":13,"req":"send_message","session_id":"s1","content":"hello"}"#).unwrap();
    assert!(matches!(
        legacy.request,
        ApiRequest::SendMessage {
            system_reminder: None,
            ..
        }
    ));
}

#[test]
fn text_framing_is_additive_and_accepts_unframed_legacy_deltas() {
    let old = r#"{"v":1,"ev":"text_delta","session_id":"s1","text":"hello"}"#;
    let frame: ServerFrame = serde_json::from_str(old).unwrap();
    assert!(matches!(frame.event, ApiEvent::TextDelta { message_id: None, .. }));
    assert_eq!(serde_json::to_string(&frame).unwrap(), old);
    for event in [
        ApiEvent::TextDelta {
            session_id: "s1".into(),
            text: "hi".into(),
            message_id: Some("m1".into()),
        },
        ApiEvent::TextDone {
            session_id: "s1".into(),
            message_id: Some("m1".into()),
        },
        ApiEvent::TextReplace {
            session_id: "s1".into(),
            text: "".into(),
            message_id: Some("m1".into()),
        },
    ] {
        let frame = ServerFrame::event(event);
        let wire = serde_json::to_value(&frame).unwrap();
        assert_eq!(wire["message_id"], "m1");
        assert_eq!(serde_json::from_value::<ServerFrame>(wire).unwrap().event, frame.event);
    }
}

#[test]
fn session_tool_control_wire_shapes_and_defaults() {
    use serde_json::json;
    for tools in [
        json!({}),
        json!({"enabled":null}),
        json!({"enabled":[]}),
        json!({"enabled":["read"],"disabled":["bash"],"custom":[{"name":"lookup","description":"Look up","parameters":{"type":"object"}}]}),
    ] {
        let wire = json!({"v":1,"id":1,"req":"configure_tools","session_id":"s1","tools":tools});
        let frame: ClientFrame = serde_json::from_value(wire).unwrap();
        let ApiRequest::ConfigureTools { tools: config, .. } = &frame.request else {
            panic!()
        };
        assert_eq!(
            config.enabled,
            tools
                .get("enabled")
                .filter(|v| !v.is_null())
                .map(|v| serde_json::from_value(v.clone()).unwrap())
        );
        assert_eq!(
            serde_json::from_value::<ClientFrame>(serde_json::to_value(&frame).unwrap()).unwrap(),
            frame
        );
    }
    for wire in [
        json!({"v":1,"id":2,"req":"list_tools","session_id":"s1"}),
        json!({"v":1,"id":3,"req":"tool_result","session_id":"s1","call_id":"c1","output":"ok"}),
        json!({"v":1,"id":4,"req":"tool_result","session_id":"s1","call_id":"c1","output":"","error":"failed"}),
    ] {
        let frame: ClientFrame = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(frame).unwrap(), wire);
    }
    for wire in [
        json!({"v":1,"reply_to":2,"ev":"tools","session_id":"s1","tools":[{"name":"read","description":"Read file","parameters":{"type":"object"}}]}),
        json!({"v":1,"ev":"tool_call","session_id":"s1","call_id":"c1","name":"lookup","input":{"key":1}}),
    ] {
        let frame: ServerFrame = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(frame).unwrap(), wire);
    }
    for parameters in [json!(null), json!([]), json!("object"), json!(42)] {
        assert!(
            serde_json::from_value::<SessionToolDefinition>(
                json!({"name":"bad","description":"bad","parameters":parameters})
            )
            .is_err()
        );
    }
    assert_eq!(serde_json::to_value(ToolConfiguration::default()).unwrap(), json!({}));
}

#[test]
fn approval_and_failover_wire_shapes_roundtrip() {
    let cases: [(ApiEvent, &str); 6] = [
        (
            ApiEvent::ApprovalPending {
                session_id: "s1".into(),
                request_id: "req-1".into(),
                action: "bash".into(),
                expires_at: Some("2026-10-17T12:00:00Z".into()),
            },
            "approval_pending",
        ),
        (
            ApiEvent::ApprovalResolved {
                session_id: "s1".into(),
                request_id: "req-1".into(),
                approved: true,
                via: "file".into(),
                resolved_by: "runner".into(),
            },
            "approval_resolved",
        ),
        (
            ApiEvent::FailoverSwitched {
                session_id: "s1".into(),
                chain_id: "chain-1".into(),
                from: "openai".into(),
                to: "anthropic".into(),
                reason: "rate_limit".into(),
                attempt: 2,
                max: 3,
            },
            "failover_switched",
        ),
        (
            ApiEvent::RunNeedsHumanResume {
                session_id: "s1".into(),
                request_id: Some("req-1".into()),
                reason: "unverifiable".into(),
                attempts: 2,
            },
            "run_needs_human_resume",
        ),
        (
            ApiEvent::ApprovalAck {
                request_id: "req-1".into(),
            },
            "approval_ack",
        ),
        (
            ApiEvent::FailoverAck {
                chain_id: "chain-1".into(),
                from: "openai".into(),
                to: "anthropic".into(),
                attempt: 1,
            },
            "failover_ack",
        ),
    ];
    for (event, tag) in cases {
        let frame = ServerFrame::event(event);
        let wire = serde_json::to_value(&frame).unwrap();
        assert_eq!(wire["ev"], tag);
        assert!(wire.get("reply_to").is_none(), "{tag} is a stream event, not a reply");
        assert_eq!(
            serde_json::from_value::<ServerFrame>(wire).unwrap(),
            frame,
            "{tag} must survive a wire round trip"
        );
    }

    // A stop that names no permission request (an exhausted failover budget)
    // must reach clients as `null`, not as a value a consumer would try to
    // resolve as an approval id.
    let stop = ServerFrame::event(ApiEvent::RunNeedsHumanResume {
        session_id: "s1".into(),
        request_id: None,
        reason: "failover_budget_exhausted".into(),
        attempts: 3,
    });
    let wire = serde_json::to_value(&stop).unwrap();
    assert_eq!(
        wire["request_id"],
        serde_json::Value::Null,
        "a stop with no request carries null: {wire}"
    );
    assert_eq!(
        serde_json::from_value::<ServerFrame>(wire).unwrap(),
        stop,
        "a null request id must survive a wire round trip"
    );
}

#[test]
fn approval_pending_omits_absent_expiry_and_ignores_unknown_fields() {
    let frame = ServerFrame::event(ApiEvent::ApprovalPending {
        session_id: "s1".into(),
        request_id: "req-1".into(),
        action: "bash".into(),
        expires_at: None,
    });
    let wire = serde_json::to_value(&frame).unwrap();
    assert!(
        wire.get("expires_at").is_none(),
        "absent expiry stays off the wire: {wire}"
    );

    let legacy =
        r#"{"v":1,"ev":"approval_pending","session_id":"s1","request_id":"req-2","action":"write","future_field":42}"#;
    let decoded: ServerFrame = serde_json::from_str(legacy).unwrap();
    assert!(
        matches!(&decoded.event, ApiEvent::ApprovalPending { request_id, expires_at, .. }
            if request_id == "req-2" && expires_at.is_none()),
        "a payload with an unknown extra field must still parse: {decoded:?}"
    );
}

#[test]
fn a_client_skips_an_event_kind_it_does_not_know() {
    // The real client parser, fed a frame from a future daemon: it must skip the
    // frame and keep going rather than fail the connection.
    let stream = concat!(
        "{\"v\":1,\"ev\":\"some_future_event\",\"payload\":123}\n",
        "{\"v\":1,\"ev\":\"turn_done\",\"session_id\":\"s1\"}\n"
    );
    let mut out: Vec<u8> = Vec::new();
    let mut client = HarnessClient::new(std::io::BufReader::new(stream.as_bytes()), &mut out);
    let frame = client.recv().unwrap();
    assert!(
        matches!(frame.event, ApiEvent::TurnDone { .. }),
        "the unknown kind must be skipped, not returned or fatal"
    );
}

#[test]
fn turn_stopped_schema_and_future_reason_compatibility() {
    for reason in ["interrupted", "failure", "crash", "provider_guardrail", "limit_reached"] {
        let wire =
            serde_json::json!({"v":1,"ev":"turn_stopped","session_id":"s1","reason":reason,"message":"Explanation"});
        let frame: ServerFrame = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(frame).unwrap(), wire);
    }
    let wire = serde_json::json!({"v":1,"ev":"turn_stopped","session_id":"s1","reason":"future_reason","message":"Explanation","provider_stop_reason":"refusal"});
    let frame: ServerFrame = serde_json::from_value(wire).unwrap();
    assert!(
        matches!(frame.event, ApiEvent::TurnStopped { reason: TurnStopReason::Unknown, provider_stop_reason: Some(reason), .. } if reason == "refusal")
    );
    let done: ServerFrame =
        serde_json::from_value(serde_json::json!({"v":1,"ev":"turn_done","session_id":"s1"})).unwrap();
    assert!(matches!(done.event, ApiEvent::TurnDone { .. }));
}

/// The ack requests travel from a client to the daemon and carry the exact key
/// the daemon clears on. A renamed field would leave the durable copy in place
/// while the client believed it had acknowledged, so the shape is pinned here.
#[test]
fn approval_and_failover_ack_wire_shape() {
    let frame = ClientFrame::new(
        12,
        ApiRequest::ApprovalAck {
            session_id: "s1".into(),
            request_id: "req-1".into(),
        },
    );
    assert_eq!(
        serde_json::to_string(&frame).unwrap(),
        r#"{"v":1,"id":12,"req":"approval_ack","session_id":"s1","request_id":"req-1"}"#
    );

    let frame = ClientFrame::new(
        13,
        ApiRequest::FailoverAck {
            session_id: "s1".into(),
            chain_id: "chain-1".into(),
            from: "openai".into(),
            to: "anthropic".into(),
            attempt: 2,
        },
    );
    let wire = serde_json::to_string(&frame).unwrap();
    assert_eq!(
        wire,
        r#"{"v":1,"id":13,"req":"failover_ack","session_id":"s1","chain_id":"chain-1","from":"openai","to":"anthropic","attempt":2}"#
    );

    let decoded: ClientFrame = serde_json::from_str(&wire).unwrap();
    assert_eq!(
        decoded.request,
        ApiRequest::FailoverAck {
            session_id: "s1".into(),
            chain_id: "chain-1".into(),
            from: "openai".into(),
            to: "anthropic".into(),
            attempt: 2,
        },
        "the full switch key survives a round trip"
    );

    // The key fields are required: a partial ack must not decode into a
    // request that would clear nothing.
    assert!(
        serde_json::from_str::<ClientFrame>(
            r#"{"v":1,"id":15,"req":"failover_ack","session_id":"s1","chain_id":"chain-1","from":"openai","to":"anthropic"}"#
        )
        .is_err(),
        "an ack without its attempt must not decode"
    );
}
