use super::Session;
#[cfg(test)]
use super::storage_paths::file_len_or_zero;
use super::storage_paths::session_path;
#[cfg(test)]
use crate::storage;
use anyhow::{Result, bail};
#[cfg(test)]
use chrono::Utc;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

static SAVE_GATE: std::sync::Mutex<(usize, bool)> = std::sync::Mutex::new((0, false));
static SAVES_FINISHED: std::sync::Condvar = std::sync::Condvar::new();
struct SaveGuard;
impl SaveGuard {
    fn enter() -> Result<Self> {
        let mut gate = SAVE_GATE.lock().map_err(|_| anyhow::anyhow!("save gate poisoned"))?;
        if gate.1 {
            bail!("session shutdown drain has started");
        }
        gate.0 += 1;
        Ok(Self)
    }
}
impl Drop for SaveGuard {
    fn drop(&mut self) {
        let mut gate = SAVE_GATE.lock().unwrap_or_else(|e| e.into_inner());
        gate.0 = gate.0.saturating_sub(1);
        SAVES_FINISHED.notify_all();
    }
}
/// Stop accepting saves and wait for every real commit reply, without polling.
pub fn drain_saves_for_shutdown(timeout: std::time::Duration) -> bool {
    let mut gate = SAVE_GATE.lock().unwrap_or_else(|e| e.into_inner());
    gate.1 = true;
    match SAVES_FINISHED.wait_timeout_while(gate, timeout, |gate| gate.0 != 0) {
        Ok((gate, _)) => gate.0 == 0,
        Err(_) => false,
    }
}
impl Session {
    pub fn strip_all_images(&mut self) -> usize {
        self.provider_messages();
        self.event_state.strip_images = true;
        let mut contents = self
            .provider_messages_cache
            .iter_mut()
            .map(|m| &mut m.content)
            .collect::<Vec<_>>();
        let stripped = horon_agent_compaction_core::strip_all_images_in_contents(&mut contents);
        self.mark_memory_profile_dirty();
        stripped
    }
    pub fn load_from_path(path: &Path) -> Result<Self> {
        let started = std::time::Instant::now();
        let result = Self::load_event_path(path);
        if let Ok(session) = &result {
            let log_bytes = session
                .event_log_path()
                .ok()
                .and_then(|path| std::fs::metadata(path).ok())
                .map(|m| m.len())
                .unwrap_or(0);
            if super::load_telemetry::note_load(started.elapsed(), log_bytes).should_log() {
                crate::logging::info(&format!(
                    "session_load: id={}, active_messages={}, log_bytes={}, elapsed_ms={}",
                    session.id,
                    session.active_context().len(),
                    log_bytes,
                    started.elapsed().as_millis()
                ));
            }
        }
        result
    }
    pub fn load(session_id: &str) -> Result<Self> {
        let id = horon_agent_session_store::SessionId::parse(session_id)?;
        let events = super::event_store::paths()?.events_log(&id)?;
        let path = if events.try_exists()? {
            events
        } else {
            session_path(session_id)?
        };
        let session = Self::load_from_path(&path)?;
        anyhow::ensure!(
            session.id == session_id,
            "session identity mismatch: requested {session_id}, found {}",
            session.id
        );
        Ok(session)
    }
    pub fn load_startup_stub(session_id: &str) -> Result<Self> {
        let mut session = Self::load(session_id)?;
        session.strip_transcript_for_remote_client();
        Ok(session)
    }
    pub fn load_for_remote_startup(session_id: &str) -> Result<Self> {
        let loaded = Self::load(session_id)?;
        let mut session = loaded.history_snapshot()?;
        session.event_state = loaded.event_state;
        session.event_state.active_start = 0;
        session.event_state.prefix_usage = None;
        session.event_state.prefix_visible = 0;
        session.event_state.auxiliary_stripped = true;
        session.env_snapshots.clear();
        session.memory_injections.clear();
        session.replay_events.clear();
        session.reset_persist_state(true);
        Ok(session)
    }
    pub fn save(&mut self) -> Result<()> {
        self.save_inner(false)
    }
    pub fn save_prepared(&mut self) -> Result<()> {
        self.save_inner(true)
    }
    fn save_inner(&mut self, force: bool) -> Result<()> {
        let _guard = SaveGuard::enter()?;
        self.save_events(force)
    }
}
// Obsolete snapshot writer retained only to construct/check legacy fixtures.
#[cfg(test)]
impl Session {
    fn pre_wipe_backup_path(path: &Path, timestamp: i64) -> PathBuf {
        let file_name = path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
        path.with_file_name(format!("{file_name}.pre-wipe-{timestamp}.bak"))
    }

