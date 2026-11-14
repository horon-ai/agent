#![cfg_attr(test, allow(clippy::items_after_test_module))]

#[path = "provider_control_auth_env.rs"]
mod provider_control_auth_env;
use provider_control_auth_env::{activate_auth_profile, auth_change_saved_credentials};

use crate::agent::Agent;
use crate::protocol::{AuthChanged, NotificationType, ServerEvent};
use crate::provider::{ModelCatalogRefreshSummary, ModelRoute, Provider, ProviderFailoverPrompt, RouteSelection};
use horon_agent_provider_core::ModelCatalogSnapshot;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Instant;
use tokio::sync::{Mutex, RwLock, mpsc};

type SessionAgents = Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>>;
static AUTH_REFRESH_GENERATIONS: OnceLock<StdMutex<HashMap<String, u64>>> = OnceLock::new();
static NEXT_AUTH_REFRESH_GENERATION: AtomicU64 = AtomicU64::new(1);

struct AuthRefreshTargets {
    providers: Vec<Arc<dyn Provider>>,
    session_providers: Vec<Arc<dyn Provider>>,
    deferred_agents: Vec<Arc<Mutex<Agent>>>,
}

fn begin_auth_refresh(session_id: &str) -> u64 {
    let generation = NEXT_AUTH_REFRESH_GENERATION.fetch_add(1, Ordering::Relaxed);
    let mut generations = AUTH_REFRESH_GENERATIONS
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    generations.insert(session_id.to_string(), generation);
    generation
}

fn auth_refresh_is_current(session_id: &str, generation: u64) -> bool {
    let generations = AUTH_REFRESH_GENERATIONS
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    generations.get(session_id).copied() == Some(generation)
}

fn finish_auth_refresh(session_id: &str, generation: u64) {
    let mut generations = AUTH_REFRESH_GENERATIONS
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if generations.get(session_id).copied() == Some(generation) {
        generations.remove(session_id);
    }
}

fn available_models_snapshot_into_event(snapshot: ModelCatalogSnapshot) -> ServerEvent {
    ServerEvent::AvailableModelsUpdated {
        provider_name: snapshot.provider_name,
        provider_model: snapshot.provider_model,
        available_models: snapshot.available_models,
        available_model_routes: snapshot.model_routes,
        identity: None,
    }
}

async fn available_models_snapshot(agent: &Arc<Mutex<Agent>>) -> ModelCatalogSnapshot {
    let agent_guard = agent.lock().await;
    super::available_models_cache::for_agent(&agent_guard).snapshot()
}

fn available_models_snapshot_from_provider(provider: &Arc<dyn Provider>) -> ModelCatalogSnapshot {
    let mut snapshot = ModelCatalogSnapshot::from_provider(provider.as_ref());
    crate::model_usage::enrich_routes(&mut snapshot.model_routes);
    snapshot
}

pub(super) fn try_available_models_catalog(
    agent: &Arc<Mutex<Agent>>,
) -> Option<Arc<super::available_models_cache::Catalog>> {
    let agent_guard = agent.try_lock().ok()?;
    Some(super::available_models_cache::for_agent(&agent_guard))
}

fn format_auth_catalog_refresh_complete(
    provider_name: Option<&str>,
    provider_model: Option<&str>,
    summary: &ModelCatalogRefreshSummary,
    has_warning: bool,
) -> String {
    let provider_label = provider_name.unwrap_or("provider");
    let title = provider_model
        .map(|model| format!("**Model ready:** `{model}`"))
        .unwrap_or_else(|| "**Model access refreshed**".to_string());
    let changed = summary.models_added > 0
        || summary.models_removed > 0
        || summary.routes_added > 0
        || summary.routes_removed > 0
        || summary.routes_changed > 0;
    let catalog_status = if has_warning {
        if changed {
            format!("{provider_label} catalog changed; some routes missing. Use `/model`.")
        } else {
            format!("{provider_label} catalog unchanged; some routes missing. Use `/model`.")
        }
    } else if changed {
        format!(
            "{provider_label} catalog changed: models +{}/-{}, routes +{}/-{}/~{}. Use `/model`.",
            summary.models_added,
            summary.models_removed,
            summary.routes_added,
            summary.routes_removed,
            summary.routes_changed,
        )
    } else {
        format!(
            "{provider_label} catalog unchanged: {} models, {} routes. Use `/model`.",
            summary.model_count_after, summary.route_count_after,
        )
    };
    format!("{title}\n{catalog_status}")
}

