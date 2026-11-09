//! Live model pricing catalog backed by <https://models.dev>.
//!
//! horon-agent's static pricing tables (`horon_agent_provider_core::pricing`) only cover
//! first-party Anthropic/OpenAI models and go stale whenever a provider ships
//! new models or changes prices. models.dev publishes a free, no-auth JSON
//! catalog (`https://models.dev/api.json`) with per-model `input`/`output`/
//! `cache_read`/`cache_write` USD prices per million tokens across 140+
//! providers, including every OpenAI-compatible profile horon-agent ships.
//!
//! This module mirrors the OpenRouter catalog pattern:
//!   - a 24h disk cache under `~/.horon/cache/models_dev_pricing.json`,
//!   - synchronous lookups that never block on the network,
//!   - a background refresh scheduled on cache miss/staleness.
//!
//! Lookup order for callers is curated static table first (exact, reviewed),
//! then this catalog, then provider-specific sources (OpenRouter endpoints),
//! and only then a generic fallback. `openai-compatible:` profiles that
//! models.dev does not list as a provider (resellers/aggregators such as
//! `kilocode`) additionally fall back to a cross-provider match of the same
//! model id: they bill the vendor's tokens, so vendor pricing seen under
//! another provider id is the price that applies.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const API_URL: &str = "https://models.dev/api.json";
const CACHE_FILE: &str = "models_dev_pricing.json";
const CACHE_TTL_SECS: u64 = 24 * 60 * 60;
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);

/// Per-model USD prices per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelCost {
    pub input_usd_per_mtok: f64,
    pub output_usd_per_mtok: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_usd_per_mtok: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_usd_per_mtok: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PricingCache {
    cached_at_unix_secs: u64,
    /// provider id -> model id -> cost. Provider ids are models.dev ids
    /// (e.g. `anthropic`, `openai`, `deepseek`, `moonshotai`).
    providers: HashMap<String, HashMap<String, ModelCost>>,
}

/// In-memory pricing cache keyed by the on-disk cache path it was loaded
/// from, so changing `HORON_HOME` (tests, multi-home setups) never serves
/// pricing that belongs to a different home directory.
///
/// Held behind an `Arc` so per-route pricing lookups share one parsed catalog
/// instead of deep-cloning the multi-thousand-model map on every call (that
/// clone dominated server CPU during client connect bursts).
static MEMORY_CACHE: Mutex<Option<(PathBuf, Arc<PricingCache>)>> = Mutex::new(None);
static REFRESH_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// On-disk catalog reads observed by [`load_cache`]. Test-only: it is what
/// proves a startup with refreshes disabled never opens the cache file.
#[cfg(test)]
static CACHE_DISK_READS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn cache_path() -> PathBuf {
    crate::storage::horon_agent_dir()
        .unwrap_or_else(|_| PathBuf::from(".").join(".horon"))
        .join("cache")
        .join(CACHE_FILE)
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn load_cache() -> Option<Arc<PricingCache>> {
    let path = cache_path();
    {
        let memory = MEMORY_CACHE.lock().ok()?;
        if let Some((cached_path, cache)) = memory.as_ref()
            && cached_path == &path
        {
            return Some(Arc::clone(cache));
        }
    }
    #[cfg(test)]
    CACHE_DISK_READS.fetch_add(1, Ordering::Relaxed);
    let cache: PricingCache = crate::storage::read_json(&path).ok()?;
    let cache = Arc::new(cache);
    if let Ok(mut memory) = MEMORY_CACHE.lock() {
        *memory = Some((path, Arc::clone(&cache)));
    }
    Some(cache)
}

fn save_cache(cache: &PricingCache) {
    let path = cache_path();
    if let Ok(mut memory) = MEMORY_CACHE.lock() {
        *memory = Some((path.clone(), Arc::new(cache.clone())));
    }
    let _ = crate::storage::write_json(&path, cache);
}

/// Translate a horon-agent provider key (runtime key, activity source key, or
/// compatible-profile id) to the models.dev provider id.
pub fn models_dev_provider_id(horon_agent_provider: &str) -> Option<&'static str> {
    let key = horon_agent_provider
        .trim()
        .strip_prefix("openai-compatible:")
        .unwrap_or_else(|| horon_agent_provider.trim());
    Some(match key {
        "anthropic" | "claude" | "claude:api-key" | "anthropic-api" => "anthropic",
        "openai" | "openai:api-key" | "openai-api" => "openai",
        "openrouter" => "openrouter",
        "opencode" => "opencode",
        "opencode-go" => "opencode-go",
        "deepseek" => "deepseek",
        "moonshotai" => "moonshotai",
        "kimi" => "kimi-for-coding",
        "zai" => "zai",
        "cerebras" => "cerebras",
        "groq" => "groq",
        "mistral" => "mistral",
        "xai" => "xai",
        "minimax" => "minimax",
        "togetherai" => "togetherai",
        "fireworks" => "fireworks-ai",
        "deepinfra" => "deepinfra",
        "perplexity" => "perplexity",
        "nebius" => "nebius",
        "scaleway" => "scaleway",
        "stackit" => "stackit",
        "huggingface" => "huggingface",
        "baseten" => "baseten",
        "chutes" => "chutes",
        "nvidia-nim" => "nvidia",
        "302ai" => "302ai",
        "cortecs" => "cortecs",
        "alibaba-coding-plan" => "alibaba",
        "bedrock" => "amazon-bedrock",
        "azure-openai" | "azure" => "azure",
        "gemini" | "gemini-api" => "google",
        _ => return None,
    })
}

