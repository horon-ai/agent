use super::openai_helpers::classify_openai_limits;
use super::{AccountUsageSnapshot, OpenAIUsageData, ProviderUsage, UsageLimit};
use std::collections::HashMap;
use std::time::Instant;

static OPENAI_USAGE_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(super) fn openai_usage_generation() -> u64 {
    OPENAI_USAGE_GENERATION.load(std::sync::atomic::Ordering::SeqCst)
}

/// Shared OpenAI usage cache keyed by account label/token prefix.
static OPENAI_ACCOUNT_USAGE_CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, OpenAIUsageData>>> =
    std::sync::OnceLock::new();

fn openai_usage_cache() -> &'static std::sync::Mutex<HashMap<String, OpenAIUsageData>> {
    OPENAI_ACCOUNT_USAGE_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

pub(super) fn invalidate_openai_usage_after_reset(access_token: &str, account_label: Option<&str>) {
    if let Ok(mut map) = openai_usage_cache().lock() {
        OPENAI_USAGE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if account_label.is_none() && access_token.is_empty() {
            // A daemon cannot receive the TUI's bearer token over the protocol.
            // Legacy/external OAuth caches have token keys, not account labels.
            map.retain(|key, _| !key.starts_with("token:"));
        } else {
            map.remove(&openai_usage_cache_key(access_token, account_label));
            map.remove(&openai_usage_cache_key(access_token, None));
        }
    }
    if let Some(cache) = super::PROVIDER_USAGE_CACHE.get()
        && let Ok(mut map) = cache.lock()
    {
        // Removing just OpenAI can leave an all-fresh map of other providers,
        // causing fetch_all_provider_usage to skip fetching OpenAI entirely.
        // Their per-account caches remain intact, so clearing this aggregate is cheap.
        map.clear();
    }
}

pub(super) fn openai_usage_cache_key(access_token: &str, account_label: Option<&str>) -> String {
    if let Some(label) = account_label.map(str::trim).filter(|label| !label.is_empty()) {
        return format!("label:{}", label);
    }

    let prefix = access_token.get(..20).unwrap_or(access_token).trim().to_string();
    format!("token:{}", prefix)
}

pub(super) fn cached_openai_usage(cache_key: &str) -> Option<OpenAIUsageData> {
    let cache = openai_usage_cache();
    let map = cache.lock().ok()?;
    let cached = map.get(cache_key)?.clone();
    (!cached.is_stale()).then_some(cached)
}

#[cfg(test)]
pub(super) fn store_openai_usage(cache_key: String, data: OpenAIUsageData) {
    store_openai_usage_for_generation(openai_usage_generation(), cache_key, data);
}

pub(super) fn store_openai_usage_for_generation(generation: u64, cache_key: String, data: OpenAIUsageData) {
    if let Ok(mut map) = openai_usage_cache().lock() {
        // A request begun before a reset must not reinstate the old exhausted quota.
        if generation != openai_usage_generation() {
            return;
        }
        let previous = map.get(&cache_key).cloned();
        let previous_exhausted = previous.as_ref().map(OpenAIUsageData::exhausted).unwrap_or(false);
        let current_exhausted = data.exhausted();
        let previous_hard_limit = previous.as_ref().map(|usage| usage.hard_limit_reached).unwrap_or(false);
        if previous.is_none()
            || previous_exhausted != current_exhausted
            || previous_hard_limit != data.hard_limit_reached
        {
            crate::logging::info(&format!(
                "OpenAI limit diag: usage cache update key={} prev_exhausted={} new_exhausted={} prev_hard_limit={} new_hard_limit={} snapshot=({})",
                cache_key,
                previous_exhausted,
                current_exhausted,
                previous_hard_limit,
                data.hard_limit_reached,
                data.diagnostic_fields()
            ));
        }
        map.insert(cache_key, data);
    }
}

pub(super) fn openai_usage_data_from_provider_report(report: &ProviderUsage) -> OpenAIUsageData {
    let mut data = classify_openai_limits(&report.limits);
    data.hard_limit_reached = report.hard_limit_reached;
    data.openai_reset_credits = if report.error.is_none() {
        report.openai_reset_credits.clone()
    } else {
        None
    };
    data.fetched_at = Some(Instant::now());
    data.last_error = report.error.clone();
    data
}

pub(super) fn provider_report_from_openai_usage_data(display_name: String, data: &OpenAIUsageData) -> ProviderUsage {
    if let Some(error) = &data.last_error {
        return ProviderUsage {
            provider_name: display_name,
            error: Some(error.clone()),
            ..Default::default()
        };
    }

    let mut limits = Vec::new();
    if let Some(window) = &data.five_hour {
        limits.push(UsageLimit {
            name: window.name.clone(),
            usage_percent: window.usage_ratio * 100.0,
            resets_at: window.resets_at.clone(),
        });
    }
    if let Some(window) = &data.seven_day {
        limits.push(UsageLimit {
            name: window.name.clone(),
            usage_percent: window.usage_ratio * 100.0,
            resets_at: window.resets_at.clone(),
        });
    }
    if let Some(window) = &data.spark {
        limits.push(UsageLimit {
            name: window.name.clone(),
            usage_percent: window.usage_ratio * 100.0,
            resets_at: window.resets_at.clone(),
        });
    }

    ProviderUsage {
        provider_name: display_name,
        limits,
        extra_info: Vec::new(),
        hard_limit_reached: data.hard_limit_reached,
        openai_reset_credits: data.openai_reset_credits.clone(),
        anthropic_limit_reset: None,
        error: None,
        last_used_unix_secs: None,
    }
}

pub(super) fn openai_snapshot_from_usage(
    label: String,
    email: Option<String>,
    usage: &OpenAIUsageData,
) -> AccountUsageSnapshot {
    let five_hour_ratio = usage.five_hour.as_ref().map(|window| window.usage_ratio);
    let seven_day_ratio = usage.seven_day.as_ref().map(|window| window.usage_ratio);
    let exhausted = usage.exhausted();

    AccountUsageSnapshot {
        label,
        email,
        exhausted,
        primary_label: usage
            .five_hour
            .as_ref()
            .map(|window| window.name.trim_end_matches(" window").to_string()),
        five_hour_ratio,
        secondary_label: usage
            .seven_day
            .as_ref()
            .map(|window| window.name.trim_end_matches(" window").to_string()),
        seven_day_ratio,
        resets_at: usage
            .five_hour
            .as_ref()
            .and_then(|window| window.resets_at.clone())
            .or_else(|| usage.seven_day.as_ref().and_then(|window| window.resets_at.clone())),
        error: usage.last_error.clone(),
    }
}
