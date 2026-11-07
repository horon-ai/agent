//! Audio attachments become audio content blocks, an audio-only message still
//! starts a turn, and the block reaches the provider request as `input_audio`.

use super::*;
use crate::agent::Agent;
use crate::message::{Message, StreamEvent, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// Records the messages each model request carries, then answers with one line
/// of text so the turn completes normally.
struct CapturingProvider {
    seen: Arc<Mutex<Vec<Vec<Message>>>>,
}

#[async_trait]
impl Provider for CapturingProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        self.seen
            .lock()
            .map_err(|_| anyhow::anyhow!("the capture lock was poisoned"))?
            .push(messages.to_vec());
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(StreamEvent::TextDelta("ok".into())),
            Ok(StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".into()),
            }),
        ])))
    }

    fn name(&self) -> &str {
        "audio-test"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(CapturingProvider {
            seen: Arc::clone(&self.seen),
        })
    }
}

fn capturing_agent() -> (Agent, Arc<Mutex<Vec<Vec<Message>>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(CapturingProvider {
        seen: Arc::clone(&seen),
    });
    (Agent::new(provider, Registry::empty()), seen)
}

fn attachment(media_type: &str, data: &str) -> (String, String) {
    (media_type.to_string(), data.to_string())
}

/// The `input_audio` parts of a Responses request body, in order.
fn input_audio_parts(messages: &[Message]) -> Vec<Value> {
    crate::provider::openai_request::build_responses_input(messages)
        .into_iter()
        .filter_map(|item| item.get("content").and_then(Value::as_array).cloned())
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("input_audio"))
        .collect()
}

#[test]
fn audio_mime_types_become_audio_blocks_and_everything_else_stays_an_image() {
    let blocks = attachment_blocks(vec![
        attachment("audio/wav", "AAAA"),
        attachment("audio/mpeg", "BBBB"),
        attachment("image/png", "CCCC"),
        attachment("audio/ogg", "DDDD"),
    ]);

    assert_eq!(blocks.len(), 4);
    assert!(matches!(
        &blocks[0],
        ContentBlock::Audio { format, data } if format == "wav" && data == "AAAA"
    ));
    assert!(matches!(
        &blocks[1],
        ContentBlock::Audio { format, data } if format == "mp3" && data == "BBBB"
    ));
    assert!(matches!(
        &blocks[2],
        ContentBlock::Image { media_type, data } if media_type == "image/png" && data == "CCCC"
    ));
    assert!(
        matches!(
            &blocks[3],
            ContentBlock::Image { media_type, .. } if media_type == "audio/ogg"
        ),
        "an unsupported audio container keeps the meaning it always had: an image attachment"
    );
}

#[test]
fn an_empty_attachment_list_produces_no_blocks() {
    assert!(attachment_blocks(Vec::new()).is_empty());
}

#[tokio::test]
async fn an_audio_only_prompt_starts_the_turn_and_reaches_the_provider_as_input_audio() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut agent, seen) = capturing_agent();
    let (events, mut rx) = tokio::sync::mpsc::unbounded_channel();

    let result = agent
        .run_once_streaming_mpsc("", vec![attachment("audio/wav", "AAAA")], None, events)
        .await;
    while rx.try_recv().is_ok() {}
    result.expect("an audio-only prompt runs the turn");

    assert!(
        agent.session.model_usage_turn_id.is_some(),
        "an attachment with no text is still a prompt the model must answer"
    );
    let calls = seen.lock().expect("the capture lock");
    assert_eq!(calls.len(), 1, "one model request for one audio prompt");
    let parts = input_audio_parts(&calls[0]);
    assert_eq!(
        parts,
        vec![json!({"type": "input_audio", "input_audio": {"data": "AAAA", "format": "wav"}})],
        "the audio block has to survive into the provider request body"
    );
}

#[tokio::test]
async fn a_message_with_no_text_and_no_attachment_starts_no_turn() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut agent, _seen) = capturing_agent();

    agent.insert_user_context_message_with_display_role("", Vec::new(), None, false, true);

    assert!(
        agent.session.model_usage_turn_id.is_none(),
        "an empty prompt with nothing attached must not open a model turn"
    );
}