/// Strip horon-agent-local suffixes/prefixes a model id may carry before catalog
/// lookup (`[1m]` long-context alias, `provider/` prefixes for OpenRouter ids).
fn normalize_model_id(model: &str) -> &str {
    let model = horon_agent_provider_core::model_id::strip_long_context_suffix(model).trim();
    model.rsplit_once('@').map_or(model, |(bare, _)| bare.trim())
}

/// Look up live pricing for `model` under a horon-agent provider key. Returns `None`
/// when the catalog has no entry; never blocks on the network. Schedules a
/// background refresh when the disk cache is missing or stale.
pub fn lookup(horon_agent_provider: &str, model: &str) -> Option<ModelCost> {
    lookup_with_provenance(horon_agent_provider, model).map(|(cost, _)| cost)
}

/// Where a [`lookup_with_provenance`] price came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PricingMatch {
    /// The provider's own models.dev table listed the model.
    ProviderTable,
    /// Inferred from other providers' listings of the same model id (reseller
    /// fallback). Plausible but not authoritative for this route.
    CrossProvider,
}

/// Like [`lookup`], but also reports whether the price came from the
/// provider's own table or from the cross-provider reseller fallback.
pub fn lookup_with_provenance(horon_agent_provider: &str, model: &str) -> Option<(ModelCost, PricingMatch)> {
    let is_openai_compatible = horon_agent_provider.trim().starts_with("openai-compatible:");
    let cache = ensure_cache_fresh()?;
    let mapped_provider = models_dev_provider_id(horon_agent_provider);
    if let Some(provider_id) = mapped_provider
        && let Some(models) = cache.providers.get(provider_id)
    {
        let model = normalize_model_id(model);
        if let Some(cost) = models.get(model) {
            return Some((*cost, PricingMatch::ProviderTable));
        }
        // OpenRouter-style ids (`anthropic/claude-...`) may reach here with the
        // provider prefix still attached; retry on the bare model name.
        if let Some((_, bare)) = model.rsplit_once('/')
            && let Some(cost) = models.get(bare)
        {
            return Some((*cost, PricingMatch::ProviderTable));
        }
    }
    // Reseller/aggregator keys that models.dev does not list as a provider at
    // all (`openai-compatible:kilocode`, ...) still serve vendor models under
    // vendor ids. Match the same id across the other providers' tables instead
    // of assuming unpriced. A profile that *does* have a models.dev mapping
    // keeps its own table as the authority: a model missing there is left
    // unpriced rather than billed at an unrelated provider's rate. First-party
    // providers never take this path either, and neither do local/keyless
    // endpoints (Ollama, LM Studio, localhost profiles): they serve vendor
    // model ids for free, so borrowing a hosted price would invent spend.
    if is_openai_compatible && mapped_provider.is_none() && !openai_compatible_profile_is_local(horon_agent_provider) {
        return cross_provider_lookup(&cache, model).map(|cost| (cost, PricingMatch::CrossProvider));
    }
    None
}

/// True when an `openai-compatible:<id>` profile runs locally or without an
/// API key, so its traffic is not billed at hosted rates. Unknown ids (not a
/// built-in profile and not a configured `[providers.<id>]`) are treated as
/// hosted resellers.
fn openai_compatible_profile_is_local(horon_agent_provider: &str) -> bool {
    let Some(id) = horon_agent_provider
        .trim()
        .strip_prefix("openai-compatible:")
        .map(str::trim)
    else {
        return false;
    };
    if let Some(profile) = crate::provider_catalog::openai_compatible_profile_by_id(id) {
        return !profile.requires_api_key || api_base_is_local(profile.api_base);
    }
    if let Some(named) = crate::config::config().providers.get(id) {
        return api_base_is_local(&named.base_url)
            || named.requires_api_key == Some(false)
            || named.auth == crate::config::NamedProviderAuth::None;
    }
    false
}

/// Loopback, private-network, link-local, and `.local`/`.lan` hosts.
fn api_base_is_local(api_base: &str) -> bool {
    let Ok(url) = url::Url::parse(api_base.trim()) else {
        return false;
    };
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
        Some(url::Host::Domain(host)) => {
            let host = host.to_ascii_lowercase();
            host == "localhost"
                || host.ends_with(".localhost")
                || host.ends_with(".local")
                || host.ends_with(".lan")
                || host == "host.docker.internal"
        }
        None => false,
    }
}

