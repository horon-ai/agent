use anyhow::Result;
use std::io::{self, Write};
use std::sync::{Arc, OnceLock};

use crate::auth;
use crate::message::{Message, ToolDefinition};
use crate::provider;
use crate::provider::EventStream;
use crate::provider::Provider;
use crate::provider_catalog::{
    LoginProviderDescriptor, LoginProviderTarget, OpenAiCompatibleProfile, force_apply_openai_compatible_profile_env,
    resolve_login_selection, resolve_openai_compatible_profile,
};
use crate::tool;

use super::login::run_login_provider;
use super::output;

pub(crate) use crate::external_auth::maybe_run_external_auth_auto_import_flow;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ProviderChoice {
    HoronAgent,
    Openai,
    #[value(alias = "openai-key", alias = "openai-apikey", alias = "openai-platform")]
    OpenaiApi,
    #[value(alias = "azure-openai", alias = "aoai")]
    Azure,
    #[value(alias = "opencode-zen", alias = "zen")]
    Opencode,
    #[value(alias = "opencodego")]
    OpencodeGo,
    #[value(alias = "z.ai", alias = "z-ai", alias = "zai-coding")]
    Zai,
    #[value(
        alias = "kimi-code",
        alias = "kimi-coding",
        alias = "kimi-coding-plan",
        alias = "kimi-for-coding",
        alias = "moonshot-coding"
    )]
    Kimi,
    #[value(alias = "302.ai")]
    Ai302,
    Baseten,
    #[value(alias = "conifer-api")]
    Conifer,
    Cortecs,
    #[value(alias = "cgc", alias = "comtegra-gpu-cloud")]
    Comtegra,
    Deepseek,
    #[value(alias = "fpt-ai", alias = "fptcloud", alias = "fpt-cloud")]
    Fpt,
    Firmware,
    #[value(alias = "hugging-face", alias = "hf")]
    HuggingFace,
    #[value(alias = "moonshot")]
    MoonshotAi,
    Nebius,
    Scaleway,
    Stackit,
    Groq,
    #[value(alias = "mistralai")]
    Mistral,
    #[value(alias = "pplx")]
    Perplexity,
    #[value(alias = "together", alias = "together-ai")]
    TogetherAi,
    #[value(alias = "deep-infra")]
    Deepinfra,
    #[value(alias = "fireworks-ai", alias = "fireworks.ai")]
    Fireworks,
    #[value(alias = "novita-ai", alias = "novita.ai")]
    Novita,
    #[value(alias = "minimax-ai", alias = "minimaxi")]
    Minimax,
    #[value(alias = "x.ai", alias = "x-ai", alias = "grok")]
    Xai,
    /// Grok Build subscription via the authenticated Grok CLI ACP transport.
    #[value(name = "grok-build")]
    GrokBuild,
    #[value(alias = "nvidia", alias = "nim")]
    NvidiaNim,
    #[value(alias = "xiaomi", alias = "mimo", alias = "xiaomi-mimo-api")]
    XiaomiMimo,
    #[value(
        alias = "meta",
        alias = "muse",
        alias = "muse-spark",
        alias = "meta-model-api",
        alias = "meta-ai"
    )]
    MetaMuse,
    #[value(alias = "celeris-ai", alias = "celeris1", alias = "celeris-1")]
    Celeris,
    YoloAuto,
    #[value(alias = "lm-studio")]
    Lmstudio,
    Ollama,
    Chutes,
    #[value(alias = "cerebrascode", alias = "cerberascode")]
    Cerebras,
    #[value(alias = "belvedir.ai", alias = "belvedir-ai")]
    Belvedir,
    #[value(alias = "orca-router")]
    Orcarouter,
    #[value(
        alias = "bailian",
        alias = "aliyun-bailian",
        alias = "coding-plan",
        alias = "alibaba-coding"
    )]
    AlibabaCodingPlan,
    #[value(alias = "compat", alias = "custom")]
    OpenaiCompatible,
    Auto,
}

