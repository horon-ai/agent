//! Token usage report rows carried by the usage reply.

use serde::{Deserialize, Serialize};

/// Durable token usage for one session.
///
/// Input and cache counts keep the provider's own accounting: a provider that
/// includes cache reads in input and one that reports them separately both
/// land here unchanged, exactly as the stored messages recorded them.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageSession {
    pub session_id: String,
    /// The model the session recorded, when its metadata names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Summed input tokens; absent when no request recorded the metric.
    pub input: Option<u64>,
    /// Summed output tokens; absent when no request recorded the metric.
    pub output: Option<u64>,
    /// Summed cache-read input tokens; absent when no provider reported them.
    pub cache_read_input: Option<u64>,
    /// Summed cache-write input tokens; absent when no provider reported them.
    pub cache_creation_input: Option<u64>,
    /// Provider requests that recorded usage, not user turns: one turn can
    /// make several requests, and tool rounds count separately.
    pub requests: u64,
}

/// Sums across every session in a report.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageTotals {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_read_input: Option<u64>,
    pub cache_creation_input: Option<u64>,
    pub requests: u64,
    /// Sessions the sums cover, including sessions that recorded no usage.
    pub sessions: usize,
}

/// Usage attributed to one model across the sessions in a report.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageModel {
    /// Model id, or `unknown` when the session metadata named none.
    pub model: String,
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_read_input: Option<u64>,
    pub cache_creation_input: Option<u64>,
    pub requests: u64,
    /// Sessions that used this model.
    pub sessions: usize,
}

/// One subscription limit the daemon published for the account.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageLimitInfo {
    /// Stable limit id, for example `subscription_spend`.
    pub name: String,
    /// Amount consumed in the current window, when the daemon reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<f64>,
    /// Window cap, when the plan has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    /// Unit for `used` and `limit`, for example `usd`.
    pub unit: String,
    /// RFC 3339 reset time, when the daemon reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
}