/// Match `model` (normalized, then bare after a `vendor/` split) against
/// every provider table in the catalog. Exact id matches win over bare-name
/// matches; among matches the modal (most common) price wins, with the
/// lexicographically-first provider id as deterministic tie-break.
fn cross_provider_lookup(cache: &PricingCache, model: &str) -> Option<ModelCost> {
    let normalized = normalize_model_id(model);
    let bare = normalized.rsplit_once('/').map_or(normalized, |(_, bare)| bare.trim());
    let mut exact: Vec<(&str, ModelCost)> = Vec::new();
    let mut bare_named: Vec<(&str, ModelCost)> = Vec::new();
    for (provider_id, models) in &cache.providers {
        if let Some(cost) = models.get(normalized) {
            exact.push((provider_id.as_str(), *cost));
        } else if let Some(cost) = models.get(bare) {
            bare_named.push((provider_id.as_str(), *cost));
        }
    }
    exact.sort_unstable_by(|a, b| a.0.cmp(b.0));
    bare_named.sort_unstable_by(|a, b| a.0.cmp(b.0));
    let candidates = if exact.is_empty() { &bare_named } else { &exact };
    modal_pricing(candidates)
}

/// Most common price among candidates; price ties resolve to the first
/// candidate in the (provider-sorted) list so repeated lookups agree.
fn modal_pricing(candidates: &[(&str, ModelCost)]) -> Option<ModelCost> {
    let mut counts: HashMap<[u64; 4], usize> = HashMap::new();
    for (_, cost) in candidates {
        *counts.entry(cost_key(cost)).or_default() += 1;
    }
    let max_count = counts.values().copied().max()?;
    candidates
        .iter()
        .find(|(_, cost)| counts.get(&cost_key(cost)) == Some(&max_count))
        .map(|(_, cost)| *cost)
}

/// Key a price for modal counting. `None` cache rates get a distinct
/// sentinel: a missing cache rate is not the same listing as an explicitly
/// free one, and merging them would let a missing/zero rate outvote a paid
/// cache majority.
fn cost_key(cost: &ModelCost) -> [u64; 4] {
    const MISSING: u64 = u64::MAX;
    [
        cost.input_usd_per_mtok.to_bits(),
        cost.output_usd_per_mtok.to_bits(),
        cost.cache_read_usd_per_mtok.map_or(MISSING, f64::to_bits),
        cost.cache_write_usd_per_mtok.map_or(MISSING, f64::to_bits),
    ]
}

/// Whether a loaded catalog is missing or older than the TTL.
fn cache_is_stale(cache: Option<&Arc<PricingCache>>) -> bool {
    cache.is_none_or(|c| now_unix_secs().saturating_sub(c.cached_at_unix_secs) >= CACHE_TTL_SECS)
}

/// Return the freshest cache available, scheduling a refresh if needed.
fn ensure_cache_fresh() -> Option<Arc<PricingCache>> {
    let cache = load_cache();
    if cache_is_stale(cache.as_ref()) {
        schedule_refresh();
    }
    cache
}

/// Load the on-disk catalog and schedule a background refresh when it is
/// missing or stale.
///
/// A process that did not ask for refreshes returns before touching the disk,
/// so the default one-shot never parses the pricing catalog; lookups keep
/// their own lazy read for the priced-model paths that need it. When refreshes
/// are enabled this is a cheap memory-cache hit for a fresh catalog.
pub fn ensure_fresh() {
    // Test builds must not read or write the real home (process-global test
    // sandboxes swap `HORON_HOME` between cases) and never reach the
    // network unless the environment opts in, so the gate comes first.
    if !refresh_requested_now() {
        return;
    }
    let cache = load_cache();
    if cache_is_stale(cache.as_ref()) {
        schedule_refresh();
    }
}

/// Refresh the catalog and log the outcome; the caller already passed the gate.
async fn refresh_with_logging() {
    if let Err(e) = refresh_now().await {
        crate::logging::warn(&format!("models.dev pricing refresh failed: {e:#}"));
    }
}

