use std::time::Duration;

use futures::{StreamExt, stream};
use horon_agent_message_types::StreamEvent;
use horon_agent_provider_core::EventStream;
use tokio::time::{Instant, timeout_at};

pub(super) fn first_event_error() -> anyhow::Error {
    anyhow::anyhow!("Provider stream timed out waiting for its first meaningful event")
}

pub(super) fn apply(stream: EventStream, first: Option<Instant>, idle: Option<Duration>) -> EventStream {
    if first.is_none() && idle.is_none() {
        return stream;
    }

    Box::pin(stream::unfold(Some((stream, first, false)), move |state| async move {
        let (mut stream, mut deadline, mut started) = state?;
        let next = match deadline {
            Some(at) => match timeout_at(at, stream.next()).await {
                Ok(next) => next,
                Err(_) => {
                    let error = if started {
                        anyhow::anyhow!("Provider stream idle timeout waiting for the next meaningful event")
                    } else {
                        first_event_error()
                    };
                    return Some((Err(error), None));
                }
            },
            None => stream.next().await,
        }?;

        if let Ok(event) = &next
            && represents_progress(event)
        {
            started = true;
            deadline = idle.map(|budget| Instant::now() + budget);
        }
        Some((next, Some((stream, deadline, started))))
    }))
}

fn represents_progress(event: &StreamEvent) -> bool {
    // The wire parsers already discard SSE comments and protocol pings.
    // Anthropic also reports input-token usage in the message-start metadata.
    match event {
        StreamEvent::ConnectionType { .. }
        | StreamEvent::ConnectionPhase { .. }
        | StreamEvent::StatusDetail { .. }
        | StreamEvent::SessionId(_)
        | StreamEvent::UpstreamProvider { .. }
        | StreamEvent::TokenUsage { .. }
        | StreamEvent::Error { .. } => false,
        StreamEvent::TextDelta(text) | StreamEvent::ThinkingDelta(text) => !text.is_empty(),
        _ => true,
    }
}
