//! Value enums for the `provider add` options.

use clap::ValueEnum;

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum ProviderAuthArg {
    /// Send the API key as Authorization: Bearer <key> (default for openai-compatible and openai-responses)
    Bearer,
    /// Send the API key in a header: api-key for the OpenAI wires, x-api-key for anthropic-compatible and x-goog-api-key for gemini-compatible
    ApiKey,
    /// Do not send authentication, useful for localhost model servers
    None,
}

/// Request/response shape of the endpoint a profile points at.
#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum ProviderTypeArg {
    /// OpenAI Chat Completions: POST <base-url>/chat/completions, Bearer auth by default
    #[value(name = "openai-compatible")]
    OpenAiCompatible,
    /// OpenAI Responses: POST <base-url>/responses, Bearer auth by default
    #[value(name = "openai-responses")]
    OpenAiResponses,
    /// Anthropic Messages: POST <base-url>/messages, x-api-key header by default
    #[value(name = "anthropic-compatible")]
    AnthropicCompatible,
    /// Gemini generateContent: POST <base-url>/models/{model}:streamGenerateContent?alt=sse, x-goog-api-key header by default
    #[value(name = "gemini-compatible")]
    GeminiCompatible,
}

impl From<ProviderTypeArg> for crate::config::NamedProviderType {
    fn from(value: ProviderTypeArg) -> Self {
        match value {
            ProviderTypeArg::OpenAiCompatible => Self::OpenAiCompatible,
            ProviderTypeArg::OpenAiResponses => Self::OpenAiResponses,
            ProviderTypeArg::AnthropicCompatible => Self::AnthropicCompatible,
            ProviderTypeArg::GeminiCompatible => Self::GeminiCompatible,
        }
    }
}
