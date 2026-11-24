use horon_agent_harness_api::{
    API_VERSION_MAJOR, ApiEvent, ApiRequest, CAPABILITY_A2UI_V1, ClientFrame, ServerFrame, read_frame, write_frame,
};
use horon_agent_sdk::{
    A2UI_BASIC_CATALOG_V1_0, A2UI_VERSION_V1_0, A2uiEnvelopeEvent, A2uiSessionCapabilities, AttachSessionOptions,
    ConnectOptions, CreateSessionOptions, ErrorKind, HoronAgentClient, RunOptions, SessionInfo, SshConnectOptions,
    Transport,
};
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct TcpTransport(TcpStream);

impl Transport for TcpTransport {
    fn shutdown_handle(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let stream = self.0.try_clone().ok()?;
        Some(Arc::new(move || {
            let _ = stream.shutdown(Shutdown::Both);
        }))
    }

    fn split(self: Box<Self>) -> horon_agent_sdk::Result<(Box<dyn BufRead + Send>, Box<dyn Write + Send>)> {
        let writer = self.0.try_clone().unwrap();
        Ok((Box::new(BufReader::new(self.0)), Box::new(writer)))
    }
}

fn stub_session(id: &str) -> SessionInfo {
    SessionInfo {
        edit_stats: None,
        user_connected: false,
        parent_session_id: None,
        agent_label: None,
        swarm_status: None,
        session_id: id.to_string(),
        working_dir: None,
        title: None,
        status: "idle".to_string(),
        transcript_bytes: None,
        saved: false,
        updated_at_ms: None,
        last_active_at_ms: None,
        archived: false,
        archived_at_ms: None,
        save_label: None,
    }
}

fn connect_mock<F>(
    options: ConnectOptions,
    capabilities: Vec<String>,
    on_request: F,
) -> (
    HoronAgentClient,
    Arc<Mutex<Vec<ApiRequest>>>,
    std::thread::JoinHandle<()>,
)
where
    F: Fn(&ClientFrame, &mut TcpStream) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_bg = Arc::clone(&seen);
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        while let Ok(frame) = read_frame::<_, ClientFrame>(&mut reader) {
            seen_bg.lock().unwrap().push(frame.request.clone());
            match &frame.request {
                ApiRequest::Hello { .. } => {
                    write_frame(
                        &mut stream,
                        &ServerFrame::reply(
                            frame.id,
                            ApiEvent::HelloOk {
                                version: API_VERSION_MAJOR,
                                server: "a2ui-sdk-test".into(),
                                capabilities: capabilities.clone(),
                            },
                        ),
                    )
                    .unwrap();
                }
                _ => on_request(&frame, &mut stream),
            }
        }
    });
    let client_stream = TcpStream::connect(addr).unwrap();
    let client = HoronAgentClient::connect_with(
        Box::new(TcpTransport(client_stream)),
        ConnectOptions {
            ensure_runtime: false,
            request_timeout: Some(Duration::from_secs(5)),
            ..options
        },
    )
    .unwrap();
    (client, seen, server)
}

#[test]
fn connect_and_session_defaults_omit_a2ui_to_keep_prompt_cache_unchanged() {
    assert!(ConnectOptions::default().a2ui.is_none());
    assert!(SshConnectOptions::new("remote").a2ui.is_none());
    assert!(CreateSessionOptions::default().a2ui.is_none());
    assert!(AttachSessionOptions::default().a2ui.is_none());

    let (client, seen, server) = connect_mock(
        ConnectOptions::default(),
        vec![CAPABILITY_A2UI_V1.into()],
        |frame, w| match &frame.request {
            ApiRequest::CreateSession { .. } => write_frame(
                w,
                &ServerFrame::reply(
                    frame.id,
                    ApiEvent::Attached {
                        session: stub_session("created"),
                    },
                ),
            )
            .unwrap(),
            ApiRequest::AttachSession { session_id, .. } => write_frame(
                w,
                &ServerFrame::reply(
                    frame.id,
                    ApiEvent::Attached {
                        session: stub_session(session_id),
                    },
                ),
            )
            .unwrap(),
            _ => {}
        },
    );

    assert!(client.supports(CAPABILITY_A2UI_V1));
    assert_eq!(client.create_session(None).unwrap().session_id, "created");
    assert_eq!(client.attach_session("existing").unwrap().session_id, "existing");
    drop(client);
    server.join().unwrap();

    let recorded = seen.lock().unwrap();
    assert!(matches!(&recorded[0], ApiRequest::Hello { a2ui: None, .. }));
    assert!(matches!(&recorded[1], ApiRequest::CreateSession { a2ui: None, .. }));
    assert!(matches!(&recorded[2], ApiRequest::AttachSession { a2ui: None, .. }));
}

