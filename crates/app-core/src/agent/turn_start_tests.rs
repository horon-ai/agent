//! Every turn path records the message it starts with as the turn's start
//! (`StoredMessage::turn_start`), so a v2 replay keys each turn's ids by it.

use super::*;
use crate::session::StoredDisplayRole;

struct AnswerProvider;

#[async_trait::async_trait]
impl Provider for AnswerProvider {
    async fn complete(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(StreamEvent::TextDelta("answer".into())),
            Ok(StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".into()),
            }),
        ])))
    }

    fn name(&self) -> &str {
        "turn-start-test"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(AnswerProvider)
    }
}

#[tokio::test]
async fn every_turn_path_marks_its_starting_message() {
    let _home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(AnswerProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    // `horon-agent run --resume`, debug and swarm turns, ambient resume, and
    // the daemon's streaming turn.
    agent.run_once("headless").await.unwrap();
    agent.run_once_capture("captured").await.unwrap();
    agent
        .run_once_capture_with_display_role("ambient", Some(StoredDisplayRole::System))
        .await
        .unwrap();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    agent
        .run_once_streaming_mpsc("streamed", vec![], None, tx)
        .await
        .unwrap();

    let marked: Vec<String> = agent
        .session
        .active_context()
        .iter()
        .filter(|message| message.turn_start)
        .map(|message| message.content_preview())
        .collect();
    assert_eq!(marked, ["headless", "captured", "ambient", "streamed"]);
    assert!(
        agent
            .session
            .active_context()
            .iter()
            .filter(|message| message.role == Role::Assistant)
            .all(|message| !message.turn_start),
        "an answer starts no turn"
    );

    let history = agent.get_history(true);
    let starts: Vec<(&str, &str)> = history
        .iter()
        .filter(|row| row.turn_start)
        .map(|row| (row.role.as_str(), row.content.as_str()))
        .collect();
    assert_eq!(
        starts,
        [
            ("user", "headless"),
            ("user", "captured"),
            ("system", "ambient"),
            ("user", "streamed")
        ]
    );
    assert!(agent.get_history(false).iter().all(|row| !row.turn_start));
}
