use horon_agent_provider_core::pricing as core_pricing;
use horon_agent_provider_core::{RouteCheapnessEstimate, RouteCostConfidence, RouteCostSource};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Route-catalog builds call the pricing helpers once per route, and the
/// OpenAI ones re-read credential files (and re-parse config.toml for
/// external-source trust checks) on every call. Across a 2000+ route catalog
/// that dominated build time, so the auth-derived inputs are memoized here with
/// a short TTL. Invalidated eagerly via
/// [`invalidate_auth_pricing_memos`] whenever auth state changes.
const AUTH_PRICING_MEMO_TTL: Duration = Duration::from_secs(5);

static OPENAI_AUTH_MODE_MEMO: Mutex<Option<(Instant, &'static str)>> = Mutex::new(None);

/// Monotonic generation bumped on every auth invalidation. Route-catalog memos
/// snapshot this at build time so `AuthStatus::invalidate_cache()` immediately
/// invalidates every provider's memoized catalog, not just the pricing inputs.
static AUTH_PRICING_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn auth_pricing_generation() -> u64 {
    AUTH_PRICING_GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

/// Drop memoized auth-derived pricing inputs (effective OpenAI auth mode).
/// Called from `AuthStatus::invalidate_cache()` so pricing labels update
/// immediately after logins/logouts instead of after the TTL.
pub(crate) fn invalidate_auth_pricing_memos() {
    AUTH_PRICING_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut memo) = OPENAI_AUTH_MODE_MEMO.lock() {
        *memo = None;
    }
}

/// Memoization is skipped in test builds: test sandboxes swap `HORON_AGENT_HOME`
/// and credential env vars between cases without going through
/// `AuthStatus::invalidate_cache()`, so a TTL'd memo would leak state across
/// tests. `test-support` covers downstream crates' test targets via feature
/// unification.
fn auth_pricing_memos_enabled() -> bool {
    !cfg!(any(test, feature = "test-support"))
}

pub(crate) fn openai_effective_auth_mode() -> &'static str {
    if auth_pricing_memos_enabled()
        && let Ok(memo) = OPENAI_AUTH_MODE_MEMO.lock()
        && let Some((cached_at, mode)) = memo.as_ref()
        && cached_at.elapsed() < AUTH_PRICING_MEMO_TTL
    {
        return mode;
    }
    let mode = match crate::auth::codex::load_credentials() {
        Ok(creds) if !creds.refresh_token.is_empty() || creds.id_token.is_some() => "oauth",
        Ok(_) => "api-key",
        Err(_) => {
            if crate::provider_catalog::load_api_key_from_env_or_config("OPENAI_API_KEY", "openai.env").is_some() {
                "api-key"
            } else {
                "oauth"
            }
        }
    };
    if let Ok(mut memo) = OPENAI_AUTH_MODE_MEMO.lock() {
        *memo = Some((Instant::now(), mode));
    }
    mode
}

pub(crate) fn openai_oauth_pricing(model: &str) -> RouteCheapnessEstimate {
    core_pricing::openai_oauth_pricing(model)
}

fn usd_to_micros(usd: f64) -> u64 {
    (usd * 1_000_000.0).round() as u64
}

/// Unified metered per-token pricing resolver for any provider/model pair.
///
/// `source_key` is the cross-provider activity key (see
/// [`crate::provider_activity::source_key_for_provider_label`]), e.g.
/// `openai:api-key`, `openai-compatible:deepseek`.
///
/// Resolution order:
///   1. Curated static tables (exact, hand-reviewed) for OpenAI.
///   2. The live models.dev pricing catalog cache (140+ providers).
///
/// Returns `None` when nothing can price the route, so callers can
/// distinguish "unknown" from "free" instead of silently guessing.
pub fn metered_pricing_for_source(source_key: &str, model: &str) -> Option<RouteCheapnessEstimate> {
    metered_pricing_for_source_with_tier(source_key, model, None)
}

