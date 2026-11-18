use super::{OpenAIUsageData, PROVIDER_USAGE_CACHE_TTL, ProviderUsage, RATE_LIMIT_BACKOFF, UsageData};
use std::time::Instant;

pub(super) fn reset_timestamp_passed(timestamp: Option<&str>) -> bool {
    usage_reset_passed([timestamp])
}

impl UsageData {
    /// Returns a display-safe snapshot that avoids showing pre-reset usage after a window rolled over.
    pub fn display_snapshot(&self) -> Self {
        let mut snapshot = self.clone();

        if reset_timestamp_passed(self.five_hour_resets_at.as_deref()) {
            snapshot.five_hour = 0.0;
            snapshot.five_hour_resets_at = None;
        }

        if reset_timestamp_passed(self.seven_day_resets_at.as_deref()) {
            snapshot.seven_day = 0.0;
            snapshot.seven_day_opus = None;
            snapshot.seven_day_resets_at = None;
        }

        snapshot
    }
}

impl OpenAIUsageData {
    /// Returns a display-safe snapshot that avoids showing pre-reset exhaustion after a window rolled over.
    pub fn display_snapshot(&self) -> Self {
        let mut snapshot = self.clone();
        let mut cleared_any_window = false;

        if let Some(window) = snapshot.five_hour.as_mut()
            && reset_timestamp_passed(window.resets_at.as_deref())
        {
            window.usage_ratio = 0.0;
            window.resets_at = None;
            cleared_any_window = true;
        }

        if let Some(window) = snapshot.seven_day.as_mut()
            && reset_timestamp_passed(window.resets_at.as_deref())
        {
            window.usage_ratio = 0.0;
            window.resets_at = None;
            cleared_any_window = true;
        }

        if let Some(window) = snapshot.spark.as_mut()
            && reset_timestamp_passed(window.resets_at.as_deref())
        {
            window.usage_ratio = 0.0;
            window.resets_at = None;
            cleared_any_window = true;
        }

        if cleared_any_window {
            snapshot.hard_limit_reached = false;
            // The old server `allowed` flag is no longer authoritative after
            // rollover. Wait for the existing usage refresh before suggesting
            // an irreversible banked reset.
            snapshot.openai_reset_credits = None;
        }

        snapshot
    }
}

pub(super) fn provider_usage_cache_is_fresh(now: Instant, fetched_at: Instant, report: &ProviderUsage) -> bool {
    let ttl = if report
        .error
        .as_ref()
        .map(|e| e.contains("429") || e.contains("rate limit") || e.contains("Rate limited"))
        .unwrap_or(false)
    {
        RATE_LIMIT_BACKOFF
    } else {
        PROVIDER_USAGE_CACHE_TTL
    };

    now.duration_since(fetched_at) < ttl
        && !usage_reset_passed(report.limits.iter().map(|limit| limit.resets_at.as_deref()))
}

fn parse_reset_timestamp(timestamp: &str) -> Result<horon_agent_time::Timestamp, horon_agent_time::ParseError> {
    // The reset times some providers emit omit the offset; the shared parser
    // reads that form with the same grammar the old fallback had.
    horon_agent_time::Timestamp::parse_rfc3339(timestamp)
        .or_else(|_| horon_agent_time::Timestamp::parse_naive_utc(timestamp))
}

pub(super) fn usage_reset_passed<'a>(timestamps: impl IntoIterator<Item = Option<&'a str>>) -> bool {
    let now = horon_agent_time::Timestamp::now();
    timestamps
        .into_iter()
        .flatten()
        .any(|stamp| parse_reset_timestamp(stamp).is_ok_and(|reset| reset <= now))
}

pub fn format_reset_time(timestamp: &str) -> String {
    if let Ok(reset) = parse_reset_timestamp(timestamp) {
        let duration = reset - horon_agent_time::Timestamp::now();
        if duration.whole_seconds() <= 0 {
            return "now".to_string();
        }
        if duration.whole_seconds() < 60 {
            return "1m".to_string();
        }
        let days = duration.whole_days();
        let hours = duration.whole_hours() % 24;
        let minutes = duration.whole_minutes() % 60;
        if days > 0 {
            if hours > 0 {
                format!("{}d {}h", days, hours)
            } else if minutes > 0 {
                format!("{}d {}m", days, minutes)
            } else {
                format!("{}d", days)
            }
        } else if hours > 0 {
            format!("{}h {}m", hours, minutes)
        } else {
            format!("{}m", minutes)
        }
    } else {
        timestamp.to_string()
    }
}

pub fn format_usage_bar(percent: f32, width: usize) -> String {
    let filled = ((percent / 100.0) * width as f32).round() as usize;
    let filled = filled.min(width);
    let empty = width.saturating_sub(filled);
    let bar: String = "█".repeat(filled) + &"░".repeat(empty);
    // Right-align the number so the `%` (and any "· resets" suffix) lines up
    // across rows with different percentages.
    format!("{} {:>3.0}%", bar, percent)
}
