//! Provider usage reporting.
//!
//! Reports per-profile API-key spend from the local activity ledger; there is
//! no vendor quota endpoint in this build.

mod api_keys;
mod cache;
mod display;
mod model;
use api_keys::enqueue_api_key_usage_tasks;
use cache::*;
pub use horon_agent_usage_types::{OpenAiResetCredits, ProviderUsage, ProviderUsageProgress, UsageLimit};
pub use model::*;

use display::provider_usage_cache_is_fresh;
pub use display::{format_reset_time, format_usage_bar};
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub(crate) fn usage_percent_from_used_limit(used: f64, limit: f64) -> f32 {
    if !used.is_finite() || !limit.is_finite() || limit <= 0.0 {
        return 0.0;
    }
    ((used.max(0.0) / limit) * 100.0) as f32
}

/// Cache duration (refresh every 5 minutes - usage data is slow-changing)
const CACHE_DURATION: Duration = Duration::from_secs(300);

/// Error backoff duration (wait 5 minutes before retrying after auth/credential errors)
const ERROR_BACKOFF: Duration = Duration::from_secs(300);

/// Rate limit backoff duration (wait 15 minutes before retrying after 429 errors)
const RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(900);

/// Minimum interval between /usage command fetches (per provider).
const PROVIDER_USAGE_CACHE_TTL: Duration = Duration::from_secs(120);

/// Cached provider usage reports (used by /usage command).
/// Keyed by provider display name.
static PROVIDER_USAGE_CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, (Instant, ProviderUsage)>>> =
    std::sync::OnceLock::new();

/// Fetch usage from every provider with credentials.
/// Returns a list of ProviderUsage, one per provider that has credentials.
/// Results are cached for 2 minutes to avoid hitting rate limits.
pub async fn fetch_all_provider_usage() -> Vec<ProviderUsage> {
    fetch_all_provider_usage_progressive(|_| {}).await
}

/// Fetch usage from all connected providers and report incremental progress as
/// each provider finishes. Cached data is emitted immediately when available so
/// the UI can show useful stale/fresh context while live refreshes are still in
/// flight.
pub async fn fetch_all_provider_usage_progressive<F>(mut on_update: F) -> Vec<ProviderUsage>
where
    F: FnMut(ProviderUsageProgress) + Send,
{
    let cache = PROVIDER_USAGE_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));

    let now = Instant::now();
    let cached_results = if let Ok(map) = cache.lock() {
        let mut cached = map.values().map(|(_, r)| r.clone()).collect::<Vec<_>>();
        sort_reports_most_recent_first(&mut cached);
        cached
    } else {
        Vec::new()
    };
    let all_fresh = if let Ok(map) = cache.lock() {
        !map.is_empty()
            && map
                .values()
                .all(|(fetched_at, report)| provider_usage_cache_is_fresh(now, *fetched_at, report))
    } else {
        false
    };

    if all_fresh {
        on_update(ProviderUsageProgress {
            completed: cached_results.len(),
            total: cached_results.len(),
            done: true,
            from_cache: true,
            results: cached_results.clone(),
        });
        return cached_results;
    }

    let mut results = cached_results.clone();
    if !cached_results.is_empty() {
        on_update(ProviderUsageProgress {
            results: cached_results,
            completed: 0,
            total: 0,
            done: false,
            from_cache: true,
        });
    }

    let mut tasks = tokio::task::JoinSet::<Option<ProviderUsage>>::new();
    let total = enqueue_provider_usage_tasks(&mut tasks);

    if total == 0 {
        if let Ok(mut map) = cache.lock() {
            map.clear();
        }
        on_update(ProviderUsageProgress {
            results: results.clone(),
            completed: 0,
            total: 0,
            done: true,
            from_cache: false,
        });
        return results;
    }

    let generation = provider_usage_generation();
    let mut completed = 0usize;
    while let Some(joined) = tasks.join_next().await {
        completed += 1;
        if let Ok(Some(report)) = joined {
            upsert_provider_usage(&mut results, report);
        }

        on_update(ProviderUsageProgress {
            results: results.clone(),
            completed,
            total,
            done: false,
            from_cache: false,
        });
    }

    if let Ok(mut map) = cache.lock()
        && generation == provider_usage_generation()
    {
        map.clear();
        let now = Instant::now();
        for r in &results {
            map.insert(r.provider_name.clone(), (now, r.clone()));
        }
    }

    on_update(ProviderUsageProgress {
        results: results.clone(),
        completed: total,
        total,
        done: true,
        from_cache: false,
    });

    results
}