impl ProviderChoice {
    #[allow(deprecated)]
    pub fn as_arg_value(&self) -> &'static str {
        match self {
            Self::HoronAgent => "horon-agent",
            Self::Openai => "openai",
            Self::OpenaiApi => "openai-api",
            Self::Azure => "azure",
            Self::Opencode => "opencode",
            Self::OpencodeGo => "opencode-go",
            Self::Zai => "zai",
            Self::Kimi => "kimi",
            Self::Ai302 => "302ai",
            Self::Baseten => "baseten",
            Self::Conifer => "conifer",
            Self::Cortecs => "cortecs",
            Self::Comtegra => "comtegra",
            Self::Deepseek => "deepseek",
            Self::Fpt => "fpt",
            Self::Firmware => "firmware",
            Self::HuggingFace => "huggingface",
            Self::MoonshotAi => "moonshotai",
            Self::Nebius => "nebius",
            Self::Scaleway => "scaleway",
            Self::Stackit => "stackit",
            Self::Groq => "groq",
            Self::Mistral => "mistral",
            Self::Perplexity => "perplexity",
            Self::TogetherAi => "togetherai",
            Self::Deepinfra => "deepinfra",
            Self::Fireworks => "fireworks",
            Self::Novita => "novita",
            Self::Minimax => "minimax",
            Self::Xai => "xai",
            Self::GrokBuild => "grok-build",
            Self::NvidiaNim => "nvidia-nim",
            Self::XiaomiMimo => "xiaomi-mimo",
            Self::MetaMuse => "meta-muse",
            Self::Celeris => "celeris",
            Self::YoloAuto => "yolo-auto",
            Self::Lmstudio => "lmstudio",
            Self::Ollama => "ollama",
            Self::Chutes => "chutes",
            Self::Cerebras => "cerebras",
            Self::Belvedir => "belvedir",
            Self::Orcarouter => "orcarouter",
            Self::AlibabaCodingPlan => "alibaba-coding-plan",
            Self::OpenaiCompatible => "openai-compatible",
            Self::Auto => "auto",
        }
    }
}

