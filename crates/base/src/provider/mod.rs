mod accessors;
mod account_failover;
mod account_selection;
pub mod activation;
pub(crate) mod catalog_cache;
mod catalog_memo;
mod catalog_routes;
pub use catalog_memo::bump_catalog_generation;
use catalog_memo::{RoutesMemoEntry, catalog_generation, next_catalog_scope};
pub mod catalog_scheduler;
mod dispatch;
pub mod external;
mod failover;
mod fingerprint;
pub mod horon_agent;
mod image_clamp;
pub mod models;
mod multi_provider;
pub mod openai;
pub mod openai_request;
pub mod pricing;
mod registry;
mod route_builders;
mod selection;
mod startup;
mod state;
mod stream_start;
mod stream_timeout;

use crate::auth;
use crate::message::{Message, ToolDefinition};
use account_failover::{
    account_usage_probe, active_account_label_for_provider, maybe_annotate_limit_summary,
    same_provider_account_failover_enabled,
};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
#[cfg(test)]
use horon_agent_provider_core::FailoverDecision;
use registry::ProviderRegistry;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

pub use account_failover::{has_alternate_account_in_pool, has_eligible_alternate_account};
pub use catalog_routes::{
    remote_current_openai_compatible_route_for_model, remote_model_routes_fallback,
    remote_model_routes_lightweight_fallback, remote_openai_compatible_route_for_model,
    simplified_model_routes_for_picker,
};
pub use horon_agent_provider_core::ProviderFailoverPrompt;
pub use horon_agent_provider_core::attempt_tracker;
pub use horon_agent_provider_core::cli_provider_arg_for_session_key;
pub use horon_agent_provider_core::failover::{
    FailoverRequired, failover_required_from_error, parse_failover_prompt_message,
};
pub use horon_agent_provider_core::{
    ALL_CLAUDE_MODELS, ALL_OPENAI_MODELS, CHATGPT_WEB_MODEL, CHEAPNESS_REFERENCE_INPUT_TOKENS,
    CHEAPNESS_REFERENCE_OUTPUT_TOKENS, CredentialMode, DEFAULT_CONTEXT_LIMIT, EventStream, HORON_AGENT_USER_AGENT,
    ModelCapabilities, ModelCatalogRefreshSummary, ModelRoute, ModelRouteApiMethod, NativeCompactionResult,
    NativeToolResult, NativeToolResultSender, PremiumMode, Provider, ProviderRequestContext, RouteBillingKind,
    RouteCheapnessEstimate, RouteCostConfidence, RouteCostSource, RouteSelection, RuntimeKey, dedupe_model_routes,
    explicit_model_provider_prefix, fresh_transport_client, grok_build_model_spec, inferred_reasoning_efforts,
    model_name_for_provider, normalize_copilot_model_name, provider_from_model_key, shared_http_client,
    summarize_model_catalog_refresh,
};
pub use horon_agent_provider_core::{
    FallbackPickOptions, error_looks_like_credential_failure, model_route_provider_labels_match,
    normalize_model_route_provider_label, pick_next_fallback_route, pick_next_fallback_route_with_options,
};
pub use route_builders::{
    build_chatgpt_web_route, build_openai_api_key_route, build_openai_oauth_route, is_listable_model_name,
    listable_model_names_from_routes, openrouter_catalog_model_id,
};

/// Process-wide handle to the live agent provider.
///
/// The memory sidecar ([`crate::sidecar::Sidecar`]) needs to make small,
/// cheap model calls (rerank / relevance / extraction). It has a dedicated fast
/// path for OpenAI (codex-spark) OAuth, so the sidecar falls
/// back to *this* handle and dispatches through the already-working
/// [`Provider::complete_simple`] path. `Server::new` registers the active
/// provider here at startup.
static ACTIVE_PROVIDER: RwLock<Option<Arc<dyn Provider>>> = RwLock::new(None);

/// Register the live agent provider so background helpers (memory sidecar) can
/// reach whatever provider the user is actually running on. Safe to call more
/// than once; the most recent registration wins.
pub fn set_active_provider(provider: Arc<dyn Provider>) {
    *ACTIVE_PROVIDER.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(provider);
}

/// Fetch the registered active provider, if any. Returns a forked handle so the
/// caller gets an independent provider instance (per the [`Provider::fork`]
/// contract) that will not interfere with the main agent's model selection.
pub fn active_provider_fork() -> Option<Arc<dyn Provider>> {
    ACTIVE_PROVIDER
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .map(|p| p.fork())
}

/// Provider-agnostic streaming idle-timeout budgets. See
/// [`stream_timeout`] for the base budget and the reasoning-effort scaling that
/// keeps long silent thinks from looking like dead connections (issue #434).
pub use stream_timeout::{
    MAX_STREAM_IDLE_TIMEOUT_MULTIPLIER, max_stream_idle_timeout, stream_idle_timeout, stream_idle_timeout_for_effort,
    stream_idle_timeout_multiplier_for_effort,
};

/// Whether reasoning deltas should be persisted in session history for later
/// provider context reconstruction.
///
/// Whether reasoning deltas should be persisted in session history for later
/// provider context reconstruction.
///
/// Display is controlled separately by `display.show_thinking`. Persist only
/// when a provider request builder can safely send the stored block back in
/// the provider-native shape.
pub fn stores_reasoning_content_for_context(provider_name: &str) -> bool {
    if !crate::config::config().provider.preserve_reasoning_context {
        return false;
    }
    matches!(provider_name.to_ascii_lowercase().as_str(), "openai")
}

// Keep inactive direct profiles on the same 15-minute soft-refresh cadence as
// the active OpenAI-compatible runtime. We continue serving the
// cached routes immediately while a background refresh updates the catalog.
pub(crate) const OPENAI_COMPATIBLE_PROFILE_CATALOG_SOFT_REFRESH_SECS: u64 = 15 * 60;

fn openai_compatible_profile_catalog_cache_is_stale(cached_at: u64, now: u64) -> bool {
    now.saturating_sub(cached_at) >= OPENAI_COMPATIBLE_PROFILE_CATALOG_SOFT_REFRESH_SECS
}

pub(crate) fn cached_live_models_for_openai_compatible_profile(
    resolved: &crate::provider_catalog::ResolvedOpenAiCompatibleProfile,
) -> Option<(Vec<String>, bool)> {
    let cache = catalog_cache::load_disk_cache_entry_for_namespace(&resolved.id)?;
    let cache_is_stale = catalog_cache::current_unix_secs()
        .map(|now| openai_compatible_profile_catalog_cache_is_stale(cache.cached_at, now))
        .unwrap_or(false);
    let source_api_base = cache
        .source_api_base
        .as_deref()
        .and_then(crate::provider_catalog::normalize_api_base)?;
    let expected_api_base = crate::provider_catalog::normalize_api_base(&resolved.api_base)?;
    if source_api_base != expected_api_base {
        return None;
    }

    let models = cache
        .models
        .into_iter()
        .map(|model| model.id.trim().to_string())
        .filter(|model| !model.is_empty())
        .collect::<Vec<_>>();
    if models.is_empty() {
        None
    } else {
        Some((models, cache_is_stale))
    }
}