/// Like [`metered_pricing_for_source`] but honoring the active service tier
/// (`/fast on` priority tier, OpenAI flex) which changes per-token rates on
/// the premium models.
pub fn metered_pricing_for_source_with_tier(
    source_key: &str,
    model: &str,
    service_tier: Option<&str>,
) -> Option<RouteCheapnessEstimate> {
    // 1. Curated static tables.
    let static_estimate = match source_key {
        "openai:api-key" => core_pricing::openai_api_pricing_with_tier(model, service_tier),
        _ => None,
    };
    if static_estimate.is_some() {
        return static_estimate;
    }

    // 2. Live models.dev catalog (disk cache; refreshes in the background).
    let (cost, matched) = crate::model_pricing::lookup_with_provenance(source_key, model)?;
    // A price borrowed from other providers' listings (reseller fallback) is
    // a reasonable estimate, not the route's own published rate.
    let (confidence, note) = match matched {
        crate::model_pricing::PricingMatch::ProviderTable => (RouteCostConfidence::High, "models.dev pricing catalog"),
        crate::model_pricing::PricingMatch::CrossProvider => (
            RouteCostConfidence::Medium,
            "models.dev pricing catalog (inferred from other providers)",
        ),
    };
    Some(RouteCheapnessEstimate::metered(
        RouteCostSource::ModelsDevCatalog,
        confidence,
        usd_to_micros(cost.input_usd_per_mtok),
        usd_to_micros(cost.output_usd_per_mtok),
        cost.cache_read_usd_per_mtok.map(usd_to_micros),
        Some(note.to_string()),
    ))
}

/// Estimated USD for one request's reported token usage on a per-token route.
///
/// `source_key` uses the activity-ledger convention (`openai:api-key`,
/// `openai-compatible:<id>`). Returns `None` when the route cannot be priced,
/// so callers can show "unknown" instead of a misleading `$0`.
///
/// OpenAI counts cache reads and writes inside `input`. Other providers are
/// inferred: cache writes or reads larger than input imply split accounting.
pub fn metered_usage_cost_usd(
    source_key: &str,
    model: &str,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_creation: u64,
) -> Option<f64> {
    let estimate = metered_pricing_for_source(source_key, model)?;
    let per_tok = |micros: u64| micros as f64 / 1_000_000.0 / 1_000_000.0;
    let input_price = per_tok(estimate.input_price_per_mtok_micros?);
    let output_price = per_tok(estimate.output_price_per_mtok_micros?);
    let read_price = estimate
        .cache_read_price_per_mtok_micros
        .map(per_tok)
        .unwrap_or(input_price);
    let openai = source_key.starts_with("openai:");
    let split = !openai && (cache_creation > 0 || cache_read > input);
    let fresh = if split {
        input
    } else {
        input.saturating_sub(cache_read).saturating_sub(cache_creation)
    };
    let write_multiplier = if openai { 1.25 } else { 1.0 };
    let cost = fresh as f64 * input_price
        + output as f64 * output_price
        + cache_read as f64 * read_price
        + cache_creation as f64 * input_price * write_multiplier;
    cost.is_finite().then_some(cost)
}

