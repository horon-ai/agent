//! A soft interrupt injected mid-turn is reported to the owning v2 connection
//! under the id its row was stored with, and only when the connection owns the
//! session as a v2 one.

use super::*;
use crate::agent::Agent;
use crate::message::ContentBlock;
use crate::message::{Message, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;

/// The injection tests stop before any model call.
struct NoModelProvider;

#[async_trait]
impl Provider for NoModelProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        Err(anyhow::anyhow!("the soft interrupt event tests never call the model"))
    }

    fn name(&self) -> &str {
        "soft-interrupt-event-test"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(NoModelProvider)
    }
}

fn agent() -> Agent {
    let provider: Arc<dyn Provider> = Arc::new(NoModelProvider);
    Agent::new(provider, Registry::empty())
}

/// The connection's events received so far, drained in order.
fn drain(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> Vec<ServerEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

fn stored_ids(events: &[ServerEvent]) -> Vec<(&str, &str)> {
    events
        .iter()
        .filter_map(|event| match event {
            ServerEvent::SoftInterruptStored { message_id, content } => Some((message_id.as_str(), content.as_str())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn an_injected_user_message_is_reported_under_its_stored_row_id() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let mut agent = agent();
    let session_id = agent.session_id().to_string();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let token = crate::tool::acp_session::install_with_version(&session_id, "conn-v2", tx, Some(2));

    agent.queue_soft_interrupt("look at this".to_string(), Vec::new(), false, SoftInterruptSource::User);
    let injected = agent.inject_soft_interrupts(None).await;
    assert_eq!(injected.len(), 1);

    let stored = agent
        .session
        .active_context()
        .last()
        .expect("the injection was stored")
        .id
        .clone();
    let events = drain(&mut rx);
    assert_eq!(
        stored_ids(&events),
        vec![(stored.as_str(), "look at this")],
        "the editor learns the injection under the id a replay will report"
    );
    let history = agent.get_history(true);
    assert!(
        history
            .iter()
            .any(|row| row.message_id.as_deref() == Some(stored.as_str()) && row.content.contains("look at this")),
        "the reported id has to be the id of the stored history row, got {history:?}"
    );

    crate::tool::acp_session::clear(&session_id, token);
}

#[tokio::test]
async fn an_injection_with_a_display_role_stays_out_of_the_v2_channel() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let mut agent = agent();
    let session_id = agent.session_id().to_string();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let token = crate::tool::acp_session::install_with_version(&session_id, "conn-v2", tx, Some(2));

    agent.queue_soft_interrupt(
        "[Background Task Completed]".to_string(),
        Vec::new(),
        false,
        SoftInterruptSource::BackgroundTask,
    );
    let injected = agent.inject_soft_interrupts(None).await;
    assert_eq!(injected.len(), 1);

    let events = drain(&mut rx);
    assert!(
        stored_ids(&events).is_empty(),
        "an internal note is already reported through the turn's own events, got {events:?}"
    );

    crate::tool::acp_session::clear(&session_id, token);
}

#[tokio::test]
async fn a_v1_connection_receives_no_stored_ids() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let mut agent = agent();
    let session_id = agent.session_id().to_string();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let token = crate::tool::acp_session::install_with_version(&session_id, "conn-v1", tx, None);

    agent.queue_soft_interrupt("hello".to_string(), Vec::new(), false, SoftInterruptSource::User);
    let injected = agent.inject_soft_interrupts(None).await;
    assert_eq!(injected.len(), 1);

    let events = drain(&mut rx);
    assert!(
        stored_ids(&events).is_empty(),
        "the stored id is a v2 side channel, got {events:?}"
    );

    crate::tool::acp_session::clear(&session_id, token);
}

#[tokio::test]
async fn an_audio_attachment_on_a_soft_interrupt_becomes_an_audio_block() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let mut agent = agent();

    agent.queue_soft_interrupt(
        "listen".to_string(),
        vec![("audio/wav".to_string(), "AAAA".to_string())],
        false,
        SoftInterruptSource::User,
    );
    let injected = agent.inject_soft_interrupts(None).await;
    assert_eq!(injected.len(), 1);

    let stored = agent.session.active_context().last().expect("the injection was stored");
    assert!(matches!(
        &stored.content[0],
        ContentBlock::Audio { format, data } if format == "wav" && data == "AAAA"
    ));
    assert!(matches!(
        &stored.content[1],
        ContentBlock::Text { text, .. } if text == "listen"
    ));
}
