use crate::protocol::{AuthChanged, CatalogNamespace, RuntimeProviderKey};
use crate::provider::ModelRoute;
use crate::provider::activation::{ProviderActivation, RuntimeProviderId};
use horon_agent_provider_core::ActiveProvider;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthActivationRequest {
    pub legacy_provider_hint: Option<String>,
    pub auth: Option<AuthChanged>,
}

impl AuthActivationRequest {
    pub fn new(legacy_provider_hint: Option<String>, auth: Option<AuthChanged>) -> Self {
        Self {
            legacy_provider_hint,
            auth,
        }
    }

    pub fn provider_id(&self) -> Option<String> {
        self.auth
            .as_ref()
            .map(|auth| auth.provider.as_str().to_string())
            .or_else(|| self.legacy_provider_hint.clone())
            .and_then(|provider| normalized_auth_provider_id(Some(provider.as_str())).map(str::to_string))
    }

    pub fn expected_runtime(&self) -> Option<&RuntimeProviderKey> {
        self.auth.as_ref().and_then(|auth| auth.expected_runtime.as_ref())
    }

    pub fn expected_catalog_namespace(&self) -> Option<&CatalogNamespace> {
        self.auth
            .as_ref()
            .and_then(|auth| auth.expected_catalog_namespace.as_ref())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthActivationResult {
    pub provider_id: Option<String>,
    pub provider_label: Option<String>,
    pub activated_model: Option<String>,
    pub expected_runtime: Option<String>,
    pub expected_catalog_namespace: Option<String>,
}

impl AuthActivationResult {
    pub fn model_switch_request(&self, current_provider_name: &str, model: &str) -> String {
        model_switch_request_for_provider_id(self.provider_id.as_deref(), current_provider_name, model)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthCatalogInvariantReport {
    pub applicable: bool,
    pub provider_id: Option<String>,
    pub provider_label: Option<String>,
    pub selectable_provider_routes: usize,
    pub selected_model: Option<String>,
    pub selected_model_matches_provider_route: bool,
    pub route_sample: Vec<String>,
}

impl AuthCatalogInvariantReport {
    pub fn ok(&self) -> bool {
        !self.applicable || (self.selectable_provider_routes > 0 && self.selected_model_matches_provider_route)
    }

    pub fn warning_message(&self) -> Option<String> {
        if self.ok() {
            return None;
        }

        let provider = self
            .provider_label
            .as_deref()
            .or(self.provider_id.as_deref())
            .unwrap_or("provider");
        let selected = self.selected_model.as_deref().unwrap_or("none");
        let sample = if self.route_sample.is_empty() {
            "none".to_string()
        } else {
            self.route_sample.join(", ")
        };
        Some(format!(
            "\n\n**Auth Model Catalog Warning**\n\nExpected selectable {provider} model routes after auth, but found {} matching route(s). Selected model: `{selected}`. Matching route sample: {sample}.",
            self.selectable_provider_routes
        ))
    }
}

pub fn validate_catalog_invariants(
    activation: &AuthActivationResult,
    selected_model: Option<&str>,
    routes: &[ModelRoute],
) -> AuthCatalogInvariantReport {
    let provider_id = activation.provider_id.clone();
    let provider_label = activation.provider_label.clone();
    let applicable = provider_id.is_some() || provider_label.is_some();
    let selected_model = selected_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(ToString::to_string);

    let matching_routes = routes
        .iter()
        .filter(|route| route.available && route_matches_activation(route, activation))
        .collect::<Vec<_>>();
    let selected_model_matches_provider_route = selected_model
        .as_ref()
        .is_some_and(|selected| matching_routes.iter().any(|route| route.model == *selected));
    let route_sample = matching_routes
        .iter()
        .take(5)
        .map(|route| format!("`{}` via {}", route.model, route.api_method))
        .collect::<Vec<_>>();

    AuthCatalogInvariantReport {
        applicable,
        provider_id,
        provider_label,
        selectable_provider_routes: matching_routes.len(),
        selected_model,
        selected_model_matches_provider_route,
        route_sample,
    }
}

pub fn provider_model_to_select_after_auth(
    activation: &AuthActivationResult,
    selected_model: Option<&str>,
    routes: &[ModelRoute],
) -> Option<String> {
    let matching_routes = routes
        .iter()
        .filter(|route| route.available && route_matches_activation(route, activation))
        .collect::<Vec<_>>();
    if matching_routes.is_empty() {
        return None;
    }

    let selected_model = selected_model.map(str::trim).filter(|model| !model.is_empty());
    if let Some(selected) = selected_model
        && matching_routes.iter().any(|route| route.model == selected)
    {
        let same_model_wrong_route_exists = routes
            .iter()
            .any(|route| route.available && route.model == selected && !route_matches_activation(route, activation));
        if same_model_wrong_route_exists {
            return Some(selected.to_string());
        }
        return None;
    }

    if let Some(activated_model) = activation
        .activated_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        && matching_routes.iter().any(|route| route.model == activated_model)
    {
        return Some(activated_model.to_string());
    }

    // No usable current model and no activation-supplied model: fall back to the
    // best available route. Plain catalog order would pick whatever the live
    // catalog happened to list first (e.g. `claude-haiku-4-5-...` ahead of
    // `claude-opus-4-8`), so an Anthropic API-key login would auto-select Haiku
    // instead of the provider's flagship default. When the provider has a
    // curated flagship-first preference order, pick the highest-ranked matching
    // route; ties and unranked providers preserve catalog order.
    let orders = provider_preferred_model_orders(activation);
    if !orders.is_empty() {
        // 1. Auto-promote a brand-new frontier release that is not yet in the
        //    curated list. Model ids in these families encode their version
        //    (`gpt-5.5`, `claude-opus-4-8`), so a strictly-newer *pure* flagship
        //    id (no cheap/specialized suffix) should become the default the day
        //    it ships, without waiting for a code change.
        if let Some(newer) = newest_frontier_release(activation, &matching_routes) {
            return Some(newer);
        }
        // 2. Otherwise pick the best curated model by quality order.
        return matching_routes
            .iter()
            .min_by_key(|route| preferred_model_rank(orders, &route.model))
            .map(|route| route.model.clone());
    }

    if let Some(provider_id) = activation.provider_id.as_deref()
        && let Some(newest_model) =
            crate::provider_catalog::newest_released_model_for_openai_compatible_profile(provider_id)
        && matching_routes.iter().any(|route| route.model == newest_model)
    {
        return Some(newest_model);
    }

    matching_routes.first().map(|route| route.model.clone())
}

/// Reconcile a provider after auth while preserving an explicit configured
/// default when that model is available for the activated provider.
pub fn provider_model_to_select_after_auth_with_configured_default(
    activation: &AuthActivationResult,
    configured_model: Option<&str>,
    selected_model: Option<&str>,
    routes: &[ModelRoute],
) -> Option<String> {
    let configured_model = configured_model.map(str::trim).filter(|model| !model.is_empty());
    // `config.provider.default_model` is persisted by the model picker as a
    // full model spec that may carry an explicit provider/credential prefix
    // (e.g. `claude-oauth:claude-sonnet-5`, issue: default model reverts to
    // Opus after /login or /refresh-model-list). `route.model` is always the
    // bare id, so compare against the prefix-stripped form or this branch
    // never matches and silently falls through to the flagship-first
    // fallback below.
    let configured_bare = configured_model.map(|model| {
        horon_agent_provider_core::selection::explicit_model_provider_prefix(model)
            .map(|(_, _, bare)| bare)
            .unwrap_or(model)
    });
    if let Some(configured) = configured_bare
        && routes
            .iter()
            .any(|route| route.available && route.model == configured && route_matches_activation(route, activation))
        && selected_model.map(str::trim) != Some(configured)
    {
        return Some(configured.to_string());
    }

    provider_model_to_select_after_auth(activation, selected_model, routes)
}

/// Pick the strongest model among `models` for `provider_id`, using the same
/// flagship-first order and new-release auto-promotion as post-login
/// selection. Unlike [`provider_model_to_select_after_auth`] this does not
/// filter by route identity, so clients that already scoped the candidates to
/// one provider (Desktop's provider switcher, Copilot tier detection) can rank
/// them directly. Returns `None` when the provider has no curated order or no
/// candidate is recognized, so callers keep their own fallback.
pub fn preferred_model_for_provider(provider_id: &str, models: &[&str]) -> Option<String> {
    if let Some(ranked) = ranked_flagship_for_provider(provider_id, models) {
        return Some(ranked);
    }
    let provider_id = normalized_auth_provider_id(Some(provider_id))
        .map(str::to_string)
        .unwrap_or_else(|| provider_id.trim().to_ascii_lowercase());
    crate::provider_catalog::newest_released_model_for_openai_compatible_profile(&provider_id)
        .filter(|newest| models.contains(&newest.as_str()))
}

/// Curated-order part of [`preferred_model_for_provider`]: new-release
/// promotion, then flagship rank. `None` for providers without a curated
/// order or when nothing in `models` is recognized. Never consults catalogs,
/// so catalog code can call it without recursing.
pub fn ranked_flagship_for_provider(provider_id: &str, models: &[&str]) -> Option<String> {
    let provider_id = normalized_auth_provider_id(Some(provider_id))
        .map(str::to_string)
        .unwrap_or_else(|| provider_id.trim().to_ascii_lowercase());
    let activation = AuthActivationResult {
        provider_id: Some(provider_id.clone()),
        ..Default::default()
    };
    let routes: Vec<ModelRoute> = models
        .iter()
        .map(|model| ModelRoute {
            model: (*model).to_string(),
            provider: String::new(),
            api_method: provider_id.clone(),
            available: true,
            detail: String::new(),
            usage: None,
            cheapness: None,
        })
        .collect();
    let route_refs: Vec<&ModelRoute> = routes.iter().collect();
    if let Some(newer) = newest_frontier_release(&activation, &route_refs) {
        return Some(newer);
    }
    let orders = provider_preferred_model_orders(&activation);
    if !orders.is_empty() {
        return routes
            .iter()
            .filter(|route| preferred_model_rank(orders, &route.model) != usize::MAX)
            .min_by_key(|route| preferred_model_rank(orders, &route.model))
            .map(|route| route.model.clone());
    }
    None
}

/// The model a fresh session starts on for one provider login, before any
/// live catalog is fetched. `None` for providers HoronAgent cannot run (or that
/// need a catalog to name a model, such as OpenRouter).
pub fn default_model_for_provider(provider_id: &str) -> Option<&'static str> {
    match provider_id.trim().to_ascii_lowercase().as_str() {
        "claude" | "claude-api" | "anthropic" | "anthropic-api" => {
            Some(horon_agent_provider_core::DEFAULT_CLAUDE_MODEL)
        }
        "openai" | "openai-api" | "codex" => Some(horon_agent_provider_core::DEFAULT_OPENAI_MODEL),
        _ => None,
    }
}

/// Predict the provider and model a first session will use once onboarding
/// finishes with these logins (already connected plus selected imports).
/// Mirrors the strongest-route choice the runtime applies after an import
/// ([`globally_preferred_default_route`]), so UIs can show the real identity
/// instead of asking the user to choose. Ties keep the caller's order.
/// Returns `(provider_id, model)`.
pub fn onboarding_default_selection(provider_ids: &[&str]) -> Option<(String, String)> {
    let routes: Vec<ModelRoute> = provider_ids
        .iter()
        .filter_map(|id| {
            default_model_for_provider(id).map(|model| ModelRoute {
                model: model.to_string(),
                provider: id.trim().to_string(),
                api_method: String::new(),
                available: true,
                detail: String::new(),
                usage: None,
                cheapness: None,
            })
        })
        .collect();
    globally_preferred_default_route(&routes).map(|route| (route.provider, route.model))
}

/// Pick the strongest available route across every authenticated provider.
///
/// This is intentionally separate from [`provider_model_to_select_after_auth`],
/// which keeps normal re-authentication scoped to the provider that changed.
/// First-run onboarding can use this global selector after importing multiple
/// accounts. Returning the complete route preserves OAuth/API-key/profile
/// identity when the caller applies the selection.
pub fn globally_preferred_default_route(routes: &[ModelRoute]) -> Option<ModelRoute> {
    routes
        .iter()
        .enumerate()
        .filter(|(_, route)| route.available)
        .min_by_key(|(catalog_index, route)| (globally_preferred_model_rank(&route.model), *catalog_index))
        .map(|(_, route)| route.clone())
}

fn globally_preferred_model_rank(model: &str) -> (u8, usize) {
    let normalized = normalize_model_for_preference(model);
    let openai_default = normalize_model_for_preference(horon_agent_provider_core::DEFAULT_OPENAI_MODEL);
    let claude_default = normalize_model_for_preference(horon_agent_provider_core::DEFAULT_CLAUDE_MODEL);

    if normalized == openai_default {
        return (0, 0);
    }
    // Previous OpenAI flagship profile, then the clean release id some catalogs
    // expose instead of horon-agent's Sol route. Both still outrank the Claude default.
    if normalized == "gpt-5.6-sol" {
        return (1, 0);
    }
    if normalized == "gpt-5.6" {
        return (1, 1);
    }
    if normalized == claude_default {
        return (2, 0);
    }
    if let Some(position) = normalized_preference_order(crate::provider::ALL_CLAUDE_MODELS)
        .iter()
        .position(|candidate| *candidate == normalized)
    {
        return (3, position);
    }
    if let Some(position) = normalized_preference_order(crate::provider::ALL_OPENAI_MODELS)
        .iter()
        .position(|candidate| *candidate == normalized)
    {
        return (4, position);
    }

    // Unknown provider families retain catalog order as the final fallback.
    (5, usize::MAX)
}

/// Curated flagship-first order for Bedrock-hosted models. Bedrock ids carry a
/// vendor prefix (`anthropic.claude-opus-4-...`, `us.anthropic.claude-...`) which
/// `parse_frontier_model`/`normalize_model_for_preference` strip before matching,
/// so the bare canonical ids here line up with the live route ids. Claude Opus
/// is the flagship, then Sonnet, then Nova/Llama/Mistral, then Haiku/cheap.
const ALL_BEDROCK_MODELS: &[&str] = &[
    "claude-opus-4",
    "claude-sonnet-4",
    "claude-3-7-sonnet",
    "claude-3-5-sonnet",
    "amazon.nova-pro",
    "meta.llama3-1-405b-instruct",
    "mistral.mistral-large",
    "claude-3-5-haiku",
    "amazon.nova-lite",
    "amazon.nova-micro",
];

/// Curated flagship-first order for Gemini (Code Assist OAuth + Gemini API).
/// `pro` is Gemini's flagship tier and `flash`/`lite` are the cheaper tiers, so
/// (unlike Claude/OpenAI) `pro` must NOT be treated as a non-flagship marker for
/// this family. Listed newest-and-strongest first.
const ALL_GEMINI_MODELS: &[&str] = &[
    "gemini-3.1-pro",
    "gemini-3-pro",
    "gemini-2.5-pro",
    "gemini-1.5-pro",
    "gemini-3-flash",
    "gemini-2.5-flash",
    "gemini-2.0-flash",
    "gemini-1.5-flash",
];

/// Curated flagship-first order for Grok (Grok Build subscription). Newer
/// bare `grok-N.M` releases auto-promote via [`frontier_families`]; `code`/
/// `fast` variants never do.
const ALL_GROK_MODELS: &[&str] = &["grok-4.7", "grok-4.6", "grok-4.5", "grok-code-fast-1"];

/// Flagship-first preference tiers used only to break ties when falling back to
/// an arbitrary matching route after a login. Each inner slice is one curated
/// family ordered best-first; earlier families outrank later ones. Returns an
/// empty slice for providers without a curated order (local OpenAI-compatible,
/// raw OpenRouter, ...), which preserves live-catalog order.
///
/// Copilot and Cursor proxy Claude/OpenAI models under their bare canonical ids
/// (`copilot:claude-opus-4-8`), so they share the same "catalog lists the cheap
/// model first" hazard as a direct login and get the combined Claude+OpenAI
/// order. The Claude/OpenAI subscription default bias mirrors horon-agent's global
/// default model. Bedrock/Azure/Gemini/Antigravity are native hosted catalogs
/// whose route lists are often ordered oldest-first, so they get an explicit
/// curated order too.
fn provider_preferred_model_orders(activation: &AuthActivationResult) -> &'static [&'static [&'static str]] {
    match activation.provider_id.as_deref() {
        Some("claude") | Some("claude-api") => &[crate::provider::ALL_CLAUDE_MODELS],
        Some("openai") | Some("openai-api") => &[crate::provider::ALL_OPENAI_MODELS],
        // OpenRouter also proxies both families (`anthropic/claude-opus-5.5`,
        // `openai/gpt-6-astra`); the vendor prefix is stripped before ranking.
        Some("copilot") | Some("cursor") | Some("openrouter") => {
            &[crate::provider::ALL_CLAUDE_MODELS, crate::provider::ALL_OPENAI_MODELS]
        }
        Some("grok-build") => &[ALL_GROK_MODELS],
        Some("bedrock") => &[ALL_BEDROCK_MODELS],
        // Azure hosts the OpenAI family.
        Some("azure-openai") => &[crate::provider::ALL_OPENAI_MODELS],
        // Gemini (Code Assist OAuth) and Antigravity both serve Gemini models.
        Some("gemini") | Some("antigravity") => &[ALL_GEMINI_MODELS],
        _ => &[],
    }
}

/// Rank a (possibly date-suffixed) catalog model id against flagship-first
/// preference tiers. Lower is more preferred: an earlier family tier always
/// outranks a later one, and within a tier the curated position decides.
/// Unknown models sort last so they only win when nothing curated matches.
fn preferred_model_rank(orders: &[&'static [&'static str]], model: &str) -> usize {
    const TIER_STRIDE: usize = 10_000;
    let normalized = normalize_model_for_preference(model);
    for (tier, order) in orders.iter().enumerate() {
        if let Some(position) = normalized_preference_order(order)
            .iter()
            .position(|candidate| *candidate == normalized)
        {
            return tier * TIER_STRIDE + position;
        }
    }
    usize::MAX
}

/// `order` with every id passed through [`normalize_model_for_preference`],
/// memoized per curated list. Ranking a catalog compares every catalog model
/// against every curated id; re-normalizing the curated side each time made a
/// single OpenRouter catalog ranking cost ~80ms (it runs on every new session).
/// Curated orders are `&'static` constants, so the slice address identifies
/// them.
fn normalized_preference_order(order: &'static [&'static str]) -> std::sync::Arc<Vec<String>> {
    type OrderCache = std::sync::Mutex<std::collections::HashMap<(usize, usize), std::sync::Arc<Vec<String>>>>;
    static CACHE: std::sync::LazyLock<OrderCache> = std::sync::LazyLock::new(Default::default);
    let key = (order.as_ptr() as usize, order.len());
    if let Ok(cache) = CACHE.lock()
        && let Some(hit) = cache.get(&key)
    {
        return hit.clone();
    }
    let normalized = std::sync::Arc::new(
        order
            .iter()
            .map(|candidate| normalize_model_for_preference(candidate))
            .collect::<Vec<_>>(),
    );
    if let Ok(mut cache) = CACHE.lock() {
        cache.insert(key, normalized.clone());
    }
    normalized
}

/// Normalize a model id for flagship-preference comparison: lowercase, drop a
/// `[1m]` long-context suffix, strip a trailing 8-digit `-YYYYMMDD` date so live
/// dated ids (`claude-haiku-4-5-20251001`) match bare canonical ids
/// (`claude-haiku-4-5`), and strip hosted-vendor prefixes/suffixes so Bedrock and
/// proxy ids line up with the curated bare ids.
///
/// Examples:
///   `us.anthropic.claude-opus-4-20250514-v1:0` -> `claude-opus-4`
///   `anthropic.claude-3-5-sonnet-20241022-v2:0` -> `claude-3-5-sonnet`
///   `accounts/fireworks/models/qwen3-coder` -> `qwen3-coder`
///   `models/gemini-3-pro-preview` -> `gemini-3-pro`
fn normalize_model_for_preference(model: &str) -> String {
    let mut id = horon_agent_provider_core::model_id::canonical(model);

    // Drop a `/`-qualified path prefix (`accounts/x/models/y`, `models/gemini`).
    if let Some(idx) = id.rfind('/') {
        id = id[idx + 1..].to_string();
    }

    // Drop a trailing Bedrock version tag (`-v1:0`, `-v2:0`, `:0`).
    if let Some(idx) = id.find(":0") {
        id = id[..idx].to_string();
    }
    if let Some(stripped) = strip_trailing_bedrock_version(&id) {
        id = stripped;
    }

    // Drop a trailing release-date suffix.
    id = horon_agent_provider_core::model_id::strip_date_suffix(&id).to_string();

    // Drop a trailing `-preview`/`-exp`/`-latest` marketing suffix so Gemini
    // preview ids match their canonical family entry.
    for suffix in ["-preview", "-exp", "-latest"] {
        if let Some(base) = id.strip_suffix(suffix) {
            id = base.to_string();
        }
    }

    // Drop a leading hosted-vendor segment (`anthropic.`, `us.anthropic.`,
    // `meta.`, `amazon.`, `mistral.`) so `anthropic.claude-opus-4` matches the
    // curated `claude-opus-4`. Keep `amazon.nova`/`meta.llama`/`mistral.` whole
    // because those families are listed with their vendor prefix in
    // `ALL_BEDROCK_MODELS`; only strip the region + the redundant `anthropic.`.
    id = strip_bedrock_region_prefix(&id);
    if let Some(rest) = id.strip_prefix("anthropic.") {
        id = rest.to_string();
    }

    // Copilot and OpenRouter spell Claude releases with dots
    // (`claude-opus-4.6`) while the curated catalog uses hyphens
    // (`claude-opus-4-6`). Without this, dotted ids never match the curated
    // order and Copilot/Desktop fall back to arbitrary catalog order.
    if id.starts_with("claude-") {
        id = id.replace('.', "-");
    }

    id
}

/// Strip a leading Bedrock region routing segment (`us.`, `eu.`, `apac.`,
/// `us-gov.`) from a model id.
fn strip_bedrock_region_prefix(id: &str) -> String {
    for region in ["us-gov.", "us.", "eu.", "apac.", "ap.", "global."] {
        if let Some(rest) = id.strip_prefix(region) {
            return rest.to_string();
        }
    }
    id.to_string()
}

/// Strip a trailing Bedrock version tag like `-v1`, `-v2` (after the `:0` has
/// already been removed). Returns `None` when there is no such tag.
fn strip_trailing_bedrock_version(id: &str) -> Option<String> {
    let (head, tail) = id.rsplit_once('-')?;
    let is_version_tag = tail.len() >= 2 && tail.starts_with('v') && tail[1..].chars().all(|c| c.is_ascii_digit());
    is_version_tag.then(|| head.to_string())
}

/// A parsed "frontier flagship" model id: its family prefix (e.g. `claude-opus`
/// or `gpt`) plus an ordered version vector parsed from the trailing numeric
/// components, used to compare releases within a family.
///
/// Only *pure* flagship ids parse successfully: ids carrying a cheaper or
/// specialized tier word (`mini`, `nano`, `haiku`, `flash`, `codex`, `pro`,
/// `chat`, ...) are rejected so a new cheap/specialized model never auto-promotes
/// over the flagship.
struct FrontierModel {
    family: String,
    version: Vec<u64>,
}

/// Tier/specialization words that disqualify an id from frontier auto-promotion
/// for the default (Claude/OpenAI-style) families. These mark cheaper or
/// non-default variants that must never outrank a clean flagship id purely
/// because they share a (possibly higher) version number.
///
/// NOTE: this list is used for families whose flagship id is *bare* (no tier
/// word), e.g. `claude-opus-4-8`, `gpt-5.5`. Families like Gemini, whose flagship
/// id contains a tier word (`gemini-3-pro`), use a family-specific rule instead
/// (see [`frontier_families`]).
const NON_FLAGSHIP_TIER_WORDS: &[&str] = &[
    "mini", "nano", "haiku", "flash", "lite", "small", "tiny", "instant", "codex", "pro", "chat", "audio", "realtime",
    "image", "tts", "embed", "search", "guard", "deep", "thinking",
];

/// One frontier-eligible family: the canonical id prefix to match (after
/// [`normalize_model_for_preference`]) and, optionally, a required flagship tier
/// token that must be present (e.g. Gemini's `pro`). When `flagship_token` is
/// `Some`, version parsing ignores that token and rejects any *other* trailing
/// word; when `None`, the id must be bare (Claude/OpenAI style) and is rejected
/// if it contains any [`NON_FLAGSHIP_TIER_WORDS`].
#[derive(Clone, Copy)]
struct FrontierFamily {
    prefix: &'static str,
    flagship_token: Option<&'static str>,
}

/// Family descriptors eligible for frontier auto-promotion per provider. Only the
/// strongest family per provider is listed (Claude Opus, GPT base, Gemini Pro) so
/// we never auto-promote a cheaper family/tier over the curated flagship.
fn frontier_families(activation: &AuthActivationResult) -> &'static [FrontierFamily] {
    const FABLE: FrontierFamily = FrontierFamily {
        prefix: "claude-fable",
        flagship_token: None,
    };
    const CLAUDE: FrontierFamily = FrontierFamily {
        prefix: "claude-opus",
        flagship_token: None,
    };
    const GPT: FrontierFamily = FrontierFamily {
        prefix: "gpt",
        flagship_token: None,
    };
    const GEMINI: FrontierFamily = FrontierFamily {
        prefix: "gemini",
        flagship_token: Some("pro"),
    };
    const GROK: FrontierFamily = FrontierFamily {
        prefix: "grok",
        flagship_token: None,
    };
    match activation.provider_id.as_deref() {
        Some("claude") | Some("claude-api") => &[CLAUDE, FABLE],
        Some("openai") | Some("openai-api") | Some("azure-openai") => &[GPT],
        // Copilot/Cursor proxy both families under canonical ids.
        Some("copilot") | Some("cursor") | Some("openrouter") => &[CLAUDE, FABLE, GPT],
        Some("grok-build") => &[GROK],
        // Bedrock hosts Claude under `anthropic.claude-opus-...` (prefix stripped
        // by normalize), so the Claude family applies.
        Some("bedrock") => &[CLAUDE],
        Some("gemini") | Some("antigravity") => &[GEMINI],
        _ => &[],
    }
}