    /// Preserve the last durable transcript before an empty in-memory session
    /// can replace it. This is deliberately best-effort: the hard rejection in
    /// `checkpoint_snapshot` is what prevents data loss, while these copies make
    /// recovery possible if another caller ever bypasses that invariant.
    fn guard_snapshot_shrink(&self, snapshot_path: &Path, journal_path: &Path) {
        const MIN_TRANSCRIPT_SNAPSHOT_BYTES: u64 = 4 * 1024;

        if !self.messages.is_empty() || file_len_or_zero(snapshot_path) <= MIN_TRANSCRIPT_SNAPSHOT_BYTES {
            return;
        }

        let timestamp = Utc::now().timestamp_millis();
        let snapshot_backup = Self::pre_wipe_backup_path(snapshot_path, timestamp);
        let journal_backup = Self::pre_wipe_backup_path(journal_path, timestamp);
        let mut backup_errors = Vec::new();

        if let Err(err) = std::fs::copy(snapshot_path, &snapshot_backup) {
            backup_errors.push(format!("snapshot {}: {err}", snapshot_backup.display()));
        }
        if journal_path.exists()
            && let Err(err) = std::fs::copy(journal_path, &journal_backup)
        {
            backup_errors.push(format!("journal {}: {err}", journal_backup.display()));
        }

        let fields = vec![
            ("phase", "pre_wipe_backup".to_string()),
            ("session_id", self.id.clone()),
            ("snapshot_path", snapshot_path.display().to_string()),
            ("snapshot_backup", snapshot_backup.display().to_string()),
            ("journal_backup", journal_backup.display().to_string()),
            ("errors", backup_errors.join("; ")),
        ];
        crate::logging::event_warn("SESSION_PERSISTENCE", fields);
    }

    fn checkpoint_snapshot(&mut self, snapshot_path: &Path, journal_path: &Path) -> Result<()> {
        // Remote `/restart` and `/reload` save the client's stub; writing it as a
        // snapshot wiped the server-owned transcript and deleted its journal.
        if self.persist_state.transcript_stripped && snapshot_path.exists() {
            bail!(
                "refusing to checkpoint transcript-less stub of session {} over the persisted transcript",
                self.id
            );
        }
        let destructive_empty_checkpoint =
            self.messages.is_empty() && self.persist_state.messages_len > 0 && snapshot_path.exists();
        if destructive_empty_checkpoint {
            self.guard_snapshot_shrink(snapshot_path, journal_path);
            bail!(
                "refusing to replace non-empty persisted transcript for session {} with an empty checkpoint",
                self.id
            );
        }
        self.guard_snapshot_shrink(snapshot_path, journal_path);
        storage::write_json_fast(snapshot_path, self)?;
        if journal_path.exists() {
            let _ = std::fs::remove_file(journal_path);
        }
        self.reset_persist_state(true);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ContentBlock, Role};

