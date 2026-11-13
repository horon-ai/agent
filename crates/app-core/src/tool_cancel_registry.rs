//! Process-global cancel signals for the tool calls each session is running.
//!
//! [`super::running_tool_registry`] answers "which call may the background
//! key move", and holds exactly one entry per session because the Alt+B
//! signal carries no target. Cancelling *one named* tool call needs the
//! opposite shape: every running call must be findable by its own id, and a
//! concurrent group must register all of its members at once.
//!
//! Both registries therefore live side by side. `running_tool_registry` keeps
//! its meaning for `background_tool` and the swarm watchdog; this one is the
//! only place a `cancel_tool_call` looks, and it never affects background
//! tasks (those are stopped by `cancel_background_terminal`).
//!
//! Entries are keyed by `(session_id, call_id)`. A registration is an RAII
//! guard: dropping it withdraws exactly that entry, so a call that finished,
//! panicked, was cancelled, or was moved to the background leaves no signal
//! behind for a later cancel to hit.

use horon_agent_agent_runtime::InterruptSignal;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

struct Entry {
    token: u64,
    signal: InterruptSignal,
}

type Entries = HashMap<(String, String), Entry>;

static RUNNING: LazyLock<Mutex<Entries>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock() -> MutexGuard<'static, Entries> {
    RUNNING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// RAII registration for one running tool call. Dropping it withdraws the
/// entry unless a newer registration for the same key replaced it.
pub struct ToolCancelGuard {
    session_id: String,
    call_id: String,
    token: u64,
    signal: InterruptSignal,
}

impl ToolCancelGuard {
    /// The signal a `cancel_tool_call` fires for this registration.
    ///
    /// Not used by the registry itself; the call site keeps its own clone of
    /// the same signal to select on, and this accessor lets a test or a future
    /// caller reach it without a second global lookup.
    #[allow(dead_code)]
    pub fn signal(&self) -> InterruptSignal {
        self.signal.clone()
    }
}

impl Drop for ToolCancelGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = RUNNING.lock()
            && map
                .get(&(self.session_id.clone(), self.call_id.clone()))
                .is_some_and(|entry| entry.token == self.token)
        {
            map.remove(&(self.session_id.clone(), self.call_id.clone()));
        }
    }
}

/// Register `call_id` as cancellable *before* the daemon lets the client see
/// that it is.
///
/// A v2 client may only cancel a call whose update advertised
/// `capabilities.cancel`, and the daemon advertises at `ServerEvent::ToolExec`
/// (`src/cli/acp/pump.rs`, `track_cancelable`). Registration used to happen
/// where the call was executed, which is later: streaming completes the whole
/// round first, and a concurrent group registers each member after it spawns.
/// A cancel that followed the advertisement but beat the registration was
/// therefore answered `accepted: false` — the client refused `-32602` for a
/// call it had just been told it could cancel (issue #274).
///
/// The guard this returns holds the entry while the call is only announced.
/// Hand it to [`claim`] when the call starts executing; dropping it sooner
/// withdraws the entry, which is what a call that refuses to run needs.
pub fn arm(session_id: &str, call_id: &str) -> (ToolCancelGuard, InterruptSignal) {
    let signal = InterruptSignal::new();
    let guard = register(session_id, call_id, signal.clone());
    (guard, signal)
}

/// Take the signal [`arm`] registered for a call that is about to run.
///
/// The guard is not consumed: it moves into the execution path so the entry
/// lives exactly as long as the call does, and its signal is what the wait
/// selects on. A cancel that landed between the advertisement and this call
/// left the signal fired, so the wait returns `Cancelled` immediately instead
/// of running work the client already stopped.
pub fn claim(guard: ToolCancelGuard) -> (ToolCancelGuard, InterruptSignal) {
    let signal = guard.signal();
    (guard, signal)
}

/// [`arm`] into the map a turn carries from announcement to execution.
pub fn arm_into(armed: &mut std::collections::HashMap<String, ToolCancelGuard>, session_id: &str, call_id: &str) {
    armed.insert(call_id.to_string(), arm(session_id, call_id).0);
}

/// Take the armed registration of a call that is about to run, arming it now
/// if it was never announced.
pub fn take_armed(
    armed: &mut std::collections::HashMap<String, ToolCancelGuard>,
    session_id: &str,
    call_id: &str,
) -> (ToolCancelGuard, InterruptSignal) {
    match armed.remove(call_id) {
        Some(guard) => claim(guard),
        None => arm(session_id, call_id),
    }
}

/// Register `signal` as the cancel target of `call_id` while it runs.
///
/// A newer registration for a key inherits a cancel that already fired for the
/// previous one. The daemon registers twice on purpose — once when the call is
/// advertised to a v2 client and again when it actually starts — and a cancel
/// can land in between; the second signal is the one the call waits on, so the
/// pending fire has to move to it or the work runs after the client stopped it.
pub fn register(session_id: &str, call_id: &str, signal: InterruptSignal) -> ToolCancelGuard {
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut map) = RUNNING.lock() {
        if map
            .get(&(session_id.to_string(), call_id.to_string()))
            .is_some_and(|entry| entry.signal.is_set())
            && !signal.is_set()
        {
            signal.fire();
        }
        map.insert(
            (session_id.to_string(), call_id.to_string()),
            Entry {
                token,
                signal: signal.clone(),
            },
        );
    }
    ToolCancelGuard {
        session_id: session_id.to_string(),
        call_id: call_id.to_string(),
        token,
        signal,
    }
}