fn log_provider_control_deferred(operation: &'static str, id: u64) -> Instant {
    let queued_at = Instant::now();
    crate::logging::event_warn(
        "SERVER_PROVIDER_CONTROL_DEFERRED",
        vec![
            ("phase", "queued".to_string()),
            ("operation", operation.to_string()),
            ("request_id", id.to_string()),
            ("reason", "agent_busy".to_string()),
        ],
    );
    queued_at
}

fn log_provider_control_lock_acquired(operation: &'static str, id: u64, queued_at: Instant) {
    crate::logging::event_info(
        "SERVER_PROVIDER_CONTROL_DEFERRED",
        vec![
            ("phase", "lock_acquired".to_string()),
            ("operation", operation.to_string()),
            ("request_id", id.to_string()),
            ("wait_ms", queued_at.elapsed().as_millis().to_string()),
        ],
    );
}

fn log_provider_control_completed(operation: &'static str, id: u64, queued_at: Instant) {
    crate::logging::event_info(
        "SERVER_PROVIDER_CONTROL_DEFERRED",
        vec![
            ("phase", "completed".to_string()),
            ("operation", operation.to_string()),
            ("request_id", id.to_string()),
            ("total_ms", queued_at.elapsed().as_millis().to_string()),
        ],
    );
}

fn spawn_deferred_agent_mutation<F>(
    operation: &'static str,
    id: u64,
    agent: Arc<Mutex<Agent>>,
    client_event_tx: mpsc::UnboundedSender<ServerEvent>,
    apply: F,
) where
    F: FnOnce(&mut Agent, &mpsc::UnboundedSender<ServerEvent>) + Send + 'static,
{
    let queued_at = log_provider_control_deferred(operation, id);
    tokio::spawn(async move {
        let mut agent_guard = agent.lock().await;
        log_provider_control_lock_acquired(operation, id, queued_at);
        apply(&mut agent_guard, &client_event_tx);
        log_provider_control_completed(operation, id, queued_at);
    });
}

async fn auth_refresh_targets(
    provider_template: &Arc<dyn Provider>,
    current_provider: &Arc<dyn Provider>,
    current_agent: &Arc<Mutex<Agent>>,
    sessions: &SessionAgents,
) -> AuthRefreshTargets {
    fn push_unique(handles: &mut Vec<Arc<dyn Provider>>, provider: Arc<dyn Provider>) {
        if !handles.iter().any(|existing| Arc::ptr_eq(existing, &provider)) {
            handles.push(provider);
        }
    }

    let mut handles = Vec::new();
    let mut session_handles = Vec::new();
    let mut deferred_agents = Vec::new();
    push_unique(&mut handles, Arc::clone(provider_template));
    push_unique(&mut handles, Arc::clone(current_provider));

    let agents: Vec<Arc<Mutex<Agent>>> = {
        let sessions_guard = sessions.read().await;
        sessions_guard.values().cloned().collect()
    };

    for agent in agents {
        // The requesting session's provider is already included explicitly,
        // even when that agent is busy and its lock cannot be inspected here.
        if Arc::ptr_eq(&agent, current_agent) {
            continue;
        }
        let Ok(agent_guard) = agent.try_lock() else {
            crate::logging::info("Deferring busy session provider auth-change refresh until the session is idle");
            deferred_agents.push(agent);
            continue;
        };
        let provider = agent_guard.provider_handle();
        if handles.iter().any(|existing| Arc::ptr_eq(existing, &provider)) {
            continue;
        }
        push_unique(&mut session_handles, provider);
    }

    AuthRefreshTargets {
        providers: handles,
        session_providers: session_handles,
        deferred_agents,
    }
}

fn spawn_deferred_auth_refreshes(agents: Vec<Arc<Mutex<Agent>>>) {
    for agent in agents {
        tokio::spawn(async move {
            let provider = {
                let agent_guard = agent.lock().await;
                agent_guard.provider_handle()
            };
            provider.on_auth_changed_preserve_current_provider();
            crate::bus::Bus::global().publish_models_updated();
        });
    }
}