fn direct_openai_compatible_profile_routes(
    profile: crate::provider_catalog::OpenAiCompatibleProfile,
) -> Vec<ModelRoute> {
    let resolved = crate::provider_catalog::resolve_openai_compatible_profile(profile);
    let static_models = crate::provider_catalog::openai_compatible_profile_static_models(profile);
    // Pure read: the catalog scheduler owns refresh cadence, so rendering
    // routes cannot fan out HTTP requests.
    let (mut models, from_live_catalog) =
        if let Some((models, _cache_is_stale)) = cached_live_models_for_openai_compatible_profile(&resolved) {
            (models, true)
        } else {
            let mut models = static_models;
            if models.is_empty()
                && let Some(default_model) = resolved.default_model.as_ref()
                && !default_model.trim().is_empty()
            {
                models.push(default_model.trim().to_string());
            }
            (models, false)
        };

    let provider = resolved.display_name.clone();
    let api_method = format!("openai-compatible:{}", resolved.id);
    let detail = if from_live_catalog {
        resolved.api_base.clone()
    } else if resolved.api_base.trim().is_empty() {
        "fallback: static provider model list".to_string()
    } else {
        format!("{}; fallback: static provider model list", resolved.api_base)
    };

    let mut routes = Vec::new();
    for model in models.drain(..) {
        if !is_listable_model_name(&model)
            || !crate::provider_catalog::openai_compatible_profile_model_supports_chat(&resolved.id, &model)
            || routes.iter().any(|route: &ModelRoute| route.model == model)
        {
            continue;
        }

        routes.push(ModelRoute {
            model,
            provider: provider.clone(),
            api_method: api_method.clone(),
            available: true,
            detail: detail.clone(),
            usage: None,
            cheapness: None,
        });
    }

    routes
}

/// Strip an explicit HoronAgent subscription prefix (`horon-agent:` or
/// `horon-agent-subscription:`) from a model spec.
pub(crate) fn horon_agent_subscription_model_prefix(model: &str) -> Option<&str> {
    let (prefix, rest) = model.split_once(':')?;
    let prefix = prefix.trim();
    if !(prefix.eq_ignore_ascii_case("horon-agent") || prefix.eq_ignore_ascii_case("horon-agent-subscription")) {
        return None;
    }
    let rest = rest.trim();
    (!rest.is_empty()).then_some(rest)
}

use self::dispatch::CompletionMode;

mod session_restore;
pub use self::models::{
    AccountModelAvailability, AccountModelAvailabilityState, ModelCatalogHttpStatus, OpenAIModelCatalog,
    begin_openai_model_catalog_refresh, cached_context_limit_for_model, cached_openai_model_ids,
    cached_openai_reasoning_efforts, clear_all_model_unavailability_for_account,
    clear_all_provider_unavailability_for_account, clear_claude_provider_unavailability_for_account_label,
    clear_model_unavailable_for_account, clear_openai_provider_unavailability_for_account_label,
    clear_provider_unavailable_for_account, context_limit_for_model, context_limit_for_model_with_provider,
    fetch_openai_model_catalog, finish_openai_model_catalog_refresh, format_account_model_availability_detail,
    get_best_available_openai_model, is_model_available_for_account, known_openai_model_ids,
    model_availability_for_account, model_unavailability_detail_for_account, note_openai_model_catalog_refresh_attempt,
    openai_platform_api_key_configured, persist_openai_model_catalog, populate_account_models, populate_context_limits,
    populate_context_limits_from_config, populate_context_limits_from_config_value, provider_for_model,
    provider_for_model_with_hint, provider_unavailability_detail_for_account,
    provider_unavailability_detail_for_account_label, record_model_unavailable_for_account,
    record_provider_unavailable_for_account, record_provider_unavailable_for_account_label, resolve_model_capabilities,
    should_refresh_openai_model_catalog,
};
pub use self::models::{
    begin_openai_model_catalog_refresh_for_scope, cached_openai_model_ids_for_scope,
    cached_openai_reasoning_efforts_for_scope, clear_model_unavailable_for_scope,
    finish_openai_model_catalog_refresh_for_scope, get_best_available_openai_model_for_scope,
    known_openai_model_ids_for_scope, model_availability_for_scope, openai_catalog_scope_for_credential,
    persist_openai_model_catalog_for_scope, populate_account_models_for_scope, record_model_unavailable_for_scope,
    should_refresh_openai_model_catalog_for_scope,
};
pub use self::selection::DefaultModelSelection;
use self::selection::{ActiveProvider, ProviderAvailability};
use self::state::ProviderState;
pub use self::state::{ProviderModelSelectionSource, ProviderRuntimeState, ProviderStateEvent};
pub use session_restore::*;

/// MultiProvider wraps multiple providers and allows seamless model switching
pub struct MultiProvider {
    openai: RwLock<Option<Arc<dyn Provider>>>,
    /// Direct OpenAI-compatible runtimes keyed by profile id.
    ///
    /// These speak the OpenAI-compatible wire protocol but are distinct runtime
    /// identities: keying them by profile id prevents a compatible endpoint
    /// selection from corrupting later model switches, catalog display, or auth
    /// refresh handling.
    openai_compatible_profiles: RwLock<HashMap<String, Arc<dyn Provider>>>,
    active_openai_compatible_profile: RwLock<Option<String>>,
    active: RwLock<ActiveProvider>,
    /// Notifications generated during provider/account auto-selection.
    /// The TUI should drain and display these on session start.
    startup_notices: RwLock<Vec<String>>,
    /// CLI/environment selection to use when creating fresh sessions. This is
    /// only an initial preference and never restricts later model switches.
    initial_provider: Option<ActiveProvider>,
    /// Short-TTL memo for the full route-catalog build.
    ///
    /// Building the catalog is expensive (per-route pricing lookups, endpoint
    /// cache reads, credential probes) and the shared server rebuilds it for
    /// every connection whenever a `ModelsUpdated` bus event fans out. During
    /// a burst of client spawns that multiplied into hundreds of builds within
    /// a couple of seconds, saturating every core. The memo collapses those
    /// into one build per TTL window; auth/model changes invalidate it
    /// explicitly so pickers never see stale routes after a switch.
    /// Unique configuration lineage, preserved only by equivalent forks.
    catalog_scope: u64,
    routes_memo: Mutex<Option<RoutesMemoEntry>>,
    /// Number of model-catalog prefetches launched by the latest auth refresh.
    /// Shared by forks so the server can wait for real work rather than sleeping
    /// through a fixed quiet period after login.
    post_auth_refreshes_pending: Arc<std::sync::atomic::AtomicUsize>,
}

impl MultiProvider {
    #[cfg(test)]
    fn same_provider_account_candidates(provider: ActiveProvider) -> Vec<String> {
        account_failover::same_provider_account_candidates(provider)
    }

