use super::*;
use crate::message::StreamEvent;
use std::sync::Mutex;

struct RecordingProvider {
    deferred_support: bool,
    seen: Arc<Mutex<Vec<ToolDefinition>>>,
}

#[async_trait::async_trait]
impl Provider for RecordingProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        *self.seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = tools.to_vec();
        Ok(Box::pin(futures::stream::iter(Vec::<Result<StreamEvent>>::new())))
    }

    fn name(&self) -> &str {
        "openai"
    }

    fn credential_mode(&self) -> CredentialMode {
        CredentialMode::ApiKey
    }

    fn supports_deferred_tools(&self) -> bool {
        self.deferred_support
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            deferred_support: self.deferred_support,
            seen: Arc::clone(&self.seen),
        })
    }
}

fn definition(name: &str, deferred: bool) -> ToolDefinition {
    let definition = ToolDefinition::new(name, format!("{name} tool"), serde_json::json!({"type": "object"}));
    if deferred { definition.deferred() } else { definition }
}

fn recording_openai(deferred_support: bool) -> (MultiProvider, Arc<Mutex<Vec<ToolDefinition>>>) {
    let provider = test_multi_provider_with_openai();
    let seen = Arc::new(Mutex::new(Vec::new()));
    *provider.openai.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(RecordingProvider {
        deferred_support,
        seen: Arc::clone(&seen),
    }));
    (provider, seen)
}

#[test]
fn a_provider_without_deferred_support_drops_deferred_definitions() {
    with_clean_provider_test_env(|| {
        enter_test_runtime().block_on(async {
            let (provider, seen) = recording_openai(false);
            assert!(
                !Provider::supports_deferred_tools(&provider),
                "this route is eager-only"
            );

            let tools = vec![
                definition("bash", false),
                definition("memory", true),
                definition("gmail", true),
            ];
            let _stream = provider
                .complete(&[], &tools, "system", None)
                .await
                .expect("the request must still build for an eager-only provider");

            let seen = seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
            assert_eq!(
                seen.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
                ["bash"],
                "deferred definitions never reach a provider that cannot load them"
            );
            assert!(seen.iter().all(|tool| !tool.defer_loading));
        });
    });
}

#[test]
fn a_provider_with_deferred_support_receives_the_flag_untouched() {
    with_clean_provider_test_env(|| {
        enter_test_runtime().block_on(async {
            let (provider, seen) = recording_openai(true);
            assert!(Provider::supports_deferred_tools(&provider));

            let tools = vec![definition("bash", false), definition("memory", true)];
            let _stream = provider
                .complete(&[], &tools, "system", None)
                .await
                .expect("the request builds");

            let seen = seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
            assert_eq!(seen.len(), 2, "a native provider keeps the whole catalog");
            let memory = seen.iter().find(|tool| tool.name == "memory").expect("memory");
            assert!(memory.defer_loading);
        });
    });
}
