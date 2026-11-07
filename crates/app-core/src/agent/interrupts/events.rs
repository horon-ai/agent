//! Soft-interrupt events leaving the daemon.
//!
//! Two audiences see an injected soft interrupt, and they need different
//! things. Every client receives one `SoftInterruptInjected` per injected group
//! describing what landed in the transcript. A connection that negotiated ACP
//! v2 additionally receives `SoftInterruptStored` naming the stored row, so an
//! editor can render the injection live under the id a later replay reports.
//! The second event is a v2 side channel: it never reaches a v1 editor or any
//! other client attached to the same session.

use super::InjectedSoftInterrupt;
use crate::logging;
use crate::protocol::ServerEvent;
use horon_agent_agent_runtime::SoftInterruptSource;

/// The role the wire event reports for an injected group.
///
/// The stored role and this one are one decision seen twice: an injection whose
/// source has a stored display role also reports it, and a plain user injection
/// reports none, which is what makes it look like the user's own message.
fn protocol_display_role(source: SoftInterruptSource) -> Option<String> {
    match source {
        SoftInterruptSource::User => None,
        SoftInterruptSource::System => Some("system".to_string()),
        SoftInterruptSource::BackgroundTask => Some("background_task".to_string()),
    }
}

impl super::Agent {
    /// The transcript events for one injection point, one per injected group.
    ///
    /// `tools_skipped` belongs to the first group only: it describes the round
    /// the interrupt cut short, not each message that arrived.
    pub(in crate::agent) fn build_soft_interrupt_events(
        injected: Vec<InjectedSoftInterrupt>,
        point: &'static str,
        tools_skipped: Option<usize>,
    ) -> Vec<ServerEvent> {
        logging::info(&format!(
            "AGENT_SOFT_INTERRUPT_EVENTS_BUILD groups={} point={} tools_skipped={:?} total_content_bytes={}",
            injected.len(),
            point,
            tools_skipped,
            injected.iter().map(|interrupt| interrupt.content.len()).sum::<usize>()
        ));
        injected
            .into_iter()
            .enumerate()
            .map(|(idx, interrupt)| ServerEvent::SoftInterruptInjected {
                content: interrupt.content,
                display_role: protocol_display_role(interrupt.source),
                point: point.to_string(),
                tools_skipped: if idx == 0 { tools_skipped } else { None },
                client_ids: interrupt.client_ids,
            })
            .collect()
    }
}

/// Tell the owning v2 connection which row an injected message was stored as.
///
/// A no-op when no v2 connection owns the session: the id is only useful to an
/// editor that renders user messages live and matches them against a replay.
pub(in crate::agent) fn send_stored(session_id: &str, message_id: &str, content: &str) {
    crate::tool::acp_session::send_v2(
        session_id,
        ServerEvent::SoftInterruptStored {
            message_id: message_id.to_string(),
            content: content.to_string(),
        },
    );
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod events_tests;
