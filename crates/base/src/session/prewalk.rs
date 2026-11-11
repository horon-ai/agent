use chrono::Utc;

use super::model::{StoredReplayEvent, StoredReplayEventKind};

impl super::Session {
    /// Record the `[prewalk]` handoff decision for this session.
    ///
    /// The event is the durable one-shot marker: a resumed or restored session
    /// that already carries one never hands off again, and a refused handoff is
    /// recorded too so a missing model is not warned about on every round.
    pub fn record_prewalk_event(&mut self, from: Option<&str>, into: Option<&str>, refused: Option<&str>) {
        let kind = StoredReplayEventKind::Prewalk {
            from: from.map(str::to_string),
            into: into.map(str::to_string),
            refused: refused.map(str::to_string),
        };
        let event = StoredReplayEvent {
            timestamp: Utc::now(),
            kind,
        };
        self.memory_profile_cache.replay_events_count += 1;
        self.memory_profile_cache.replay_events_json_bytes += super::estimate_json_bytes(&event);
        self.replay_events.push(event);
        self.mark_replay_events_append_dirty();
    }

    /// The handoff this session already decided, if it decided one.
    pub fn prewalk_event(&self) -> Option<&StoredReplayEventKind> {
        self.replay_events
            .iter()
            .map(|event| &event.kind)
            .find(|kind| matches!(kind, StoredReplayEventKind::Prewalk { .. }))
    }
}