#[allow(deprecated)]
const PROVIDER_CHOICE_LOGIN_PROVIDERS: &[(ProviderChoice, LoginProviderDescriptor)] = &[
    (
        ProviderChoice::HoronAgent,
        crate::provider_catalog::HORON_AGENT_LOGIN_PROVIDER,
    ),
    (ProviderChoice::Openai, crate::provider_catalog::OPENAI_LOGIN_PROVIDER),
    (
        ProviderChoice::OpenaiApi,
        crate::provider_catalog::OPENAI_API_LOGIN_PROVIDER,
    ),
    (ProviderChoice::Azure, crate::provider_catalog::AZURE_LOGIN_PROVIDER),
    (
        ProviderChoice::Opencode,
        crate::provider_catalog::OPENCODE_LOGIN_PROVIDER,
    ),
    (
        ProviderChoice::OpencodeGo,
        crate::provider_catalog::OPENCODE_GO_LOGIN_PROVIDER,
    ),
    (ProviderChoice::Zai, crate::provider_catalog::ZAI_LOGIN_PROVIDER),
    (ProviderChoice::Kimi, crate::provider_catalog::KIMI_LOGIN_PROVIDER),
    (ProviderChoice::Ai302, crate::provider_catalog::AI302_LOGIN_PROVIDER),
    (ProviderChoice::Baseten, crate::provider_catalog::BASETEN_LOGIN_PROVIDER),
    (ProviderChoice::Conifer, crate::provider_catalog::CONIFER_LOGIN_PROVIDER),
    (ProviderChoice::Cortecs, crate::provider_catalog::CORTECS_LOGIN_PROVIDER),
    (
        ProviderChoice::Comtegra,
        crate::provider_catalog::COMTEGRA_LOGIN_PROVIDER,
    ),
    (
        ProviderChoice::Deepseek,
        crate::provider_catalog::DEEPSEEK_LOGIN_PROVIDER,
    ),
    (ProviderChoice::Fpt, crate::provider_catalog::FPT_LOGIN_PROVIDER),
    (
        ProviderChoice::Firmware,
        crate::provider_catalog::FIRMWARE_LOGIN_PROVIDER,
    ),
    (
        ProviderChoice::HuggingFace,
        crate::provider_catalog::HUGGING_FACE_LOGIN_PROVIDER,
    ),
    (
        ProviderChoice::MoonshotAi,
        crate::provider_catalog::MOONSHOT_LOGIN_PROVIDER,
    ),
    (ProviderChoice::Nebius, crate::provider_catalog::NEBIUS_LOGIN_PROVIDER),
    (
        ProviderChoice::Scaleway,
        crate::provider_catalog::SCALEWAY_LOGIN_PROVIDER,
    ),
    (ProviderChoice::Stackit, crate::provider_catalog::STACKIT_LOGIN_PROVIDER),
    (ProviderChoice::Groq, crate::provider_catalog::GROQ_LOGIN_PROVIDER),
    (ProviderChoice::Mistral, crate::provider_catalog::MISTRAL_LOGIN_PROVIDER),
    (
        ProviderChoice::Perplexity,
        crate::provider_catalog::PERPLEXITY_LOGIN_PROVIDER,
    ),
    (
        ProviderChoice::TogetherAi,
        crate::provider_catalog::TOGETHER_AI_LOGIN_PROVIDER,
    ),
    (
        ProviderChoice::Deepinfra,
        crate::provider_catalog::DEEPINFRA_LOGIN_PROVIDER,
    ),
    (
        ProviderChoice::Fireworks,
        crate::provider_catalog::FIREWORKS_LOGIN_PROVIDER,
    ),
    (ProviderChoice::Novita, crate::provider_catalog::NOVITA_LOGIN_PROVIDER),
    (ProviderChoice::Minimax, crate::provider_catalog::MINIMAX_LOGIN_PROVIDER),
    (ProviderChoice::Xai, crate::provider_catalog::XAI_LOGIN_PROVIDER),
    (
        ProviderChoice::GrokBuild,
        crate::provider_catalog::GROK_BUILD_LOGIN_PROVIDER,
    ),
    (
        ProviderChoice::NvidiaNim,
        crate::provider_catalog::NVIDIA_NIM_LOGIN_PROVIDER,
    ),
    (
        ProviderChoice::XiaomiMimo,
        crate::provider_catalog::XIAOMI_MIMO_LOGIN_PROVIDER,
    ),
    (
        ProviderChoice::MetaMuse,
        crate::provider_catalog::META_MUSE_LOGIN_PROVIDER,
    ),
    (ProviderChoice::Celeris, crate::provider_catalog::CELERIS_LOGIN_PROVIDER),
    (
        ProviderChoice::YoloAuto,
        crate::provider_catalog::YOLO_AUTO_LOGIN_PROVIDER,
    ),
    (
        ProviderChoice::Lmstudio,
        crate::provider_catalog::LMSTUDIO_LOGIN_PROVIDER,
    ),
    (ProviderChoice::Ollama, crate::provider_catalog::OLLAMA_LOGIN_PROVIDER),
    (ProviderChoice::Chutes, crate::provider_catalog::CHUTES_LOGIN_PROVIDER),
    (
        ProviderChoice::Cerebras,
        crate::provider_catalog::CEREBRAS_LOGIN_PROVIDER,
    ),
    (
        ProviderChoice::Belvedir,
        crate::provider_catalog::BELVEDIR_LOGIN_PROVIDER,
    ),
    (
        ProviderChoice::Orcarouter,
        crate::provider_catalog::ORCAROUTER_LOGIN_PROVIDER,
    ),
    (
        ProviderChoice::AlibabaCodingPlan,
        crate::provider_catalog::ALIBABA_CODING_PLAN_LOGIN_PROVIDER,
    ),
    (
        ProviderChoice::OpenaiCompatible,
        crate::provider_catalog::OPENAI_COMPAT_LOGIN_PROVIDER,
    ),
];

pub fn login_provider_choice_mappings() -> &'static [(ProviderChoice, LoginProviderDescriptor)] {
    PROVIDER_CHOICE_LOGIN_PROVIDERS
}