fn upsert_provider_usage(results: &mut Vec<ProviderUsage>, report: ProviderUsage) {
    if let Some(existing) = results
        .iter_mut()
        .find(|existing| existing.provider_name == report.provider_name)
    {
        *existing = report;
    } else {
        results.push(report);
    }
    sort_reports_most_recent_first(results);
}

/// Order reports so the most recently used login comes first. Sources horon-agent
/// has never used sort last, alphabetically for stability.
fn sort_reports_most_recent_first(results: &mut [ProviderUsage]) {
    results.sort_by(|a, b| {
        b.last_used_unix_secs
            .cmp(&a.last_used_unix_secs)
            .then_with(|| a.provider_name.cmp(&b.provider_name))
    });
}

/// Stamp a report with last-used recency from the activity ledger.
fn attach_activity(report: &mut ProviderUsage, source_key: &str) {
    if let Some(used) = crate::provider_activity::last_used_unix_secs(source_key) {
        report.last_used_unix_secs = Some(used);
        report.extra_info.push((
            "Last used".to_string(),
            crate::provider_activity::format_relative_age(used),
        ));
    }
}

fn enqueue_provider_usage_tasks(tasks: &mut tokio::task::JoinSet<Option<ProviderUsage>>) -> usize {
    let mut total = 0usize;

    total += enqueue_api_key_usage_tasks(tasks);

    total += enqueue_activity_sweeper_task(tasks);

    total
}

/// Source-key prefixes that already get a dedicated `/usage` report; the
/// sweeper skips these so used-but-unreported logins still show recency + local
/// spend without double-listing covered providers.
fn activity_source_has_dedicated_report(source_key: &str) -> bool {
    // Direct OpenAI-compatible profiles are reported by the API-key module
    // whenever their key is configured. Every other source falls through to
    // the sweeper.
    source_key
        .strip_prefix("openai-compatible:")
        .and_then(crate::provider_catalog::openai_compatible_profile_by_id)
        .map(|profile| {
            crate::provider_catalog::load_api_key_from_env_or_config(profile.api_key_env, profile.env_file).is_some()
        })
        .unwrap_or(false)
}

/// One catch-all task that reports every ledger entry without a dedicated
/// fetcher: last-used recency plus locally tracked spend.
fn enqueue_activity_sweeper_task(tasks: &mut tokio::task::JoinSet<Option<ProviderUsage>>) -> usize {
    let leftover: Vec<(String, crate::provider_activity::ProviderActivityEntry)> =
        crate::provider_activity::all_entries()
            .into_iter()
            .filter(|(key, _)| !activity_source_has_dedicated_report(key))
            .collect();
    if leftover.is_empty() {
        return 0;
    }

    let count = leftover.len();
    for (source_key, entry) in leftover {
        tasks.spawn(async move {
            let mut extra_info = Vec::new();
            if let Some(spend) = entry.spend {
                extra_info.push((
                    "Local spend (this machine)".to_string(),
                    format!(
                        "${:.2} today · ${:.2} this month · ${:.2} all-time",
                        spend.day_usd, spend.month_usd, spend.all_time_usd
                    ),
                ));
            }
            let mut report = ProviderUsage {
                provider_name: crate::provider_activity::display_name_for_source_key(&source_key),
                extra_info,
                ..Default::default()
            };
            attach_activity(&mut report, &source_key);
            Some(report)
        });
    }
    count
}

#[cfg(test)]
mod tests;
