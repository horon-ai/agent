//! Session process-ownership markers under the historical `~/.horon/active_pids` name.
//!
//! “Active” means a process owns the session, not that a window is open, a client
//! is connected, or a model is generating. In server mode, the owning PID is the
//! daemon's PID and can be shared by many sessions, including disconnected ones.
//! Markers can outlive their owner, so consumers needing live sessions must check
//! PID liveness (as [`session_presence`] does).
//!
//! This is pure filesystem state keyed by session ID, used to discover session
//! ownership (and to map a PID back to one of its sessions). It
//! lives in the storage crate because it only needs [`horon_agent_dir`] and is a
//! low-level concern shared by session management, dictation, and crash
//! recovery, none of which should pull the full `session` module into scope.

use crate::horon_agent_dir;
use std::path::PathBuf;

/// Directory holding one ownership marker per session ID (`~/.horon/active_pids`).
/// Each file contains the owning process PID, not a client/window PID in server mode.
pub fn active_pids_dir() -> Option<PathBuf> {
    horon_agent_dir().ok().map(|d| d.join("active_pids"))
}

/// Directory holding per-session "currently streaming" markers. A marker file
/// exists only while a session is actively generating a model response. The
/// file content is the owning process PID so stale markers (from crashed
/// processes) can be detected and ignored.
pub fn streaming_pids_dir() -> Option<std::path::PathBuf> {
    horon_agent_dir().ok().map(|d| d.join("streaming_pids"))
}

/// Directory holding markers for internal sessions (debug/test sessions and
/// spawned children such as swarm workers). These sessions keep their normal
/// active-pid lifecycle markers, but presence UIs like the menu bar indicator
/// only want to show top-level sessions the user opened directly, so internal
/// ones are flagged here and filtered out of user-facing counts (issue #508).
pub fn internal_pids_dir() -> Option<std::path::PathBuf> {
    horon_agent_dir().ok().map(|d| d.join("internal_pids"))
}

/// Flag (or unflag) `session_id` as an internal session for presence UIs.
pub fn set_session_internal(session_id: &str, internal: bool) {
    let Some(dir) = internal_pids_dir() else {
        return;
    };
    if internal {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(session_id), "");
    } else {
        let _ = std::fs::remove_file(dir.join(session_id));
    }
}

/// Whether `session_id` is flagged as an internal session.
pub fn session_is_internal(session_id: &str) -> bool {
    internal_pids_dir().is_some_and(|dir| dir.join(session_id).exists())
}

/// Record that `session_id` is owned by process `pid`.
pub fn register_active_pid(session_id: &str, pid: u32) {
    if let Some(dir) = active_pids_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(session_id), pid.to_string());
    }
}

/// Remove the active-PID record for `session_id`, if present.
pub fn unregister_active_pid(session_id: &str) {
    if let Some(dir) = active_pids_dir() {
        let _ = std::fs::remove_file(dir.join(session_id));
    }
    // A closed session is never streaming, and its internal flag is moot.
    unmark_streaming(session_id);
    set_session_internal(session_id, false);
}

/// Remove every active-PID marker owned by `pid`, returning
/// `(removed, failed)`.
///
/// Used at server startup after an exec-based reload. `exec` preserves the
/// daemon PID, so markers written by the previous process image still look live
/// to [`session_presence`]'s liveness check, yet the fresh image owns no
/// sessions until clients reconnect and re-register via `mark_active`. Reaping
/// them here keeps presence counts honest. Crash-restart is unaffected: a
/// genuinely new process has a different PID, so its markers are left for crash
/// recovery.
///
/// `failed` is non-zero when a marker could not be unlinked.
pub fn prune_active_pids_owned_by(pid: u32) -> (usize, usize) {
    let Some(dir) = active_pids_dir() else {
        return (0, 0);
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return (0, 0);
    };

    let owned: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            std::fs::read_to_string(entry.path())
                .ok()
                .and_then(|raw| raw.trim().parse::<u32>().ok())
                == Some(pid)
        })
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();

    let mut removed = 0;
    let mut failed = 0;
    for session_id in &owned {
        unregister_active_pid(session_id);
        // `unregister_active_pid` discards filesystem errors, so confirm the
        // marker is actually gone. A failed unlink leaves it live; count it as
        // failed rather than reporting a prune that did not happen.
        if dir.join(session_id).exists() {
            failed += 1;
        } else {
            removed += 1;
        }
    }
    (removed, failed)
}