/// `HORON_AGENT_OPENROUTER_MODEL` as set by the user when the process started.
///
/// Provider init clears and rewrites the `HORON_AGENT_OPENROUTER_*` env slots for
/// whichever runtime it activates, so by the time the OpenRouter runtime reads
/// the variable a user-supplied value is already gone. Snapshot it first.
static USER_OPENROUTER_MODEL: OnceLock<Option<String>> = OnceLock::new();

/// Called once from `startup::run` before any provider init. Entry points
/// that skip startup (unit tests, embedded flows) never consult the snapshot.
pub fn capture_user_openrouter_model_env() {
    let _ = USER_OPENROUTER_MODEL.set(
        std::env::var("HORON_AGENT_OPENROUTER_MODEL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
    );
}

fn user_openrouter_model_env() -> Option<&'static str> {
    USER_OPENROUTER_MODEL.get()?.as_deref()
}

/// Model to use when the caller passed no `--model`: the user's
/// `HORON_AGENT_OPENROUTER_MODEL`, but only for runtimes that variable targets
/// (OpenRouter and OpenAI-compatible profiles).
fn env_model_for_choice(choice: &ProviderChoice) -> Option<&'static str> {
    env_model_for_choice_from(choice, user_openrouter_model_env())
}

fn env_model_for_choice_from<'a>(choice: &ProviderChoice, env_model: Option<&'a str>) -> Option<&'a str> {
    if profile_for_choice(choice).is_some() {
        env_model
    } else {
        None
    }
}

pub fn profile_for_choice(choice: &ProviderChoice) -> Option<OpenAiCompatibleProfile> {
    match login_provider_for_choice(choice)?.target {
        LoginProviderTarget::OpenAiCompatible(profile) => Some(profile),
        _ => None,
    }
}

#[allow(deprecated)]
pub fn login_provider_for_choice(choice: &ProviderChoice) -> Option<LoginProviderDescriptor> {
    PROVIDER_CHOICE_LOGIN_PROVIDERS
        .iter()
        .find(|(candidate, _)| candidate == choice)
        .map(|(_, provider)| *provider)
}

#[allow(deprecated)]
pub fn choice_for_login_provider(provider: LoginProviderDescriptor) -> Option<ProviderChoice> {
    PROVIDER_CHOICE_LOGIN_PROVIDERS
        .iter()
        .find(|(_, candidate)| candidate.id == provider.id)
        .map(|(choice, _)| *choice)
}

pub fn prompt_login_provider_selection(
    providers: &[LoginProviderDescriptor],
    heading: &str,
) -> Result<LoginProviderDescriptor> {
    prompt_login_provider_selection_optional(providers, heading)?
        .ok_or_else(|| anyhow::anyhow!("Login skipped. Run `horon-agent login` when you're ready to authenticate."))
}

pub fn prompt_login_provider_selection_optional(
    providers: &[LoginProviderDescriptor],
    heading: &str,
) -> Result<Option<LoginProviderDescriptor>> {
    let status = auth::AuthStatus::check_fast();
    eprint!("{}", render_login_provider_selection_menu(heading, providers, &status));
    eprint!("\nEnter 1-{}, provider name, or Enter=skip: ", providers.len());
    io::stderr().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    parse_login_provider_selection_input(&input, providers)
}

pub fn parse_login_provider_selection_input(
    input: &str,
    providers: &[LoginProviderDescriptor],
) -> Result<Option<LoginProviderDescriptor>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let normalized = trimmed.to_ascii_lowercase();
    if matches!(normalized.as_str(), "s" | "skip" | "q" | "quit" | "cancel" | "none") {
        return Ok(None);
    }

    resolve_login_selection(trimmed, providers).map(Some).ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid choice '{}'. Enter 1-{}, a provider name, or 'skip'.",
            trimmed,
            providers.len()
        )
    })
}

