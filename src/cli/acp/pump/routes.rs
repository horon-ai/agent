//! Which stream an out-of-request daemon event belongs to.
//!
//! The pump (`pump.rs`) reads every daemon line and asks these predicates where
//! the event goes: a registered waiter, the running foreground turn, the v2
//! session stream, or the side channel. Nothing here touches a socket or a
//! route, so each rule can be read on its own.

use crate::protocol::{ServerEvent, TurnStopReason};

/// Events that wait on the client and therefore each run in their own task.
pub(super) fn needs_client_round_trip(event: &ServerEvent) -> bool {
    matches!(
        event,
        ServerEvent::AskRequested { .. }
            | ServerEvent::ClientRequest { .. }
            | ServerEvent::ToolPermissionRequested { .. }
    )
}

/// Events that always go to the side channel, never to a prompt stream: the
/// client round trips, and permission withdrawals, which close a client call
/// the side channel owns.
pub(super) fn bypasses_prompt_stream(event: &ServerEvent) -> bool {
    needs_client_round_trip(event) || matches!(event, ServerEvent::ToolPermissionResolved { .. })
}

/// Events the side channel receives when no prompt is running. While a prompt
/// runs they stay on its ordered foreground stream instead.
pub(super) fn is_side_event(event: &ServerEvent) -> bool {
    matches!(
        event,
        ServerEvent::SessionRenamed { .. }
            | ServerEvent::ModelChanged { .. }
            | ServerEvent::PlanUpdated { .. }
            | ServerEvent::ToolDiff { .. }
    )
}

/// The request id an event answers, if it answers one.
///
/// `tool_call_cancel_result` and `background_work_list` are here so a
/// `tool_call/cancel`, `terminal/cancel` or `session/resume` handler waiting
/// on the daemon gets its answer directly instead of through the turn's
/// stream, which never sees it.
pub(super) fn reply_id(event: &ServerEvent) -> Option<u64> {
    match event {
        ServerEvent::Ack { id }
        | ServerEvent::BackgroundWorkList { id, .. }
        | ServerEvent::Done { id }
        | ServerEvent::Error { id, .. }
        | ServerEvent::History { id, .. }
        | ServerEvent::ModelChanged { id, .. }
        | ServerEvent::ReasoningEffortChanged { id, .. }
        | ServerEvent::State { id, .. }
        | ServerEvent::ToolCallCancelResult { id, .. }
        | ServerEvent::UserMessageInserted { id, .. } => Some(*id),
        _ => None,
    }
}

/// A reply to a request that is not a turn terminal. The daemon writes these
/// straight to the socket (`GetState` in
/// crates/app-core/src/server/client_state.rs:130-153), so they can overtake
/// turn events still queued on its ordered event channel and say nothing
/// about where a turn's stream stands.
pub(super) fn is_request_reply(event: &ServerEvent) -> bool {
    reply_id(event).is_some() && !matches!(event, ServerEvent::Done { .. } | ServerEvent::Error { .. })
}

/// An event only a running turn produces: what the prompt maps into
/// `session/update` content (crates/protocol/src/wire/events.rs), plus a
/// non-interrupt stop. After a turn's terminal the daemon still sends
/// bookkeeping for it - the swarm member status of a cancel
/// (client_lifecycle.rs:4118-4130, :4193-4203), queue counts - before the
/// cancel's `Interrupted`, so only an event like these proves the next turn
/// has started.
///
/// `tool_cancelled` is the per-call cancellation the v2 stream reports as a
/// final update, so it belongs to the turn that produced it like any other tool
/// frame.
pub(super) fn is_turn_content(event: &ServerEvent) -> bool {
    match event {
        ServerEvent::TextDelta { .. }
        | ServerEvent::TextReplace { .. }
        | ServerEvent::TextSuffixReplace { .. }
        | ServerEvent::ReasoningDelta { .. }
        | ServerEvent::ToolStart { .. }
        | ServerEvent::ToolInput { .. }
        | ServerEvent::ToolExec { .. }
        | ServerEvent::ToolDone { .. }
        | ServerEvent::ToolCancelled { .. }
        | ServerEvent::ToolDiff { .. }
        | ServerEvent::PlanUpdated { .. }
        | ServerEvent::GeneratedImage { .. }
        | ServerEvent::Compaction { .. }
        | ServerEvent::ProviderGuardrail { .. }
        | ServerEvent::TokenUsage { .. } => true,
        ServerEvent::TurnStopped { reason, .. } => !matches!(reason, TurnStopReason::Interrupted),
        _ => false,
    }
}

/// Whether the v2 session stream claims this event.
///
/// A v2 session reports every turn, so its stream events reach the v2 stream
/// rather than being dropped. A model change made outside any request stays
/// with the side channel, which owns the selector update.
pub(super) fn belongs_to_v2_stream(event: &ServerEvent) -> bool {
    !matches!(event, ServerEvent::ModelChanged { .. })
}
