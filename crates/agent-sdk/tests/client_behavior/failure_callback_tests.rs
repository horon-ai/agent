use super::*;

#[test]
fn failure_callback_receives_structured_stop_before_error() {
    use horon_agent_sdk::{RunOptions, TurnStopReason};
    let client = fake_harness(|frame, writer| {
        if let ApiRequest::SendMessage { session_id, .. } = &frame.request {
            push(
                ApiEvent::MessageAccepted {
                    session_id: session_id.clone(),
                },
                writer,
            );
            push(
                ApiEvent::TurnStopped {
                    session_id: session_id.clone(),
                    reason: TurnStopReason::Crash,
                    message: "Caught runtime panic".into(),
                    provider_stop_reason: None,
                },
                writer,
            );
            push(
                ApiEvent::Error {
                    code: horon_agent_harness_api::ErrorCode::Internal,
                    message: "Caught runtime panic".into(),
                    details: None,
                },
                writer,
            );
            push(
                ApiEvent::TurnDone {
                    session_id: session_id.clone(),
                    pending_soft_interrupts: None,
                },
                writer,
            );
        }
    });
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let received = events.clone();
    let result = client.run(
        "s1",
        "hello",
        RunOptions {
            on_event: Some(Box::new(move |event| received.lock().unwrap().push(event.clone()))),
            ..Default::default()
        },
    );
    assert!(result.is_err());
    let events = events.lock().unwrap();
    let stop = events
        .iter()
        .position(|event| {
            matches!(
                event,
                ApiEvent::TurnStopped {
                    reason: TurnStopReason::Crash,
                    ..
                }
            )
        })
        .unwrap();
    let error = events
        .iter()
        .position(|event| matches!(event, ApiEvent::Error { .. }))
        .unwrap();
    assert!(stop < error);
}
