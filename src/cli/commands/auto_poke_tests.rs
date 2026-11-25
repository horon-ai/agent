use super::super::tests::SavedEnv;
use super::*;
use crate::message::{Message, StreamEvent, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc as tokio_mpsc;
use tokio_stream::wrappers::ReceiverStream;

#[test]
fn run_auto_poke_defaults_to_bounded_max_turns() {
    let _guard = crate::storage::lock_test_env();
    let _saved = SavedEnv::capture(&["HORON_AGENT_RUN_AUTO_POKE_MAX_TURNS"]);
    crate::env::remove_var("HORON_AGENT_RUN_AUTO_POKE_MAX_TURNS");
    assert_eq!(run_command_auto_poke_max_turns(), Some(DEFAULT_RUN_AUTO_POKE_MAX_TURNS));

    crate::env::set_var("HORON_AGENT_RUN_AUTO_POKE_MAX_TURNS", "3");
    assert_eq!(run_command_auto_poke_max_turns(), Some(3));
}

#[tokio::test]
async fn run_auto_poke_stops_on_identical_incomplete_todos() {
    let _guard = crate::storage::lock_test_env();
    let _saved = SavedEnv::capture(&[
        "HORON_HOME",
        "HORON_AGENT_RUN_AUTO_POKE",
        "HORON_AGENT_RUN_AUTO_POKE_MAX_TURNS",
    ]);
    let temp = tempfile::tempdir().expect("tempdir");
    crate::env::set_var("HORON_HOME", temp.path());
    crate::env::set_var("HORON_AGENT_RUN_AUTO_POKE", "1");
    crate::env::remove_var("HORON_AGENT_RUN_AUTO_POKE_MAX_TURNS");

    struct CountingProvider(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    #[async_trait]
    impl Provider for CountingProvider {
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> anyhow::Result<EventStream> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (tx, rx) = tokio_mpsc::channel(4);
            tokio::spawn(async move {
                let _ = tx.send(Ok(StreamEvent::TextDelta("still pending".to_string()))).await;
                let _ = tx
                    .send(Ok(StreamEvent::MessageEnd {
                        stop_reason: Some("end_turn".to_string()),
                    }))
                    .await;
            });
            Ok(Box::pin(ReceiverStream::new(rx)))
        }
        fn name(&self) -> &'static str {
            "mock"
        }
        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(Self(self.0.clone()))
        }
    }

    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(CountingProvider(calls.clone()));
    let registry = Registry::new(provider.clone()).await;
    let mut agent = crate::agent::Agent::for_memory_only_run(provider, registry);
    crate::todo::save_todos(
        agent.session_id(),
        &[crate::todo::TodoItem {
            id: "1".to_string(),
            content: "finish task".to_string(),
            status: "pending".to_string(),
            priority: "high".to_string(),
            ..Default::default()
        }],
    )
    .expect("save todos");

    let output = run_single_message_command_capture_with_auto_poke(&mut agent, "go")
        .await
        .expect("stalled auto_poke finishes cleanly");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(output.contains("Stopped auto-poke:"), "{output}");
}