/// Provider id of an auth change: the protocol field, else the legacy hint.
/// Unknown values stay display labels; only OpenAI-compatible profiles resolve.
fn auth_provider_id(legacy_provider_hint: Option<String>, auth: Option<AuthChanged>) -> Option<String> {
    auth.map(|auth| auth.provider.0)
        .or(legacy_provider_hint)
        .map(|provider| provider.trim().to_string())
        .filter(|provider| !provider.is_empty())
}

/// Pin a model switch to the auth change's profile (`profile:model`) when known.
fn auth_model_switch_request(provider_id: Option<&str>, model: &str) -> String {
    match provider_id.map(str::trim).filter(|provider| !provider.is_empty()) {
        Some(provider_id) if crate::provider_catalog::openai_compatible_profile_by_id(provider_id).is_some() => {
            format!("{provider_id}:{model}")
        }
        _ => model.to_string(),
    }
}

fn route_matches_provider(route: &ModelRoute, provider_id: Option<&str>) -> bool {
    provider_id
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .is_none_or(|provider_id| route_provider_id(route) == provider_id)
}

fn route_provider_id(route: &ModelRoute) -> &str {
    let method = route.api_method.as_str();
    if let Some(profile) = method.strip_prefix("openai-compatible:") {
        return profile;
    }
    match method {
        "openai-oauth" | "openai-api-key" | "openai-api" => "openai",
        "claude-oauth" | "claude-api-key" | "claude-api" => "anthropic",
        other => other,
    }
}

/// Pick the strongest available route across provider groups.
fn globally_preferred_default_route(routes: &[ModelRoute]) -> Option<ModelRoute> {
    let available: Vec<&ModelRoute> = routes.iter().filter(|route| route.available).collect();
    let mut methods: Vec<&str> = Vec::new();
    for route in &available {
        if !methods.contains(&route.api_method.as_str()) {
            methods.push(route.api_method.as_str());
        }
    }
    let mut best: Option<(usize, ModelRoute)> = None;
    for method in methods {
        let group: Vec<&ModelRoute> = available
            .iter()
            .copied()
            .filter(|route| route.api_method == method)
            .collect();
        let ids: Vec<&str> = group.iter().map(|route| route.model.as_str()).collect();
        let provider_id = group
            .first()
            .map(|route| route_provider_id(route))
            .unwrap_or("openai-compatible");
        let route = crate::provider::model_rank::ranked_flagship_for_provider(provider_id, &ids)
            .and_then(|model| group.iter().find(|route| route.model == model).copied())
            .or_else(|| group.first().copied());
        let Some(route) = route else {
            continue;
        };
        let rank = crate::provider::model_rank::preferred_model_rank(provider_id, &route.model);
        if best.as_ref().is_none_or(|(best_rank, _)| rank < *best_rank) {
            best = Some((rank, route.clone()));
        }
    }
    best.map(|(_, route)| route)
}

/// Model to select after an auth change: keep the configured default when it is
/// available, keep the current model when it is still available, else rank the
/// provider's routes.
fn auth_model_to_select(
    provider_id: Option<&str>,
    configured_model: Option<&str>,
    current_model: Option<&str>,
    routes: &[ModelRoute],
) -> Option<String> {
    let matching: Vec<&ModelRoute> = routes
        .iter()
        .filter(|route| route.available && route_matches_provider(route, provider_id))
        .collect();
    if matching.is_empty() {
        return None;
    }
    if let Some(configured) = configured_model.map(str::trim).filter(|model| !model.is_empty()) {
        let bare = horon_agent_provider_core::selection::explicit_model_provider_prefix(configured)
            .map(|(_, _, bare)| bare)
            .unwrap_or(configured);
        if matching.iter().any(|route| route.model == bare) && current_model != Some(bare) {
            return Some(bare.to_string());
        }
    }
    if let Some(current) = current_model
        && matching.iter().any(|route| route.model == current)
    {
        return None;
    }
    let ids: Vec<&str> = matching.iter().map(|route| route.model.as_str()).collect();
    let ranked = matching
        .first()
        .map(|route| route_provider_id(route))
        .and_then(|provider| crate::provider::model_rank::ranked_flagship_for_provider(provider, &ids));
    ranked.or_else(|| matching.first().map(|route| route.model.clone()))
}

