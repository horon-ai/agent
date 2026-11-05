//! Forgetting every per-session registry a removed server session owned.

use super::unregister_session_event_sink;
use crate::protocol::ServerEvent;
use tokio::sync::mpsc;

/// Forget the per-session registries a removed session owned: its event sink,
/// its metrics, its recorded reasoning effort, and its IRC bus state.
///
/// Split out from `remove_session_entry` for callers that already hold
/// the `sessions` write guard: re-entering `remove_session_entry` there would
/// take the same lock twice. `retired_sink` is the channel the event-sink
/// registry held before the removal; `None` means no sink was registered.
///
/// The presence marker is deliberately not part of this: `remove_session_entry`
/// owns it, and the two resume paths that call this directly never cleared it.
pub(crate) fn forget_removed_session(session_id: &str, retired_sink: Option<&mpsc::UnboundedSender<ServerEvent>>) {
    if let Some(retired) = retired_sink {
        unregister_session_event_sink(session_id, retired);
    }
    crate::session_metrics::forget(session_id);
    crate::session_effort::forget_session_effort(session_id);
    // Per-session caches that would otherwise outlive the session: the monitor
    // runtime (and its sampler thread), the observation record, and the durable
    // input store. Without this a daemon that opens and closes sessions forever
    // keeps every one of them, and their threads, alive.
    crate::tool::forget_session_caches(session_id);
    crate::server::session_observation::forget_session(session_id);
    crate::durable_input_runtime::forget_session(session_id);
    crate::server::control_receipts::forget_session(session_id);
    if let Some(bus) = horon_agent_irc_core::global::get() {
        bus.forget(session_id);
    }
}