/// Parse a model id into a [`FrontierModel`] if it is a clean flagship id for one
/// of `families`. Returns `None` for non-matching families, ids with a
/// non-flagship tier word, or ids without a parseable version.
fn parse_frontier_model(model: &str, families: &[FrontierFamily]) -> Option<FrontierModel> {
    let normalized = normalize_model_for_preference(model);
    // Find the family this id belongs to (longest prefix wins so `claude-opus`
    // is preferred over a hypothetical `claude`).
    let family = families
        .iter()
        .filter(|fam| normalized.starts_with(fam.prefix))
        .max_by_key(|fam| fam.prefix.len())?;

    let version = parse_frontier_version_for_family(&normalized, *family, false)?;
    Some(FrontierModel {
        family: family.prefix.to_string(),
        version,
    })
}

/// Parse a model version within one known frontier family. Curated defaults may
/// carry a quality-profile suffix such as `gpt-5.6-sol`; when `allow_suffix` is
/// true, the leading numeric release is still used as the promotion baseline.
/// Live promotion candidates remain strict, so cheap/specialized variants never
/// self-promote merely because they contain a higher version number.
fn parse_frontier_version_for_family(
    normalized_model: &str,
    family: FrontierFamily,
    allow_suffix: bool,
) -> Option<Vec<u64>> {
    if !normalized_model.starts_with(family.prefix) {
        return None;
    }

    match family.flagship_token {
        None => {
            // Bare-flagship family (Claude/OpenAI): reject any tier word, then
            // require a pure-numeric remainder after the prefix.
            if !allow_suffix
                && NON_FLAGSHIP_TIER_WORDS
                    .iter()
                    .any(|word| normalized_model.contains(word))
            {
                return None;
            }
            let remainder = normalized_model[family.prefix.len()..].trim_matches(['-', '.', ' ']);
            if !allow_suffix {
                return parse_version_components(remainder);
            }
            let mut version = Vec::new();
            for part in remainder.split(['.', '-']) {
                if part.is_empty() {
                    continue;
                }
                match part.parse::<u64>() {
                    Ok(number) => version.push(number),
                    Err(_) if !version.is_empty() => break,
                    Err(_) => return None,
                }
            }
            (!version.is_empty()).then_some(version)
        }
        Some(flagship_token) => {
            // Flagship-token family (Gemini): the id must contain the flagship
            // token and nothing else but version numbers + that token. Reject any
            // other word (e.g. `flash`, `lite`).
            let remainder = normalized_model[family.prefix.len()..].trim_matches(['-', '.', ' ']);
            if remainder.is_empty() {
                return None;
            }
            let mut version = Vec::new();
            let mut saw_flagship_token = false;
            for part in remainder.split(['.', '-']) {
                if part.is_empty() {
                    continue;
                }
                if part == flagship_token {
                    saw_flagship_token = true;
                    continue;
                }
                let number: u64 = part.parse().ok()?; // any other word => reject
                version.push(number);
            }
            if !saw_flagship_token || version.is_empty() {
                return None;
            }
            Some(version)
        }
    }
}