/// Reap markers that claim `pid` but predate `before`, returning
/// `(removed, failed)`.
///
/// After an exec-based reload the daemon PID is preserved, so markers written by
/// the previous process image still name a live PID and `process_is_running`
/// cannot tell them apart from markers this process owns right now. Comparing the
/// marker's mtime against the instant this process started does tell them apart:
/// a marker written after that instant was written by this image.
///
/// [`prune_active_pids_owned_by`] removes every marker claiming `pid`, which is
/// only safe while the process owns no sessions yet. This variant is safe from a
/// background thread, where a client may already have registered a marker.
pub fn prune_active_pids_owned_by_before(pid: u32, before: std::time::SystemTime) -> (usize, usize) {
    let Some(dir) = active_pids_dir() else {
        return (0, 0);
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return (0, 0);
    };

    let mut removed = 0;
    let mut failed = 0;
    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        let claims_pid = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| raw.trim().parse::<u32>().ok())
            == Some(pid);
        if !claims_pid {
            continue;
        }
        let predates = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok()
            .is_some_and(|modified| modified < before);
        if !predates {
            continue;
        }

        let session_id = entry.file_name().to_string_lossy().into_owned();
        unregister_active_pid(&session_id);
        if dir.join(&session_id).exists() {
            failed += 1;
        } else {
            removed += 1;
        }
    }
    (removed, failed)
}

/// Remove ownership markers whose owning process is gone and whose marker is
/// older than `min_age`, returning `(removed, failed)`.
///
/// Markers outlive their owner whenever a process dies without running its
/// shutdown path (a crash, a `SIGKILL`, a closed laptop lid). Nothing removed
/// them: [`prune_active_pids_owned_by`] only reclaims markers that name the
/// *current* process, so a long-lived machine accumulates one dead marker per
/// session ever run. That registry is then read in full by [`session_presence`]
/// (the menu bar polls it every second) and by
/// [`active_session_ids`] consumers, so its size sets their cost.
///
/// `min_age` keeps crash recovery intact: a marker for a session that died
/// minutes ago still names its owner, and the restart-snapshot path can still
/// attribute it. Only markers past the caller's recovery window are reaped.
///
/// A marker whose contents do not parse is left alone and counted as `failed`:
/// its owner is unknown, so it cannot be proven dead.
pub fn prune_dead_owner_markers(now: std::time::SystemTime, min_age: std::time::Duration) -> (usize, usize) {
    let Some(dir) = active_pids_dir() else {
        return (0, 0);
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return (0, 0);
    };

    let mut removed = 0;
    let mut failed = 0;
    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        let Some(pid) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| raw.trim().parse::<u32>().ok())
        else {
            // Unreadable or unparsable: the owner is unknown, so the marker is
            // preserved rather than guessed dead.
            failed += 1;
            continue;
        };
        if process_is_running(pid) {
            continue;
        }
        let old_enough = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= min_age);
        if !old_enough {
            continue;
        }

        let session_id = entry.file_name().to_string_lossy().into_owned();
        unregister_active_pid(&session_id);
        // `unregister_active_pid` discards filesystem errors, so confirm the
        // marker is actually gone before reporting a removal.
        if dir.join(&session_id).exists() {
            failed += 1;
        } else {
            removed += 1;
        }
    }
    (removed, failed)
}

/// Mark a session as actively streaming a model response.
pub fn mark_streaming(session_id: &str) {
    if let Some(dir) = streaming_pids_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(session_id), std::process::id().to_string());
    }
}

/// Clear the streaming marker for a session (turn finished or interrupted).
pub fn unmark_streaming(session_id: &str) {
    if let Some(dir) = streaming_pids_dir() {
        let _ = std::fs::remove_file(dir.join(session_id));
    }
}

/// Session IDs whose streaming marker names a live process, i.e. every session
/// some horon-agent process (normally the shared daemon) is running a turn for right
/// now. Cheaper than [`session_presence`]: it reads only the handful of
/// streaming markers, never the whole active-PID registry, so clients such as
/// the desktop sidebar can poll it every second for sessions they have not
/// attached to.
pub fn streaming_session_ids() -> Vec<String> {
    let Some(dir) = streaming_pids_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            std::fs::read_to_string(entry.path())
                .ok()
                .and_then(|raw| raw.trim().parse::<u32>().ok())
                .is_some_and(process_is_running)
        })
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect()
}