async fn apply_auth_runtime_model_to_agent(
    provider_id: Option<&str>,
    model: Option<&str>,
    agent: &Arc<Mutex<Agent>>,
    unless_user_selected_after: Option<u64>,
) {
    let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) else {
        return;
    };

    let provider = provider_id.unwrap_or("auth");
    let result = {
        let mut agent_guard = agent.lock().await;
        if unless_user_selected_after
            .is_some_and(|generation| agent_guard.user_selected_provider_model_after(generation))
        {
            crate::logging::auth_event(
                "auth_changed_auto_model_skipped_after_manual_switch",
                provider,
                &[("reason", "user_selected_provider_model_during_refresh")],
            );
            return;
        }
        let model_request = auth_model_switch_request(provider_id, model);
        let result = agent_guard.set_model_from_auth(&model_request);
        if result.is_ok() {
            agent_guard.reset_provider_session();
        }
        result.map(|_| agent_guard.provider_model())
    };

    match result {
        Ok(resolved_model) => crate::logging::auth_event(
            "auth_changed_runtime_model_applied",
            provider,
            &[
                ("requested_model", model),
                ("resolved_model", resolved_model.as_str()),
                ("provider_session", "reset"),
            ],
        ),
        Err(error) => {
            let message = error.to_string();
            crate::logging::auth_event(
                "auth_changed_runtime_model_failed",
                provider,
                &[("requested_model", model), ("reason", message.as_str())],
            );
        }
    }
}

async fn apply_auth_route_to_agent(
    route: &ModelRoute,
    agent: &Arc<Mutex<Agent>>,
    unless_user_selected_after: Option<u64>,
) {
    let selection = RouteSelection::from_model_route(route);
    let requested_model = selection.routed_model_spec();
    let result = {
        let mut agent_guard = agent.lock().await;
        if unless_user_selected_after
            .is_some_and(|generation| agent_guard.user_selected_provider_model_after(generation))
        {
            crate::logging::auth_event(
                "auth_changed_auto_model_skipped_after_manual_switch",
                &route.provider,
                &[("reason", "user_selected_provider_model_during_refresh")],
            );
            return;
        }
        let result = agent_guard.set_route_selection_from_auth(&selection);
        if result.is_ok() {
            agent_guard.reset_provider_session();
        }
        result.map(|_| agent_guard.provider_model())
    };

    match result {
        Ok(resolved_model) => crate::logging::auth_event(
            "auth_changed_global_route_applied",
            &route.provider,
            &[
                ("requested_model", requested_model.as_str()),
                ("resolved_model", resolved_model.as_str()),
                ("api_method", route.api_method.as_str()),
                ("provider_session", "reset"),
            ],
        ),
        Err(error) => {
            let message = error.to_string();
            crate::logging::auth_event(
                "auth_changed_global_route_failed",
                &route.provider,
                &[
                    ("requested_model", requested_model.as_str()),
                    ("api_method", route.api_method.as_str()),
                    ("reason", message.as_str()),
                ],
            );
        }
    }
}

fn send_model_changed_result(
    id: u64,
    result: anyhow::Result<(
        String,
        String,
        Option<horon_agent_provider_core::ResolvedCredential>,
        Option<String>,
    )>,
    fallback_model: String,
    agent: &Agent,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    match result {
        Ok((updated, provider_name, resolved_credential, reasoning_effort)) => {
            crate::logging::event_info(
                "server_model_changed",
                vec![
                    ("id", id.to_string()),
                    ("model", updated.clone()),
                    ("provider", provider_name.clone()),
                ],
            );
            let _ = client_event_tx.send(ServerEvent::ModelChanged {
                id,
                model: updated,
                provider_name: Some(provider_name),
                context_window: Some(agent.provider_context_window() as u64),
                error: None,
                resolved_credential,
                reasoning_effort,
            });
        }
        Err(error) => {
            crate::logging::event_error(
                "server_model_change_failed",
                vec![
                    ("id", id.to_string()),
                    ("fallback_model", fallback_model.clone()),
                    ("error", error.to_string()),
                ],
            );
            let _ = client_event_tx.send(ServerEvent::ModelChanged {
                id,
                model: fallback_model,
                provider_name: None,
                context_window: Some(agent.provider_context_window() as u64),
                error: Some(error.to_string()),
                resolved_credential: None,
                reasoning_effort: None,
            });
        }
    }
}