    async fn complete_with_failover(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        mode: CompletionMode<'_>,
        resume_session_id: Option<&str>,
        context: Option<&ProviderRequestContext>,
    ) -> Result<EventStream> {
        self.bind_auto_account_for_request(self.active_provider())?;
        self.spawn_openai_catalog_refresh_if_needed();

        // Provider capabilities are authoritative at this request chokepoint.
        // Keep images in persisted history, but replace them in the ephemeral
        // request snapshot when the selected model/provider is text-only (#755).
        let filtered_messages = image_clamp::filter_unsupported_outbound_images(messages, self.supports_image_input());
        let messages: &[Message] = filtered_messages.as_deref().unwrap_or(messages);

        // Downscale any images whose pixel dimensions exceed provider per-image
        // limits before they reach the wire. Resuming a session with >20 large
        // screenshots otherwise trips a provider's many-image pixel cap and the
        // whole turn is rejected (#381). Only clones when a clamp is required.
        let clamped_messages = image_clamp::clamp_outbound_images(messages);
        let messages: &[Message] = clamped_messages.as_deref().unwrap_or(messages);

        // Deferred definitions are only meaningful to providers with native
        // deferred loading. Never let them reach a provider that would send
        // them as ordinary eager tools (which would also defeat the point).
        let eager_tools;
        let tools: &[ToolDefinition] = if !self.supports_deferred_tools() && tools.iter().any(|tool| tool.defer_loading)
        {
            eager_tools = ToolDefinition::eager(tools);
            &eager_tools
        } else {
            tools
        };

        let active = self.active_provider();
        // Saved account requests remain session-local. Only configured pool
        // members may propose another exact account after a startup failure.
        if let Some(bound_label) = self.selected_account_for_provider(active) {
            let key = Self::provider_key(active);
            let exists = crate::auth::codex::list_accounts()?
                .iter()
                .any(|a| a.label == bound_label);
            if !exists {
                anyhow::bail!(
                    "Saved account '{}' no longer exists. Select an account with /model.",
                    bound_label
                );
            }
            if let Some(reason) = models::provider_unavailability_detail_for_account_label(key, Some(&bound_label)) {
                self.try_same_provider_account_failover(active, messages, tools, mode, &reason, &mut Vec::new())
                    .await?;
                anyhow::bail!(
                    "{} ({}) is unavailable: {}",
                    Self::provider_label(active),
                    bound_label,
                    reason
                );
            }
            let attempt = match mode {
                CompletionMode::Unified { system } => {
                    self.complete_on_provider(active, messages, tools, system, resume_session_id, context)
                        .await
                }
                CompletionMode::Split {
                    system_static,
                    system_dynamic,
                } => {
                    self.complete_split_on_provider(
                        active,
                        messages,
                        tools,
                        system_static,
                        system_dynamic,
                        resume_session_id,
                        context,
                    )
                    .await
                }
            };
            let attempt = self.await_account_stream_start(active, attempt).await;
            match attempt {
                Ok(stream) => {
                    if active == ActiveProvider::OpenAI {
                        models::clear_openai_provider_unavailability_for_account_label(Some(&bound_label));
                    }
                    return Ok(stream);
                }
                Err(err) => {
                    let decision = Self::classify_failover_error(&err);
                    if decision.should_failover() {
                        let reason = Self::summarize_error(&err);
                        if decision.should_mark_provider_unavailable() {
                            models::record_provider_unavailable_for_account_label(key, Some(&bound_label), &reason);
                        }
                        self.try_same_provider_account_failover(
                            active,
                            messages,
                            tools,
                            mode,
                            &reason,
                            &mut Vec::new(),
                        )
                        .await?;
                    }
                    return Err(err);
                }
            }
        }
        let sequence = Self::fallback_sequence(active);
        let mut notes: Vec<String> = Vec::new();
        let mut failover_reason: Option<String> = None;
        let (estimated_input_chars, estimated_input_tokens) = Self::estimate_request_input(messages, tools, mode);

        for candidate in sequence {
            let label = Self::provider_label(candidate);
            let key = self.provider_runtime_cooldown_key(candidate);

            if candidate != active {
                // Account rotation may have exhausted the pool since the first
                // error. Render guidance from current cooldowns, never from an
                // annotation captured before alternate logins were attempted.
                let reason = failover_reason
                    .clone()
                    .unwrap_or_else(|| "provider unavailable".to_string());
                let reason = if self.provider_uses_oauth_accounts(active) {
                    maybe_annotate_limit_summary(active, reason)
                } else {
                    reason
                };
                if let Some(prompt) = self.cross_provider_pool_prompt(
                    active,
                    candidate,
                    reason,
                    estimated_input_chars,
                    estimated_input_tokens,
                ) {
                    return Err(anyhow::Error::new(FailoverRequired { prompt }));
                }
                continue;
            }

            if !self.provider_is_configured(candidate) {
                let note = format!("{}: not configured", label);
                if candidate == active {
                    crate::logging::warn(&format!(
                        "Failover{}: skipping active provider {} (not configured)",
                        mode.log_suffix(),
                        label
                    ));
                }
                notes.push(note);
                continue;
            }

            let cached_unavailability = provider_unavailability_detail_for_account(key);
            if let Some(reason) = cached_unavailability.clone() {
                let note = format!("{}: {}", label, reason);
                if candidate == active {
                    crate::logging::warn(&format!(
                        "Failover{}: skipping active provider {} - {}",
                        mode.log_suffix(),
                        label,
                        reason
                    ));
                    failover_reason = Some(reason.clone());
                }
                notes.push(note);
                if cached_unavailability.is_none() {
                    record_provider_unavailable_for_account(key, &reason);
                }
                if candidate == active
                    && let Some(stream) = self
                        .try_same_provider_account_failover(candidate, messages, tools, mode, &reason, &mut notes)
                        .await?
                {
                    return Ok(stream);
                }
                continue;
            }

            let attempt = match mode {
                CompletionMode::Unified { system } => {
                    self.complete_on_provider(candidate, messages, tools, system, resume_session_id, context)
                        .await
                }
                CompletionMode::Split {
                    system_static,
                    system_dynamic,
                } => {
                    self.complete_split_on_provider(
                        candidate,
                        messages,
                        tools,
                        system_static,
                        system_dynamic,
                        resume_session_id,
                        context,
                    )
                    .await
                }
            };
            let attempt = self.await_account_stream_start(candidate, attempt).await;

            match attempt {
                Ok(stream) => {
                    clear_provider_unavailable_for_account(key);
                    self.record_provider_activity(candidate);
                    if candidate != active {
                        self.set_active_provider(candidate);
                        let from_label = Self::provider_label(active);
                        let to_label = Self::provider_label(candidate);
                        crate::logging::info(&format!(
                            "{}: switched from {} to {}",
                            mode.switch_log_prefix(),
                            from_label,
                            to_label
                        ));
                        self.startup_notices
                            .write()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .push(format!(
                                "⚡ Auto-fallback: {} unavailable, switched to {}",
                                from_label, to_label
                            ));
                    }
                    return Ok(stream);
                }
                Err(err) => {
                    let summary = Self::summarize_error(&err);
                    let display_summary = if self.provider_uses_oauth_accounts(candidate) {
                        maybe_annotate_limit_summary(candidate, summary.clone())
                    } else {
                        summary.clone()
                    };
                    let decision = Self::classify_failover_error(&err);
                    crate::logging::info(&format!(
                        "Provider {} failed{}: {} (failover={} decision={})",
                        label,
                        mode.log_suffix(),
                        display_summary,
                        decision.should_failover(),
                        decision.as_str()
                    ));
                    notes.push(format!("{}: {}", label, display_summary));
                    if decision.should_failover() {
                        if decision.should_mark_provider_unavailable() {
                            record_provider_unavailable_for_account(key, &summary);
                        }
                        if candidate == active
                            && decision.should_mark_provider_unavailable()
                            && let Some(stream) = self
                                .try_same_provider_account_failover(
                                    candidate, messages, tools, mode, &summary, &mut notes,
                                )
                                .await?
                        {
                            return Ok(stream);
                        }
                        if candidate == active {
                            failover_reason = Some(summary);
                        }
                    } else {
                        return Err(err);
                    }
                }
            }
        }

        Err(self.no_provider_available_error(&notes))
    }

    /// Record which login/credential just served a request in the
    /// cross-provider activity ledger (drives `/usage` recency sorting).
    /// Spawned off-thread: the ledger does file IO and a request was already
    /// accepted, so this must never block or fail the completion path.
    fn record_provider_activity(&self, provider: ActiveProvider) {
        let source_key = self.activity_source_key(provider);
        tokio::task::spawn_blocking(move || {
            crate::provider_activity::record_use(&source_key);
        });
    }

    /// Ledger source key for the credential `provider` will use right now.
    /// Mirrors `active_resolved_credential` for the dual-auth providers and
    /// the runtime profile resolution for the shared compatible-profile slot, but resolves
    /// against the *passed* provider so failover candidates attribute
    /// correctly even before `set_active_provider` runs.
    fn activity_source_key(&self, _provider: ActiveProvider) -> String {
        if let Some(profile_id) = ProviderRegistry::new(self).active_compatible_profile_id() {
            return format!("provider-profile:{profile_id}");
        }
        let uses_api_key = self
            .openai_provider()
            .map(|openai| match openai.credential_mode() {
                openai::OpenAICredentialMode::ApiKey => true,
                openai::OpenAICredentialMode::OAuth => false,
                openai::OpenAICredentialMode::Auto => crate::auth::codex::load_oauth_credentials().is_err(),
            })
            .unwrap_or(false);
        if uses_api_key {
            "openai:api-key".to_string()
        } else {
            let label = crate::auth::codex::active_account_label().unwrap_or_else(|| "default".to_string());
            format!("openai:oauth:{}", label)
        }
    }