/// Parse a dash/dot-separated numeric version remainder (`4-8`, `5.5`) into a
/// component vector. Returns `None` if any component is non-numeric or empty.
fn parse_version_components(remainder: &str) -> Option<Vec<u64>> {
    if remainder.is_empty() {
        return None;
    }
    let mut version = Vec::new();
    for part in remainder.split(['.', '-']) {
        if part.is_empty() {
            continue;
        }
        version.push(part.parse::<u64>().ok()?);
    }
    (!version.is_empty()).then_some(version)
}

/// Compare two version vectors component-wise (semver-like). Missing trailing
/// components are treated as 0 so `[5]` < `[5, 1]`.
fn version_cmp(a: &[u64], b: &[u64]) -> std::cmp::Ordering {
    let len = a.len().max(b.len());
    for i in 0..len {
        let av = a.get(i).copied().unwrap_or(0);
        let bv = b.get(i).copied().unwrap_or(0);
        match av.cmp(&bv) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

/// Auto-detect a brand-new frontier flagship release among `routes` that is
/// strictly newer than the curated baseline flagship for the same family, and is
/// not already the curated #1. Returns the chosen model id, or `None` when the
/// curated default is still the newest known flagship.
///
/// This is the new-release robustness layer: the day Anthropic ships
/// `claude-opus-4-9` or OpenAI ships `gpt-5.6`, the live catalog will carry it
/// and it auto-promotes to the post-login default without a code change, while
/// cheaper/specialized variants are excluded by [`parse_frontier_model`].
fn newest_frontier_release(activation: &AuthActivationResult, routes: &[&ModelRoute]) -> Option<String> {
    let families = frontier_families(activation);
    if families.is_empty() {
        return None;
    }

    // Only auto-promote within the strongest frontier family that is actually
    // present. This prevents a newer lower-priority Opus id from displacing an
    // available Fable default, while still allowing Opus promotion when Fable is
    // absent and GPT promotion when Sol is the active quality profile.
    let active_family = families.iter().copied().find(|family| {
        routes.iter().any(|route| {
            let normalized = normalize_model_for_preference(&route.model);
            parse_frontier_version_for_family(&normalized, *family, true).is_some()
        })
    })?;

    // Baseline: the strongest *available* curated release in the active family.
    // If the preferred profile (for example Sol 5.6) is unavailable, a clean
    // model at that release may still promote over the next available fallback
    // (for example GPT 5.5). When no curated route is available at all, fall back
    // to the shipped catalog baseline so an old unknown id cannot self-promote.
    let curated_baseline = |family: FrontierFamily| -> Option<Vec<u64>> {
        let orders = provider_preferred_model_orders(activation);
        let available_baseline = routes
            .iter()
            .filter(|route| preferred_model_rank(orders, &route.model) != usize::MAX)
            .filter_map(|route| {
                let normalized = normalize_model_for_preference(&route.model);
                parse_frontier_version_for_family(&normalized, family, true)
            })
            .max_by(|a, b| version_cmp(a, b));
        available_baseline.or_else(|| {
            orders
                .iter()
                .flat_map(|order| order.iter())
                .filter_map(|id| {
                    let normalized = normalize_model_for_preference(id);
                    parse_frontier_version_for_family(&normalized, family, true)
                })
                .max_by(|a, b| version_cmp(a, b))
        })
    };

    let mut best: Option<(FrontierModel, String)> = None;
    for route in routes {
        let Some(parsed) = parse_frontier_model(&route.model, families) else {
            continue;
        };
        if parsed.family != active_family.prefix {
            continue;
        }
        // Must strictly beat the curated baseline for its family.
        let Some(baseline) = curated_baseline(active_family) else {
            continue;
        };
        if version_cmp(&parsed.version, &baseline) != std::cmp::Ordering::Greater {
            continue;
        }
        // Among qualifying releases, keep the highest version (preferring the
        // strongest family by the order in `families` on ties).
        let is_better = match &best {
            None => true,
            Some((current, _)) => match version_cmp(&parsed.version, &current.version) {
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Equal => {
                    // Tie on version: prefer the family listed earlier (stronger).
                    let rank = |fam: &str| families.iter().position(|f| f.prefix == fam).unwrap_or(usize::MAX);
                    rank(&parsed.family) < rank(&current.family)
                }
                std::cmp::Ordering::Less => false,
            },
        };
        if is_better {
            best = Some((parsed, route.model.clone()));
        }
    }

    best.map(|(_, model)| model)
}

fn route_matches_activation(route: &ModelRoute, activation: &AuthActivationResult) -> bool {
    let api_method = route.api_method_kind();
    let Some(provider_id) = activation.provider_id.as_deref() else {
        if let Some(label) = activation.provider_label.as_deref()
            && route.provider.eq_ignore_ascii_case(label)
        {
            return true;
        }
        return false;
    };

    if api_method.matches_openai_compatible_profile(provider_id) {
        return true;
    }

    if route.api_method.eq_ignore_ascii_case(provider_id) {
        return true;
    }

    match provider_id {
        "claude" => {
            return matches!(api_method, crate::provider::ModelRouteApiMethod::ClaudeOAuth);
        }
        "claude-api" => {
            return route.provider.eq_ignore_ascii_case("Anthropic")
                && matches!(api_method, crate::provider::ModelRouteApiMethod::AnthropicApiKey);
        }
        "openai" => {
            return matches!(api_method, crate::provider::ModelRouteApiMethod::OpenAIOAuth);
        }
        "openai-api" => {
            return matches!(api_method, crate::provider::ModelRouteApiMethod::OpenAIApiKey);
        }
        "gemini" => {
            // Gemini's Code Assist OAuth routes carry the `code-assist-oauth`
            // api_method (not the bare provider id), so match on the parsed kind
            // like the other native credential routes above.
            return matches!(api_method, crate::provider::ModelRouteApiMethod::CodeAssistOAuth);
        }
        "horon-agent" => {
            // HoronAgent subscription routes deliberately keep their managed public
            // identity even though the runtime reuses OpenRouter transport code.
            return matches!(api_method, crate::provider::ModelRouteApiMethod::HoronAgentSubscription);
        }
        "grok-build" => {
            return matches!(api_method, crate::provider::ModelRouteApiMethod::GrokBuild);
        }
        "azure-openai" => {
            // Azure OpenAI reuses the OpenRouter transport (configured via Azure
            // env), so its routes carry the `openrouter` api_method while keeping
            // the `azure-openai` runtime identity.
            return matches!(api_method, crate::provider::ModelRouteApiMethod::OpenRouter);
        }
        _ => {}
    }

    // OpenAI-compatible auth has a concrete catalog namespace. Accepting a
    // matching display label or generic `openai-compatible` route as success can
    // hide stale/mixed catalogs, especially when providers share model IDs.
    // Legacy/local TUI auth notifications only carry the provider label, so
    // derive this constraint from the normalized profile id as well as explicit
    // protocol metadata.
    if crate::provider_catalog::openai_compatible_profile_by_id(provider_id).is_some()
        || activation.expected_runtime.as_deref() == Some("openai-compatible")
        || activation.expected_catalog_namespace.is_some()
    {
        return false;
    }

    if let Some(label) = activation.provider_label.as_deref()
        && route.provider.eq_ignore_ascii_case(label)
    {
        return true;
    }

    false
}

pub fn normalized_auth_provider_id(provider_hint: Option<&str>) -> Option<&'static str> {
    let provider = provider_hint?.trim();
    if provider.eq_ignore_ascii_case("azure")
        || provider.eq_ignore_ascii_case("azure-openai")
        || provider.eq_ignore_ascii_case("azure openai")
    {
        Some("azure-openai")
    } else if let Some(profile) = crate::provider_catalog::resolve_openai_compatible_profile_selection(provider) {
        Some(profile.id)
    } else if let Some(descriptor) = crate::provider_catalog::resolve_login_provider(provider) {
        normalized_login_provider_id(descriptor.id)
    } else {
        None
    }
}

/// Pick the preferred first-run provider when the machine already has working
/// OpenAI and/or Anthropic credentials. Keep this aligned with
/// `horon_agent_provider_core::auto_default_provider`: Claude wins when both families
/// are available, and OAuth wins over an API key within the same family.
///
/// Returning the auth-specific provider id (`openai` vs `openai-api`, `claude`
/// vs `claude-api`) matters because post-login activation uses it to select the
/// matching route and its strongest available model.
pub fn preferred_frontier_auth_provider(status: &crate::auth::AuthStatus) -> Option<&'static str> {
    use crate::auth::AuthState;

    if status.openai == AuthState::Available {
        if status.openai_has_oauth && status.openai_oauth_state == AuthState::Available {
            return Some("openai");
        }
        if status.openai_has_api_key {
            return Some("openai-api");
        }
        // Preserve compatibility with older/partial status snapshots that only
        // populated the aggregate state.
        return Some("openai");
    }

    None
}

