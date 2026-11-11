//! Process-local store of degradation receipts, one entry per session.
//!
//! A provider builds a request, notices while doing so that the catalog it is
//! about to send is narrower than the catalog it was given, and leaves the
//! receipts here. The turn loop takes them when it stores the assistant message
//! for that request, so the loss is recorded on the record that carried the
//! turn rather than in a log line nothing reads.
//!
//! Two properties matter and neither is a cache:
//!
//! * **Once per session.** A schema that drops `uniqueItems` drops it on every
//!   request for the whole session. Reporting it each turn would bury the one
//!   fact worth knowing under a per-request repeat, so the first sighting is the
//!   only one retained.
//! * **Never blocking.** Recording is a mutex-protected insert with no I/O, every
//!   lock acquisition tolerates poisoning, and the pending list is bounded. A
//!   failure here can at worst lose a receipt, never the request carrying it.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex, MutexGuard};

use horon_agent_message_types::degradation::{DegradationKind, DegradationReceipt};

/// Receipts kept per session before further sightings are dropped.
///
/// A session reports a handful of distinct degradations; the cap exists so a
/// pathological catalog cannot grow this map without bound.
const PENDING_CAP: usize = 64;

/// Sessions tracked at once. Reaching this means the cleanup path did not run,
/// so the whole map is cleared rather than grown: every entry is process-local
/// bookkeeping that a later turn can re-report.
const SESSION_CAP: usize = 1_024;

/// What one session has seen and not yet reported.
#[derive(Debug, Default)]
struct SessionReceipts {
    seen: HashSet<(DegradationKind, String, String)>,
    pending: Vec<DegradationReceipt>,
}

static RECEIPTS: LazyLock<Mutex<HashMap<String, SessionReceipts>>> = LazyLock::new(Mutex::default);

/// Acquire `mutex`, recovering the guard from a poisoned lock.
///
/// A panic while the lock was held leaves the map unchanged in every path here,
/// so the poisoned state carries no corruption and the next caller may proceed.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Leave `receipts` for `session_id`, keeping only the ones this session has not
/// already seen.
///
/// Repeating a sighting is free: it is neither retained nor queued a second
/// time. Callers pass the receipts of one request, so the common case is a
/// request that degraded nothing and therefore records nothing at all.
pub fn record(session_id: &str, receipts: &[DegradationReceipt]) {
    if receipts.is_empty() {
        return;
    }
    let mut map = lock(&RECEIPTS);
    if map.len() >= SESSION_CAP && !map.contains_key(session_id) {
        map.clear();
    }
    let entry = map.entry(session_id.to_string()).or_default();
    for receipt in receipts {
        let key = (receipt.kind, receipt.tool.clone(), receipt.target.clone());
        if !entry.seen.insert(key) {
            continue;
        }
        if entry.pending.len() >= PENDING_CAP {
            // Leave it unseen so a later turn can still report it once room
            // frees up; reporting a subset is better than an unbounded queue.
            entry
                .seen
                .remove(&(receipt.kind, receipt.tool.clone(), receipt.target.clone()));
            continue;
        }
        entry.pending.push(receipt.clone());
    }
}

/// Take every receipt queued for `session_id`, leaving the seen set behind so a
/// repeat of the same degradation does not get reported twice.
pub fn take_pending(session_id: &str) -> Vec<DegradationReceipt> {
    let mut map = lock(&RECEIPTS);
    let Some(entry) = map.get_mut(session_id) else {
        return Vec::new();
    };
    let pending = std::mem::take(&mut entry.pending);
    if entry.seen.is_empty() {
        map.remove(session_id);
    }
    pending
}

/// Drop everything recorded for `session_id`, for a session that ended or was
/// torn down before its turns stored a message.
pub fn forget_session(session_id: &str) {
    lock(&RECEIPTS).remove(session_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(target: &str) -> DegradationReceipt {
        DegradationReceipt::new(DegradationKind::SchemaKeywordDropped, "bash", target)
    }

    #[test]
    fn a_repeated_degradation_is_queued_once_per_session() {
        // Given a session that recorded one dropped keyword.
        record("session-dedupe", &[receipt("uniqueItems")]);
        // When the same degradation is recorded again by a later request.
        record("session-dedupe", &[receipt("uniqueItems")]);
        // Then the session reports it once, and a second drain is empty.
        assert_eq!(take_pending("session-dedupe"), vec![receipt("uniqueItems")]);
        assert!(take_pending("session-dedupe").is_empty());
    }

    #[test]
    fn a_new_degradation_after_a_drain_is_reported_again() {
        // Given a session whose earlier receipt has already been reported.
        record("session-later", &[receipt("uniqueItems")]);
        assert_eq!(take_pending("session-later").len(), 1);
        // When a different keyword is dropped afterwards.
        record("session-later", &[receipt("propertyNames")]);
        // Then only the new one is reported, and the old one is not repeated.
        assert_eq!(take_pending("session-later"), vec![receipt("propertyNames")]);
    }

    #[test]
    fn recording_nothing_leaves_nothing_to_take() {
        // Given a request that degraded nothing.
        record("session-clean", &[]);
        // Then there is no receipt to report and no entry left behind.
        assert!(take_pending("session-clean").is_empty());
        assert!(!lock(&RECEIPTS).contains_key("session-clean"));
    }

    #[test]
    fn forgetting_a_session_drops_its_pending_receipts_and_its_seen_set() {
        // Given a session that saw and drained a degradation.
        record("session-forgotten", &[receipt("uniqueItems")]);
        assert_eq!(take_pending("session-forgotten").len(), 1);
        // When the session is torn down.
        forget_session("session-forgotten");
        // Then a new session reusing that id starts clean.
        record("session-forgotten", &[receipt("uniqueItems")]);
        assert_eq!(take_pending("session-forgotten").len(), 1);
    }

    #[test]
    fn a_saturated_store_accepts_more_without_failing_or_growing() {
        // Given a session that has already queued the cap.
        let session = "session-saturated";
        let receipts: Vec<DegradationReceipt> = (0..PENDING_CAP).map(|i| receipt(&format!("keyword{i}"))).collect();
        record(session, &receipts);
        // When a further request degrades something else.
        record(session, &[receipt("keyword-overflow")]);
        // Then recording still returned normally and the queue stayed bounded:
        // losing the extra receipt is acceptable, blocking the request is not.
        let pending = take_pending(session);
        assert_eq!(pending.len(), PENDING_CAP);
        assert!(!pending.contains(&receipt("keyword-overflow")));
    }

    #[test]
    fn a_poisoned_lock_still_yields_the_guard() {
        // Given a mutex poisoned by a panic while it was held.
        let mutex = Mutex::new(Vec::new());
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = mutex.lock().expect("unpoisoned");
            panic!("poison the lock");
        }));
        // When a caller takes it through the recovering helper.
        // Then it still gets a usable guard rather than propagating the panic.
        lock(&mutex).push(1);
        assert_eq!(lock(&mutex).len(), 1);
    }
}