    fn openai_compatible_model_prefix(model: &str) -> Option<(crate::provider_catalog::OpenAiCompatibleProfile, &str)> {
        let (prefix, rest) = model.split_once(':')?;
        if explicit_model_provider_prefix(model).is_some() {
            return None;
        }
        let rest = rest.trim();
        if rest.is_empty() {
            return None;
        }

        let profile = crate::provider_catalog::openai_compatible_profile_by_id(prefix)?;
        Some((profile, rest))
    }

    /// Find the configured OpenAI-compatible profile that serves a bare model
    /// id, using the live route catalog as the source of truth.
    ///
    /// Route specs from the picker carry a `<profile>:<model>` prefix, but
    /// hand-typed `/model <id>` and saved sessions can carry the bare id. The
    /// active profile wins when several profiles serve the same id, so a
    /// re-select of the current model never silently hops endpoints.
    fn openai_compatible_profile_owning_model(
        &self,
        model: &str,
    ) -> Option<crate::provider_catalog::OpenAiCompatibleProfile> {
        let model = model.trim();
        if model.is_empty() {
            return None;
        }

        let active_profile_id = ProviderRegistry::new(self).active_compatible_profile_id();
        let mut fallback: Option<String> = None;
        for route in self.fresh_routes_memo_entry().routes {
            if !route.available || route.model != model {
                continue;
            }
            let Some(profile_id) = route
                .api_method
                .strip_prefix("openai-compatible:")
                .map(str::trim)
                .filter(|profile_id| !profile_id.is_empty())
            else {
                continue;
            };
            if active_profile_id.as_deref() == Some(profile_id) {
                fallback = Some(profile_id.to_string());
                break;
            }
            if fallback.is_none() {
                fallback = Some(profile_id.to_string());
            }
        }

        crate::provider_catalog::openai_compatible_profile_by_id(&fallback?)
    }

    /// Parse a `<name>:<model>` spec whose prefix is a user-defined named
    /// provider profile from config (`[providers.<name>]`). Built-in provider
    /// prefixes and catalog profile ids take precedence and never reach here.
    fn named_provider_profile_model_prefix(model: &str) -> Option<(String, String)> {
        let (prefix, rest) = model.split_once(':')?;
        if explicit_model_provider_prefix(model).is_some()
            || model
                .split_once(':')
                .is_some_and(|(method, _)| horon_agent_provider_core::oauth_account_label(method).is_some())
            || Self::openai_compatible_model_prefix(model).is_some()
        {
            return None;
        }
        let prefix = prefix.trim();
        let rest = rest.trim();
        if prefix.is_empty() || rest.is_empty() {
            return None;
        }
        crate::config::config()
            .providers
            .contains_key(prefix)
            .then(|| (prefix.to_string(), rest.to_string()))
    }

    /// Bind (or reuse) the runtime for a named config provider profile and
    /// select `model` on it (issue #444).
    fn set_model_on_named_provider_profile(&self, profile_name: &str, model: &str) -> Result<()> {
        let model = model.trim();
        if model.is_empty() {
            anyhow::bail!("Model cannot be empty");
        }
        let config = crate::config::config()
            .providers
            .get(profile_name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Unknown provider profile '{}'", profile_name))?;

        let registry = ProviderRegistry::new(self);
        let provider = external::build_named_openai_compatible_runtime(profile_name, &config)?;
        registry.install_compatible_profile(profile_name.to_string(), Arc::clone(&provider));
        provider.set_model(model)?;
        registry.set_active_compatible_profile(profile_name.to_string());
        self.set_active_provider(ActiveProvider::OpenAI);
        Ok(())
    }

    fn set_model_on_provider(&self, provider: ActiveProvider, model: &str) -> Result<()> {
        self.set_model_on_provider_with_credential_modes(provider, model, None, true)
    }

    /// Bind an explicitly prefixed OpenAI spec (`openai:`, `openai-api:`) to
    /// the built-in OpenAI runtime.
    ///
    /// The prefix names the provider the caller asked for, so the shared slot's
    /// active direct profile must not absorb the request. Reusing
    /// `execution_provider()` here retargeted an active OpenAI-compatible
    /// profile and reported its display name while the caller believed it had
    /// selected built-in OpenAI. Without the built-in runtime the switch must
    /// fail and say so instead of silently landing on the profile.
    fn set_model_on_explicit_provider(&self, provider: ActiveProvider, model: &str) -> Result<()> {
        self.set_model_on_provider_with_credential_modes(provider, model, None, false)
    }

    /// Bind the shared OpenAI-compatible slot to the managed horon-agent endpoint.
    ///
    /// Subscription model ids intentionally overlap with [OI] ids.
    /// A picker selection therefore cannot be reduced to a bare
    /// model name: doing so lets `set_model`'s family heuristic spend the
    /// user's unrelated provider credentials instead of their subscription.
    fn set_model_on_horon_agent_subscription(&self, model: &str) -> Result<()> {
        let model = model.trim();
        if model.is_empty() {
            anyhow::bail!("Model cannot be empty");
        }
        models::ensure_model_allowed_for_subscription(model)?;
        crate::subscription_catalog::apply_runtime_env();
        crate::provider::activation::ProviderActivation::horon_agent_subscription(model).apply_env()?;

        // Always construct a fresh environment-derived runtime. Reusing the
        // slot is unsafe because it may currently hold another
        // OpenAI-compatible profile with different credentials and endpoint.
        let base_url = std::env::var("HORON_AGENT_OPENROUTER_API_BASE")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(crate::subscription_catalog::configured_api_base)
            .unwrap_or_else(|| crate::subscription_catalog::DEFAULT_HORON_AGENT_API_BASE.to_string());
        let key_env = std::env::var("HORON_AGENT_OPENROUTER_API_KEY_NAME")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| crate::subscription_catalog::HORON_AGENT_API_KEY_ENV.to_string());
        let env_file = std::env::var("HORON_AGENT_OPENROUTER_ENV_FILE")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| crate::subscription_catalog::HORON_AGENT_ENV_FILE.to_string());
        let api_key = crate::provider_catalog::load_api_key_from_env_or_config(&key_env, &env_file);
        if api_key.is_none() {
            anyhow::bail!(
                "The HoronAgent subscription requires an API key, but {key_env} is not set. \
                 Run `horon-agent login` or set {key_env}."
            );
        }
        let models = crate::subscription_catalog::curated_models()
            .iter()
            .map(|entry| entry.id.to_string())
            .collect::<Vec<_>>();
        let runtime = external::build_direct_openai_compatible_runtime(external::DirectOpenAiCompatibleSpec {
            base_url,
            auth: horon_agent_provider_openai::ChatAuth::Bearer,
            api_key,
            display_name: crate::subscription_catalog::HORON_AGENT_PROVIDER_DISPLAY_NAME.to_string(),
            profile_id: crate::subscription_catalog::HORON_AGENT_ROUTE_API_METHOD.to_string(),
            default_model: model.to_string(),
            models,
            headers: Vec::new(),
            extra_body: None,
        })?;
        runtime.set_model(model)?;