pub fn render_login_provider_selection_menu(
    heading: &str,
    providers: &[LoginProviderDescriptor],
    status: &auth::AuthStatus,
) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(out, "{heading}");
    let _ = writeln!(out);

    let detected = providers
        .iter()
        .copied()
        .filter_map(|provider| {
            let assessment = status.assessment_for_provider(provider);
            (assessment.state != auth::AuthState::NotConfigured).then(|| {
                format!(
                    "  - {}: {}",
                    provider.display_name,
                    login_provider_detection_detail(provider, &assessment)
                )
            })
        })
        .collect::<Vec<_>>();

    if detected.is_empty() {
        let _ = writeln!(out, "Autodetected auth: none found yet.");
    } else {
        let _ = writeln!(out, "Autodetected auth:");
        for line in detected {
            let _ = writeln!(out, "{line}");
        }
    }

    let _ = writeln!(out);
    for (index, provider) in providers.iter().copied().enumerate() {
        let assessment = status.assessment_for_provider(provider);
        let _ = writeln!(
            out,
            "  {}. {:<22} [{:<15}] - {}",
            index + 1,
            provider.display_name,
            login_provider_state_badge(provider, assessment.state),
            provider.menu_detail
        );
    }

    let recommended = providers
        .iter()
        .filter(|provider| provider.recommended)
        .map(|provider| provider.display_name)
        .collect::<Vec<_>>();
    if !recommended.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "  Recommended if you have a subscription: {}.",
            recommended.join(", ")
        );
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "  Skip: press Enter, or type `skip`.");
    out
}

fn login_provider_state_badge(provider: LoginProviderDescriptor, state: auth::AuthState) -> &'static str {
    match state {
        auth::AuthState::Available => {
            if matches!(provider.target, LoginProviderTarget::AutoImport) {
                "detected"
            } else {
                "configured"
            }
        }
        auth::AuthState::Expired => "needs attention",
        auth::AuthState::NotConfigured => "not configured",
    }
}

fn login_provider_detection_detail(
    provider: LoginProviderDescriptor,
    assessment: &auth::ProviderAuthAssessment,
) -> String {
    match assessment.state {
        auth::AuthState::Available => {
            let prefix = if matches!(provider.target, LoginProviderTarget::AutoImport) {
                "detected"
            } else {
                "configured"
            };
            format!("{}: {}", prefix, assessment.method_detail)
        }
        auth::AuthState::Expired => format!("needs attention: {}", assessment.method_detail),
        auth::AuthState::NotConfigured => "not configured".to_string(),
    }
}

fn disable_subscription_runtime_mode() {
    crate::subscription_catalog::clear_runtime_env();
}

pub fn apply_login_provider_profile_env(provider: LoginProviderDescriptor) {
    // #712: the arms below clear an explicitly selected named profile, which
    // made auth-test probe (and false-negative) the generic compatible slot.
    if std::env::var_os("HORON_AGENT_NAMED_PROVIDER_PROFILE").is_some() {
        return;
    }
    match provider.target {
        LoginProviderTarget::OpenAiCompatible(profile) => {
            force_apply_openai_compatible_profile_env(Some(profile));
            // Bootstrap login still spawns the daemon with `--provider auto`. Mark the
            // just-selected compatible provider as active so the child process does
            // not clear these inherited runtime vars before credential detection.
            crate::env::set_var("HORON_AGENT_PROVIDER_PROFILE_ACTIVE", "1");
        }
        LoginProviderTarget::AutoImport => {}
        _ => {
            // A later non-compatible login selection must not inherit a stale
            // compatible-provider profile from an earlier bootstrap/login path.
            force_apply_openai_compatible_profile_env(None);
        }
    }
}

fn resolved_profile_default_model(profile: OpenAiCompatibleProfile) -> Option<String> {
    resolve_openai_compatible_profile(profile).default_model
}

