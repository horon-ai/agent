//! Waiting for a session's turn event forwarders to catch up.
//!
//! A client-driven turn sends its events through a forwarder task
//! (`state::session_event_fanout_sender_with_fallback`) that hands them to the
//! session's attachments later. A reply the request loop writes straight to a
//! client can therefore overtake events the turn queued before it: a cancel's
//! `Interrupted` and `Done` used to reach the editor ahead of the cancelled
//! turn's last tool frame, which then landed in the next prompt. A flush waits
//! until every forwarder of the session has forwarded what was queued on it
//! before the flush was asked for.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// The flush request a forwarder answers once its event queue is empty.
pub(super) type FlushRequest = oneshot::Sender<()>;

/// The flush channels of every live forwarder, per session. A forwarder that
/// ended leaves a closed sender behind, pruned on the next registration and on
/// the forwarder's own exit.
static FORWARDERS: LazyLock<Mutex<HashMap<String, Vec<mpsc::UnboundedSender<FlushRequest>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Register a forwarder of `session_id` and return the requests it must answer.
pub(super) fn register(session_id: &str) -> mpsc::UnboundedReceiver<FlushRequest> {
    let (flush_tx, flush_rx) = mpsc::unbounded_channel();
    let mut forwarders = FORWARDERS.lock().unwrap_or_else(PoisonError::into_inner);
    let entry = forwarders.entry(session_id.to_string()).or_default();
    entry.retain(|flush| !flush.is_closed());
    entry.push(flush_tx);
    flush_rx
}

/// Drop the closed flush channels of `session_id`, called by a forwarder after
/// it dropped its receiver.
pub(super) fn prune(session_id: &str) {
    let mut forwarders = FORWARDERS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(entry) = forwarders.get_mut(session_id) {
        entry.retain(|flush| !flush.is_closed());
        if entry.is_empty() {
            forwarders.remove(session_id);
        }
    }
}

/// Answer one flush request. A waiter whose bound ran out is gone, and needs
/// no answer.
pub(super) fn answer(request: FlushRequest) {
    if request.send(()).is_err() {
        crate::logging::info("fanout flush answered after its waiter gave up");
    }
}

/// Wait until every forwarder of `session_id` has forwarded the events queued
/// on it before this call, for at most `bound`. A session without a live
/// forwarder returns at once.
pub(super) async fn flush(session_id: &str, bound: Duration) {
    let waits: Vec<oneshot::Receiver<()>> = {
        let forwarders = FORWARDERS.lock().unwrap_or_else(PoisonError::into_inner);
        forwarders
            .get(session_id)
            .into_iter()
            .flatten()
            .filter_map(|flush| {
                let (request, wait) = oneshot::channel();
                flush.send(request).is_ok().then_some(wait)
            })
            .collect()
    };
    if waits.is_empty() {
        return;
    }
    if tokio::time::timeout(bound, futures::future::join_all(waits))
        .await
        .is_err()
    {
        crate::logging::warn(&format!(
            "fanout flush for session {session_id} ran out its {}ms bound; replying anyway",
            bound.as_millis()
        ));
    }
}