        let registry = ProviderRegistry::new(self);
        registry.install_compatible_profile(
            crate::subscription_catalog::HORON_AGENT_ROUTE_API_METHOD,
            Arc::clone(&runtime),
        );
        registry.set_active_compatible_profile(crate::subscription_catalog::HORON_AGENT_ROUTE_API_METHOD);
        self.set_active_provider(ActiveProvider::OpenAI);
        Ok(())
    }

    fn set_model_on_provider_with_credential_modes(
        &self,
        provider: ActiveProvider,
        model: &str,
        openai_credential_mode: Option<openai::OpenAICredentialMode>,
        prefer_active_profile: bool,
    ) -> Result<()> {
        let model = model.trim();
        if model.is_empty() {
            anyhow::bail!("Model cannot be empty");
        }

        self.reconcile_auth_if_provider_missing(provider);

        match provider {
            ActiveProvider::OpenAI => {
                if prefer_active_profile
                    && openai_credential_mode.is_none()
                    && let Some(runtime) = self.execution_provider()
                {
                    runtime.set_model(model)?;
                    self.set_active_provider(ActiveProvider::OpenAI);
                    return Ok(());
                }
                let Some(openai) = self.openai_provider() else {
                    // No OpenAI runtime: still run the same model-name
                    // validation the runtime itself would. A cross-provider
                    // model under a forced/locked OpenAI selection must report
                    // the real problem (wrong model family), not demand a
                    // login that would never make the model valid. Keeps the
                    // error independent of which credentials exist on disk.
                    if !known_openai_model_ids().iter().any(|known| known == model) {
                        anyhow::bail!(
                            "Unsupported OpenAI model '{}'. Use /model to choose from the models available to your account.",
                            model
                        );
                    }
                    // This build ships only the direct OpenAI-compatible
                    // runtime, so an explicit OpenAI route usually has no
                    // built-in runtime behind it. Name the active profile:
                    // it is the runtime the user is actually talking to, and
                    // silently routing the OpenAI spec there would send an
                    // OpenAI model id to a foreign endpoint.
                    if let Some(profile_id) = ProviderRegistry::new(self).active_compatible_profile_id() {
                        anyhow::bail!(
                            "The built-in OpenAI runtime is not available, so '{model}' cannot be routed to OpenAI. \
                             The active provider profile '{profile_id}' is an OpenAI-compatible profile; request \
                             '{profile_id}:{model}' to use that profile, or configure OpenAI credentials to enable \
                             the built-in runtime."
                        );
                    }
                    anyhow::bail!(
                        "No OpenAI-compatible provider is configured. Add [providers.<name>] with \
                         type = \"openai-compatible\" to config.toml and select it with --provider-profile <name>."
                    );
                };
                if let Some(mode) = openai_credential_mode {
                    openai.set_credential_mode(mode)?;
                }
                // A pinned built-in route must not keep the previous direct
                // profile as the execution target.
                self.clear_active_compatible_profile();
                openai.set_model(model)?;
                self.set_active_provider(ActiveProvider::OpenAI);
                Ok(())
            }
        }
    }

    fn set_model_on_openai_compatible_profile(
        &self,
        profile: crate::provider_catalog::OpenAiCompatibleProfile,
        model: &str,
    ) -> Result<()> {
        let model = model.trim();
        if model.is_empty() {
            anyhow::bail!("Model cannot be empty");
        }
        let resolved = crate::provider_catalog::resolve_openai_compatible_profile(profile);
        let profile_id = resolved.id.clone();
        let registry = ProviderRegistry::new(self);
        let provider = external::build_openai_compatible_profile_runtime(profile)?;
        registry.install_compatible_profile(profile_id.clone(), Arc::clone(&provider));
        provider.set_model(model)?;
        registry.set_active_compatible_profile(profile_id);
        self.set_active_provider(ActiveProvider::OpenAI);
        Ok(())
    }

    fn handle_auth_changed(&self, _preserve_existing_runtime_profile: bool) {
        crate::logging::auth_event("auth_changed_received", "multi-provider", &[]);
        // Credentials feed route availability/pricing, so every memoized
        // catalog in the process is stale the moment auth changes.
        self.invalidate_routes_memo_globally();
        // Auth just changed, so discard any stale full/fast snapshots before
        // using cheap local probes to hot-initialize newly configured providers.
        crate::auth::AuthStatus::invalidate_cache();

        if let Some(openai) = self.openai_provider() {
            openai.reload_credentials();
        } else if crate::auth::codex::load_credentials().is_ok()
            && let Some(openai) = external::instantiate_expected_external_provider(external::OPENAI_RUNTIME)
        {
            crate::logging::info("Hot-initialized OpenAI provider after auth change");
            *self.openai.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(openai);
        }

        if let Some(openai) = self.openai_provider() {
            self.spawn_post_auth_model_refresh(openai, "OpenAI");
        }
        crate::logging::auth_event("auth_changed_completed", "multi-provider", &[]);
    }

    pub(super) fn set_config_default_model(&self, model: &str, default_provider: Option<&str>) -> Result<()> {
        let model = model.trim();
        if model.is_empty() {
            anyhow::bail!("Model cannot be empty");
        }

        // The model picker persists default_model as a full model spec that
        // may carry an explicit provider/credential prefix (e.g.
        // `claude-api:claude-fable-5`). Provider-local `set_model`
        // implementations validate bare model ids, so a prefixed spec must go
        // through the canonical prefix-aware path. Handing the raw spec to a
        // single provider would make it reject the id and silently keep its
        // fallback default model.
        if explicit_model_provider_prefix(model).is_some()
            || model
                .split_once(':')
                .is_some_and(|(method, _)| horon_agent_provider_core::oauth_account_label(method).is_some())
            || Self::openai_compatible_model_prefix(model).is_some()
        {
            return self.set_model(model);
        }

        // A configured default_provider is a routing decision, not just a
        // startup hint. Treat default_model as provider-local when the config
        // names a concrete provider/profile so global model-name heuristics
        // cannot undo that decision. This is especially important for
        // OpenAI-compatible gateways whose model IDs often look like built-in
        // [OI] models.
        if let Some(pref) = default_provider.and_then(|pref| {
            let trimmed = pref.trim();
            (!trimmed.is_empty()).then_some(trimmed)
        }) && let Some(selection) = Self::resolve_config_provider_selection(pref, crate::config::config())
        {
            // A known OpenAI-compatible catalog profile (deepseek, zai, ...)
            // must be handled profile-locally: binding it through the generic
            // compatible-profile path would replace the profile runtime with a
            // default environment-derived one and silently drop the configured
            // default (issue #448).
            if let selection::ConfigProviderSelection::OpenAiCompatibleProfile(profile_id) = &selection
                && let Some(profile) = crate::provider_catalog::openai_compatible_profile_by_id(profile_id)
            {
                return self.set_model_on_openai_compatible_profile(profile, model);
            }

            // Same reasoning for user-defined named provider profiles from
            // config: bind the named profile runtime directly instead of the
            // generic compatible-profile path.
            if let selection::ConfigProviderSelection::NamedProfile(profile_name, _) = &selection {
                return self.set_model_on_named_provider_profile(profile_name, model);
            }

            // A dual-auth config provider key (`anthropic-api`, `claude-oauth`,
            // `openai-api`, ...) also pins the OAuth-vs-API credential. Carry
            // that through so the active credential -- and every surface that
            // reads it (header auth tag, model picker) -- matches the route the
            // user configured, instead of leaving the provider in Auto mode
            // (which prefers OAuth) and silently mislabeling an API default.
            //
            // Bare provider keys (`claude`, `anthropic`, `openai`) intentionally
            // do NOT pin a credential: they keep Auto mode (so an API-only user
            // with `default_provider = "claude"` still resolves their key
            // instead of failing to load absent OAuth credentials).
            let pinned = horon_agent_provider_core::AuthRoute::parse_explicit_credential_prefix(pref);
            let openai_credential_mode = pinned.and_then(|route| {
                matches!(route.provider, horon_agent_provider_core::DualAuthProvider::OpenAI).then(|| {
                    match route.mode {
                        horon_agent_provider_core::AuthMode::ApiKey => openai::OpenAICredentialMode::ApiKey,
                        horon_agent_provider_core::AuthMode::Oauth => openai::OpenAICredentialMode::OAuth,
                    }
                })
            });
            return self.set_model_on_provider_with_credential_modes(
                selection.active_provider(),
                model,
                openai_credential_mode,
                true,
            );
        }

        self.set_model(model)
    }

    fn fork_model_switch_request(&self, active: ActiveProvider, current_model: &str) -> String {
        let registry = ProviderRegistry::new(self);
        if let Some(profile_id) = registry.active_compatible_profile_id()
            && registry.compatible_profile(&profile_id).is_some()
        {
            // The active direct profile owns the model namespace; a bare id
            // would be reclassified by the family heuristics.
            return format!("{profile_id}:{current_model}");
        }
        if let Some(label) = self.selected_account_for_provider(active) {
            return format!("openai-oauth@{label}:{current_model}");
        }
        let prefix = match active {
            ActiveProvider::OpenAI => {
                if let Some(openai) = self.openai_provider() {
                    openai
                        .credential_mode()
                        .auth_route(horon_agent_provider_core::DualAuthProvider::OpenAI)
                        .map(|route| route.model_prefix())
                        .unwrap_or("openai")
                } else {
                    "openai"
                }
            }
        };
        format!("{prefix}:{current_model}")
    }
}

