//! Periodic release of the derived caches of idle sessions.
//!
//! The daemon is the only runtime that keeps sessions resident between turns,
//! so it is the only one that can reclaim what an idle session is still holding.
//! The sweep is deliberately conservative: it touches memory only, never the
//! transcript, and a session that refuses one tick is retried only after a
//! whole idle window, not on every tick.
//!
//! Nothing here writes a session. A save that a migration lease would have
//! skipped cannot hide a lost original, because there is no save at all.

use super::{SessionAgents, SwarmMember};

/// The live roster this sweep reads only to see whether a session is busy.
pub(super) type SwarmMembers = Arc<RwLock<HashMap<String, SwarmMember>>>;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// How often the sweep looks for idle sessions.
///
/// Kept well below the default 30-minute threshold so reclamation is prompt
/// without making the tick itself a cost.
const CHECK_INTERVAL: Duration = Duration::from_secs(10);

struct IdleState {
    /// Start of the current idle window. Reset when a release is refused.
    since: Instant,
    /// The member status generation this window was opened for.
    last_status_change: Instant,
    /// Whether this window already released its caches.
    released: bool,
}

impl IdleState {
    fn new(now: Instant, last_status_change: Instant) -> Self {
        Self {
            since: now,
            last_status_change,
            released: false,
        }
    }

    fn observe_status(&mut self, last_status_change: Instant) {
        // Compare the actual generation rather than its age: a whole turn can
        // start and finish between two sweeps, even for thresholds below 10s.
        if self.last_status_change != last_status_change {
            self.since = last_status_change;
            self.last_status_change = last_status_change;
            self.released = false;
        }
    }

    fn due(&self, now: Instant, threshold: Duration, status_idle: Duration, activity_idle: Option<Duration>) -> bool {
        !threshold.is_zero()
            && !self.released
            && now.saturating_duration_since(self.since) >= threshold
            && status_idle >= threshold
            && activity_idle.is_none_or(|age| age >= threshold)
    }
}

/// Start the sweep. `threshold` is read from config on every tick so an edited
/// `[server] idle_hibernation_secs` applies without a restart.
pub(super) fn spawn(sessions: SessionAgents, members: SwarmMembers) {
    tokio::spawn(async move {
        let mut idle: HashMap<String, IdleState> = HashMap::new();
        let mut interval = tokio::time::interval(CHECK_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let threshold = Duration::from_secs(crate::config::config().server.idle_hibernation_secs);
            reclaim_idle_caches(&sessions, &members, &mut idle, threshold).await;
        }
    });
}

/// One sweep. Returns how many sessions released their derived caches.
///
/// A contended session lock, roster lock or Agent lock is skipped rather than
/// awaited: the sweep must never queue in front of a turn.
async fn reclaim_idle_caches(
    sessions: &SessionAgents,
    members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    idle: &mut HashMap<String, IdleState>,
    threshold: Duration,
) -> usize {
    if threshold.is_zero() {
        idle.clear();
        return 0;
    }
    let Ok(sessions) = sessions.try_read() else {
        return 0;
    };
    let Ok(members) = members.try_read() else {
        return 0;
    };
    // A destroyed session must not leave its window behind for a reused id.
    idle.retain(|id, _| sessions.contains_key(id));
    let candidates: Vec<_> = sessions
        .iter()
        .map(|(id, agent)| {
            (
                id.clone(),
                Arc::clone(agent),
                members
                    .get(id)
                    .map(|member| (member.status == "running", member.last_status_change)),
            )
        })
        .collect();
    drop(members);
    drop(sessions);

    let mut releases = Vec::new();
    let mut before = None;
    for (id, agent, member) in candidates {
        let Some((running, last_status_change)) = member else {
            idle.remove(&id);
            continue;
        };
        if running || crate::turn_cancel_registry::has_active_turn(&id) {
            idle.remove(&id);
            continue;
        }
        let Ok(mut agent) = agent.try_lock() else {
            idle.remove(&id);
            continue;
        };
        let now = Instant::now();
        let activity_idle = crate::session_metrics::last_activity_age_secs(&id).map(Duration::from_secs);
        let state = idle
            .entry(id.clone())
            .or_insert_with(|| IdleState::new(now, last_status_change));
        state.observe_status(last_status_change);
        if !state.due(now, threshold, last_status_change.elapsed(), activity_idle) {
            continue;
        }
        before.get_or_insert_with(|| crate::process_memory::snapshot_with_source("server_idle_hibernation_before"));
        let Some(released) = agent.hibernate_idle_caches() else {
            // A pending alert, compaction or live tool resets the idle window,
            // so a session a tool holds is retried after a whole threshold.
            state.since = now;
            continue;
        };
        state.released = true;
        releases.push((id, released));
    }
    if !releases.is_empty() {
        // RSS is process-wide, not a per-session metric, so purge the retained
        // heap once, after every Agent and roster lock is released.
        crate::process_memory::release_retained_heap("server_idle_hibernation");
        let after = crate::process_memory::snapshot_with_source("server_idle_hibernation_after");
        let before =
            before.unwrap_or_else(|| crate::process_memory::snapshot_with_source("server_idle_hibernation_before"));
        let session_ids = releases.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>().join(",");
        let provider_cache_bytes: usize = releases.iter().map(|(_, release)| release.provider_cache_bytes).sum();
        let tool_index_entries: usize = releases.iter().map(|(_, release)| release.tool_index_entries).sum();
        crate::logging::info(&format!(
            "Server idle hibernation: sessions={session_ids} mode=derived_caches provider_cache_json_bytes={provider_cache_bytes} tool_index_entries={tool_index_entries} rss_before_bytes={:?} rss_after_bytes={:?} rss_released_bytes={:?}",
            before.rss_bytes,
            after.rss_bytes,
            before
                .rss_bytes
                .zip(after.rss_bytes)
                .map(|(before, after)| before as i128 - after as i128),
        ));
    }
    releases.len()
}

#[cfg(test)]
#[path = "idle_hibernation_tests.rs"]
mod tests;