pub(crate) fn cheapness_for_route(model: &str, _provider: &str, api_method: &str) -> Option<RouteCheapnessEstimate> {
    use horon_agent_provider_core::{AuthMode, AuthRoute, DualAuthProvider};

    // Dual-auth (OpenAI OAuth-vs-API) methods are recognized through the single
    // shared parser so pricing never disagrees with the routing layer about
    // whether a route is subscription (OAuth) or metered (API key).
    if let Some(route) = AuthRoute::parse(api_method) {
        return match (route.provider, route.mode) {
            (DualAuthProvider::OpenAI, AuthMode::ApiKey) => {
                Some(metered_pricing_for_source("openai:api-key", model).unwrap_or_else(|| openai_oauth_pricing(model)))
            }
            (DualAuthProvider::OpenAI, AuthMode::Oauth) => {
                // An "OAuth" route still bills per token when only an API key is
                // actually configured, so honor the live effective auth mode.
                if openai_effective_auth_mode() == "api-key" {
                    Some(
                        metered_pricing_for_source("openai:api-key", model)
                            .unwrap_or_else(|| openai_oauth_pricing(model)),
                    )
                } else {
                    Some(openai_oauth_pricing(model))
                }
            }
        };
    }

    if let Some(profile_id) = api_method.strip_prefix("openai-compatible:") {
        return metered_pricing_for_source(&format!("openai-compatible:{}", profile_id), model);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env;
    use horon_agent_provider_core::{RouteBillingKind, RouteCostSource};

    #[test]
    fn unpriced_routes_are_unknown() {
        // Unpriced routes are unknown, never $0.
        assert!(metered_usage_cost_usd("unregistered-provider", "whatever", 1, 1, 0, 0).is_none());
    }

    fn with_clean_provider_test_env<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var_os("HORON_AGENT_HOME");
        let prev_openai_api_key = std::env::var_os("OPENAI_API_KEY");
        crate::auth::codex::set_active_account_override(None);
        env::set_var("HORON_AGENT_HOME", temp.path());
        env::remove_var("OPENAI_API_KEY");

        let result = f();

        crate::auth::codex::set_active_account_override(None);
        if let Some(prev_home) = prev_home {
            env::set_var("HORON_AGENT_HOME", prev_home);
        } else {
            env::remove_var("HORON_AGENT_HOME");
        }
        if let Some(prev_openai_api_key) = prev_openai_api_key {
            env::set_var("OPENAI_API_KEY", prev_openai_api_key);
        } else {
            env::remove_var("OPENAI_API_KEY");
        }
        result
    }

    #[test]
    fn cheapness_for_openai_route_falls_back_to_subscription_for_unpriced_api_key_models() {
        with_clean_provider_test_env(|| {
            env::set_var("OPENAI_API_KEY", "test-key");
            let estimate = cheapness_for_route("gpt-4.1-mini", "OpenAI", "openai-oauth").expect("cheapness estimate");
            assert_eq!(estimate.billing_kind, RouteBillingKind::Subscription);
            assert_eq!(estimate.source, RouteCostSource::PublicPlanPricing);
        });
    }

    #[test]
    fn cheapness_for_openai_route_prefers_metered_api_prices_when_available() {
        with_clean_provider_test_env(|| {
            env::set_var("OPENAI_API_KEY", "test-key");
            let estimate = cheapness_for_route("gpt-5.4", "OpenAI", "openai-oauth").expect("cheapness estimate");
            assert_eq!(estimate.billing_kind, RouteBillingKind::Metered);
            assert_eq!(estimate.source, RouteCostSource::PublicApiPricing);
        });
    }

    #[test]
    fn unified_resolver_reads_models_dev_catalog_for_compatible_profiles() {
        with_clean_provider_test_env(|| {
            crate::model_pricing::clear_memory_cache_for_tests();
            crate::model_pricing::save_test_cache(&[(
                "deepseek",
                "deepseek-v4-flash",
                crate::model_pricing::ModelCost {
                    input_usd_per_mtok: 0.14,
                    output_usd_per_mtok: 0.28,
                    cache_read_usd_per_mtok: Some(0.0028),
                    cache_write_usd_per_mtok: None,
                },
            )]);

            // Compatible profiles resolve through the models.dev catalog.
            let flash =
                metered_pricing_for_source("openai-compatible:deepseek", "deepseek-v4-flash").expect("priced model");
            assert_eq!(flash.source, RouteCostSource::ModelsDevCatalog);
            assert_eq!(flash.input_price_per_mtok_micros, Some(140_000));
            assert_eq!(flash.output_price_per_mtok_micros, Some(280_000));
            assert_eq!(flash.cache_read_price_per_mtok_micros, Some(2_800));

            // Unknown models return None instead of a fabricated price.
            assert!(metered_pricing_for_source("openai-compatible:deepseek", "unknown-model").is_none());

            crate::model_pricing::clear_memory_cache_for_tests();
        });
    }
}