#[test]
fn opt_in_a2ui_options_forward_on_connect_create_and_attach() {
    let caps = A2uiSessionCapabilities {
        version: A2UI_VERSION_V1_0.into(),
        supported_catalog_ids: vec![A2UI_BASIC_CATALOG_V1_0.into()],
        accepts_inline_catalogs: true,
    };
    let (client, seen, server) = connect_mock(
        ConnectOptions {
            a2ui: Some(caps.clone()),
            ..Default::default()
        },
        vec![CAPABILITY_A2UI_V1.into()],
        |frame, w| match &frame.request {
            ApiRequest::CreateSession { .. } | ApiRequest::AttachSession { .. } => {
                write_frame(
                    w,
                    &ServerFrame::reply(
                        frame.id,
                        ApiEvent::Attached {
                            session: stub_session("s-a2ui"),
                        },
                    ),
                )
                .unwrap();
            }
            _ => {}
        },
    );

    client
        .create_session_with_options(CreateSessionOptions {
            a2ui: Some(caps.clone()),
            ..Default::default()
        })
        .unwrap();
    client
        .attach_session_with_options(
            "s-a2ui",
            AttachSessionOptions {
                a2ui: Some(caps.clone()),
            },
        )
        .unwrap();
    drop(client);
    server.join().unwrap();

    let recorded = seen.lock().unwrap();
    assert!(matches!(&recorded[0], ApiRequest::Hello { a2ui: Some(c), .. } if c == &caps));
    assert!(matches!(&recorded[1], ApiRequest::CreateSession { a2ui: Some(c), .. } if c == &caps));
    assert!(matches!(&recorded[2], ApiRequest::AttachSession { a2ui: Some(c), .. } if c == &caps));
}