fn apply_set_model(id: u64, model: String, agent: &mut Agent, client_event_tx: &mpsc::UnboundedSender<ServerEvent>) {
    crate::logging::event_info(
        "server_set_model_request",
        vec![
            ("id", id.to_string()),
            ("requested_model", model.clone()),
            ("current_model", agent.provider_model()),
            ("current_provider", agent.provider_name()),
        ],
    );

    let current = agent.provider_model();
    let result = {
        let result = agent.set_model(&model);
        if result.is_ok() {
            agent.reset_provider_session();
        }
        result.map(|_| {
            (
                agent.provider_model(),
                agent.provider_name(),
                agent.active_resolved_credential(),
                agent.provider_reasoning_effort(),
            )
        })
    };
    send_model_changed_result(id, result, current, agent, client_event_tx);
}

/// Apply a route the caller already validated and drop the provider session so
/// the next turn resends its context on the new route.
///
/// The server-side autonomous failover executor is the only caller, so the
/// switch itself cannot drift from the eligibility rule that guards it.
fn apply_validated_route(agent: &mut Agent, selection: &RouteSelection) -> anyhow::Result<()> {
    let result = agent.set_route_selection(selection);
    if result.is_ok() {
        agent.reset_provider_session();
    }
    result
}

/// Recover the structured route a failover prompt's `target_route` names.
///
/// `target_route` is the `routed_model_spec` of a route the provider itself
/// advertised, so the live catalog is its authoritative inverse. A spec the
/// catalog does not list is parsed from its credential prefix
/// (`openai-oauth@label:model`, `copilot:model`, ...) and accepted only when
/// that selection renders back to exactly the same spec, so a stale or unknown
/// route is refused instead of being applied approximately.
fn autonomous_failover_selection(target_route: &str, agent: &Agent) -> Option<RouteSelection> {
    // The catalog names the specs that carry no prefix of their own
    // (`gemini-2.5-pro`, `model@Provider` for OpenRouter) and keeps the exact
    // runtime identity of every route it still lists.
    if let Some(route) = agent
        .model_routes()
        .into_iter()
        .find(|route| RouteSelection::from_model_route(route).routed_model_spec() == target_route)
    {
        return Some(RouteSelection::from_model_route(&route));
    }

    // A prompt can also name a saved-account rotation the catalog has not
    // listed yet; those specs describe themselves as `method[@account]:model`.
    let (api_method, model) = target_route.split_once(':')?;
    let api_method = api_method.trim();
    let model = model.trim();
    if api_method.is_empty() || model.is_empty() {
        return None;
    }
    let selection = RouteSelection {
        model: model.to_string(),
        runtime_key: horon_agent_provider_core::RuntimeKey::from_api_method(
            &horon_agent_provider_core::ModelRouteApiMethod::parse(api_method),
            "",
        ),
        api_method: api_method.to_string(),
        provider_label: String::new(),
        detail: String::new(),
    };
    // Only a spec that round-trips is the route the prompt approved. Anything
    // else would silently land somewhere the eligibility check never saw.
    (selection.routed_model_spec() == target_route).then_some(selection)
}

/// Apply a provider failover the server decided on its own (GAP-3).
///
/// An unattended run has no client to approve the interactive countdown, so the
/// turn loop calls this when a provider error carries a [`ProviderFailoverPrompt`].
/// `provider.autonomous_failover` gates this here as well as at the call site, so
/// a disabled setting refuses the switch even if the caller forgets to check it.
/// The prompt must name the exact pool-approved route; one without it predates
/// route-carrying failover and still requires a human's consent, and an unknown
/// or no-longer-eligible route is refused with `Err` rather than approximated.
/// Every failure is an error (never a panic), so the caller falls back to its
/// normal error path.
///
/// Source-account eligibility is not re-checked here: only
/// `MultiProvider::cross_provider_pool_prompt` produces one of these prompts,
/// and it already refuses a source account that is out of the auto-switch pool.
pub(crate) async fn apply_autonomous_failover(
    prompt: &ProviderFailoverPrompt,
    agent: &mut Agent,
) -> anyhow::Result<()> {
    if !crate::config::Config::load().provider.autonomous_failover {
        anyhow::bail!("Autonomous failover is disabled; enable provider.autonomous_failover for unattended runs.");
    }

    let Some(target_route) = prompt
        .target_route
        .as_deref()
        .map(str::trim)
        .filter(|target_route| !target_route.is_empty())
    else {
        anyhow::bail!(
            "Autonomous failover needs an explicit target route; prompts without one require manual consent."
        );
    };

    let Some(selection) = autonomous_failover_selection(target_route, agent) else {
        anyhow::bail!("Autonomous failover target '{target_route}' is not a route this session can apply.");
    };

    let previous_model = agent.provider_model();
    apply_validated_route(agent, &selection)?;
    crate::logging::event_info(
        "server_autonomous_failover_applied",
        vec![
            ("target_route", target_route.to_string()),
            ("from_model", previous_model),
            ("to_model", agent.provider_model()),
            ("provider_session", "reset".to_string()),
        ],
    );
    Ok(())
}

