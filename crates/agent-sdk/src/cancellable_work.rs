//! Cancellable work: stop a run, a tool call or a background terminal, and
//! list what is still running.
//!
//! Every method refuses before writing when the daemon does not advertise
//! `cancellable_work_v1`, so a client on an older daemon fails with the same
//! error as any other unnegotiated feature instead of waiting on a reply the
//! daemon never sends.

use crate::client::{HoronAgentClient, unexpected};
use crate::errors::Result;
use horon_agent_harness_api::{ApiEvent, ApiRequest, BackgroundWorkItem, ToolCallCancelOutcome};

impl HoronAgentClient {
    /// Cancel the run the caller saw start, and only that run.
    ///
    /// `accepted: false` with `reason: "unknown"` means the session is not
    /// running that run, so nothing was stopped and a successor turn keeps
    /// going. A plain [`Self::cancel`] still stops whatever is running.
    /// Requires `cancellable_work_v1`.
    pub fn cancel_turn(&self, session_id: &str, run_id: &str) -> Result<(bool, Option<String>)> {
        self.require_capability(horon_agent_harness_api::CAPABILITY_CANCELLABLE_WORK_V1, "cancel_turn")?;
        match self
            .request_ok(ApiRequest::CancelTurn {
                session_id: session_id.to_string(),
                run_id: run_id.to_string(),
            })?
            .event
        {
            ApiEvent::CancelTurnResult { accepted, reason, .. } => Ok((accepted, reason)),
            other => Err(unexpected("cancel_turn", &other)),
        }
    }

    /// Stop one tool call while its turn keeps running.
    ///
    /// The cancelled call still reports a `tool_done`, whose result text the
    /// model sees, so the turn and its transcript stay valid. A background
    /// terminal has its own [`Self::cancel_background_terminal`]. Requires
    /// `cancellable_work_v1`.
    pub fn cancel_tool_call(&self, session_id: &str, call_id: &str) -> Result<ToolCallCancelOutcome> {
        self.require_capability(
            horon_agent_harness_api::CAPABILITY_CANCELLABLE_WORK_V1,
            "cancel_tool_call",
        )?;
        match self
            .request_ok(ApiRequest::CancelToolCall {
                session_id: session_id.to_string(),
                call_id: call_id.to_string(),
            })?
            .event
        {
            ApiEvent::ToolCallCancelResult {
                call_id,
                accepted,
                reason,
                ..
            } => Ok(ToolCallCancelOutcome {
                call_id,
                accepted,
                reason,
            }),
            other => Err(unexpected("cancel_tool_call", &other)),
        }
    }

    /// Stop one background terminal this session started.
    ///
    /// The final `background_work` event for the item confirms the stop; this
    /// reply only says the cancellation was accepted. Requires
    /// `cancellable_work_v1`.
    pub fn cancel_background_terminal(&self, session_id: &str, call_id: &str) -> Result<ToolCallCancelOutcome> {
        self.require_capability(
            horon_agent_harness_api::CAPABILITY_CANCELLABLE_WORK_V1,
            "cancel_background_terminal",
        )?;
        match self
            .request_ok(ApiRequest::CancelBackgroundTerminal {
                session_id: session_id.to_string(),
                call_id: call_id.to_string(),
            })?
            .event
        {
            ApiEvent::ToolCallCancelResult {
                call_id,
                accepted,
                reason,
                ..
            } => Ok(ToolCallCancelOutcome {
                call_id,
                accepted,
                reason,
            }),
            other => Err(unexpected("cancel_background_terminal", &other)),
        }
    }

    /// The background work still running in this session.
    ///
    /// A client that reattached after a reconnect asks this to learn what
    /// survived, instead of guessing from a transcript it missed. Requires
    /// `cancellable_work_v1`.
    pub fn list_background_work(&self, session_id: &str) -> Result<Vec<BackgroundWorkItem>> {
        self.require_capability(
            horon_agent_harness_api::CAPABILITY_CANCELLABLE_WORK_V1,
            "list_background_work",
        )?;
        match self
            .request_ok(ApiRequest::ListBackgroundWork {
                session_id: session_id.to_string(),
            })?
            .event
        {
            ApiEvent::BackgroundWorkList { items, .. } => Ok(items),
            other => Err(unexpected("list_background_work", &other)),
        }
    }
}