#[test]
fn configure_a2ui_and_send_a2ui_event_enforce_capability_and_roundtrip() {
    let (unsupported, _, s1) = connect_mock(ConnectOptions::default(), vec![], |_, _| {});
    let err = unsupported
        .configure_a2ui("s1", Some(A2uiSessionCapabilities::default()))
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
    let err = unsupported
        .send_a2ui_event("s1", json!({"version": "v0.9"}), None)
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
    let err = unsupported
        .create_session_with_options(CreateSessionOptions {
            a2ui: Some(A2uiSessionCapabilities::default()),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
    drop(unsupported);
    s1.join().unwrap();

    let (client, seen, s2) = connect_mock(
        ConnectOptions::default(),
        vec![CAPABILITY_A2UI_V1.into()],
        |frame, w| {
            if matches!(
                frame.request,
                ApiRequest::ConfigureA2ui { .. } | ApiRequest::A2uiEvent { .. }
            ) {
                write_frame(w, &ServerFrame::reply(frame.id, ApiEvent::Ok)).unwrap();
            }
        },
    );
    let caps = A2uiSessionCapabilities::default();
    client.configure_a2ui("s1", Some(caps.clone())).unwrap();
    client.configure_a2ui("s1", None).unwrap();
    client
        .send_a2ui_event(
            "s1",
            json!({"version": "v0.9", "action": {"name": "confirm", "surfaceId": "main"}}),
            Some(json!({"step": 1})),
        )
        .unwrap();
    drop(client);
    s2.join().unwrap();

    let recorded = seen.lock().unwrap();
    assert!(matches!(&recorded[1], ApiRequest::ConfigureA2ui { capabilities: Some(c), .. } if c == &caps));
    assert!(matches!(
        &recorded[2],
        ApiRequest::ConfigureA2ui { capabilities: None, .. }
    ));
    assert!(matches!(
        &recorded[3],
        ApiRequest::A2uiEvent {
            data_model: Some(_),
            ..
        }
    ));
}

#[test]
fn run_collects_a2ui_envelopes_and_drops_unnegotiated_envelopes() {
    let envelope = json!({
        "version": "v0.9",
        "createSurface": { "surfaceId": "main", "catalogId": A2UI_BASIC_CATALOG_V1_0 }
    });
    let env_clone = envelope.clone();
    let (client, _, s1) = connect_mock(
        ConnectOptions::default(),
        vec![CAPABILITY_A2UI_V1.into()],
        move |frame, w| {
            if let ApiRequest::SendMessage { session_id, .. } = &frame.request {
                write_frame(
                    w,
                    &ServerFrame::event(ApiEvent::MessageAccepted {
                        session_id: session_id.clone(),
                    }),
                )
                .unwrap();
                write_frame(
                    w,
                    &ServerFrame::event(ApiEvent::A2uiEnvelope {
                        session_id: session_id.clone(),
                        tool_call_id: "call-1".into(),
                        sequence: 0,
                        envelope: env_clone.clone(),
                        fallback_text: Some("UI fallback".into()),
                    }),
                )
                .unwrap();
                write_frame(
                    w,
                    &ServerFrame::event(ApiEvent::TextDelta {
                        session_id: session_id.clone(),
                        text: "Rendered".into(),
                        message_id: None,
                    }),
                )
                .unwrap();
                write_frame(
                    w,
                    &ServerFrame::event(ApiEvent::TurnDone {
                        session_id: session_id.clone(),
                        pending_soft_interrupts: None,
                    }),
                )
                .unwrap();
            }
        },
    );

    let streamed = Arc::new(Mutex::new(Vec::new()));
    let streamed_ref = Arc::clone(&streamed);
    let turn = client
        .run(
            "s1",
            "show ui",
            RunOptions {
                on_event: Some(Box::new(move |ev| {
                    if let ApiEvent::A2uiEnvelope { sequence, .. } = ev {
                        streamed_ref.lock().unwrap().push(*sequence);
                    }
                })),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(turn.final_text, "Rendered");
    assert_eq!(*streamed.lock().unwrap(), vec![0]);
    assert_eq!(
        turn.a2ui_envelopes,
        vec![A2uiEnvelopeEvent {
            tool_call_id: "call-1".into(),
            sequence: 0,
            envelope: envelope.clone(),
            fallback_text: Some("UI fallback".into()),
        }]
    );
    drop(client);
    s1.join().unwrap();

    // Even if an older/misbehaving server sends A2uiEnvelope without advertising
    // `a2ui_v1`, the Rust SDK drops it before delivery or TurnResult collection.
    let (unopted, _, s2) = connect_mock(ConnectOptions::default(), vec![], move |frame, w| {
        if let ApiRequest::SendMessage { session_id, .. } = &frame.request {
            write_frame(
                w,
                &ServerFrame::event(ApiEvent::MessageAccepted {
                    session_id: session_id.clone(),
                }),
            )
            .unwrap();
            write_frame(
                w,
                &ServerFrame::event(ApiEvent::A2uiEnvelope {
                    session_id: session_id.clone(),
                    tool_call_id: "call-1".into(),
                    sequence: 0,
                    envelope: envelope.clone(),
                    fallback_text: None,
                }),
            )
            .unwrap();
            write_frame(
                w,
                &ServerFrame::event(ApiEvent::TurnDone {
                    session_id: session_id.clone(),
                    pending_soft_interrupts: None,
                }),
            )
            .unwrap();
        }
    });
    let turn = unopted.run("s1", "hi", RunOptions::default()).unwrap();
    assert!(turn.a2ui_envelopes.is_empty());
    drop(unopted);
    s2.join().unwrap();
}