fn normalized_login_provider_id(provider_id: &str) -> Option<&'static str> {
    match provider_id.trim().to_ascii_lowercase().as_str() {
        "claude" | "anthropic" => Some("claude"),
        "anthropic-api" | "claude-api" | "anthropic-key" | "claude-key" => Some("claude-api"),
        "openai" => Some("openai"),
        "openai-api" | "openai-key" | "openai-apikey" | "openai-platform" | "platform-openai" => Some("openai-api"),
        "openrouter" => Some("openrouter"),
        "horon-agent" | "subscription" | "horon-agent-subscription" => Some("horon-agent"),
        "grok-build" => Some("grok-build"),
        "bedrock" | "aws-bedrock" | "aws_bedrock" => Some("bedrock"),
        "cursor" => Some("cursor"),
        "copilot" => Some("copilot"),
        "gemini" => Some("gemini"),
        "antigravity" => Some("antigravity"),
        _ => None,
    }
}

pub fn provider_display_label(provider_id: Option<&str>) -> Option<String> {
    let provider = normalized_auth_provider_id(provider_id)?;
    if provider == "azure-openai" {
        return Some("Azure OpenAI".to_string());
    }
    crate::provider_catalog::openai_compatible_profile_by_id(provider)
        .map(|profile| profile.display_name.to_string())
        .or_else(|| {
            crate::provider_catalog::resolve_login_provider(provider)
                .map(|descriptor| descriptor.display_name.to_string())
        })
        .or_else(|| Some(provider.to_string()))
}