pub async fn login_and_bootstrap_provider(
    provider: LoginProviderDescriptor,
    account_label: Option<&str>,
) -> Result<Arc<dyn provider::Provider>> {
    run_login_provider(provider, account_label, crate::cli::login::LoginOptions::default()).await?;
    eprintln!();

    let runtime: Arc<dyn provider::Provider> = match provider.target {
        LoginProviderTarget::AutoImport => {
            disable_subscription_runtime_mode();
            Arc::new(provider::MultiProvider::new())
        }
        LoginProviderTarget::HoronAgent => Arc::new(provider::horon_agent::HoronAgentProvider::new()),
        LoginProviderTarget::OpenAi => {
            disable_subscription_runtime_mode();
            Arc::new(provider::MultiProvider::new())
        }
        LoginProviderTarget::OpenAiApiKey => {
            disable_subscription_runtime_mode();
            Arc::new(provider::MultiProvider::new())
        }
        LoginProviderTarget::Azure => {
            disable_subscription_runtime_mode();
            Arc::new(provider::MultiProvider::new())
        }
        LoginProviderTarget::OpenAiCompatible(profile) => {
            disable_subscription_runtime_mode();
            force_apply_openai_compatible_profile_env(Some(profile));
            let multi = provider::MultiProvider::from_openai_compatible_profile(profile)?;
            let resolved = resolve_openai_compatible_profile(profile);
            if let Some(model) = resolved.default_model.as_deref() {
                let _ = multi.set_model(model);
            }
            Arc::new(multi)
        }
        _ => anyhow::bail!("That provider is not available in this build"),
    };

    Ok(runtime)
}

pub async fn init_provider(choice: &ProviderChoice, model: Option<&str>) -> Result<Arc<dyn provider::Provider>> {
    init_provider_with_options(choice, model, true, true).await
}

/// Provider init for one-shot `horon-agent run`. A requested model that cannot be applied is an
/// error: silently answering with a different model is worse than failing for a scripted run.
pub async fn init_provider_for_run(
    choice: &ProviderChoice,
    model: Option<&str>,
) -> Result<Arc<dyn provider::Provider>> {
    init_provider_with_options(choice, model, false, true).await
}

pub async fn init_provider_for_serve(
    choice: &ProviderChoice,
    model: Option<&str>,
) -> Result<Arc<dyn provider::Provider>> {
    init_provider_for_control_plane(choice, model, true).await
}

pub async fn init_provider_for_diagnostics(
    choice: &ProviderChoice,
    model: Option<&str>,
) -> Result<Arc<dyn provider::Provider>> {
    init_provider_for_control_plane(choice, model, false).await
}

/// A server/control-plane launch stays up without a model provider so ACP
/// clients can still authenticate and be told that no model is usable. Model
/// calls fail with the setup instructions instead of pretending to work.
struct UnconfiguredProvider {
    reason: String,
}

#[async_trait::async_trait]
impl provider::Provider for UnconfiguredProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        anyhow::bail!("{}", self.reason)
    }

    fn name(&self) -> &str {
        "unconfigured"
    }

    fn model(&self) -> String {
        String::new()
    }

    fn unavailable_reason(&self) -> Option<String> {
        Some(self.reason.clone())
    }

    fn set_model(&self, _model: &str) -> Result<()> {
        anyhow::bail!("{}", self.reason)
    }

    fn fork(&self) -> Arc<dyn provider::Provider> {
        Arc::new(Self {
            reason: self.reason.clone(),
        })
    }
}

async fn init_provider_for_control_plane(
    choice: &ProviderChoice,
    model: Option<&str>,
    show_init_messages: bool,
) -> Result<Arc<dyn provider::Provider>> {
    match init_provider_with_options(choice, model, show_init_messages, false).await {
        Ok(provider) => Ok(provider),
        Err(error) if matches!(choice, ProviderChoice::Auto) && auto_provider_is_unconfigured() => {
            let provider: Arc<dyn provider::Provider> = Arc::new(UnconfiguredProvider {
                reason: error.to_string(),
            });
            crate::provider::set_active_provider(Arc::clone(&provider));
            Ok(provider)
        }
        Err(error) => Err(error),
    }
}

fn auto_provider_is_unconfigured() -> bool {
    named_profile_from_env().is_none()
        && crate::config::config()
            .provider
            .default_provider
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
        && std::env::var("HORON_AGENT_OPENROUTER_CACHE_NAMESPACE")
            .ok()
            .as_deref()
            .and_then(builtin_profile_by_id)
            .is_none()
}