/// Startup hook: settle the catalog before the process does any work.
///
/// When refreshes are enabled and no catalog exists yet, fetch it inline: a
/// one-shot process can exit long before a background fetch finishes, so an
/// operator who asked for the catalog would otherwise never get one. An
/// existing catalog (fresh or stale) only schedules the usual background
/// refresh, so enabling this costs one fetch, not a fetch per start.
pub fn ensure_fresh_at_startup() {
    // Test builds must not read or write the real home and never reach the
    // network unless the environment opts in. Disabled refreshes return here
    // without reading the catalog at all.
    if !refresh_requested_now() {
        return;
    }
    let cache = load_cache();
    if cache.is_some() {
        if cache_is_stale(cache.as_ref()) {
            schedule_refresh();
        }
        return;
    }
    if REFRESH_IN_FLIGHT.swap(true, Ordering::SeqCst) {
        return;
    }
    // A plain thread plus `join`: entering another runtime from inside the
    // caller's async context panics, and the join is what guarantees the file
    // exists before a short-lived process ends.
    let worker = std::thread::spawn(|| {
        let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(runtime) => runtime,
            Err(err) => return Err(anyhow::anyhow!("pricing refresh runtime: {err}")),
        };
        runtime.block_on(refresh_now())
    });
    match worker.join() {
        Ok(Ok(())) => {}
        Ok(Err(err)) => crate::logging::warn(&format!("models.dev pricing refresh failed: {err:#}")),
        Err(_) => crate::logging::warn("models.dev pricing refresh worker panicked"),
    }
    REFRESH_IN_FLIGHT.store(false, Ordering::SeqCst);
}

/// Environment switch for the background catalog refresh. `1` enables it even
/// in a test build (manual e2e checks); `0` disables it regardless of
/// `[features] pricing_refresh`.
const REFRESH_ENV: &str = "HORON_AGENT_PRICING_REFRESH";

/// The environment override, when the variable names a value we understand.
fn refresh_env_override() -> Option<bool> {
    let Ok(raw) = std::env::var(REFRESH_ENV) else {
        // Unset (or not valid UTF-8): leave the decision to the config.
        return None;
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Whether a refresh is wanted for a process whose config says
/// `config_enabled`. The environment wins when it is set.
fn refresh_wanted(config_enabled: bool) -> bool {
    refresh_env_override().unwrap_or(config_enabled)
}

/// Whether this process may start a refresh. Test builds never reach the
/// network unless the environment opts in explicitly, so the unit tests stay
/// hermetic.
fn refresh_requested(config_enabled: bool) -> bool {
    match refresh_env_override() {
        Some(force) => force,
        None => !cfg!(any(test, feature = "test-support")) && refresh_wanted(config_enabled),
    }
}

/// Whether this process wants a catalog refresh, decided before any file
/// access.
///
/// The environment wins when set, and an unset environment in a test build is
/// a "no" without resolving the config, so the startup hooks cannot read the
/// real home of a test binary just to answer this question.
fn refresh_requested_now() -> bool {
    match refresh_env_override() {
        Some(force) => force,
        None if cfg!(any(test, feature = "test-support")) => false,
        None => refresh_wanted(crate::config::config().features.pricing_refresh),
    }
}

/// Spawn one background refresh at a time. Safe to call from sync contexts;
/// uses a thread + ad-hoc runtime when no Tokio runtime is active.
///
/// Off by default: a refresh only starts when `[features] pricing_refresh` is
/// true or `HORON_AGENT_PRICING_REFRESH=1`. The on-disk cache is still read
/// either way, so a previously fetched catalog keeps working without the
/// network.
pub fn schedule_refresh() {
    if !refresh_requested(crate::config::config().features.pricing_refresh) {
        return;
    }
    if REFRESH_IN_FLIGHT.swap(true, Ordering::SeqCst) {
        return;
    }
    let work = || async {
        refresh_with_logging().await;
        REFRESH_IN_FLIGHT.store(false, Ordering::SeqCst);
    };
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(work());
    } else {
        std::thread::spawn(move || {
            if let Ok(runtime) = tokio::runtime::Runtime::new() {
                runtime.block_on(work());
            } else {
                REFRESH_IN_FLIGHT.store(false, Ordering::SeqCst);
            }
        });
    }
}

/// Fetch the catalog and persist the parsed cache.
async fn refresh_now() -> anyhow::Result<()> {
    let client = crate::provider::shared_http_client();
    let response = client
        .get(API_URL)
        .header("Accept", "application/json")
        .timeout(HTTP_TIMEOUT)
        .send()
        .await?;
    if !response.status().is_success() {
        anyhow::bail!("HTTP {}", response.status());
    }
    let body = response.text().await?;
    let cache = parse_api_response(&body)?;
    save_cache(&cache);
    crate::logging::info(&format!(
        "models.dev pricing refreshed: {} providers, {} priced models",
        cache.providers.len(),
        cache.providers.values().map(HashMap::len).sum::<usize>()
    ));
    Ok(())
}

fn parse_api_response(body: &str) -> anyhow::Result<PricingCache> {
    let json: serde_json::Value = serde_json::from_str(body)?;
    let top = json
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("expected top-level provider object"))?;

    let mut providers: HashMap<String, HashMap<String, ModelCost>> = HashMap::new();
    for (provider_id, provider) in top {
        let Some(models) = provider.get("models").and_then(|m| m.as_object()) else {
            continue;
        };
        let mut parsed_models = HashMap::new();
        for (model_id, model) in models {
            let Some(cost) = model.get("cost") else {
                continue;
            };
            let (Some(input), Some(output)) = (
                cost.get("input").and_then(|v| v.as_f64()),
                cost.get("output").and_then(|v| v.as_f64()),
            ) else {
                continue;
            };
            parsed_models.insert(
                model_id.clone(),
                ModelCost {
                    input_usd_per_mtok: input,
                    output_usd_per_mtok: output,
                    cache_read_usd_per_mtok: cost.get("cache_read").and_then(|v| v.as_f64()),
                    cache_write_usd_per_mtok: cost.get("cache_write").and_then(|v| v.as_f64()),
                },
            );
        }
        if !parsed_models.is_empty() {
            providers.insert(provider_id.clone(), parsed_models);
        }
    }

    if providers.is_empty() {
        anyhow::bail!("no priced models in models.dev response");
    }
    Ok(PricingCache {
        cached_at_unix_secs: now_unix_secs(),
        providers,
    })
}