pub(super) async fn handle_set_model(
    id: u64,
    model: String,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    if let Ok(mut agent_guard) = agent.try_lock() {
        apply_set_model(id, model, &mut agent_guard, client_event_tx);
    } else {
        spawn_deferred_agent_mutation(
            "set_model",
            id,
            Arc::clone(agent),
            client_event_tx.clone(),
            move |agent_guard, client_event_tx| {
                apply_set_model(id, model, agent_guard, client_event_tx);
            },
        );
    }
}

fn send_catalog_activity(client_event_tx: &mpsc::UnboundedSender<ServerEvent>, message: &str) {
    let _ = client_event_tx.send(ServerEvent::Notification {
        from_session: "horon-agent".to_string(),
        from_name: Some("HoronAgent".to_string()),
        notification_type: NotificationType::Message {
            scope: Some("catalog_activity".to_string()),
            tldr: None,
        },
        message: message.to_string(),
    });
}

pub(super) async fn handle_set_reasoning_effort(
    id: u64,
    effort: String,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let result = if let Ok(mut agent_guard) = agent.try_lock() {
        agent_guard.set_reasoning_effort(&effort)
    } else {
        spawn_deferred_reasoning_effort_change(id, effort, Arc::clone(agent), client_event_tx.clone());
        return;
    };

    send_reasoning_effort_result(id, result, client_event_tx);
}

fn send_reasoning_effort_result(
    id: u64,
    result: anyhow::Result<Option<String>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    match result {
        Ok(effort) => {
            let _ = client_event_tx.send(ServerEvent::ReasoningEffortChanged {
                id,
                effort,
                error: None,
            });
        }
        Err(e) => {
            let _ = client_event_tx.send(ServerEvent::ReasoningEffortChanged {
                id,
                effort: None,
                error: Some(e.to_string()),
            });
        }
    }
}