#[path = "lifecycle_activation.rs"]
mod activation;
pub use activation::*;
pub fn model_switch_request_for_provider_id(provider_id: Option<&str>, _provider_name: &str, model: &str) -> String {
    match normalized_auth_provider_id(provider_id) {
        Some("azure-openai") => format!("openrouter:{}", model),
        Some(profile_id)
            if profile_id != "azure-openai"
                && crate::provider_catalog::openai_compatible_profile_by_id(profile_id).is_some() =>
        {
            format!("{}:{}", profile_id, model)
        }
        Some("claude") => format!("claude-oauth:{}", model),
        Some("claude-api") => format!("claude-api:{}", model),
        Some("openai") => format!("openai-oauth:{}", model),
        Some("openai-api") => format!("openai-api:{}", model),
        Some("openrouter") => format!("openrouter:{}", model),
        Some("horon-agent") => model.to_string(),
        Some("grok-build") => crate::provider::grok_build_model_spec(model),
        Some("bedrock") => format!("bedrock:{}", model),
        Some("cursor") => format!("cursor:{}", model),
        Some("copilot") => format!("copilot:{}", model),
        Some("gemini") => format!("gemini:{}", model),
        Some("antigravity") => format!("antigravity:{}", model),
        _ => model.to_string(),
    }
}

#[cfg(test)]
mod tests {
    fn activation_for_provider_id(provider_id: &str) -> AuthActivationResult {
        AuthActivationResult {
            provider_id: Some(provider_id.to_string()),
            provider_label: provider_display_label(Some(provider_id)),
            activated_model: None,
            expected_runtime: None,
            expected_catalog_namespace: None,
        }
    }