#[cfg(test)]
pub(crate) fn save_test_cache(entries: &[(&str, &str, ModelCost)]) {
    let mut providers: HashMap<String, HashMap<String, ModelCost>> = HashMap::new();
    for (provider, model, cost) in entries {
        providers
            .entry((*provider).to_string())
            .or_default()
            .insert((*model).to_string(), *cost);
    }
    save_cache(&PricingCache {
        cached_at_unix_secs: now_unix_secs(),
        providers,
    });
}

#[cfg(test)]
pub(crate) fn clear_memory_cache_for_tests() {
    if let Ok(mut memory) = MEMORY_CACHE.lock() {
        *memory = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Restore the refresh switch so a test never leaks its value into another.
    fn restore_refresh_env(prev: Option<std::ffi::OsString>) {
        match prev {
            Some(value) => crate::env::set_var(REFRESH_ENV, value),
            None => crate::env::remove_var(REFRESH_ENV),
        }
    }

    #[test]
    fn refresh_is_off_by_default() {
        let _guard = crate::storage::lock_test_env();
        let prev = std::env::var_os(REFRESH_ENV);
        crate::env::remove_var(REFRESH_ENV);

        // Shipped defaults keep the toggle off, so a one-shot run never
        // contacts models.dev unless the operator asks for it.
        assert!(!crate::config::FeatureConfig::default().pricing_refresh);
        assert!(!refresh_wanted(false));
        assert!(!refresh_requested(false));

        restore_refresh_env(prev);
    }

    #[test]
    fn env_override_enables_refresh() {
        let _guard = crate::storage::lock_test_env();
        let prev = std::env::var_os(REFRESH_ENV);

        crate::env::set_var(REFRESH_ENV, "1");
        assert!(refresh_wanted(false));
        // An explicit opt-in also reaches manual e2e runs made from a test build.
        assert!(refresh_requested(false));

        crate::env::set_var(REFRESH_ENV, "0");
        assert!(!refresh_wanted(true));
        assert!(!refresh_requested(true));

        restore_refresh_env(prev);
    }

    #[test]
    fn config_enables_refresh() {
        let _guard = crate::storage::lock_test_env();
        let prev = std::env::var_os(REFRESH_ENV);
        crate::env::remove_var(REFRESH_ENV);

        assert!(refresh_wanted(true));
        // Test builds stay hermetic even when the config asks for a refresh;
        // only the environment can start one from a test binary.
        assert!(!refresh_requested(true));

        restore_refresh_env(prev);
    }

    #[test]
    fn startup_skips_cache_read_when_refresh_off() {
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var_os("HORON_HOME");
        let prev_refresh = std::env::var_os(REFRESH_ENV);
        crate::env::set_var("HORON_HOME", temp.path());
        crate::env::remove_var(REFRESH_ENV);
        clear_memory_cache_for_tests();

        // Leave a cache file behind so an unconditional startup read would be
        // visible in the counter.
        save_test_cache(&[(
            "anthropic",
            "claude-sonnet-4",
            ModelCost {
                input_usd_per_mtok: 3.0,
                output_usd_per_mtok: 15.0,
                cache_read_usd_per_mtok: Some(0.3),
                cache_write_usd_per_mtok: None,
            },
        )]);
        clear_memory_cache_for_tests();
        let reads_before = CACHE_DISK_READS.load(Ordering::Relaxed);

        ensure_fresh();
        ensure_fresh_at_startup();

        assert_eq!(
            CACHE_DISK_READS.load(Ordering::Relaxed),
            reads_before,
            "a disabled refresh must not read the pricing cache"
        );

        // Positive control: the catalog is read only when a refresh is enabled.
        crate::env::set_var(REFRESH_ENV, "1");
        ensure_fresh_at_startup();
        assert!(
            CACHE_DISK_READS.load(Ordering::Relaxed) > reads_before,
            "enabling the refresh must read the catalog to decide freshness"
        );

        clear_memory_cache_for_tests();
        restore_refresh_env(prev_refresh);
        match prev_home {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
    }

    #[test]
    fn resellers_fall_back_to_vendor_pricing_seen_elsewhere() {
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", temp.path());
        clear_memory_cache_for_tests();

        save_test_cache(&[
            (
                "zai",
                "glm-5.3-flash",
                ModelCost {
                    input_usd_per_mtok: 0.15,
                    output_usd_per_mtok: 0.5,
                    cache_read_usd_per_mtok: Some(0.03),
                    cache_write_usd_per_mtok: Some(0.0),
                },
            ),
            (
                "zenmux",
                "z-ai/glm-5.3-flash",
                ModelCost {
                    input_usd_per_mtok: 0.15,
                    output_usd_per_mtok: 0.5,
                    cache_read_usd_per_mtok: Some(0.03),
                    cache_write_usd_per_mtok: Some(0.0),
                },
            ),
            (
                "vultr",
                "glm-5.3-flash",
                ModelCost {
                    input_usd_per_mtok: 0.10,
                    output_usd_per_mtok: 0.35,
                    cache_read_usd_per_mtok: None,
                    cache_write_usd_per_mtok: None,
                },
            ),
        ]);

        // Unknown to models.dev as a provider, but the same vendor model id is
        // priced under other providers; the exact-id match supplies the price.
        let cost = lookup("openai-compatible:kilocode", "z-ai/glm-5.3-flash").expect("priced");
        assert!((cost.input_usd_per_mtok - 0.15).abs() < 1e-9);
        assert!((cost.output_usd_per_mtok - 0.5).abs() < 1e-9);
        assert_eq!(cost.cache_read_usd_per_mtok, Some(0.03));

        // Unknown models stay unpriced.
        assert!(lookup("openai-compatible:kilocode", "z-ai/no-such-model").is_none());
        // First-party providers never use the fallback (curated lists stay curated).
        assert!(lookup("claude:api-key", "z-ai/glm-5.3-flash").is_none());

        // Modal pricing beats the alphabetically-first provider: three bare-id
        // listings where the majority price is not the first candidate's.
        save_test_cache(&[
            (
                "a-provider",
                "tiny-model",
                ModelCost {
                    input_usd_per_mtok: 0.10,
                    output_usd_per_mtok: 0.99,
                    cache_read_usd_per_mtok: None,
                    cache_write_usd_per_mtok: None,
                },
            ),
            (
                "m-provider",
                "tiny-model",
                ModelCost {
                    input_usd_per_mtok: 0.20,
                    output_usd_per_mtok: 1.50,
                    cache_read_usd_per_mtok: Some(0.02),
                    cache_write_usd_per_mtok: None,
                },
            ),
            (
                "z-provider",
                "tiny-model",
                ModelCost {
                    input_usd_per_mtok: 0.20,
                    output_usd_per_mtok: 1.50,
                    cache_read_usd_per_mtok: Some(0.02),
                    cache_write_usd_per_mtok: None,
                },
            ),
        ]);
        let cost = lookup("openai-compatible:kilocode", "acme/tiny-model").expect("priced");
        assert!((cost.input_usd_per_mtok - 0.20).abs() < 1e-9);
        assert!((cost.output_usd_per_mtok - 1.50).abs() < 1e-9);
        assert_eq!(cost.cache_read_usd_per_mtok, Some(0.02));

        // Exact-id match beats bare-name match: when one provider lists the
        // full `vendor/model` id and others list only the bare name, the
        // full-id listing is the authority even if the bare listings agree on
        // a competing price and outnumber it.
        save_test_cache(&[
            (
                "first-party-vendor",
                "acme/tiny-model",
                ModelCost {
                    input_usd_per_mtok: 0.30,
                    output_usd_per_mtok: 2.50,
                    cache_read_usd_per_mtok: Some(0.05),
                    cache_write_usd_per_mtok: None,
                },
            ),
            (
                "reseller-bare-listing",
                "tiny-model",
                ModelCost {
                    input_usd_per_mtok: 0.99,
                    output_usd_per_mtok: 9.99,
                    cache_read_usd_per_mtok: None,
                    cache_write_usd_per_mtok: None,
                },
            ),
            (
                "another-bare-listing",
                "tiny-model",
                ModelCost {
                    input_usd_per_mtok: 0.99,
                    output_usd_per_mtok: 9.99,
                    cache_read_usd_per_mtok: None,
                    cache_write_usd_per_mtok: None,
                },
            ),
        ]);
        let cost = lookup("openai-compatible:kilocode", "acme/tiny-model").expect("priced");
        assert!((cost.input_usd_per_mtok - 0.30).abs() < 1e-9);
        assert!((cost.output_usd_per_mtok - 2.50).abs() < 1e-9);
        assert_eq!(cost.cache_read_usd_per_mtok, Some(0.05));

        clear_memory_cache_for_tests();
        if let Some(prev) = prev_home {
            crate::env::set_var("HORON_HOME", prev);
        } else {
            crate::env::remove_var("HORON_HOME");
        }
    }

    #[test]
    fn local_profiles_do_not_borrow_hosted_pricing() {
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", temp.path());
        clear_memory_cache_for_tests();

        save_test_cache(&[(
            "ollama-cloud",
            "gpt-oss:20b",
            ModelCost {
                input_usd_per_mtok: 0.07,
                output_usd_per_mtok: 0.3,
                cache_read_usd_per_mtok: None,
                cache_write_usd_per_mtok: None,
            },
        )]);

        // Built-in local/keyless profiles run the model for free.
        assert!(lookup("openai-compatible:ollama", "gpt-oss:20b").is_none());
        assert!(lookup("openai-compatible:lmstudio", "gpt-oss:20b").is_none());
        // An unknown hosted reseller still gets the inferred price, flagged
        // as cross-provider.
        let (_, matched) = lookup_with_provenance("openai-compatible:kilocode", "gpt-oss:20b").expect("priced");
        assert_eq!(matched, PricingMatch::CrossProvider);

        clear_memory_cache_for_tests();
        if let Some(prev) = prev_home {
            crate::env::set_var("HORON_HOME", prev);
        } else {
            crate::env::remove_var("HORON_HOME");
        }
    }

    #[test]
    fn local_api_bases_are_detected() {
        for base in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:8080/v1",
            "http://[::1]:8080/v1",
            "http://192.168.1.20:1234/v1",
            "http://10.0.0.5/v1",
            "http://gpu-box.local:8000/v1",
            "http://host.docker.internal:11434/v1",
        ] {
            assert!(api_base_is_local(base), "{base} should be local");
        }
        for base in [
            "https://api.kilo.ai/api/openrouter",
            "https://openrouter.ai/api/v1",
            "not a url",
        ] {
            assert!(!api_base_is_local(base), "{base} should be hosted");
        }
    }

    #[test]
    fn mapped_compatible_profiles_do_not_borrow_foreign_pricing() {
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", temp.path());
        clear_memory_cache_for_tests();

        save_test_cache(&[
            (
                "deepseek",
                "deepseek-chat",
                ModelCost {
                    input_usd_per_mtok: 0.28,
                    output_usd_per_mtok: 0.42,
                    cache_read_usd_per_mtok: Some(0.028),
                    cache_write_usd_per_mtok: None,
                },
            ),
            (
                "zai",
                "glm-5.3-flash",
                ModelCost {
                    input_usd_per_mtok: 0.15,
                    output_usd_per_mtok: 0.50,
                    cache_read_usd_per_mtok: Some(0.03),
                    cache_write_usd_per_mtok: None,
                },
            ),
        ]);

        // A mapped profile reads its own table.
        let owned = lookup("openai-compatible:deepseek", "deepseek-chat").expect("priced");
        assert!((owned.input_usd_per_mtok - 0.28).abs() < 1e-9);

        // A model absent from the mapped profile's own table stays unpriced
        // even though the catalog lists it under a different provider: the
        // mapping is the authority, so another provider's rate is never
        // inherited.
        assert!(lookup("openai-compatible:deepseek", "glm-5.3-flash").is_none());

        clear_memory_cache_for_tests();
        if let Some(prev) = prev_home {
            crate::env::set_var("HORON_HOME", prev);
        } else {
            crate::env::remove_var("HORON_HOME");
        }
    }

    #[test]
    fn missing_cache_rates_do_not_merge_with_free_ones() {
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", temp.path());
        clear_memory_cache_for_tests();

        // Alphabetically-first listings have a missing cache_read and an
        // explicitly free one; two later listings agree on a paid rate.
        // Treating missing (None) and free (0.0) as the same listing would
        // merge them into a 2-2 tie that the alphabetically-first provider
        // wins, reporting a missing rate instead of the paid majority.
        save_test_cache(&[
            (
                "a-missing",
                "tiny-model",
                ModelCost {
                    input_usd_per_mtok: 0.15,
                    output_usd_per_mtok: 0.50,
                    cache_read_usd_per_mtok: None,
                    cache_write_usd_per_mtok: None,
                },
            ),
            (
                "b-free",
                "tiny-model",
                ModelCost {
                    input_usd_per_mtok: 0.15,
                    output_usd_per_mtok: 0.50,
                    cache_read_usd_per_mtok: Some(0.0),
                    cache_write_usd_per_mtok: None,
                },
            ),
            (
                "c-paid",
                "tiny-model",
                ModelCost {
                    input_usd_per_mtok: 0.15,
                    output_usd_per_mtok: 0.50,
                    cache_read_usd_per_mtok: Some(0.03),
                    cache_write_usd_per_mtok: None,
                },
            ),
            (
                "d-paid",
                "tiny-model",
                ModelCost {
                    input_usd_per_mtok: 0.15,
                    output_usd_per_mtok: 0.50,
                    cache_read_usd_per_mtok: Some(0.03),
                    cache_write_usd_per_mtok: None,
                },
            ),
        ]);

        let cost = lookup("openai-compatible:kilocode", "tiny-model").expect("priced");
        assert!((cost.input_usd_per_mtok - 0.15).abs() < 1e-9);
        assert_eq!(cost.cache_read_usd_per_mtok, Some(0.03));

        clear_memory_cache_for_tests();
        if let Some(prev) = prev_home {
            crate::env::set_var("HORON_HOME", prev);
        } else {
            crate::env::remove_var("HORON_HOME");
        }
    }

    #[test]
    fn parses_models_dev_shape() {
        let body = r#"{
            "deepseek": {
                "id": "deepseek",
                "models": {
                    "deepseek-v4-flash": {
                        "cost": {"input": 0.14, "output": 0.28, "cache_read": 0.0028}
                    },
                    "free-model": {"cost": {"input": 0, "output": 0}},
                    "no-cost-model": {}
                }
            },
            "anthropic": {
                "models": {
                    "claude-fable-5": {
                        "cost": {"input": 10, "output": 50, "cache_read": 1, "cache_write": 12.5}
                    }
                }
            }
        }"#;
        let cache = parse_api_response(body).expect("parsed");
        let deepseek = cache.providers.get("deepseek").expect("deepseek");
        assert_eq!(deepseek.len(), 2, "model without cost is skipped");
        let flash = deepseek.get("deepseek-v4-flash").expect("flash");
        assert!((flash.input_usd_per_mtok - 0.14).abs() < 1e-9);
        assert!((flash.output_usd_per_mtok - 0.28).abs() < 1e-9);
        assert_eq!(flash.cache_read_usd_per_mtok, Some(0.0028));
        assert_eq!(flash.cache_write_usd_per_mtok, None);

        let fable = cache
            .providers
            .get("anthropic")
            .and_then(|m| m.get("claude-fable-5"))
            .expect("fable");
        assert!((fable.input_usd_per_mtok - 10.0).abs() < 1e-9);
        assert_eq!(fable.cache_write_usd_per_mtok, Some(12.5));
    }

    #[test]
    fn rejects_empty_response() {
        assert!(parse_api_response("{}").is_err());
        assert!(parse_api_response("[]").is_err());
    }

    #[test]
    fn provider_key_mapping_covers_horon_agent_providers() {
        assert_eq!(models_dev_provider_id("claude:api-key"), Some("anthropic"));
        assert_eq!(models_dev_provider_id("openai:api-key"), Some("openai"));
        assert_eq!(models_dev_provider_id("openai-compatible:deepseek"), Some("deepseek"));
        assert_eq!(models_dev_provider_id("openai-compatible:nvidia-nim"), Some("nvidia"));
        assert_eq!(models_dev_provider_id("bedrock"), Some("amazon-bedrock"));
        assert_eq!(models_dev_provider_id("unknown-thing"), None);
    }

    #[test]
    fn lookup_normalizes_model_ids() {
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", temp.path());
        clear_memory_cache_for_tests();

        save_test_cache(&[
            (
                "anthropic",
                "claude-opus-4-6",
                ModelCost {
                    input_usd_per_mtok: 5.0,
                    output_usd_per_mtok: 25.0,
                    cache_read_usd_per_mtok: Some(0.5),
                    cache_write_usd_per_mtok: Some(6.25),
                },
            ),
            (
                "openrouter",
                "kimi-k2",
                ModelCost {
                    input_usd_per_mtok: 0.5,
                    output_usd_per_mtok: 2.0,
                    cache_read_usd_per_mtok: None,
                    cache_write_usd_per_mtok: None,
                },
            ),
        ]);

        // [1m] suffix strips before lookup.
        let opus = lookup("claude:api-key", "claude-opus-4-6[1m]").expect("priced");
        assert!((opus.input_usd_per_mtok - 5.0).abs() < 1e-9);

        // provider/model ids fall back to the bare model name.
        let kimi = lookup("openrouter", "moonshotai/kimi-k2").expect("priced");
        assert!((kimi.output_usd_per_mtok - 2.0).abs() < 1e-9);
        let pinned = lookup("openrouter", "moonshotai/kimi-k2@Sail Research").expect("priced");
        assert!((pinned.output_usd_per_mtok - 2.0).abs() < 1e-9);

        assert!(lookup("claude:api-key", "claude-unknown").is_none());

        clear_memory_cache_for_tests();
        if let Some(prev) = prev_home {
            crate::env::set_var("HORON_HOME", prev);
        } else {
            crate::env::remove_var("HORON_HOME");
        }
    }
}