fn spawn_deferred_reasoning_effort_change(
    id: u64,
    effort: String,
    agent: Arc<Mutex<Agent>>,
    client_event_tx: mpsc::UnboundedSender<ServerEvent>,
) {
    let queued_at = log_provider_control_deferred("set_reasoning_effort", id);
    tokio::spawn(async move {
        let mut agent_guard = agent.lock().await;
        log_provider_control_lock_acquired("set_reasoning_effort", id, queued_at);
        let result = agent_guard.set_reasoning_effort(&effort);
        crate::logging::info(&format!(
            "Deferred reasoning effort change completed request_id={} requested={} success={}",
            id,
            effort,
            result.is_ok()
        ));
        send_reasoning_effort_result(id, result, &client_event_tx);
        log_provider_control_completed("set_reasoning_effort", id, queued_at);
    });
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_notify_auth_changed(
    id: u64,
    provider_hint: Option<String>,
    auth: Option<AuthChanged>,
    prefer_strongest: bool,
    provider: &Arc<dyn Provider>,
    provider_template: &Arc<dyn Provider>,
    sessions: &SessionAgents,
    client_session_id: &str,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let refresh_started = Instant::now();
    crate::auth::AuthStatus::invalidate_cache();
    let (session_id, before_snapshot) = if let Ok(agent_guard) = agent.try_lock() {
        (
            agent_guard.session_id().to_string(),
            super::available_models_cache::for_agent(&agent_guard).snapshot(),
        )
    } else {
        crate::logging::event_warn(
            "SERVER_PROVIDER_CONTROL_DEFERRED",
            vec![
                ("phase", "fallback_snapshot".to_string()),
                ("operation", "notify_auth_changed".to_string()),
                ("request_id", id.to_string()),
                ("session_id", client_session_id.to_string()),
                ("reason", "agent_busy".to_string()),
            ],
        );
        (
            client_session_id.to_string(),
            available_models_snapshot_from_provider(provider),
        )
    };
    let auth_refresh_generation = begin_auth_refresh(&session_id);
    let saved_credentials_changed = auth_change_saved_credentials(auth.as_ref());
    let auth_provider_id = auth_provider_id(provider_hint, auth);
    if let Some(provider_id) = auth_provider_id.as_deref() {
        activate_auth_profile(provider_id, saved_credentials_changed);
    }
    crate::bus::Bus::global().publish(crate::bus::BusEvent::UiActivity(crate::bus::UiActivity::auth(
        Some(session_id.clone()),
        "",
        Some("Auth: refreshing providers..."),
    )));
    let targets = auth_refresh_targets(provider_template, provider, agent, sessions).await;
    let client_event_tx_clone = client_event_tx.clone();
    let agent_clone = agent.clone();
    tokio::spawn(async move {
        if !auth_refresh_is_current(&session_id, auth_refresh_generation) {
            return;
        }
        if let Some(provider_id) = auth_provider_id.as_deref() {
            // A renewed credential must get a real request instead of inheriting
            // the previous credential's cached runtime failure.
            crate::provider::clear_provider_unavailable_for_account(provider_id);
        }
        // Snapshot which providers horon-agent now believes are configured right after
        // an auth change activates. This is the cornerstone for diagnosing
        // "logged in but model picker still empty / only OpenAI+Anthropic" and
        // "paste key silently returns to menu" reports (#312, #292, #304): if a
        // provider the user just configured is not Available here, the failure is
        // upstream of the picker.
        crate::auth::AuthStatus::check_fast().log_snapshot("auth_changed");
        let mut bus_rx = crate::bus::Bus::global().subscribe();
        let AuthRefreshTargets {
            providers,
            session_providers,
            deferred_agents,
        } = targets;
        let mut refresh_providers = providers.clone();
        for candidate in &session_providers {
            if !refresh_providers
                .iter()
                .any(|existing| Arc::ptr_eq(existing, candidate))
            {
                refresh_providers.push(Arc::clone(candidate));
            }
        }
        for provider in providers {
            provider.on_auth_changed();
        }
        for provider in session_providers {
            provider.on_auth_changed_preserve_current_provider();
        }

        let auth_selection_generation = {
            let agent_guard = agent_clone.lock().await;
            agent_guard.provider_model_selection_generation()
        };

        crate::bus::Bus::global().publish_models_updated();
        crate::bus::Bus::global().publish(crate::bus::BusEvent::UiActivity(crate::bus::UiActivity::catalog(
            Some(session_id.clone()),
            "",
            Some("Auth: model routes updating..."),
        )));

        spawn_deferred_auth_refreshes(deferred_agents);

        // Hot-initializing providers is synchronous, while dynamic catalogs may
        // continue refreshing in the background. Push an immediate snapshot so
        // the model picker/header stop looking stale right after login, then
        // push another snapshot when the background refresh announces itself.
        let mut latest_snapshot = available_models_snapshot(&agent_clone).await;
        let _ = client_event_tx_clone.send(available_models_snapshot_into_event(latest_snapshot.clone()));

        // Wait for the catalog work that providers actually launched. The old
        // implementation waited for two stacked 750 ms debounce windows even
        // when every provider had already finished. Tracking real work removes
        // that fixed tax while retaining the 10 s safety ceiling.
        let settle_started = Instant::now();
        let max_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut model_update_events = 0_u64;
        while refresh_providers
            .iter()
            .any(|provider| provider.auth_model_refresh_pending())
            && tokio::time::Instant::now() < max_deadline
        {
            tokio::select! {
                event = bus_rx.recv() => {
                    if matches!(event, Ok(crate::bus::BusEvent::ModelsUpdated)) {
                        model_update_events = model_update_events.saturating_add(1);
                        latest_snapshot = available_models_snapshot(&agent_clone).await;
                        let _ = client_event_tx_clone.send(available_models_snapshot_into_event(latest_snapshot.clone()));
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
            }
        }
        let refresh_timed_out = refresh_providers
            .iter()
            .any(|provider| provider.auth_model_refresh_pending());
        latest_snapshot = available_models_snapshot(&agent_clone).await;
        let _ = client_event_tx_clone.send(available_models_snapshot_into_event(latest_snapshot.clone()));
        let settle_ms = settle_started.elapsed().as_millis();

        if !auth_refresh_is_current(&session_id, auth_refresh_generation) {
            crate::logging::event_info(
                "SERVER_AUTH_MODEL_REFRESH_SUPERSEDED",
                vec![
                    ("session_id", session_id.clone()),
                    ("generation", auth_refresh_generation.to_string()),
                    ("total_ms", refresh_started.elapsed().as_millis().to_string()),
                ],
            );
            finish_auth_refresh(&session_id, auth_refresh_generation);
            return;
        }

        let manual_model_selected_during_auth_refresh = {
            let agent_guard = agent_clone.lock().await;
            agent_guard.user_selected_provider_model_after(auth_selection_generation)
        };
        if manual_model_selected_during_auth_refresh {
            crate::logging::auth_event(
                "auth_changed_auto_model_skipped_after_manual_switch",
                auth_provider_id.as_deref().unwrap_or("auth"),
                &[("reason", "user_selected_provider_model_during_refresh")],
            );
            latest_snapshot = available_models_snapshot(&agent_clone).await;
            let _ = client_event_tx_clone.send(available_models_snapshot_into_event(latest_snapshot.clone()));
        } else {
            if prefer_strongest {
                if let Some(route) = globally_preferred_default_route(&latest_snapshot.model_routes) {
                    apply_auth_route_to_agent(&route, &agent_clone, Some(auth_selection_generation)).await;
                }
            } else if let Some(model_to_select) = auth_model_to_select(
                auth_provider_id.as_deref(),
                crate::config::config().provider.default_model.as_deref(),
                latest_snapshot.provider_model.as_deref(),
                &latest_snapshot.model_routes,
            ) {
                apply_auth_runtime_model_to_agent(
                    auth_provider_id.as_deref(),
                    Some(&model_to_select),
                    &agent_clone,
                    Some(auth_selection_generation),
                )
                .await;
            }
            latest_snapshot = available_models_snapshot(&agent_clone).await;
            let _ = client_event_tx_clone.send(available_models_snapshot_into_event(latest_snapshot.clone()));
        }

        let summary = crate::provider::summarize_model_catalog_refresh(
            before_snapshot.available_models,
            latest_snapshot.available_models.clone(),
            before_snapshot.model_routes,
            latest_snapshot.model_routes.clone(),
        );
        let catalog_message = format_auth_catalog_refresh_complete(
            latest_snapshot.provider_name.as_deref(),
            latest_snapshot.provider_model.as_deref(),
            &summary,
            false,
        );
        crate::logging::event_info(
            "SERVER_AUTH_MODEL_REFRESH_COMPLETED",
            vec![
                ("total_ms", refresh_started.elapsed().as_millis().to_string()),
                ("settle_ms", settle_ms.to_string()),
                ("models_before", summary.model_count_before.to_string()),
                ("models_after", summary.model_count_after.to_string()),
                ("routes_before", summary.route_count_before.to_string()),
                ("routes_after", summary.route_count_after.to_string()),
                ("model_update_events", model_update_events.to_string()),
                ("timed_out", refresh_timed_out.to_string()),
            ],
        );
        send_catalog_activity(&client_event_tx_clone, &catalog_message);
        finish_auth_refresh(&session_id, auth_refresh_generation);
    });
    let _ = client_event_tx.send(ServerEvent::Done { id });
}

#[cfg(test)]
#[path = "provider_control_tests.rs"]
mod provider_control_tests;

pub(super) async fn handle_invalidate_openai_usage(
    id: u64,
    _account_label: Option<String>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    // Usage is reported from the local activity ledger in this build; there is
    // no vendor quota state to invalidate. A retry is harmless, so acknowledge.
    let _ = client_event_tx.send(ServerEvent::Done { id });
}

#[cfg(test)]
#[path = "provider_control_busy_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "provider_control_autonomous_failover_tests.rs"]
mod autonomous_failover_tests;