pub async fn init_provider_quiet(choice: &ProviderChoice, model: Option<&str>) -> Result<Arc<dyn provider::Provider>> {
    init_provider_with_options(choice, model, false, true).await
}

/// Auth validation sends model and tool-smoke requests, so it goes through the
/// same provider boundary as a real run.
pub async fn init_provider_for_validation(
    choice: &ProviderChoice,
    model: Option<&str>,
) -> Result<Arc<dyn provider::Provider>> {
    init_provider_with_options(choice, model, false, false).await
}

/// The refusal a built-in `--provider` gets when it has no direct
/// OpenAI-compatible profile.
///
/// Only direct `/chat/completions` profiles are supported: a provider name that
/// names no such profile cannot be turned into a model path.
fn builtin_provider_refusal(choice: &ProviderChoice) -> anyhow::Error {
    anyhow::anyhow!(
        "'--provider {}' is not available: model requests go directly to an OpenAI-compatible /chat/completions endpoint. \
         Configure a [providers.<name>] profile with type = \"openai-compatible\" in config.toml, then select it with \
         --provider-profile <name> or [provider] default_provider = \"<name>\". Built-in OpenAI-compatible profiles such as \
         openai-compatible, deepseek, groq and ollama can also be selected with --provider <name>.",
        choice.as_arg_value()
    )
}

/// The error a launch gets when nothing names a model endpoint.
fn unconfigured_provider_error() -> anyhow::Error {
    anyhow::anyhow!(
        "No model provider is configured. Add a [providers.<name>] profile with type = \"openai-compatible\" to config.toml \
         (set base_url, default_model, and either api_key_env or auth = \"none\"), then select it with \
         --provider-profile <name> or [provider] default_provider = \"<name>\". For a local server use e.g. \
         base_url = \"http://127.0.0.1:11434/v1\" and auth = \"none\"."
    )
}

fn builtin_profile_by_id(id: &str) -> Option<OpenAiCompatibleProfile> {
    let id = id.trim();
    if id.is_empty() {
        return None;
    }
    crate::provider_catalog::openai_compatible_profiles()
        .iter()
        .copied()
        .find(|profile| profile.id.eq_ignore_ascii_case(id))
}