/// RAII guard that marks a session as streaming for its lifetime and clears the
/// marker on drop. This guarantees the marker is cleared on every exit path
/// (normal return, `?` propagation, interrupt, or panic) so the menu bar count
/// never gets stuck showing a phantom streaming session.
pub struct StreamingGuard {
    session_id: String,
}

impl StreamingGuard {
    pub fn new(session_id: impl Into<String>) -> Self {
        let session_id = session_id.into();
        mark_streaming(&session_id);
        Self { session_id }
    }
}

impl Drop for StreamingGuard {
    fn drop(&mut self) {
        unmark_streaming(&self.session_id);
    }
}

/// Find one session ID registered to the given process ID, without a liveness check.
/// A daemon may own multiple sessions, so the returned session is not necessarily
/// connected to a client or focused in a window.
pub fn find_active_session_id_by_pid(pid: u32) -> Option<String> {
    let dir = active_pids_dir()?;
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let session_id = entry.file_name().to_string_lossy().to_string();
        let stored = std::fs::read_to_string(entry.path()).ok()?;
        if stored.trim().parse::<u32>().ok()? == pid {
            return Some(session_id);
        }
    }
    None
}

/// List session IDs with ownership markers in `~/.horon/active_pids`.
/// Does not check PID liveness or whether a client/window is connected.
pub fn active_session_ids() -> Vec<String> {
    let Some(dir) = active_pids_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect()
}

fn process_is_running(pid: u32) -> bool {
    // Pid zero names no process; the platform probe would answer about the
    // caller's own group.
    if pid == 0 {
        return false;
    }
    horon_agent_platform::is_process_running(pid)
}

/// Live snapshot of how many horon-agent sessions are running, and how many of those
/// are actively streaming a model response right now. Used by the menu bar
/// indicator (`horon-agent menubar`) and any other presence UI.
/// These are process-owned session counts, not open-window or connected-client counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionCounts {
    /// Number of live sessions (registered PID is still running).
    pub total: usize,
    /// Number of live sessions currently streaming a model response.
    pub streaming: usize,
}

/// Live presence info for one running session, derived from the active-pid
/// registry and the streaming markers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPresence {
    /// Session ID, e.g. `session_fox_1234567890_deadbeef`.
    pub session_id: String,
    /// PID of the process that owns the session.
    pub pid: u32,
    /// Whether the session is actively streaming a model response right now.
    pub streaming: bool,
    /// When the current streaming turn started (streaming marker mtime), if
    /// the session is streaming. Lets presence UIs show "working for 2m".
    pub streaming_since: Option<std::time::SystemTime>,
    /// Whether this is an internal session (debug/test or a spawned child
    /// such as a swarm worker) that user-facing presence UIs should hide.
    pub internal: bool,
}

/// Snapshot per-session presence by scanning the active-pid registry and
/// streaming markers, skipping any entries whose owning process is no longer
/// alive. This is a cheap O(n) scan over a handful of tiny files; used by the
/// menu bar indicator and other presence UI.
/// A live owner does not imply a connected client or an open window.
pub fn session_presence() -> Vec<SessionPresence> {
    let Some(active_dir) = active_pids_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&active_dir) else {
        return Vec::new();
    };

    let streaming_dir = streaming_pids_dir();
    let internal_dir = internal_pids_dir();
    let mut sessions = Vec::new();

    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        let session_id = entry.file_name().to_string_lossy().to_string();
        let Some(pid) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| raw.trim().parse::<u32>().ok())
        else {
            continue;
        };
        if !process_is_running(pid) {
            continue;
        }

        let marker_path = streaming_dir.as_ref().map(|dir| dir.join(&session_id));
        let streaming = marker_path.as_ref().is_some_and(|marker| {
            std::fs::read_to_string(marker)
                .ok()
                .and_then(|raw| raw.trim().parse::<u32>().ok())
                .is_some_and(process_is_running)
        });
        let streaming_since = if streaming {
            marker_path
                .as_ref()
                .and_then(|marker| std::fs::metadata(marker).ok())
                .and_then(|meta| meta.modified().ok())
        } else {
            None
        };

        sessions.push(SessionPresence {
            internal: internal_dir.as_ref().is_some_and(|dir| dir.join(&session_id).exists()),
            session_id,
            pid,
            streaming,
            streaming_since,
        });
    }

    sessions
}