/// Fire the cancel signal of `call_id` in `session_id`.
///
/// Returns false when no call by that id is running here, which is the only
/// case a `cancel_tool_call` must report back to its client.
pub fn cancel(session_id: &str, call_id: &str) -> bool {
    let signal = lock()
        .get(&(session_id.to_string(), call_id.to_string()))
        .map(|entry| entry.signal.clone());
    match signal {
        Some(signal) => {
            signal.fire();
            true
        }
        None => false,
    }
}

/// The call ids currently registered for `session_id`.
pub fn running_call_ids(session_id: &str) -> Vec<String> {
    lock()
        .keys()
        .filter(|(session, _)| session == session_id)
        .map(|(_, call_id)| call_id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_scopes_the_entry_and_cancel_fires_only_the_named_call() {
        let session = "ses_tool_cancel_registry_test";
        let first_signal = InterruptSignal::new();
        let second_signal = InterruptSignal::new();
        let first = register(session, "call_1", first_signal.clone());
        let second = register(session, "call_2", second_signal.clone());

        let mut ids = running_call_ids(session);
        ids.sort();
        assert_eq!(ids, vec!["call_1".to_string(), "call_2".to_string()]);

        assert!(cancel(session, "call_1"));
        assert!(first_signal.is_set(), "the named call is signalled");
        assert!(!second_signal.is_set(), "its sibling is untouched");

        assert!(!cancel(session, "call_3"), "an unknown call is refused");

        drop(first);
        assert!(!cancel(session, "call_1"), "a finished call is gone");
        assert_eq!(running_call_ids(session), vec!["call_2".to_string()]);
        first_signal.reset();
        assert!(cancel(session, "call_2"));
        assert!(!first_signal.is_set());
        drop(second);
        assert!(running_call_ids(session).is_empty());
    }

    #[test]
    fn sessions_do_not_share_call_ids() {
        let a = register("ses_tool_cancel_a", "call", InterruptSignal::new());
        let b = register("ses_tool_cancel_b", "call", InterruptSignal::new());
        assert!(!cancel("ses_tool_cancel_c", "call"));
        assert!(cancel("ses_tool_cancel_a", "call"));
        assert!(cancel("ses_tool_cancel_b", "call"));
        drop(a);
        drop(b);
    }

    #[test]
    fn newer_registration_for_the_same_call_wins_and_stale_drop_spares_it() {
        let session = "ses_tool_cancel_registry_replacement";
        let stale = register(session, "call", InterruptSignal::new());
        let live_signal = InterruptSignal::new();
        let live = register(session, "call", live_signal.clone());
        drop(stale);
        assert!(cancel(session, "call"), "the newer registration stays");
        assert!(live_signal.is_set());
        drop(live);
    }

    /// Issue #274. The daemon announces a call at `ToolExec` and registers it
    /// only when it runs, so a cancel in between found nothing and the client
    /// was told `-32602` for a call it had just been invited to cancel. Arming
    /// at the announcement closes that window: the entry exists before the
    /// client can see the control, and the cancel is accepted.
    #[test]
    fn a_cancel_after_arm_and_before_claim_is_accepted() {
        let session = "ses_tool_cancel_registry_armed";
        let (armed, _) = arm(session, "call_armed");

        assert!(
            cancel(session, "call_armed"),
            "the call is cancellable the moment it is announced"
        );

        // The call now runs: claiming the registration keeps it, and the
        // signal the wait would select on is already fired.
        let (_guard, signal) = claim(armed);
        assert!(
            signal.is_set(),
            "the cancel landed before the call started and must not be lost"
        );
        drop(_guard);
        assert!(!cancel(session, "call_armed"), "the finished call is gone");
    }

    /// A call refused before it runs drops its registration with the guard, so
    /// the client cannot cancel something that will never start.
    #[test]
    fn dropping_an_armed_registration_withdraws_it() {
        let session = "ses_tool_cancel_registry_armed_withdrawn";
        let (armed, _) = arm(session, "call_withdrawn");
        assert_eq!(running_call_ids(session), vec!["call_withdrawn".to_string()]);
        drop(armed);
        assert_eq!(running_call_ids(session), Vec::<String>::new());
        assert!(!cancel(session, "call_withdrawn"));
    }

    /// The second registration of a key must not erase a cancel the first one
    /// already took, or the call would run after the client stopped it.
    ///
    /// This is the concurrent-group order: the streaming loop arms the call
    /// when it announces it, and the group registers each member again when it
    /// dispatches. The first guard is still held at that point (the round
    /// releases unclaimed announcements afterwards), so the entry is live.
    #[test]
    fn re_registration_inherits_a_pending_cancel() {
        let session = "ses_tool_cancel_registry_inherit";
        let (armed, _) = arm(session, "call_inherit");
        assert!(cancel(session, "call_inherit"));

        let second = InterruptSignal::new();
        let _guard = register(session, "call_inherit", second.clone());
        assert!(second.is_set(), "the pending cancel moved to the new signal");

        // The stale announcement withdraws nothing: the live entry belongs to
        // the newer registration.
        drop(armed);
        assert_eq!(running_call_ids(session), vec!["call_inherit".to_string()]);
        assert!(
            cancel(session, "call_inherit"),
            "the running member is still cancellable"
        );
    }
}
