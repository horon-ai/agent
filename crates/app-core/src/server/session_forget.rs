//! Forgetting every per-session registry a removed server session owned.

use super::unregister_session_event_sink;
use crate::protocol::ServerEvent;
use horon_agent_irc_core::AgentId;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex as StdMutex, Weak};
use tokio::sync::mpsc;

/// The counters one delivery layer keeps for its live relays.
type RelayMap = StdMutex<HashMap<AgentId, usize>>;

/// Sessions with a wake relay still in flight, and how many each one has.
///
/// A relay task outlives the session record it was started for: it waits for
/// the woken turn's final answer and only then drops its count. Each delivery
/// layer owns one of these maps, and [`register_pending_relays`] lets removal
/// clear the entry of a session that disappears mid-relay, so a reused session
/// id never inherits a stale "still answering" mark.
pub(crate) type PendingRelays = Arc<RelayMap>;

/// Every live delivery layer's counters, held weakly so a retired one is not
/// kept alive just to be forgotten from.
static REGISTERED_RELAYS: LazyLock<StdMutex<Vec<Weak<RelayMap>>>> = LazyLock::new(|| StdMutex::new(Vec::new()));

/// Track `relays` so session removal can clear a session's counters in it.
pub(crate) fn register_pending_relays(relays: &PendingRelays) {
    let mut registered = match REGISTERED_RELAYS.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    registered.retain(|weak| weak.strong_count() > 0);
    let entry = Arc::downgrade(relays);
    if !registered.iter().any(|weak| weak.ptr_eq(&entry)) {
        registered.push(entry);
    }
}

/// Drop the relay counters a removed session still owned.
///
/// A relay that finishes afterwards finds no entry and leaves the map alone,
/// so clearing here never panics or pushes a count below zero.
fn forget_pending_relays(session_id: &str) {
    let registered = match REGISTERED_RELAYS.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    for weak in registered.iter() {
        let Some(relays) = weak.upgrade() else {
            continue;
        };
        let mut relays = match relays.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        relays.remove(session_id);
    }
}

/// Forget the per-session registries a removed session owned: its event sink,
/// its metrics, its recorded reasoning effort, and its IRC bus state, including
/// the wake relays still in flight for it.
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
    horon_agent_provider_core::degradation::forget_session(session_id);
    crate::server::session_observation::forget_session(session_id);
    crate::durable_input_runtime::forget_session(session_id);
    crate::server::control_receipts::forget_session(session_id);
    if let Some(bus) = horon_agent_irc_core::global::get() {
        bus.forget(session_id);
    }
    forget_pending_relays(session_id);
}