/// Compute the current session counts from [`session_presence`].
pub fn session_counts() -> SessionCounts {
    let sessions = session_presence();
    SessionCounts {
        total: sessions.len(),
        streaming: sessions.iter().filter(|s| s.streaming).count(),
    }
}

/// [`session_presence`] filtered to sessions the user opened directly,
/// excluding internal debug/test sessions and spawned children such as swarm
/// workers (issue #508). Used by the menu bar indicator.
pub fn user_session_presence() -> Vec<SessionPresence> {
    session_presence().into_iter().filter(|s| !s.internal).collect()
}

/// Session counts over [`user_session_presence`] only.
pub fn user_session_counts() -> SessionCounts {
    let sessions = user_session_presence();
    SessionCounts {
        total: sessions.len(),
        streaming: sessions.iter().filter(|s| s.streaming).count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize tests that mutate `HORON_HOME`.
    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        crate::test_horon_agent_home_lock()
    }

    #[test]
    fn session_counts_counts_live_and_streaming_only() {
        let _guard = lock_env();
        let temp = tempfile::tempdir().expect("tempdir");
        horon_agent_core::env::set_var("HORON_HOME", temp.path());

        let live = std::process::id();
        // Pick a PID that is almost certainly dead.
        let dead = 999_999u32;

        // live + streaming
        register_active_pid("session_alpha", live);
        mark_streaming("session_alpha");
        // live + not streaming
        register_active_pid("session_beta", live);
        // dead session (should be ignored entirely)
        register_active_pid("session_gamma", dead);
        // live session whose streaming marker points at a dead pid (ignored for streaming)
        register_active_pid("session_delta", live);
        if let Some(dir) = streaming_pids_dir() {
            let _ = std::fs::write(dir.join("session_delta"), dead.to_string());
        }

        let mut streaming_ids = streaming_session_ids();
        streaming_ids.sort();
        assert_eq!(streaming_ids, vec!["session_alpha".to_string()]);

        let counts = session_counts();
        assert_eq!(counts.total, 3, "three live sessions expected");
        assert_eq!(counts.streaming, 1, "only one live streaming session expected");

        // Per-session presence reports the same view, keyed by session.
        let sessions = session_presence();
        assert_eq!(sessions.len(), 3);
        let by_id = |id: &str| {
            sessions
                .iter()
                .find(|s| s.session_id == id)
                .unwrap_or_else(|| panic!("{id} should be present"))
        };
        assert!(by_id("session_alpha").streaming);
        assert!(!by_id("session_beta").streaming);
        assert!(!by_id("session_delta").streaming);
        assert_eq!(by_id("session_alpha").pid, live);
        assert!(!sessions.iter().any(|s| s.session_id == "session_gamma"));

        // Clearing the streaming marker drops the streaming count.
        unmark_streaming("session_alpha");
        assert_eq!(session_counts().streaming, 0);

        // Unregistering also clears any leftover streaming marker.
        register_active_pid("session_epsilon", live);
        mark_streaming("session_epsilon");
        assert_eq!(session_counts().streaming, 1);
        unregister_active_pid("session_epsilon");
        assert_eq!(session_counts().streaming, 0);

        horon_agent_core::env::remove_var("HORON_HOME");
    }

    #[test]
    fn streaming_guard_marks_and_clears_on_drop() {
        let _guard = lock_env();
        let temp = tempfile::tempdir().expect("tempdir");
        horon_agent_core::env::set_var("HORON_HOME", temp.path());

        register_active_pid("session_guard", std::process::id());
        assert_eq!(session_counts().streaming, 0);
        {
            let _streaming = StreamingGuard::new("session_guard");
            assert_eq!(session_counts().streaming, 1);
        }
        assert_eq!(session_counts().streaming, 0);

        horon_agent_core::env::remove_var("HORON_HOME");
    }

    /// Issue #508: internal (debug/child) sessions stay in the raw registry
    /// but are excluded from user-facing presence and counts.
    #[test]
    fn user_session_counts_exclude_internal_sessions() {
        let _guard = lock_env();
        let temp = tempfile::tempdir().expect("tempdir");
        horon_agent_core::env::set_var("HORON_HOME", temp.path());

        let live = std::process::id();
        register_active_pid("session_user", live);
        register_active_pid("session_worker", live);
        set_session_internal("session_worker", true);
        mark_streaming("session_worker");

        assert!(session_is_internal("session_worker"));
        assert!(!session_is_internal("session_user"));

        // Raw view keeps everything (lifecycle/crash detection needs it).
        assert_eq!(session_counts().total, 2);
        assert_eq!(session_counts().streaming, 1);

        // User-facing view hides the internal worker.
        let user_counts = user_session_counts();
        assert_eq!(user_counts.total, 1);
        assert_eq!(user_counts.streaming, 0);
        let user_sessions = user_session_presence();
        assert_eq!(user_sessions.len(), 1);
        assert_eq!(user_sessions[0].session_id, "session_user");

        // Unflagging restores visibility; unregistering clears the flag file.
        set_session_internal("session_worker", false);
        assert_eq!(user_session_counts().total, 2);
        set_session_internal("session_worker", true);
        unregister_active_pid("session_worker");
        assert!(!session_is_internal("session_worker"));

        horon_agent_core::env::remove_var("HORON_HOME");
    }

    /// Regression for stale markers left by an exec-based reload: markers that
    /// claim our own PID are reaped, others are left for crash recovery, and
    /// companion streaming/internal markers go with the pruned session.
    #[test]
    fn prune_active_pids_owned_by_removes_only_that_pid_and_companions() {
        let _guard = lock_env();
        let temp = tempfile::tempdir().expect("tempdir");
        horon_agent_core::env::set_var("HORON_HOME", temp.path());

        let me = std::process::id();
        let other = 999_999u32;
        register_active_pid("session_mine", me);
        register_active_pid("session_theirs", other);
        mark_streaming("session_mine");
        set_session_internal("session_mine", true);

        assert_eq!(prune_active_pids_owned_by(me), (1, 0), "only our own marker");
        let ids = active_session_ids();
        assert!(!ids.contains(&"session_mine".to_string()));
        assert!(ids.contains(&"session_theirs".to_string()));

        // Pruning a session clears its companion markers too.
        assert!(!session_is_internal("session_mine"));
        if let Some(dir) = streaming_pids_dir() {
            assert!(!dir.join("session_mine").exists());
        }

        // Idempotent: nothing left to prune on a second pass.
        assert_eq!(prune_active_pids_owned_by(me), (0, 0));

        horon_agent_core::env::remove_var("HORON_HOME");
    }

    /// A marker that cannot be unlinked must not be reported as pruned, since
    /// `unregister_active_pid` swallows filesystem errors.
    #[cfg(unix)]
    #[test]
    fn prune_counts_only_successful_deletions() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = lock_env();
        // Root ignores the directory write bit, so unlink would succeed anyway.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        horon_agent_core::env::set_var("HORON_HOME", temp.path());

        let me = std::process::id();
        register_active_pid("session_mine", me);

        let dir = active_pids_dir().expect("active_pids dir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).expect("make markers undeletable");

        // A failed unlink is reported as failed, not as a prune.
        assert_eq!(prune_active_pids_owned_by(me), (0, 1));
        assert!(active_session_ids().contains(&"session_mine".to_string()));

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("restore write access");
        assert_eq!(prune_active_pids_owned_by(me), (1, 0));

        horon_agent_core::env::remove_var("HORON_HOME");
    }

    /// A marker whose owner process is gone and which is older than the
    /// retention window is reclaimed, so the registry stops growing forever.
    #[test]
    fn prune_dead_owner_markers_removes_only_old_dead_owners() {
        use std::time::{Duration, SystemTime};

        let _guard = lock_env();
        let temp = tempfile::tempdir().expect("tempdir");
        horon_agent_core::env::set_var("HORON_HOME", temp.path());

        let dead = 999_999u32;
        register_active_pid("session_dead_old", dead);
        register_active_pid("session_dead_recent", dead);
        register_active_pid("session_live_old", std::process::id());
        mark_streaming("session_dead_old");
        set_session_internal("session_dead_old", true);

        // Age two markers past the retention window by backdating their mtime.
        let dir = active_pids_dir().expect("active_pids dir");
        let old = SystemTime::now() - Duration::from_secs(48 * 60 * 60);
        for name in ["session_dead_old", "session_live_old"] {
            let file = std::fs::File::options()
                .write(true)
                .open(dir.join(name))
                .expect("open marker");
            file.set_modified(old).expect("backdate marker");
        }

        let (removed, failed) = prune_dead_owner_markers(SystemTime::now(), Duration::from_secs(24 * 60 * 60));
        assert_eq!((removed, failed), (1, 0), "only the old dead-owner marker goes");

        let ids = active_session_ids();
        assert!(!ids.contains(&"session_dead_old".to_string()));
        assert!(
            ids.contains(&"session_dead_recent".to_string()),
            "recent crash stays recoverable"
        );
        assert!(
            ids.contains(&"session_live_old".to_string()),
            "live owner is never reaped"
        );

        // Companion markers follow the session that was reaped.
        assert!(!session_is_internal("session_dead_old"));
        if let Some(streaming) = streaming_pids_dir() {
            assert!(!streaming.join("session_dead_old").exists());
        }

        // Idempotent: a second pass has nothing left to do.
        assert_eq!(
            prune_dead_owner_markers(SystemTime::now(), Duration::from_secs(24 * 60 * 60)),
            (0, 0)
        );

        horon_agent_core::env::remove_var("HORON_HOME");
    }

    /// An unreadable marker is preserved: its owner is unknown, so it cannot be
    /// proven dead.
    #[test]
    fn prune_dead_owner_markers_preserves_unparsable_markers() {
        use std::time::{Duration, SystemTime};

        let _guard = lock_env();
        let temp = tempfile::tempdir().expect("tempdir");
        horon_agent_core::env::set_var("HORON_HOME", temp.path());

        let dir = active_pids_dir().expect("active_pids dir");
        std::fs::create_dir_all(&dir).expect("create active_pids dir");
        std::fs::write(dir.join("session_garbage"), "not-a-pid").expect("write marker");

        let (removed, failed) = prune_dead_owner_markers(SystemTime::now(), Duration::from_secs(24 * 60 * 60));
        assert_eq!((removed, failed), (0, 1));
        assert!(dir.join("session_garbage").exists());

        horon_agent_core::env::remove_var("HORON_HOME");
    }

    /// The startup reap is destructive, so it must delete only markers that name
    /// this process *and* predate it. A client that registers after the cutoff
    /// is a live session and survives; another process keeps its markers
    /// however old they are.
    #[test]
    fn prune_active_pids_owned_by_before_reaps_only_markers_older_than_the_cutoff() {
        use std::time::{Duration, SystemTime};

        let _guard = lock_env();
        let temp = tempfile::tempdir().expect("tempdir");
        horon_agent_core::env::set_var("HORON_HOME", temp.path());

        let me = std::process::id();
        let other = 999_999u32;
        register_active_pid("session_mine_old", me);
        register_active_pid("session_mine_new", me);
        register_active_pid("session_theirs_old", other);

        // Backdate the two markers that belong to a previous process image; the
        // third keeps the mtime it was written with, which is after the cutoff.
        let dir = active_pids_dir().expect("active_pids dir");
        let old = SystemTime::now() - Duration::from_secs(48 * 60 * 60);
        for name in ["session_mine_old", "session_theirs_old"] {
            let file = std::fs::File::options()
                .write(true)
                .open(dir.join(name))
                .expect("open marker");
            file.set_modified(old).expect("backdate marker");
        }

        let cutoff = SystemTime::now() - Duration::from_secs(24 * 60 * 60);
        let (removed, failed) = prune_active_pids_owned_by_before(me, cutoff);
        assert_eq!(
            (removed, failed),
            (1, 0),
            "only our own marker from before the cutoff goes"
        );

        let ids = active_session_ids();
        assert!(!ids.contains(&"session_mine_old".to_string()));
        assert!(
            ids.contains(&"session_mine_new".to_string()),
            "a marker written after the cutoff belongs to a session this image is serving"
        );
        assert!(
            ids.contains(&"session_theirs_old".to_string()),
            "another process keeps its markers however old they are"
        );

        // Idempotent: a second pass has nothing left that predates the cutoff.
        assert_eq!(prune_active_pids_owned_by_before(me, cutoff), (0, 0));

        horon_agent_core::env::remove_var("HORON_HOME");
    }
}
