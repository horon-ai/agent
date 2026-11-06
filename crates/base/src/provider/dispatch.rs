use super::*;

#[derive(Clone, Copy)]
pub(super) enum CompletionMode<'a> {
    Unified {
        system: &'a str,
    },
    Split {
        system_static: &'a str,
        system_dynamic: &'a str,
    },
}

impl CompletionMode<'_> {
    pub(super) fn log_suffix(self) -> &'static str {
        match self {
            CompletionMode::Unified { .. } => "",
            CompletionMode::Split { .. } => " (split)",
        }
    }
}

/// Error shown when no direct OpenAI-compatible runtime is configured.
fn missing_provider_error() -> anyhow::Error {
    anyhow::anyhow!(
        "No OpenAI-compatible provider is configured. Add [providers.<name>] with \
         type = \"openai-compatible\" to config.toml, then select it with --provider-profile <name> \
         or [provider] default_provider = \"<name>\"."
    )
}

impl MultiProvider {
    pub(super) async fn complete_on_provider(
        &self,
        provider: ActiveProvider,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
        resume_session_id: Option<&str>,
        context: Option<&ProviderRequestContext>,
    ) -> Result<EventStream> {
        self.reconcile_auth_if_provider_missing(provider);
        let Some(runtime) = self.execution_provider() else {
            return Err(missing_provider_error());
        };
        match context {
            Some(context) => {
                runtime
                    .complete_with_context(messages, tools, system, resume_session_id, context)
                    .await
            }
            None => runtime.complete(messages, tools, system, resume_session_id).await,
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "split dispatch threads the explicit request fields unchanged from the old signature plus the optional session identity"
    )]
    pub(super) async fn complete_split_on_provider(
        &self,
        provider: ActiveProvider,
        messages: &[Message],
        tools: &[ToolDefinition],
        system_static: &str,
        system_dynamic: &str,
        resume_session_id: Option<&str>,
        context: Option<&ProviderRequestContext>,
    ) -> Result<EventStream> {
        self.reconcile_auth_if_provider_missing(provider);
        let Some(runtime) = self.execution_provider() else {
            return Err(missing_provider_error());
        };
        match context {
            Some(context) => {
                runtime
                    .complete_split_with_context(
                        messages,
                        tools,
                        system_static,
                        system_dynamic,
                        resume_session_id,
                        context,
                    )
                    .await
            }
            None => {
                runtime
                    .complete_split(messages, tools, system_static, system_dynamic, resume_session_id)
                    .await
            }
        }
    }
}
