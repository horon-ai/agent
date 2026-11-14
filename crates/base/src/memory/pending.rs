use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

type InjectedMemoryIdsBySession = HashMap<String, HashMap<String, Instant>>;

/// Memory IDs already surfaced for a session, with the time they were surfaced,
/// so the same memory is not repeated while it is still in the transcript.
/// Keyed by session ID.
///
/// A payload is not persisted into history, but the model's response that
/// consumed it is, so surfacing the same memory again shortly afterwards is
/// pure noise. This tracking used to be cleared on every detected topic change,
/// which fires often on real sessions (similarity between consecutive coding
/// turns regularly sits below any topic threshold), so the same memory could
/// come back minutes apart in one transcript. A TTL keeps the dedup stable
/// across topic wobble while still letting genuinely old memories resurface in
/// long sessions once they may have scrolled out of, or been compacted from,
/// the context window.
static INJECTED_MEMORY_IDS: Mutex<Option<InjectedMemoryIdsBySession>> = Mutex::new(None);

/// How long a surfaced memory counts as already known to a session.
const INJECTED_MEMORY_TTL_SECS: u64 = 45 * 60;

fn injected_recently(at: &Instant) -> bool {
    at.elapsed().as_secs() < INJECTED_MEMORY_TTL_SECS
}

/// One turn's memory guidance, ready to inject.
#[derive(Debug, Clone)]
pub struct PendingMemory {
    /// The formatted memory prompt ready for injection.
    pub prompt: String,
    /// Optional UI-focused rendering of the injected memory payload.
    /// This can contain extra display-only metadata that is not sent to the model.
    pub display_prompt: Option<String>,
    /// When this was computed.
    pub computed_at: Instant,
    /// Number of relevant memories found.
    pub count: usize,
    /// IDs of memories included in this prompt (for dedup tracking).
    pub memory_ids: Vec<String>,
}

/// Mark memory IDs as already surfaced for a session, so they are not repeated
/// while they stay fresh in the transcript.
pub fn mark_memories_injected(session_id: &str, ids: &[String]) {
    crate::memory_log::log_marked_injected(session_id, ids);
    insert_injected_ids(session_id, ids);
}

/// Mark memory IDs as already KNOWN to a session without them having been
/// injected, e.g. because they were just extracted from this session's own
/// transcript. The conversation already contains this information, so
/// re-injecting it would be a pure echo.
pub fn mark_memories_known(session_id: &str, ids: &[String], reason: &str) {
    if ids.is_empty() {
        return;
    }
    crate::memory_log::log_marked_known(session_id, ids, reason);
    insert_injected_ids(session_id, ids);
}

fn insert_injected_ids(session_id: &str, ids: &[String]) {
    if let Ok(mut guard) = INJECTED_MEMORY_IDS.lock() {
        let outer = guard.get_or_insert_with(HashMap::new);
        let set = outer.entry(session_id.to_string()).or_insert_with(HashMap::new);
        set.retain(|_, at| injected_recently(at));
        let now = Instant::now();
        for id in ids {
            set.insert(id.clone(), now);
        }
        crate::logging::info(&format!(
            "[{}] Marked {} memory IDs as injected (total tracked: {})",
            session_id,
            ids.len(),
            set.len()
        ));
    }
}

/// Replace injected memory tracking for a session with the provided IDs.
/// Used when restoring persisted session state so the same logical session does
/// not re-inject memories after reload/resume.
pub fn sync_injected_memories(session_id: &str, ids: &[String]) {
    if let Ok(mut guard) = INJECTED_MEMORY_IDS.lock() {
        let outer = guard.get_or_insert_with(HashMap::new);
        if ids.is_empty() {
            outer.remove(session_id);
            return;
        }

        let now = Instant::now();
        outer.insert(
            session_id.to_string(),
            ids.iter().cloned().map(|id| (id, now)).collect(),
        );
    }
}

/// Check if a memory ID has already been injected for a session.
/// An injected ID "expires" after [`INJECTED_MEMORY_TTL_SECS`], at which point
/// the memory may be surfaced again.
pub fn is_memory_injected(session_id: &str, id: &str) -> bool {
    if let Ok(guard) = INJECTED_MEMORY_IDS.lock()
        && let Some(outer) = guard.as_ref()
        && let Some(set) = outer.get(session_id)
        && let Some(at) = set.get(id)
    {
        return injected_recently(at);
    }
    false
}

/// Clear injected memory tracking for a session (call on session reset or topic change).
pub fn clear_injected_memories(session_id: &str) {
    if let Ok(mut guard) = INJECTED_MEMORY_IDS.lock()
        && let Some(outer) = guard.as_mut()
        && let Some(set) = outer.remove(session_id)
        && !set.is_empty()
    {
        crate::logging::info(&format!(
            "[{}] Clearing {} tracked injected memory IDs",
            session_id,
            set.len()
        ));
    }
}

/// Clear all injected memory tracking across all sessions.
pub fn clear_all_injected_memories() {
    if let Ok(mut guard) = INJECTED_MEMORY_IDS.lock() {
        if let Some(outer) = guard.as_ref() {
            let total: usize = outer.values().map(|s| s.len()).sum();
            if total > 0 {
                crate::logging::info(&format!(
                    "Clearing {} tracked injected memory IDs across {} sessions",
                    total,
                    outer.len()
                ));
            }
        }
        *guard = None;
    }
}

/// Forget what a session has already seen.
///
/// The only per-session memory state left is the surfaced-id set, so this is a
/// reset of that set. Callers use it when a session starts over, changes
/// project, or is deleted.
pub fn clear_pending_memory(session_id: &str) {
    clear_injected_memories(session_id);
}

/// Forget what every session has already seen.
pub fn clear_all_pending_memory() {
    clear_all_injected_memories();
}

/// True when this session has been told about any memory it may still repeat.
pub fn has_pending_memory(session_id: &str) -> bool {
    INJECTED_MEMORY_IDS
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|outer| outer.contains_key(session_id)))
        .unwrap_or(false)
}

#[cfg(test)]
pub(super) fn backdate_injected_memory_for_test(session_id: &str, id: &str, age: std::time::Duration) {
    if let Ok(mut guard) = INJECTED_MEMORY_IDS.lock()
        && let Some(outer) = guard.as_mut()
        && let Some(set) = outer.get_mut(session_id)
        && let Some(at) = set.get_mut(id)
    {
        *at = Instant::now() - age;
    }
}
