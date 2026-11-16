//! Reports and acknowledgements: search across stored transcripts, list what a
//! conversation produced, read token usage, and retire the daemon's durable
//! copy of an approval or failover announcement.
//!
//! The two acks answer with a bare `ok` and no event follows it: an ack is a
//! message a client sends, and the daemon never emits `approval_ack` or
//! `failover_ack` itself, so both methods return `()` once the `ok` arrives.
//! None of the five methods is gated by a capability: the requests carry
//! nothing a client must negotiate first, and a daemon that does not serve one
//! answers with a structured error, which these methods surface unchanged.

use crate::client::{HoronAgentClient, unexpected};
use crate::errors::Result;
use horon_agent_harness_api::{
    ApiEvent, ApiRequest, ArtifactEntry, SessionSearchHit, UsageLimitInfo, UsageModel, UsageSession, UsageTotals,
};

/// A usage report: one row per session in scope, their sums, per-model
/// aggregates, the active context window, and any limits the daemon published.
///
/// A metric no request recorded stays `None` rather than `0`, because a
/// provider that omitted cache accounting did not report zero cache tokens.
/// `pricing` is always `"unknown"` and `estimated_cost` always `None`: this
/// harness carries no price table, so there is nothing to multiply tokens by.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageReport {
    /// The session the report was narrowed to; `None` for a full report.
    pub session_id: Option<String>,
    /// One row per session in scope, ordered by session id.
    pub sessions: Vec<UsageSession>,
    /// Sums across `sessions`.
    pub totals: UsageTotals,
    /// Per-model aggregates across `sessions`, ordered by model id.
    pub models: Vec<UsageModel>,
    /// Context window of the active route in tokens, when the daemon reported
    /// it for the attached session.
    pub context_window: Option<u64>,
    /// Subscription limits the daemon published for this account.
    pub limits: Vec<UsageLimitInfo>,
    /// Always `"unknown"`: this build has no price table.
    pub pricing: String,
    /// Always `None`: no price table means no cost estimate.
    pub estimated_cost: Option<f64>,
}

impl HoronAgentClient {
    /// Search every stored session's transcript for a literal string.
    ///
    /// `limit` defaults to 20 hits and is capped at 50; `None` takes the
    /// default. A blank query is refused with `invalid_request` instead of
    /// matching everything, and the string is interpreted as text only, never
    /// as a path, glob, or query fragment.
    pub fn search_sessions(&self, query: &str, limit: Option<u32>) -> Result<Vec<SessionSearchHit>> {
        match self
            .request_ok(ApiRequest::SearchSessions {
                query: query.to_string(),
                limit,
            })?
            .event
        {
            ApiEvent::SessionSearchResults { results, .. } => Ok(results),
            other => Err(unexpected("session_search_results", &other)),
        }
    }

    /// The files a conversation produced: images it carries and HTML or image
    /// files in its working directory written since it began.
    ///
    /// Metadata only, so a 40 MB report costs one `stat`; read the bytes with
    /// [`Self::read_file`]. A session id with no stored transcript answers
    /// `unknown_session`.
    pub fn list_artifacts(&self, session_id: &str) -> Result<Vec<ArtifactEntry>> {
        match self
            .request_ok(ApiRequest::ListArtifacts {
                session_id: session_id.to_string(),
            })?
            .event
        {
            ApiEvent::Artifacts { artifacts, .. } => Ok(artifacts),
            other => Err(unexpected("artifacts", &other)),
        }
    }

    /// Token usage: durable per-request totals, per-model aggregates, the
    /// active context window, and any subscription limits the daemon published.
    ///
    /// Omit `session_id` for the whole account. A named session id with no
    /// stored record answers `unknown_session` rather than an empty report,
    /// because "this session spent nothing" and "there is no such session" are
    /// different facts.
    pub fn usage_report(&self, session_id: Option<&str>) -> Result<UsageReport> {
        match self
            .request_ok(ApiRequest::UsageReport {
                session_id: session_id.map(str::to_string),
            })?
            .event
        {
            ApiEvent::UsageReport {
                session_id,
                sessions,
                totals,
                models,
                context_window,
                limits,
                pricing,
                estimated_cost,
            } => Ok(UsageReport {
                session_id,
                sessions,
                totals,
                models,
                context_window,
                limits,
                pricing,
                estimated_cost,
            }),
            other => Err(unexpected("usage_report", &other)),
        }
    }

    /// Retire the daemon's durable copy of an `approval_resolved` event.
    ///
    /// An approval stays parked until it is acked, so a client that resolved
    /// one out of band calls this once the decision has landed; an empty id is
    /// refused with `invalid_request` because it would clear nothing while
    /// reporting success. The daemon only accepts the ack on a connection
    /// attached to `session_id`.
    pub fn ack_approval(&self, session_id: &str, request_id: &str) -> Result<()> {
        match self
            .request_ok(ApiRequest::ApprovalAck {
                session_id: session_id.to_string(),
                request_id: request_id.to_string(),
            })?
            .event
        {
            ApiEvent::Ok => Ok(()),
            other => Err(unexpected("ok", &other)),
        }
    }

    /// Retire the daemon's durable copy of a `failover_switched` event.
    ///
    /// Echo the whole announcement: `chain_id`, `from`, `to` and `attempt` all
    /// travel, because a partial key would either clear nothing or read as a
    /// different switch. Empty strings are refused with `invalid_request`, and
    /// the daemon only accepts the ack on a connection attached to
    /// `session_id`.
    pub fn ack_failover(&self, session_id: &str, chain_id: &str, from: &str, to: &str, attempt: u32) -> Result<()> {
        match self
            .request_ok(ApiRequest::FailoverAck {
                session_id: session_id.to_string(),
                chain_id: chain_id.to_string(),
                from: from.to_string(),
                to: to.to_string(),
                attempt,
            })?
            .event
        {
            ApiEvent::Ok => Ok(()),
            other => Err(unexpected("ok", &other)),
        }
    }
}
