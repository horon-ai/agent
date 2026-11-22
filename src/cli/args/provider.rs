use clap::{Subcommand, ValueEnum};

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

#[derive(Subcommand, Debug)]
pub(crate) enum ProviderCommand {
    /// List provider IDs you can pass to -p/--provider
    List {
        /// Emit JSON instead of plain text
        #[arg(long)]
        json: bool,
    },

    /// Show each configured profile's credential source and local status
    ///
    /// Reads local configuration only. It never prints credential values or
    /// contacts a provider, so a profile with no resolvable credential is
    /// `unauthenticated` and one that uses no credential is `unknown`.
    Status {
        /// Emit JSON instead of plain text
        #[arg(long)]
        json: bool,
    },

    /// Show the currently requested and resolved provider selection
    Current {
        /// Emit JSON instead of plain text
        #[arg(long)]
        json: bool,
    },

    /// Show the failover order the configuration produces and why a step is excluded
    ///
    /// Read-only: sends no model request, changes no account, quota or provider
    /// selection, and prints no credential. Quota cooldowns live in a running
    /// daemon, so they are not shown.
    Failover {
        /// Emit JSON instead of plain text
        #[arg(long)]
        json: bool,
    },

    /// Check configured profiles: missing config, missing credential variable, and (with --check) reachability
    Diagnose {
        /// Contact each endpoint's model-list URL with the configured credentials
        #[arg(long)]
        check: bool,

        /// Emit JSON instead of plain text
        #[arg(long)]
        json: bool,
    },

    /// Add a named API provider profile
    Add {
        /// Profile name used with --provider-profile and config defaults, e.g. my-gateway
        name: String,

        /// API base URL including the version prefix, e.g. https://llm.example.com/v1. Each type appends its own path: /chat/completions, /responses, /messages, or /models/{model}:streamGenerateContent
        #[arg(long, alias = "api-base")]
        base_url: String,

        /// Default model id for this provider profile
        #[arg(short, long)]
        model: String,

        /// Optional model context window in tokens
        #[arg(long)]
        context_window: Option<usize>,

        /// Environment variable name that contains the API key
        #[arg(long, conflicts_with = "no_api_key")]
        api_key_env: Option<String>,

        /// API key value to store in horon-agent's private provider env file. Prefer --api-key-stdin for shell history safety.
        #[arg(long, conflicts_with_all = ["api_key_stdin", "no_api_key"])]
        api_key: Option<String>,

        /// Read the API key from stdin and store it in horon-agent's private provider env file
        #[arg(long, conflicts_with = "no_api_key")]
        api_key_stdin: bool,

        /// Configure the provider with no API key/authentication
        #[arg(long, conflicts_with_all = ["api_key", "api_key_stdin", "api_key_env"])]
        no_api_key: bool,

        /// Request/response shape of the endpoint: openai-compatible, openai-responses, anthropic-compatible, or gemini-compatible
        #[arg(long = "type", value_enum, default_value = "openai-compatible")]
        provider_type: ProviderTypeArg,

        /// Authentication style for the API key
        #[arg(long, value_enum)]
        auth: Option<ProviderAuthArg>,

        /// Header name when --auth api-key is used (default: api-key for the OpenAI types, x-api-key for anthropic-compatible, x-goog-api-key for gemini-compatible)
        #[arg(long)]
        auth_header: Option<String>,

        /// Private env file name under horon-agent's app config directory for stored API keys
        #[arg(long)]
        env_file: Option<String>,

        /// Make this profile the startup default provider/model
        #[arg(long, alias = "default")]
        set_default: bool,

        /// Replace an existing profile with the same name
        #[arg(long)]
        overwrite: bool,

        /// Allow provider-routing features for OpenRouter-style gateways
        #[arg(long)]
        provider_routing: bool,

        /// Fetch/list models from the provider's /models endpoint
        #[arg(long)]
        model_catalog: bool,

        /// Emit JSON instead of human-readable setup output
        #[arg(long)]
        json: bool,
    },
}