    fn pre_wipe_backups(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.to_string_lossy().contains(".pre-wipe-"))
            .collect()
    }

    #[test]
    fn empty_checkpoint_preserves_and_refuses_to_wipe_large_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot_path = dir.path().join("session_guard.json");
        let journal_path = dir.path().join("session_guard.jsonl");
        let original_snapshot = vec![b'x'; 5 * 1024];
        let original_journal = b"durable journal tail\n";
        std::fs::write(&snapshot_path, &original_snapshot).unwrap();
        std::fs::write(&journal_path, original_journal).unwrap();

        let mut session = Session::create_with_id("session_guard".into(), None, None);
        session.persist_state.snapshot_exists = true;
        session.persist_state.messages_len = 686;

        let error = session.checkpoint_snapshot(&snapshot_path, &journal_path).unwrap_err();
        assert!(error.to_string().contains("refusing to replace"));
        assert_eq!(std::fs::read(&snapshot_path).unwrap(), original_snapshot);
        assert_eq!(std::fs::read(&journal_path).unwrap(), original_journal);

        let backups = pre_wipe_backups(dir.path());
        assert_eq!(backups.len(), 2);
        assert!(backups.iter().any(|path| {
            path.to_string_lossy().contains(".json.pre-wipe-") && std::fs::read(path).unwrap() == original_snapshot
        }));
        assert!(backups.iter().any(|path| {
            path.to_string_lossy().contains(".jsonl.pre-wipe-") && std::fs::read(path).unwrap() == original_journal
        }));
    }

    #[test]
    fn stripped_remote_stub_cannot_checkpoint_over_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot_path = dir.path().join("session_stub.json");
        let journal_path = dir.path().join("session_stub.jsonl");
        let original_snapshot = vec![b'x'; 5 * 1024];
        std::fs::write(&snapshot_path, &original_snapshot).unwrap();
        std::fs::write(&journal_path, b"journal tail\n").unwrap();

        let mut session = Session::create_with_id("session_stub".into(), None, None);
        session.persist_state.snapshot_exists = true;
        session.persist_state.messages_len = 3;
        session.strip_transcript_for_remote_client();
        // `/restart` changes status, which forces a full snapshot.
        session.set_status(crate::session::SessionStatus::Reloaded);

        let error = session.checkpoint_snapshot(&snapshot_path, &journal_path).unwrap_err();
        assert!(error.to_string().contains("transcript-less stub"));
        assert_eq!(std::fs::read(&snapshot_path).unwrap(), original_snapshot);
        assert!(journal_path.exists());
    }

    #[test]
    fn non_empty_shrink_checkpoint_remains_allowed_without_pre_wipe_backup() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot_path = dir.path().join("session_compacted.json");
        let journal_path = dir.path().join("session_compacted.jsonl");
        std::fs::write(&snapshot_path, vec![b'x'; 5 * 1024]).unwrap();
        std::fs::write(&journal_path, b"old journal\n").unwrap();

        let mut session = Session::create_with_id("session_compacted".into(), None, None);
        session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: "retained compacted message".into(),
                cache_control: None,
            }],
        );
        session.persist_state.snapshot_exists = true;
        session.persist_state.messages_len = 2;

        session.checkpoint_snapshot(&snapshot_path, &journal_path).unwrap();
        assert!(!journal_path.exists());
        assert!(pre_wipe_backups(dir.path()).is_empty());
        let restored: Session = storage::read_json(&snapshot_path).unwrap();
        assert_eq!(restored.messages.len(), 1);
    }

    #[test]
    fn small_empty_snapshot_is_rejected_without_creating_noise_backup() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot_path = dir.path().join("session_small.json");
        let journal_path = dir.path().join("session_small.jsonl");
        let original = b"small metadata stub";
        std::fs::write(&snapshot_path, original).unwrap();

        let mut session = Session::create_with_id("session_small".into(), None, None);
        session.persist_state.snapshot_exists = true;
        session.persist_state.messages_len = 1;

        assert!(session.checkpoint_snapshot(&snapshot_path, &journal_path).is_err());
        assert_eq!(std::fs::read(&snapshot_path).unwrap(), original);
        assert!(pre_wipe_backups(dir.path()).is_empty());
    }
}