impl Default for MultiProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for MultiProvider {
    fn catalog_sharing_identity(&self) -> Option<String> {
        Some(format!(
            "{:?}",
            (
                "multiprovider-v1",
                self.routes_memo_key(),
                pricing::auth_pricing_generation(),
                catalog_generation(),
                self.display_name(),
            )
        ))
    }
    async fn prewarm(&self, tools: &[ToolDefinition], system_static: &str) {
        if let Some(provider) = self.execution_provider() {
            provider.prewarm(tools, system_static).await;
        }
    }

    async fn prewarm_with_context(
        &self,
        tools: &[ToolDefinition],
        system_static: &str,
        context: &ProviderRequestContext,
    ) {
        let provider = self.execution_provider();
        if let Some(provider) = provider {
            provider.prewarm_with_context(tools, system_static, context).await;
        }
    }

    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
        resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        self.complete_with_failover(
            messages,
            tools,
            CompletionMode::Unified { system },
            resume_session_id,
            None,
        )
        .await
    }

    async fn complete_with_context(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
        resume_session_id: Option<&str>,
        context: &ProviderRequestContext,
    ) -> Result<EventStream> {
        self.complete_with_failover(
            messages,
            tools,
            CompletionMode::Unified { system },
            resume_session_id,
            Some(context),
        )
        .await
    }

    /// Split system prompt completion - delegates to underlying provider for better caching
    async fn complete_split(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system_static: &str,
        system_dynamic: &str,
        resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        self.complete_with_failover(
            messages,
            tools,
            CompletionMode::Split {
                system_static,
                system_dynamic,
            },
            resume_session_id,
            None,
        )
        .await
    }

    async fn complete_split_with_context(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system_static: &str,
        system_dynamic: &str,
        resume_session_id: Option<&str>,
        context: &ProviderRequestContext,
    ) -> Result<EventStream> {
        self.complete_with_failover(
            messages,
            tools,
            CompletionMode::Split {
                system_static,
                system_dynamic,
            },
            resume_session_id,
            Some(context),
        )
        .await
    }

    fn name(&self) -> &str {
        match self.active_provider() {
            ActiveProvider::OpenAI => "OpenAI",
        }
    }

    fn display_name(&self) -> String {
        // The slot multiplexes the public aggregator and every
        // direct OpenAI-compatible profile (NVIDIA NIM, DeepSeek, ...). Ask the
        // active execution runtime for its own label so the UI reflects the
        // profile selected at runtime rather than the fixed slot name.
        if matches!(self.active_provider(), ActiveProvider::OpenAI)
            && let Some(execution) = self.active_openrouter_execution_provider()
        {
            return execution.runtime_display_name();
        }
        self.name().to_string()
    }

    fn model(&self) -> String {
        match self.active_provider() {
            ActiveProvider::OpenAI => self
                .execution_provider()
                .map(|o| o.model())
                .unwrap_or_else(|| horon_agent_provider_core::DEFAULT_OPENAI_MODEL.to_string()),
        }
    }

    fn explicit_provider_pin_for_current_model(&self) -> Option<String> {
        matches!(self.active_provider(), ActiveProvider::OpenAI)
            .then(|| self.active_openrouter_execution_provider())
            .flatten()
            .and_then(|provider| provider.explicit_provider_pin_for_current_model())
    }

    fn active_resolved_credential(&self) -> Option<horon_agent_provider_core::ResolvedCredential> {
        use horon_agent_provider_core::ResolvedCredential;
        let openai = self.openai_provider()?;
        Some(match openai.credential_mode() {
            openai::OpenAICredentialMode::OAuth => ResolvedCredential::Oauth,
            openai::OpenAICredentialMode::ApiKey => ResolvedCredential::ApiKey,
            // Auto resolves to OAuth first when available, otherwise API key.
            openai::OpenAICredentialMode::Auto => {
                if crate::auth::codex::load_oauth_credentials().is_ok() {
                    ResolvedCredential::Oauth
                } else {
                    ResolvedCredential::ApiKey
                }
            }
        })
    }

    fn credential_mode(&self) -> CredentialMode {
        self.openai_provider()
            .map(|provider| provider.credential_mode())
            .unwrap_or(CredentialMode::Auto)
    }

    fn set_credential_mode(&self, mode: CredentialMode) -> Result<()> {
        let active = self.active_provider();
        self.openai_provider()
            .ok_or_else(|| anyhow!("OpenAI provider is not configured"))?
            .set_credential_mode(mode)?;
        self.clear_active_compatible_profile();
        self.set_active_provider(active);
        Ok(())
    }

    fn oauth_account_label(&self) -> Option<String> {
        self.selected_account_for_provider(self.active_provider())
    }

    fn set_oauth_account(&self, label: Option<&str>) -> Result<()> {
        if let Some(label) = label {
            return self.set_model_on_saved_account("openai-oauth", label, &self.model());
        }
        if let Some(runtime) = self.openai_provider() {
            runtime.set_oauth_account(None)?;
        }
        self.invalidate_routes_memo();
        Ok(())
    }

    fn active_explicit_credential(&self) -> Option<horon_agent_provider_core::ResolvedCredential> {
        use horon_agent_provider_core::ResolvedCredential;
        // Only report an *explicit* in-memory pin. Auto mode returns `None` so
        // callers fall back to their cheaper cached heuristic without forcing
        // a disk read on every frame. This stays in lockstep with
        // `active_resolved_credential`'s explicit arms above.
        match self.openai_provider()?.credential_mode() {
            openai::OpenAICredentialMode::OAuth => Some(ResolvedCredential::Oauth),
            openai::OpenAICredentialMode::ApiKey => Some(ResolvedCredential::ApiKey),
            openai::OpenAICredentialMode::Auto => None,
        }
    }

    fn supports_image_input(&self) -> bool {
        match self.active_provider() {
            ActiveProvider::OpenAI => self
                .execution_provider()
                .map(|provider| provider.supports_image_input())
                .unwrap_or(false),
        }
    }

    fn supports_deferred_tools(&self) -> bool {
        // Only the first-party OpenAI Responses path implements provider-native
        // deferred loading. Every other route receives an eager-only tool list
        // (see `complete_with_failover`).
        match self.active_provider() {
            ActiveProvider::OpenAI => self
                .execution_provider()
                .is_some_and(|provider| provider.supports_deferred_tools()),
        }
    }

    fn set_model(&self, model: &str) -> Result<()> {
        self.spawn_openai_catalog_refresh_if_needed();
        // Model/profile switches change route availability details; rebuild
        // the catalog on next read instead of serving the memoized copy.
        self.invalidate_routes_memo();

        let requested_model = model.trim();
        if requested_model.is_empty() {
            anyhow::bail!("Model cannot be empty");
        }

        if let Some((prefix, target_model)) = requested_model.split_once(':')
            && let Some((method, label)) = prefix.split_once('@')
            && method == "openai-oauth"
        {
            return self.set_model_on_saved_account(method, label, target_model);
        }
        // Bare model changes keep the selected login. Explicit generic routes
        // opt back into the normal active-account preference.
        if let Some((prefix, _)) = requested_model.split_once(':')
            && let Some(route) = horon_agent_provider_core::AuthRoute::parse_explicit_credential_prefix(prefix)
            && matches!(route.provider, horon_agent_provider_core::DualAuthProvider::OpenAI)
            && let Some(runtime) = self.openai_provider()
        {
            runtime.set_oauth_account(None)?;
        }

        // `horon-agent:<model>` is the explicit spec for the HoronAgent subscription
        // runtime. Without this it fell through to the active provider and
        // `--model horon-agent:gpt-5.5` silently stayed on the previous model.
        if let Some(target_model) = horon_agent_subscription_model_prefix(requested_model) {
            return self.set_model_on_horon_agent_subscription(target_model);
        }

        if let Some((profile, target_model)) = Self::openai_compatible_model_prefix(requested_model) {
            return self.set_model_on_openai_compatible_profile(profile, target_model);
        }

        // User-defined named provider profiles from config (`[providers.<name>]`).
        // The model picker emits `<name>:<model>` specs for their routes
        // (issue #444), so the switch must bind that profile's runtime instead
        // of falling through to global model-name heuristics.
        if let Some((profile_name, target_model)) = Self::named_provider_profile_model_prefix(requested_model) {
            return self.set_model_on_named_provider_profile(&profile_name, &target_model);
        }

        // Provider-prefixed model names are explicit routing directives. They
        // must never silently fall through to another provider when the target
        // is unavailable.
        if let Some((target, prefix, target_model)) = explicit_model_provider_prefix(requested_model) {
            // The single canonical parser decides whether this prefix pins a
            // dual-auth credential (and which provider/mode). A bare `openai:`
            // prefix routes without pinning a credential.
            let pinned = horon_agent_provider_core::AuthRoute::parse_explicit_credential_prefix(prefix);
            let openai_credential_mode = pinned.and_then(|route| {
                matches!(route.provider, horon_agent_provider_core::DualAuthProvider::OpenAI).then(|| {
                    match route.mode {
                        horon_agent_provider_core::AuthMode::ApiKey => openai::OpenAICredentialMode::ApiKey,
                        horon_agent_provider_core::AuthMode::Oauth => openai::OpenAICredentialMode::OAuth,
                    }
                })
            });
            if openai_credential_mode.is_some() {
                return self.set_model_on_provider_with_credential_modes(
                    target,
                    target_model,
                    openai_credential_mode,
                    false,
                );
            }
            return self.set_model_on_explicit_provider(target, target_model);
        }

        // A custom OpenAI-compatible endpoint owns opaque, provider-local model
        // IDs. Keep unprefixed names on that active endpoint, even when they
        // resemble a globally known model family. Picker and slash-command
        // route specs carry explicit prefixes, so the branch above can switch
        // to any configured provider at any time.
        if self
            .active_openrouter_execution_provider()
            .is_some_and(|provider| !provider.supports_provider_routing_features())
        {
            return self.set_model_on_provider(ActiveProvider::OpenAI, requested_model);
        }

        // Detect which provider an unprefixed model belongs to.
        let target_provider = provider_for_model(requested_model);
        if let Some(target_provider) = target_provider
            && let Some(target) = provider_from_model_key(target_provider)
        {
            self.set_model_on_provider(target, requested_model)
        } else if let Some(profile) = self.openai_compatible_profile_owning_model(requested_model) {
            // Bare ids from an OpenAI-compatible catalog (`celeris-1`,
            // `mimo-v2.5`, ...) match none of the built-in model-name
            // heuristics. Without this, `/model <bare-id>` fell through to
            // whichever provider happened to be active and failed with a
            // misleading "not supported by <active provider>" error.
            self.set_model_on_openai_compatible_profile(profile, requested_model)
        } else {
            // Unknown model - try current provider.
            self.set_model_on_provider(self.active_provider(), requested_model)
        }
    }

    fn set_route_selection(&self, selection: &RouteSelection) -> Result<()> {
        if selection.model.trim().is_empty() {
            anyhow::bail!("Model cannot be empty");
        }

        // The subscription is a distinct endpoint/auth runtime, not a model
        // alias. Handle its structured identity before converting other routes
        // back into their legacy string specs.
        if selection.runtime_key == RuntimeKey::HoronAgentSubscription {
            return self.set_model_on_horon_agent_subscription(&selection.model);
        }

        // Routing-prefix policy lives once in RouteSelection::routed_model_spec
        // so this orchestrator and every single-runtime provider agree on the
        // spec string. set_model then dispatches it to the right sub-provider.
        self.set_model(&selection.routed_model_spec())
    }

    fn available_models(&self) -> Vec<&'static str> {
        let mut models = Vec::new();
        models.extend_from_slice(ALL_OPENAI_MODELS);
        models
    }

    fn available_models_for_switching(&self) -> Vec<String> {
        match self.active_provider() {
            ActiveProvider::OpenAI => self
                .execution_provider()
                .map(|openai| openai.available_models_for_switching())
                .unwrap_or_default(),
        }
    }

    fn available_models_display(&self) -> Vec<String> {
        self.fresh_routes_memo_entry().listable_models
    }

    fn ready_models_display(&self) -> Option<Vec<String>> {
        self.peek_fresh_routes_memo_entry().map(|entry| entry.listable_models)
    }

    fn model_routes(&self) -> Vec<ModelRoute> {
        self.fresh_routes_memo_entry().routes
    }

    async fn prefetch_models(&self) -> Result<()> {
        let provider = self.execution_provider();
        let result = match provider {
            Some(provider) => provider.prefetch_models().await,
            None => Ok(()),
        };

        if let Err(err) = result {
            return Err(anyhow!("provider: {err}"));
        }

        // Fresh catalogs may have arrived; retire every memoized copy.
        self.invalidate_routes_memo_globally();
        Ok(())
    }

    fn on_auth_changed(&self) {
        self.handle_auth_changed(false);
    }

    fn on_auth_changed_preserve_current_provider(&self) {
        self.handle_auth_changed(true);
    }

    fn auth_model_refresh_pending(&self) -> bool {
        self.post_auth_refreshes_pending
            .load(std::sync::atomic::Ordering::Acquire)
            > 0
    }

    async fn invalidate_credentials(&self) {
        if let Some(openai) = self.openai_provider() {
            openai.invalidate_credentials().await;
        }
        if let Some(profile) = self.active_openrouter_execution_provider() {
            profile.invalidate_credentials().await;
        }
    }

    fn handles_tools_internally(&self) -> bool {
        match self.active_provider() {
            ActiveProvider::OpenAI => self
                .execution_provider()
                .map(|o| o.handles_tools_internally())
                .unwrap_or(false),
        }
    }

    fn reasoning_effort(&self) -> Option<String> {
        match self.active_provider() {
            ActiveProvider::OpenAI => self.execution_provider().and_then(|o| o.reasoning_effort()),
        }
    }

    fn set_reasoning_effort(&self, effort: &str) -> Result<()> {
        match self.active_provider() {
            ActiveProvider::OpenAI => self
                .execution_provider()
                .ok_or_else(|| anyhow::anyhow!("No OpenAI-compatible provider is configured"))?
                .set_reasoning_effort(effort),
        }
    }

    fn available_efforts(&self) -> Vec<&'static str> {
        match self.active_provider() {
            ActiveProvider::OpenAI => self
                .execution_provider()
                .map(|o| o.available_efforts())
                .unwrap_or_default(),
        }
    }

    fn service_tier(&self) -> Option<String> {
        match self.active_provider() {
            ActiveProvider::OpenAI => self.execution_provider().and_then(|o| o.service_tier()),
        }
    }

    fn set_service_tier(&self, service_tier: &str) -> Result<()> {
        match self.active_provider() {
            ActiveProvider::OpenAI => self
                .execution_provider()
                .ok_or_else(|| anyhow::anyhow!("No OpenAI-compatible provider is configured"))?
                .set_service_tier(service_tier),
        }
    }

    fn available_service_tiers(&self) -> Vec<&'static str> {
        match self.active_provider() {
            ActiveProvider::OpenAI => self
                .execution_provider()
                .map(|o| o.available_service_tiers())
                .unwrap_or_default(),
        }
    }

    fn native_compaction_mode(&self) -> Option<String> {
        match self.active_provider() {
            ActiveProvider::OpenAI => self.execution_provider().and_then(|o| o.native_compaction_mode()),
        }
    }

    fn native_compaction_threshold_tokens(&self) -> Option<usize> {
        match self.active_provider() {
            ActiveProvider::OpenAI => self
                .execution_provider()
                .and_then(|o| o.native_compaction_threshold_tokens()),
        }
    }

    fn transport(&self) -> Option<String> {
        match self.active_provider() {
            ActiveProvider::OpenAI => self.execution_provider().and_then(|o| o.transport()),
        }
    }

    fn set_transport(&self, transport: &str) -> Result<()> {
        match self.active_provider() {
            ActiveProvider::OpenAI => self
                .execution_provider()
                .ok_or_else(|| anyhow::anyhow!("No OpenAI-compatible provider is configured"))?
                .set_transport(transport),
        }
    }

    fn available_transports(&self) -> Vec<&'static str> {
        match self.active_provider() {
            ActiveProvider::OpenAI => self
                .execution_provider()
                .map(|o| o.available_transports())
                .unwrap_or_default(),
        }
    }

    fn supports_compaction(&self) -> bool {
        match self.active_provider() {
            ActiveProvider::OpenAI => self
                .execution_provider()
                .map(|o| o.supports_compaction())
                .unwrap_or(false),
        }
    }

    fn uses_horon_agent_compaction(&self) -> bool {
        match self.active_provider() {
            ActiveProvider::OpenAI => self
                .execution_provider()
                .map(|o| o.uses_horon_agent_compaction())
                .unwrap_or(false),
        }
    }

    async fn native_compact(
        &self,
        messages: &[Message],
        existing_summary_text: Option<&str>,
        existing_openai_encrypted_content: Option<&str>,
    ) -> Result<NativeCompactionResult> {
        match self.active_provider() {
            ActiveProvider::OpenAI => {
                if let Some(runtime) = self.execution_provider() {
                    runtime
                        .native_compact(messages, existing_summary_text, existing_openai_encrypted_content)
                        .await
                } else {
                    Err(anyhow::anyhow!("No OpenAI-compatible provider is configured"))
                }
            }
        }
    }

    async fn native_compact_window(
        &self,
        messages: &[Message],
        existing_summary_text: Option<&str>,
        existing_window: &[serde_json::Value],
    ) -> Result<NativeCompactionResult> {
        match self.active_provider() {
            ActiveProvider::OpenAI => match self.execution_provider() {
                Some(runtime) => {
                    runtime
                        .native_compact_window(messages, existing_summary_text, existing_window)
                        .await
                }
                None => Err(anyhow::anyhow!("No OpenAI-compatible provider is configured")),
            },
        }
    }

    fn drain_startup_notices(&self) -> Vec<String> {
        std::mem::take(
            &mut *self
                .startup_notices
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    fn context_window(&self) -> usize {
        match self.active_provider() {
            ActiveProvider::OpenAI => self
                .execution_provider()
                .map(|o| o.context_window())
                .unwrap_or(DEFAULT_CONTEXT_LIMIT),
        }
    }

    fn restore_model(&self, model: &str) -> Result<()> {
        self.set_model(model)
    }

    fn unavailable_reason(&self) -> Option<String> {
        None
    }

    fn fork(&self) -> Arc<dyn Provider> {
        let current_model = self.model();
        let active = self.active_provider();

        let openai = if self.openai_provider().is_some() {
            external::instantiate_expected_external_provider(external::OPENAI_RUNTIME)
        } else {
            None
        };

        let provider = Self {
            openai: RwLock::new(openai),
            openai_compatible_profiles: RwLock::new(HashMap::new()),
            active_openai_compatible_profile: RwLock::new(None),
            active: RwLock::new(active),
            startup_notices: RwLock::new(Vec::new()),
            initial_provider: self.initial_provider,
            catalog_scope: self.catalog_scope,
            routes_memo: Mutex::new(None),
            post_auth_refreshes_pending: Arc::clone(&self.post_auth_refreshes_pending),
        };

        provider.spawn_openai_catalog_refresh_if_needed();
        let switch_request = self.fork_model_switch_request(active, &current_model);
        let _ = provider.set_model(&switch_request);
        Arc::new(provider)
    }

    fn fork_for_new_session(&self) -> Arc<dyn Provider> {
        // A shared server can outlive config changes made by a model picker in a
        // client process. Ordinary `fork()` intentionally preserves this
        // template's current selection, which is correct for resumed sessions and
        // in-flight helper work but stale for a brand-new session. Reconstruct the
        // orchestrator so the reloadable config cache and current auth state choose
        // the provider/model again.
        let mut provider = Self::new_fast();
        // Reconstruction uses the same composition root. Selection, home,
        // reloadable config and auth/catalog revisions remain part of the key.
        provider.catalog_scope = self.catalog_scope;

        // An explicit CLI initial provider/model remains the starting selection
        // for new sessions, while each session can switch freely afterward.
        if self.initial_provider.is_some() {
            let active = self.active_provider();
            let current_model = self.model();
            let switch_request = self.fork_model_switch_request(active, &current_model);
            if let Err(error) = provider.set_model(&switch_request) {
                crate::logging::warn(&format!(
                    "Failed to preserve initial provider model '{}' for new session: {}",
                    switch_request, error
                ));
            }
        }

        Arc::new(provider)
    }

    fn switch_active_provider_to(&self, provider: &str) -> Result<()> {
        let target =
            Self::parse_provider_hint(provider).ok_or_else(|| anyhow::anyhow!("Unknown provider `{}`", provider))?;
        if !self.provider_is_configured(target) {
            anyhow::bail!(
                "Provider `{}` is not configured in this session",
                Self::provider_key(target)
            );
        }
        self.set_active_provider(target);
        self.auto_select_multi_account_for_provider(target);
        Ok(())
    }
}

/// Get the prompt cache TTL in seconds for a given provider name.
/// Returns None if the provider doesn't support prompt caching or TTL is unknown.
pub fn cache_ttl_for_provider(provider: &str) -> Option<u64> {
    cache_ttl_for_provider_model(provider, None)
}

/// Whether a reported cache lifetime is an estimate rather than a hard expiry.
/// OpenAI documents typical, maximum, or minimum lifetimes depending on model.
/// The subscription value is also only a heuristic.
pub fn cache_ttl_is_estimate(provider: &str) -> bool {
    horon_agent_provider_core::AuthRoute::parse(provider)
        .is_some_and(|route| route.provider == horon_agent_provider_core::DualAuthProvider::OpenAI)
        || matches!(
            provider.trim().to_ascii_lowercase().as_str(),
            "horon-agent subscription"
        )
}

/// Get the prompt cache TTL in seconds for a given provider/model pair.
///
/// This is a cache-retention estimate, not a guaranteed expiry or cache hit.
/// It depends on the auth route, model and configured request retention.
pub fn cache_ttl_for_provider_model(provider: &str, model: Option<&str>) -> Option<u64> {
    if let Some(route) = horon_agent_provider_core::AuthRoute::parse(provider)
        && route.provider == horon_agent_provider_core::DualAuthProvider::OpenAI
    {
        // Codex OAuth omits API retention controls. A generic runtime name does
        // not identify the credential mode, so don't claim an API-only lifetime.
        return (route.mode == horon_agent_provider_core::AuthMode::ApiKey)
            .then(|| openai::prompt_cache_ttl_for_model(model, openai::prompt_cache_retention_from_env().as_deref()));
    }
    match provider.trim().to_ascii_lowercase().as_str() {
        "horon-agent subscription" => Some(300),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