    use super::*;
    use std::sync::MutexGuard;

    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn new(keys: &[&'static str]) -> Self {
            let lock = crate::storage::lock_test_env();
            let saved = keys.iter().map(|key| (*key, std::env::var(key).ok())).collect();
            for key in keys {
                crate::env::remove_var(key);
            }
            Self { _lock: lock, saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.saved.drain(..) {
                if let Some(value) = value {
                    crate::env::set_var(key, value);
                } else {
                    crate::env::remove_var(key);
                }
            }
        }
    }

    fn route(model: &str, provider: &str, api_method: &str, available: bool) -> ModelRoute {
        ModelRoute {
            model: model.to_string(),
            provider: provider.to_string(),
            api_method: api_method.to_string(),
            available,
            detail: String::new(),
            usage: None,
            cheapness: None,
        }
    }

    #[test]
    fn onboarding_default_selection_follows_global_preference() {
        assert_eq!(onboarding_default_selection(&[]), None);
        assert_eq!(onboarding_default_selection(&["openrouter"]), None);
        assert_eq!(
            onboarding_default_selection(&["gemini", "claude", "openai"]),
            Some((
                "openai".to_string(),
                horon_agent_provider_core::DEFAULT_OPENAI_MODEL.to_string()
            ))
        );
        assert_eq!(
            onboarding_default_selection(&["gemini", "claude-api"]),
            Some((
                "claude-api".to_string(),
                horon_agent_provider_core::DEFAULT_CLAUDE_MODEL.to_string()
            ))
        );
    }

    #[test]
    fn oauth_auth_change_does_not_touch_api_key_process_env() {
        let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().expect("sandbox");
        crate::env::set_var("ANTHROPIC_API_KEY", "env-key-left-alone");
        sandbox
            .write_env_file("anthropic.env", "ANTHROPIC_API_KEY", "file-key")
            .expect("write env file");

        let mut auth = AuthChanged::new("claude-api");
        auth.auth_method = Some(crate::protocol::AuthMethod::OAuthBrowser);
        auth.credential_source = Some(crate::protocol::AuthCredentialSource::OAuthTokenStore);
        let _ = activate_auth_change(&AuthActivationRequest::new(None, Some(auth)));

        assert_eq!(std::env::var("ANTHROPIC_API_KEY").as_deref(), Ok("env-key-left-alone"));
    }

    #[test]
    fn typed_auth_request_provider_id_wins_over_legacy_hint() {
        let request = AuthActivationRequest::new(Some("openai".to_string()), Some(AuthChanged::new("cerebras")));

        assert_eq!(request.provider_id().as_deref(), Some("cerebras"));
        assert_eq!(
            provider_display_label(request.provider_id().as_deref()).as_deref(),
            Some("Cerebras")
        );
    }

    #[test]
    fn every_model_login_provider_has_explicit_lifecycle_normalization() {
        let mut missing = Vec::new();
        for provider in crate::provider_catalog::login_providers() {
            let is_non_model_auth_surface = matches!(
                provider.target,
                crate::provider_catalog::LoginProviderTarget::AutoImport
            );
            let normalized = normalized_auth_provider_id(Some(provider.id));
            if is_non_model_auth_surface {
                assert!(
                    normalized.is_none(),
                    "non-model auth provider {} should stay out of model lifecycle normalization",
                    provider.id
                );
            } else if normalized.is_none() {
                missing.push(provider.id);
            }
        }

        assert!(
            missing.is_empty(),
            "model login providers missing lifecycle normalization: {:?}",
            missing
        );
    }

    #[test]
    fn grok_build_login_selects_subscription_route_and_preserves_prefix() {
        let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().expect("sandbox");
        let activation = activate_auth_change(&AuthActivationRequest::new(None, Some(AuthChanged::new("grok-build"))));
        assert_eq!(activation.provider_id.as_deref(), Some("grok-build"));
        assert_eq!(activation.provider_label.as_deref(), Some("Grok Build"));
        let routes = vec![
            route("grok-4.6", "xAI", "openrouter", true),
            route("grok-build:grok-4.6", "Grok Build", "grok-build-acp", true),
        ];
        let selected = provider_model_to_select_after_auth(&activation, Some("grok-4.6"), &routes);
        assert_eq!(selected.as_deref(), Some("grok-build:grok-4.6"));
        assert!(validate_catalog_invariants(&activation, selected.as_deref(), &routes).ok());
        for model in ["grok-4.6", "grok-build:grok-4.6"] {
            assert_eq!(
                activation.model_switch_request("OpenRouter", model),
                "grok-build:grok-4.6"
            );
        }
    }

    #[test]
    fn login_does_not_override_configured_default_provider_for_new_sessions() {
        let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().expect("sandbox");
        std::fs::write(
            sandbox.root().join("config.toml"),
            "[provider]\ndefault_model = \"claude-oauth:claude-opus-5-5\"\ndefault_provider = \"claude-oauth\"\n",
        )
        .expect("write config");
        crate::config::invalidate_config_cache();
        crate::env::set_var("HORON_AGENT_RUNTIME_PROVIDER", "claude");
        crate::env::set_var("HORON_AGENT_ACTIVE_PROVIDER", "claude");
        crate::env::remove_var("HORON_AGENT_INITIAL_PROVIDER_EXPLICIT");

        let activation = activate_auth_change(&AuthActivationRequest::new(None, Some(AuthChanged::new("openai"))));

        assert_eq!(activation.provider_id.as_deref(), Some("openai"));
        assert_eq!(
            std::env::var("HORON_AGENT_ACTIVE_PROVIDER").as_deref(),
            Ok("claude"),
            "logging into OpenAI must not retarget the daemon-wide initial provider"
        );
        assert!(std::env::var("HORON_AGENT_INITIAL_PROVIDER_EXPLICIT").is_err());

        std::fs::remove_file(sandbox.root().join("config.toml")).ok();
        crate::config::invalidate_config_cache();
    }

    #[test]
    fn model_switch_request_prefixes_openai_compatible_profiles_with_profile_id() {
        assert_eq!(
            model_switch_request_for_provider_id(Some("cerebras"), "mock-auth", "llama3.1-8b"),
            "cerebras:llama3.1-8b"
        );
        assert_eq!(
            model_switch_request_for_provider_id(Some("cerebras"), "openrouter", "llama3.1-8b"),
            "cerebras:llama3.1-8b"
        );
    }

    #[test]
    fn horon_agent_auth_lifecycle_matches_only_managed_subscription_routes() {
        let activation = AuthActivationResult {
            provider_id: Some("horon-agent".to_string()),
            provider_label: Some("HoronAgent Subscription".to_string()),
            activated_model: Some("gpt-5.5".to_string()),
            expected_runtime: Some("horon-agent-subscription".to_string()),
            expected_catalog_namespace: Some("horon-agent-subscription".to_string()),
        };
        let routes = vec![
            route("gpt-5.5", "OpenRouter", "openrouter", true),
            route(
                "gpt-5.5",
                "HoronAgent Subscription",
                crate::subscription_catalog::HORON_AGENT_ROUTE_API_METHOD,
                true,
            ),
        ];

        let report = validate_catalog_invariants(&activation, Some("gpt-5.5"), &routes);
        assert!(report.ok(), "canonical HoronAgent route should match: {report:?}");
        assert_eq!(report.selectable_provider_routes, 1);
        assert_eq!(
            report.route_sample,
            vec![format!(
                "`gpt-5.5` via {}",
                crate::subscription_catalog::HORON_AGENT_ROUTE_API_METHOD
            )]
        );
        assert_eq!(
            activation.model_switch_request("HoronAgent Subscription", "gpt-5.5"),
            "gpt-5.5"
        );
    }

    #[test]
    fn post_auth_model_selection_reselects_duplicate_model_name_from_matching_provider_route() {
        let activation = AuthActivationResult {
            provider_id: Some("cerebras".to_string()),
            provider_label: Some("Cerebras".to_string()),
            activated_model: Some("llama3.1-8b".to_string()),
            expected_runtime: Some("openai-compatible".to_string()),
            expected_catalog_namespace: Some("cerebras".to_string()),
        };
        let routes = vec![
            route("llama3.1-8b", "Other Gateway", "openai-compatible:other", true),
            route("llama3.1-8b", "Cerebras", "openai-compatible:cerebras", true),
        ];

        assert_eq!(
            provider_model_to_select_after_auth(&activation, Some("llama3.1-8b"), &routes),
            Some("llama3.1-8b".to_string()),
            "duplicate model IDs must force an explicit provider-profile model switch"
        );
    }

    #[test]
    fn catalog_invariants_pass_when_selected_model_matches_provider_route() {
        let activation = AuthActivationResult {
            provider_id: Some("cerebras".to_string()),
            provider_label: Some("Cerebras".to_string()),
            activated_model: Some("llama3.1-8b".to_string()),
            expected_runtime: Some("openai-compatible".to_string()),
            expected_catalog_namespace: Some("cerebras".to_string()),
        };
        let routes = vec![
            route("gpt-5.5", "OpenAI", "openai", true),
            route("llama3.1-8b", "Cerebras", "openai-compatible:cerebras", true),
        ];

        let report = validate_catalog_invariants(&activation, Some("llama3.1-8b"), &routes);

        assert!(report.ok(), "unexpected warning: {:?}", report.warning_message());
        assert_eq!(report.selectable_provider_routes, 1);
    }

    #[test]
    fn catalog_invariants_reject_generic_openai_compatible_route_for_namespaced_auth() {
        let activation = AuthActivationResult {
            provider_id: Some("cerebras".to_string()),
            provider_label: Some("Cerebras".to_string()),
            activated_model: Some("llama3.1-8b".to_string()),
            expected_runtime: Some("openai-compatible".to_string()),
            expected_catalog_namespace: Some("cerebras".to_string()),
        };
        let routes = vec![route("llama3.1-8b", "Cerebras", "openai-compatible", true)];

        let report = validate_catalog_invariants(&activation, Some("llama3.1-8b"), &routes);

        assert!(
            !report.ok(),
            "generic openai-compatible route should not satisfy namespaced auth: {report:?}"
        );
        assert_eq!(report.selectable_provider_routes, 0);
        assert!(
            report
                .warning_message()
                .expect("warning")
                .contains("Expected selectable Cerebras model routes")
        );
    }

    #[test]
    fn catalog_invariants_warn_when_selected_model_is_from_stale_provider() {
        let activation = AuthActivationResult {
            provider_id: Some("cerebras".to_string()),
            provider_label: Some("Cerebras".to_string()),
            activated_model: Some("llama3.1-8b".to_string()),
            expected_runtime: Some("openai-compatible".to_string()),
            expected_catalog_namespace: Some("cerebras".to_string()),
        };
        let routes = vec![route("gpt-5.5", "OpenAI", "openai", true)];

        let report = validate_catalog_invariants(&activation, Some("gpt-5.5"), &routes);

        assert!(!report.ok());
        let warning = report.warning_message().expect("warning expected");
        assert!(warning.contains("Expected selectable Cerebras model routes"));
        assert!(warning.contains("Selected model: `gpt-5.5`"));
    }

    #[test]
    fn post_auth_model_selection_prefers_matching_provider_route_over_stale_model() {
        let activation = AuthActivationResult {
            provider_id: Some("cerebras".to_string()),
            provider_label: Some("Cerebras".to_string()),
            activated_model: Some("qwen-3-235b-a22b-instruct-2507".to_string()),
            expected_runtime: Some("openai-compatible".to_string()),
            expected_catalog_namespace: Some("cerebras".to_string()),
        };
        let routes = vec![
            route("gpt-5.5", "OpenAI", "openai", true),
            route(
                "qwen-3-235b-a22b-instruct-2507",
                "Cerebras",
                "openai-compatible:cerebras",
                true,
            ),
            route("llama3.1-8b", "Cerebras", "openai-compatible:cerebras", true),
        ];

        assert_eq!(
            provider_model_to_select_after_auth(&activation, Some("gpt-5.5"), &routes).as_deref(),
            Some("qwen-3-235b-a22b-instruct-2507")
        );
        assert_eq!(
            provider_model_to_select_after_auth(&activation, Some("qwen-3-235b-a22b-instruct-2507"), &routes),
            None
        );
    }

    #[test]
    fn post_auth_model_selection_prefers_anthropic_flagship_over_catalog_order() {
        // Live Anthropic catalogs list `claude-haiku-4-5-...` before the
        // flagship, and an API-key login supplies no activated model. Plain
        // catalog order would auto-select Haiku; the flagship-first fallback
        // must land on the curated quality-first default (Opus 5) instead.
        let activation = AuthActivationResult {
            provider_id: Some("claude-api".to_string()),
            provider_label: Some("Anthropic".to_string()),
            activated_model: None,
            expected_runtime: None,
            expected_catalog_namespace: None,
        };
        let routes = vec![
            route("claude-haiku-4-5-20251001", "Anthropic", "claude-api", true),
            route("claude-opus-4-6", "Anthropic", "claude-api", true),
            route("claude-opus-4-8", "Anthropic", "claude-api", true),
            route("claude-opus-5", "Anthropic", "claude-api", true),
            route("claude-fable-5", "Anthropic", "claude-api", true),
            route("claude-sonnet-4-6", "Anthropic", "claude-api", true),
        ];

        assert_eq!(
            provider_model_to_select_after_auth(&activation, None, &routes).as_deref(),
            Some("claude-opus-5"),
            "API-key login should auto-select the Anthropic flagship, not the first catalog route"
        );
    }

    #[test]
    fn post_auth_model_selection_preserves_configured_claude_model() {
        let activation = AuthActivationResult {
            provider_id: Some("claude-api".to_string()),
            provider_label: Some("Anthropic".to_string()),
            activated_model: None,
            expected_runtime: None,
            expected_catalog_namespace: None,
        };
        let routes = vec![
            route(
                horon_agent_provider_core::DEFAULT_CLAUDE_MODEL,
                "Anthropic",
                "claude-api",
                true,
            ),
            route("claude-opus-4-6", "Anthropic", "claude-api", true),
        ];

        assert_eq!(
            provider_model_to_select_after_auth_with_configured_default(
                &activation,
                Some("claude-opus-4-6"),
                Some(horon_agent_provider_core::DEFAULT_CLAUDE_MODEL),
                &routes,
            )
            .as_deref(),
            Some("claude-opus-4-6")
        );
    }

    #[test]
    fn post_auth_model_selection_prefers_openai_flagship_over_catalog_order() {
        let activation = AuthActivationResult {
            provider_id: Some("openai-api".to_string()),
            provider_label: Some("OpenAI".to_string()),
            activated_model: None,
            expected_runtime: None,
            expected_catalog_namespace: None,
        };
        let routes = vec![
            route("gpt-5.1", "OpenAI", "openai-api", true),
            route("gpt-5.5", "OpenAI", "openai-api", true),
            route("gpt-5.6-sol", "OpenAI", "openai-api", true),
        ];

        assert_eq!(
            provider_model_to_select_after_auth(&activation, None, &routes).as_deref(),
            Some("gpt-5.6-sol")
        );
    }

    #[test]
    fn global_default_route_prefers_gpt_5_6_over_fable_and_preserves_route() {
        let routes = vec![
            route("gpt-5.5", "OpenAI", "openai-api-key", true),
            route("claude-fable-5", "Anthropic", "anthropic-api-key", true),
            route("gpt-5.6-sol", "OpenAI", "openai-oauth", true),
        ];

        let selected = globally_preferred_default_route(&routes).expect("strongest route");
        assert_eq!(selected.model, "gpt-5.6-sol");
        assert_eq!(selected.provider, "OpenAI");
        assert_eq!(selected.api_method, "openai-oauth");
    }

    #[test]
    fn global_default_route_uses_clean_gpt_5_6_then_fable_before_weaker_models() {
        let clean_release = vec![
            route("claude-fable-5", "Anthropic", "claude-oauth", true),
            route("gpt-5.6", "OpenAI", "openai-api-key", true),
        ];
        assert_eq!(
            globally_preferred_default_route(&clean_release)
                .as_ref()
                .map(|route| route.model.as_str()),
            Some("gpt-5.6")
        );

        let unavailable_gpt = vec![
            route("gpt-5.6-sol", "OpenAI", "openai-api-key", false),
            route("gpt-5.5", "OpenAI", "openai-api-key", true),
            route("claude-fable-5", "Anthropic", "claude-oauth", true),
        ];
        assert_eq!(
            globally_preferred_default_route(&unavailable_gpt)
                .as_ref()
                .map(|route| route.model.as_str()),
            Some("claude-fable-5")
        );
    }

    #[test]
    fn global_default_route_ignores_unavailable_routes_and_preserves_unknown_order() {
        let routes = vec![
            route("provider-a-frontier", "Provider A", "provider-a", true),
            route("gpt-5.6-sol", "OpenAI", "openai-api-key", false),
            route("provider-b-frontier", "Provider B", "provider-b", true),
        ];

        let selected = globally_preferred_default_route(&routes).expect("fallback route");
        assert_eq!(selected.model, "provider-a-frontier");
        assert_eq!(selected.api_method, "provider-a");
    }

    #[test]
    fn post_auth_model_selection_falls_back_when_quality_first_model_is_unavailable() {
        let claude = activation_for_provider_id("claude-api");
        let claude_routes = vec![
            route("claude-opus-5", "Anthropic", "claude-api", false),
            route("claude-fable-5", "Anthropic", "claude-api", true),
            route("claude-opus-4-8", "Anthropic", "claude-api", true),
        ];
        assert_eq!(
            provider_model_to_select_after_auth(&claude, None, &claude_routes).as_deref(),
            Some("claude-fable-5")
        );

        let openai = activation_for_provider_id("openai-api");
        let openai_routes = vec![
            route("gpt-5.6-sol", "OpenAI", "openai-api", false),
            route("gpt-5.5", "OpenAI", "openai-api", true),
        ];
        assert_eq!(
            provider_model_to_select_after_auth(&openai, None, &openai_routes).as_deref(),
            Some("gpt-5.5")
        );

        let openai_routes_with_clean_release = vec![
            route("gpt-5.6-sol", "OpenAI", "openai-api", false),
            route("gpt-5.5", "OpenAI", "openai-api", true),
            route("gpt-5.6", "OpenAI", "openai-api", true),
        ];
        assert_eq!(
            provider_model_to_select_after_auth(&openai, None, &openai_routes_with_clean_release).as_deref(),
            Some("gpt-5.6"),
            "a clean same-generation release should beat GPT 5.5 when Sol is unavailable"
        );
    }

    #[test]
    fn post_auth_model_selection_keeps_catalog_order_for_unranked_providers() {
        // OpenAI-compatible / namespaced providers have no curated flagship
        // order; the fallback must preserve live-catalog order for them.
        //
        // Selection consults the process-global namespaced catalog, so hold the
        // shared test-env lock (and a private HORON_AGENT_HOME) or a sibling test's
        // cached catalog decides this assertion depending on ordering.
        let _env = EnvGuard::new(&["HORON_AGENT_HOME"]);
        let temp = tempfile::tempdir().expect("tempdir");
        crate::env::set_var("HORON_AGENT_HOME", temp.path());
        let activation = AuthActivationResult {
            provider_id: Some("cerebras".to_string()),
            provider_label: Some("Cerebras".to_string()),
            activated_model: None,
            expected_runtime: Some("openai-compatible".to_string()),
            expected_catalog_namespace: Some("cerebras".to_string()),
        };
        let routes = vec![
            route("llama3.1-8b", "Cerebras", "openai-compatible:cerebras", true),
            route(
                "qwen-3-235b-a22b-instruct-2507",
                "Cerebras",
                "openai-compatible:cerebras",
                true,
            ),
        ];

        assert_eq!(
            provider_model_to_select_after_auth(&activation, None, &routes).as_deref(),
            Some("llama3.1-8b"),
            "providers without a curated flagship order keep live-catalog order"
        );
    }

    #[test]
    fn post_auth_model_selection_prefers_newest_live_release_for_unranked_provider() {
        let _env = EnvGuard::new(&["HORON_AGENT_HOME"]);
        let temp = tempfile::tempdir().expect("tempdir");
        crate::env::set_var("HORON_AGENT_HOME", temp.path());
        crate::provider::catalog_cache::save_disk_cache_with_source_for_namespace(
            "cerebras",
            &[
                crate::provider::catalog_cache::ModelInfo {
                    id: "llama3.1-8b".to_string(),
                    name: String::new(),
                    context_length: None,
                    pricing: Default::default(),
                    created: Some(1_700_000_000),
                    ..Default::default()
                },
                crate::provider::catalog_cache::ModelInfo {
                    id: "qwen-3-235b-a22b-instruct-2507".to_string(),
                    name: String::new(),
                    context_length: None,
                    pricing: Default::default(),
                    created: Some(1_800_000_000),
                    ..Default::default()
                },
            ],
            Some("https://api.cerebras.ai/v1"),
        );

        let activation = AuthActivationResult {
            provider_id: Some("cerebras".to_string()),
            provider_label: Some("Cerebras".to_string()),
            activated_model: None,
            expected_runtime: Some("openai-compatible".to_string()),
            expected_catalog_namespace: Some("cerebras".to_string()),
        };
        let routes = vec![
            route("llama3.1-8b", "Cerebras", "openai-compatible:cerebras", true),
            route(
                "qwen-3-235b-a22b-instruct-2507",
                "Cerebras",
                "openai-compatible:cerebras",
                true,
            ),
        ];

        assert_eq!(
            provider_model_to_select_after_auth(&activation, None, &routes).as_deref(),
            Some("qwen-3-235b-a22b-instruct-2507"),
            "unranked providers should prefer the newest live release when the catalog includes release timestamps"
        );
    }

    #[test]
    fn post_auth_auto_promotes_newer_frontier_release_not_yet_in_curated_list() {
        // The day Anthropic ships a stronger Opus than the curated flagship, the
        // live catalog carries it and it must auto-promote to the post-login
        // default without a code change. Here `claude-opus-4-9` beats the curated
        // baseline `claude-opus-4-8`.
        let activation = activation_for_provider_id("claude-api");
        let routes = vec![
            route("claude-haiku-4-5", "Anthropic", "claude-api", true),
            route("claude-opus-4-8", "Anthropic", "claude-api", true),
            route("claude-opus-4-9", "Anthropic", "claude-api", true),
            route("claude-sonnet-4-6", "Anthropic", "claude-api", true),
        ];
        assert_eq!(
            provider_model_to_select_after_auth(&activation, None, &routes).as_deref(),
            Some("claude-opus-4-9"),
            "a newer pure Opus flagship in the live catalog should auto-promote"
        );

        // Same for OpenAI: a future `gpt-5.7` beats the curated Sol 5.6 baseline.
        let activation = activation_for_provider_id("openai");
        let routes = vec![
            route("gpt-5-mini", "OpenAI", "openai", true),
            route("gpt-5.6-sol", "OpenAI", "openai", true),
            route("gpt-5.7", "OpenAI", "openai", true),
        ];
        assert_eq!(
            provider_model_to_select_after_auth(&activation, None, &routes).as_deref(),
            Some("gpt-5.7")
        );
    }

    #[test]
    fn quality_first_defaults_are_not_displaced_by_lower_family_or_equal_release() {
        let claude = activation_for_provider_id("claude-api");
        let claude_routes = vec![
            route("claude-opus-4-9", "Anthropic", "claude-api", true),
            route("claude-fable-5", "Anthropic", "claude-api", true),
        ];
        assert_eq!(
            provider_model_to_select_after_auth(&claude, None, &claude_routes).as_deref(),
            Some("claude-fable-5"),
            "a newer lower-priority Opus release must not displace available Fable"
        );

        let openai = activation_for_provider_id("openai");
        let openai_routes = vec![
            route("gpt-5.6", "OpenAI", "openai", true),
            route("gpt-5.6-sol", "OpenAI", "openai", true),
        ];
        assert_eq!(
            provider_model_to_select_after_auth(&openai, None, &openai_routes).as_deref(),
            Some("gpt-5.6-sol"),
            "the base model at the same release must not displace the Sol quality profile"
        );
    }

    #[test]
    fn post_auth_frontier_promotion_ignores_cheaper_and_specialized_variants() {
        // A newer *cheaper/specialized* variant must NOT auto-promote over the
        // curated flagship: only clean flagship ids qualify. Even though
        // `claude-haiku-5` and `gpt-6-mini`/`gpt-6-codex` have higher version
        // numbers, they carry non-flagship tier words and must be rejected, so
        // selection stays on the curated flagship.
        let activation = activation_for_provider_id("claude-api");
        let routes = vec![
            route("claude-haiku-5", "Anthropic", "claude-api", true),
            route("claude-opus-4-8", "Anthropic", "claude-api", true),
            route("claude-sonnet-5", "Anthropic", "claude-api", true),
        ];
        assert_eq!(
            provider_model_to_select_after_auth(&activation, None, &routes).as_deref(),
            Some("claude-opus-4-8"),
            "cheaper/other-family models must not auto-promote over the curated Opus flagship"
        );

        let activation = activation_for_provider_id("openai");
        let routes = vec![
            route("gpt-6-mini", "OpenAI", "openai", true),
            route("gpt-6-codex", "OpenAI", "openai", true),
            route("gpt-5.5", "OpenAI", "openai", true),
        ];
        assert_eq!(
            provider_model_to_select_after_auth(&activation, None, &routes).as_deref(),
            Some("gpt-5.5"),
            "mini/codex variants must not auto-promote over the clean gpt flagship"
        );
    }

    #[test]
    fn post_auth_frontier_promotion_no_op_when_curated_is_still_newest() {
        // When the live catalog contains nothing newer than the curated flagship,
        // the curated quality order decides and frontier promotion is a no-op.
        let activation = activation_for_provider_id("claude-api");
        let routes = vec![
            route("claude-haiku-4-5-20251001", "Anthropic", "claude-api", true),
            route("claude-opus-4-6", "Anthropic", "claude-api", true),
            route("claude-opus-4-8", "Anthropic", "claude-api", true),
        ];
        assert_eq!(
            provider_model_to_select_after_auth(&activation, None, &routes).as_deref(),
            Some("claude-opus-4-8")
        );
    }

    #[test]
    fn post_auth_frontier_promotion_covers_bedrock_and_gemini() {
        // Bedrock: a newer Opus 5 (vendor-prefixed + dated) auto-promotes over the
        // curated Opus 4 baseline, and never falls back to the year-old 3.5.
        let activation = activation_for_provider_id("bedrock");
        let routes = vec![
            route(
                "anthropic.claude-3-5-sonnet-20241022-v2:0",
                "AWS Bedrock",
                "bedrock",
                true,
            ),
            route("anthropic.claude-opus-4-20250514-v1:0", "AWS Bedrock", "bedrock", true),
            route("anthropic.claude-opus-5-20260101-v1:0", "AWS Bedrock", "bedrock", true),
        ];
        assert_eq!(
            provider_model_to_select_after_auth(&activation, None, &routes).as_deref(),
            Some("anthropic.claude-opus-5-20260101-v1:0"),
            "a newer Bedrock Opus must auto-promote over the curated Opus 4"
        );

        // Gemini: a newer pro auto-promotes; a newer flash never displaces it.
        let activation = activation_for_provider_id("gemini");
        let routes = vec![
            route("gemini-2.5-flash", "Google Gemini", "code-assist-oauth", true),
            route("gemini-3-pro-preview", "Google Gemini", "code-assist-oauth", true),
            route("gemini-4-pro-preview", "Google Gemini", "code-assist-oauth", true),
            route("gemini-9-flash-preview", "Google Gemini", "code-assist-oauth", true),
        ];
        assert_eq!(
            provider_model_to_select_after_auth(&activation, None, &routes).as_deref(),
            Some("gemini-4-pro-preview"),
            "the newest Gemini *pro* must win; a higher-numbered flash must not"
        );
    }

    #[test]
    fn frontier_version_parsing_and_compare() {
        let fams = &[
            FrontierFamily {
                prefix: "claude-opus",
                flagship_token: None,
            },
            FrontierFamily {
                prefix: "gpt",
                flagship_token: None,
            },
        ];
        // Clean flagship ids parse with a version vector.
        let opus = parse_frontier_model("claude-opus-4-8", fams).expect("opus parses");
        assert_eq!(opus.family, "claude-opus");
        assert_eq!(opus.version, vec![4, 8]);
        let gpt = parse_frontier_model("gpt-5.5", fams).expect("gpt parses");
        assert_eq!(gpt.family, "gpt");
        assert_eq!(gpt.version, vec![5, 5]);
        // Dated id parses on the canonical base.
        assert_eq!(
            parse_frontier_model("claude-opus-4-9-20260101", fams)
                .expect("dated opus parses")
                .version,
            vec![4, 9]
        );
        // Specialized/cheap tiers and other families are rejected.
        assert!(parse_frontier_model("claude-haiku-5", fams).is_none());
        assert!(parse_frontier_model("gpt-6-mini", fams).is_none());
        assert!(parse_frontier_model("gpt-5-codex", fams).is_none());
        assert!(parse_frontier_model("claude-sonnet-5", fams).is_none());
        // Version comparison is component-wise with zero-padding.
        assert_eq!(version_cmp(&[4, 8], &[4, 9]), std::cmp::Ordering::Less);
        assert_eq!(version_cmp(&[5], &[5, 1]), std::cmp::Ordering::Less);
        assert_eq!(version_cmp(&[6], &[5, 9]), std::cmp::Ordering::Greater);
        assert_eq!(version_cmp(&[5, 5], &[5, 5]), std::cmp::Ordering::Equal);

        // Bedrock vendor-prefixed/versioned ids normalize to the bare Claude
        // family and parse as flagship.
        let bedrock = parse_frontier_model(
            "us.anthropic.claude-opus-4-20250514-v1:0",
            &[FrontierFamily {
                prefix: "claude-opus",
                flagship_token: None,
            }],
        )
        .expect("bedrock opus parses");
        assert_eq!(bedrock.version, vec![4]);

        // Gemini flagship token: `pro` is required and `flash`/`lite` are rejected.
        let gem_fams = &[FrontierFamily {
            prefix: "gemini",
            flagship_token: Some("pro"),
        }];
        let gpro = parse_frontier_model("gemini-3-pro-preview", gem_fams).expect("gemini pro");
        assert_eq!(gpro.family, "gemini");
        assert_eq!(gpro.version, vec![3]);
        assert_eq!(
            parse_frontier_model("gemini-3.1-pro", gem_fams)
                .expect("gemini 3.1 pro")
                .version,
            vec![3, 1]
        );
        assert!(
            parse_frontier_model("gemini-3-flash", gem_fams).is_none(),
            "gemini flash is the cheap tier and must not be a frontier flagship"
        );
        assert!(
            parse_frontier_model("gemini-2.5-flash-lite", gem_fams).is_none(),
            "gemini flash-lite must be rejected"
        );
    }

    #[test]
    fn normalize_model_for_preference_strips_hosted_prefixes_and_suffixes() {
        assert_eq!(
            normalize_model_for_preference("us.anthropic.claude-opus-4-20250514-v1:0"),
            "claude-opus-4"
        );
        assert_eq!(
            normalize_model_for_preference("anthropic.claude-3-5-sonnet-20241022-v2:0"),
            "claude-3-5-sonnet"
        );
        assert_eq!(
            normalize_model_for_preference("models/gemini-3-pro-preview"),
            "gemini-3-pro"
        );
        assert_eq!(
            normalize_model_for_preference("accounts/fireworks/models/qwen3-coder"),
            "qwen3-coder"
        );
        // Non-hosted ids are unchanged apart from canonicalization.
        assert_eq!(
            normalize_model_for_preference("claude-haiku-4-5-20251001"),
            "claude-haiku-4-5"
        );
        assert_eq!(normalize_model_for_preference("gpt-5.5"), "gpt-5.5");
    }

    #[test]
    fn preferred_model_for_provider_auto_promotes_unlisted_releases() {
        assert_eq!(
            preferred_model_for_provider("openrouter", &["anthropic/claude-opus-5.5", "anthropic/claude-opus-6"])
                .as_deref(),
            Some("anthropic/claude-opus-6")
        );
        assert_eq!(
            preferred_model_for_provider("grok-build", &["grok-4.7", "grok-5"]).as_deref(),
            Some("grok-5")
        );
        assert_eq!(
            preferred_model_for_provider("grok-build", &["grok-4.7", "grok-code-fast-9"]).as_deref(),
            Some("grok-4.7"),
            "specialized variants must not self-promote"
        );
    }

    #[test]
    fn preferred_model_for_provider_unknown_provider_defers_to_caller() {
        assert_eq!(preferred_model_for_provider("some-local-llm", &["a", "b"]), None);
    }

    /// Copilot proxies both families; the cross-family tie-break must prefer the
    /// Claude flagship over the OpenAI flagship to mirror horon-agent's default model.
    #[test]
    fn post_auth_model_selection_copilot_prefers_claude_family_over_openai() {
        let activation = activation_for_provider_id("copilot");
        let routes = vec![
            route("gpt-5.5", "Copilot", "copilot", true),
            route("claude-opus-4-8", "Copilot", "copilot", true),
        ];
        assert_eq!(
            provider_model_to_select_after_auth(&activation, None, &routes).as_deref(),
            Some("claude-opus-4-8"),
            "copilot tie-break should prefer the Claude flagship family first"
        );
    }
}

#[cfg(test)]
mod real_catalog_tests {
    /// Snapshot of the real OpenRouter `/models` catalog (466 ids, Oct 2026).
    /// Includes the hazards a synthetic list misses: `:batch` variants,
    /// `~anthropic/claude-opus-latest` aliases, and newer non-Claude releases
    /// (`mistralai/mistral-large-4-0`) that recency alone would pick.
    const OPENROUTER_IDS: &str = include_str!("openrouter_catalog_snapshot.json");

    #[test]
    fn real_openrouter_catalog_ranks_to_claude_opus_flagship() {
        let ids: Vec<String> = serde_json::from_str(OPENROUTER_IDS).expect("snapshot");
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        assert_eq!(
            super::ranked_flagship_for_provider("openrouter", &refs).as_deref(),
            Some("anthropic/claude-opus-5.5")
        );
    }
}