fn named_profile_from_env() -> Option<String> {
    [
        "HORON_AGENT_PROVIDER_PROFILE_NAME",
        "HORON_AGENT_NAMED_PROVIDER_PROFILE",
    ]
    .iter()
    .find_map(|key| {
        std::env::var(key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

/// Build the runtime for one named `[providers.<name>]` profile.
fn provider_for_named_profile(
    name: &str,
    model: Option<&str>,
    strict_model: bool,
) -> Result<Arc<dyn provider::Provider>> {
    let runtime = provider::MultiProvider::from_named_provider_profile(name)?;
    let runtime: Arc<dyn provider::Provider> = Arc::new(runtime);
    if let Some(model) = model
        && let Err(error) = runtime.set_model(model)
    {
        if strict_model {
            anyhow::bail!("Failed to select model '{model}': {error}");
        }
        output::stderr_info(format!("Warning: failed to set model '{model}': {error}"));
    }
    Ok(runtime)
}

/// Build the runtime for one built-in OpenAI-compatible profile.
fn provider_for_builtin_profile(
    profile: OpenAiCompatibleProfile,
    model: Option<&str>,
    strict_model: bool,
) -> Result<Arc<dyn provider::Provider>> {
    force_apply_openai_compatible_profile_env(Some(profile));
    let runtime = provider::MultiProvider::from_openai_compatible_profile(profile)?;
    let runtime: Arc<dyn provider::Provider> = Arc::new(runtime);
    let model = model
        .map(str::to_owned)
        .or_else(|| user_openrouter_model_env().map(str::to_owned))
        .or_else(|| resolved_profile_default_model(profile));
    if let Some(model) = model.as_deref()
        && let Err(error) = runtime.set_model(model)
    {
        if strict_model {
            anyhow::bail!("Failed to select model '{model}': {error}");
        }
        output::stderr_info(format!("Warning: failed to set model '{model}': {error}"));
    }
    Ok(runtime)
}

fn provider_for_configured_name(
    name: &str,
    model: Option<&str>,
    strict_model: bool,
) -> Result<Arc<dyn provider::Provider>> {
    if crate::config::config().providers.contains_key(name) {
        return provider_for_named_profile(name, model, strict_model);
    }
    if let Some(profile) = builtin_profile_by_id(name) {
        return provider_for_builtin_profile(profile, model, strict_model);
    }
    anyhow::bail!(
        "Unknown provider profile '{}'. Add [providers.{}] with type = \"openai-compatible\" to config.toml, \
         or use a built-in OpenAI-compatible profile id.",
        name,
        name
    )
}

/// `strict_model`: a requested model that cannot be applied is an error instead of a warning.
async fn init_provider_with_options(
    choice: &ProviderChoice,
    model: Option<&str>,
    show_init_messages: bool,
    strict_model: bool,
) -> Result<Arc<dyn provider::Provider>> {
    let init_notice = |message: &str| {
        if show_init_messages {
            output::stderr_info(message);
        }
    };

    // An explicit `--provider-profile` (projected as PROFILE_NAME by dispatch) or
    // a login/bootstrap selection always names the profile to use.
    if let Some(profile_name) = named_profile_from_env() {
        crate::provider_catalog::apply_named_provider_profile_env(&profile_name)?;
        crate::env::set_var("HORON_AGENT_PROVIDER_PROFILE_ACTIVE", "1");
        init_notice(&format!("Using provider profile '{}'", profile_name));
        return provider_for_named_profile(&profile_name, model, strict_model);
    }

    match choice {
        ProviderChoice::Auto => {
            if let Some(default) = crate::config::config()
                .provider
                .default_provider
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                init_notice(&format!("Using configured default provider '{}'", default));
                return provider_for_configured_name(default, model, strict_model);
            }
            let projected = std::env::var("HORON_AGENT_OPENROUTER_CACHE_NAMESPACE").ok();
            if let Some(profile) = projected.as_deref().and_then(builtin_profile_by_id) {
                init_notice(&format!("Using {} via the OpenAI-compatible API", profile.display_name));
                return provider_for_builtin_profile(profile, model, strict_model);
            }
            Err(unconfigured_provider_error())
        }
        ProviderChoice::OpenaiCompatible => {
            let profile = profile_for_choice(choice).ok_or_else(|| builtin_provider_refusal(choice))?;
            init_notice(&format!(
                "Using {} via the OpenAI-compatible API",
                resolve_openai_compatible_profile(profile).display_name
            ));
            let model = model.or_else(|| env_model_for_choice(choice));
            provider_for_builtin_profile(profile, model, strict_model)
        }
        _ => match profile_for_choice(choice) {
            Some(profile) => {
                if std::env::var_os("HORON_AGENT_NAMED_PROVIDER_PROFILE").is_none() {
                    force_apply_openai_compatible_profile_env(Some(profile));
                }
                init_notice(&format!(
                    "Using {} via the OpenAI-compatible API",
                    resolve_openai_compatible_profile(profile).display_name
                ));
                let model = model.or_else(|| env_model_for_choice(choice));
                provider_for_builtin_profile(profile, model, strict_model)
            }
            None => Err(builtin_provider_refusal(choice)),
        },
    }
}

pub async fn init_provider_and_registry(
    choice: &ProviderChoice,
    model: Option<&str>,
) -> Result<(Arc<dyn provider::Provider>, tool::Registry)> {
    let provider = init_provider(choice, model).await?;
    let registry = tool::Registry::new(provider.clone()).await;
    Ok((provider, registry))
}

pub async fn init_provider_and_registry_for_validation(
    choice: &ProviderChoice,
    model: Option<&str>,
) -> Result<(Arc<dyn provider::Provider>, tool::Registry)> {
    let provider = init_provider_for_validation(choice, model).await?;
    let registry = tool::Registry::new(provider.clone()).await;
    Ok((provider, registry))
}

#[cfg(test)]
#[path = "provider_init_tests.rs"]
mod tests;
