use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const TEMP_SERVER_ENV: &str = "HORON_AGENT_TEMP_SERVER";
const SERVER_SCOPE_ENV: &str = "HORON_AGENT_SERVER_SCOPE";
const OWNER_PID_ENV: &str = "HORON_AGENT_SERVER_OWNER_PID";
const TEMP_IDLE_SECS_ENV: &str = "HORON_AGENT_TEMP_SERVER_IDLE_SECS";
const DEFAULT_TEMP_IDLE_SECS: u64 = 30 * 60;
const TEMP_SERVER_EXIT_CODE: i32 = super::EXIT_IDLE_TIMEOUT;
static TERMINATION_SHUTDOWN_STARTED: AtomicBool = AtomicBool::new(false);

/// Whether the idle monitor may start counting idle time.
///
/// A live headless worker keeps the server up even with no client attached: it
/// has work of its own and no surface to reconnect from.
pub(crate) fn idle_monitor_should_start(client_count: usize, has_live_headless_worker: bool) -> bool {
    client_count == 0 && !has_live_headless_worker
}

/// Wait (bounded) for an in-flight session save before the process exits.
/// Exiting between a checkpoint's snapshot write and journal delete used to
/// duplicate history on the next load (#1632).
pub(crate) async fn drain_session_saves_before_exit(signal_name: &str) {
    crate::logging::info(&format!("Server received {signal_name}, shutting down gracefully"));
    let drained = tokio::task::spawn_blocking(|| crate::session::drain_saves_for_shutdown(Duration::from_secs(5)))
        .await
        .unwrap_or(false);
    if !drained {
        crate::logging::warn("Shutdown: a session save was still running after 5s; exiting anyway");
    }
}

/// Remove the sockets, temporary metadata, build-hash sidecar, and daemon lock
/// owned by this server process.
///
/// `std::process::exit` skips `DaemonLockGuard::drop`, so every explicit exit
/// path cleans up its own sockets and sidecars while still holding the lock and
/// unlinks the lock file last.
pub(crate) fn cleanup_server_runtime_files(socket_path: &Path, debug_socket_path: &Path) {
    crate::transport::remove_socket(socket_path);
    crate::transport::remove_socket(debug_socket_path);
    cleanup_temporary_metadata(socket_path);
    let _ = std::fs::remove_file(format!("{}.hash", socket_path.display()));
    super::socket::cleanup_held_daemon_lock();
}

/// Watch for `SIGTERM` or `SIGINT` and run the graceful shutdown sequence once.
///
/// Cancels background compaction and drains in-flight session saves before
/// unregistering from `servers.json` and removing runtime sockets/locks so a
/// caller polling the socket sees the stop only after durable writes finish.
/// Where the platform has no POSIX termination signal (Windows), the watcher
/// stays pending.
pub(crate) fn spawn_termination_signal_handler(
    server_name: String,
    sessions: super::SessionAgents,
    socket_path: PathBuf,
    debug_socket_path: PathBuf,
) {
    tokio::spawn(async move {
        let signal_name = horon_agent_platform::signal::wait_for_termination().await;
        if TERMINATION_SHUTDOWN_STARTED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        // A job that is still summarizing must not race the save or apply after
        // the transcript is drained.
        for agent in sessions.read().await.values() {
            if let Ok(mut agent) = agent.try_lock() {
                agent.cancel_background_compaction(crate::compaction::jobs::CancelReason::Shutdown);
            }
        }
        drain_session_saves_before_exit(signal_name).await;
        if let Err(error) = crate::registry::unregister_server(&server_name).await {
            crate::logging::warn(&format!("Shutdown: failed to unregister server {server_name}: {error}"));
        }
        cleanup_server_runtime_files(&socket_path, &debug_socket_path);
        std::process::exit(0);
    });
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TemporaryServerPolicy {
    pub(crate) owner_pid: Option<u32>,
    pub(crate) idle_timeout_secs: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct TemporaryServerMetadata {
    schema_version: u32,
    scope: String,
    pid: u32,
    ppid: Option<u32>,
    owner_pid: Option<u32>,
    started_at: String,
    socket_path: String,
    debug_socket_path: String,
    idle_timeout_secs: u64,
    argv: Vec<String>,
}

pub fn configure_temporary_server(owner_pid: Option<u32>, idle_timeout_secs: Option<u64>) {
    crate::env::set_var(TEMP_SERVER_ENV, "1");
    crate::env::set_var(SERVER_SCOPE_ENV, "temporary");
    if let Some(owner_pid) = owner_pid {
        crate::env::set_var(OWNER_PID_ENV, owner_pid.to_string());
    }
    if let Some(idle_timeout_secs) = idle_timeout_secs {
        crate::env::set_var(TEMP_IDLE_SECS_ENV, idle_timeout_secs.to_string());
    }
}

pub(crate) fn temporary_server_policy_from_env() -> Option<TemporaryServerPolicy> {
    if !temporary_server_env_enabled() {
        return None;
    }

    let owner_pid = std::env::var(OWNER_PID_ENV)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|pid| *pid > 0);
    let idle_timeout_secs = std::env::var(TEMP_IDLE_SECS_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_TEMP_IDLE_SECS);

    Some(TemporaryServerPolicy {
        owner_pid,
        idle_timeout_secs,
    })
}

fn temporary_server_env_enabled() -> bool {
    env_truthy(TEMP_SERVER_ENV)
        || std::env::var(SERVER_SCOPE_ENV)
            .ok()
            .map(|value| value.eq_ignore_ascii_case("temporary"))
            .unwrap_or(false)
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

pub(crate) fn metadata_path(socket_path: &Path) -> PathBuf {
    let filename = socket_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("horon-agent.sock");
    socket_path.with_file_name(format!("{filename}.server.json"))
}

pub(crate) fn write_temporary_metadata(
    socket_path: &Path,
    debug_socket_path: &Path,
    policy: &TemporaryServerPolicy,
) -> Option<PathBuf> {
    let path = metadata_path(socket_path);
    let metadata = TemporaryServerMetadata {
        schema_version: 1,
        scope: "temporary".to_string(),
        pid: std::process::id(),
        ppid: parent_pid(),
        owner_pid: policy.owner_pid,
        started_at: horon_agent_time::Timestamp::now().to_rfc3339(),
        socket_path: socket_path.display().to_string(),
        debug_socket_path: debug_socket_path.display().to_string(),
        idle_timeout_secs: policy.idle_timeout_secs,
        argv: std::env::args().collect(),
    };

    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        crate::logging::warn(&format!(
            "Failed to create temporary server metadata directory {}: {}",
            parent.display(),
            error
        ));
        return None;
    }

    match serde_json::to_vec_pretty(&metadata)
        .ok()
        .and_then(|bytes| std::fs::write(&path, bytes).ok().map(|_| ()))
    {
        Some(()) => Some(path),
        None => {
            crate::logging::warn(&format!("Failed to write temporary server metadata {}", path.display()));
            None
        }
    }
}

pub(crate) fn cleanup_temporary_metadata(socket_path: &Path) {
    let _ = std::fs::remove_file(metadata_path(socket_path));
}

pub(crate) fn spawn_temporary_lifecycle_monitor(
    client_count: Arc<RwLock<usize>>,
    sessions: super::SessionAgents,
    swarm_state: super::SwarmState,
    socket_path: PathBuf,
    debug_socket_path: PathBuf,
    server_name: String,
    policy: TemporaryServerPolicy,
) {
    tokio::spawn(async move {
        let mut idle_since: Option<Instant> = None;
        let mut check_interval = tokio::time::interval(Duration::from_secs(10));

        loop {
            check_interval.tick().await;

            if let Some(owner_pid) = policy.owner_pid
                && owner_pid != std::process::id()
                && !process_alive(owner_pid)
            {
                crate::logging::info(&format!(
                    "Temporary server owner pid {} is gone. Shutting down.",
                    owner_pid
                ));
                shutdown_temporary_server(&server_name, &socket_path, &debug_socket_path).await;
            }

            let count = *client_count.read().await;
            let has_live_headless_worker = super::has_live_headless_worker(&sessions, &swarm_state).await;
            if idle_monitor_should_start(count, has_live_headless_worker) {
                if idle_since.is_none() {
                    idle_since = Some(Instant::now());
                    crate::logging::info(&format!(
                        "Temporary server has no clients. It will exit after {} seconds idle.",
                        policy.idle_timeout_secs
                    ));
                }

                if let Some(since) = idle_since
                    && since.elapsed().as_secs() >= policy.idle_timeout_secs
                {
                    crate::logging::info(&format!(
                        "Temporary server idle for {} seconds. Shutting down.",
                        since.elapsed().as_secs()
                    ));
                    shutdown_temporary_server(&server_name, &socket_path, &debug_socket_path).await;
                }
            } else {
                if idle_since.is_some() {
                    crate::logging::info("Temporary server client connected. Idle timer cancelled.");
                }
                idle_since = None;
            }
        }
    });
}

async fn shutdown_temporary_server(server_name: &str, socket_path: &Path, debug_socket_path: &Path) -> ! {
    let _ = crate::registry::unregister_server(server_name).await;
    cleanup_server_runtime_files(socket_path, debug_socket_path);
    std::process::exit(TEMP_SERVER_EXIT_CODE);
}

fn parent_pid() -> Option<u32> {
    horon_agent_platform::signal::parent_pid()
}

/// Whether the process that owns this temporary server is still running.
///
/// Pid 0 is never a real owner, and the adapter's liveness check already treats
/// a process this one may not signal as alive, so a same-user owner that we
/// spawned ourselves is reported correctly on every platform.
pub(crate) fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }

    horon_agent_platform::is_process_running(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        entries: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl EnvGuard {
        fn capture(names: &[&'static str]) -> Self {
            // Shared process-wide lock: env vars are global, so a private
            // mutex here would race every other env-mutating test (issue #593).
            let _lock = crate::storage::lock_test_env();
            let entries = names.iter().map(|name| (*name, std::env::var_os(name))).collect();
            Self { _lock, entries }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, value) in &self.entries {
                if let Some(value) = value {
                    crate::env::set_var(name, value);
                } else {
                    crate::env::remove_var(name);
                }
            }
        }
    }

    #[test]
    fn temporary_policy_requires_explicit_marker() {
        let _guard = EnvGuard::capture(&[TEMP_SERVER_ENV, SERVER_SCOPE_ENV, OWNER_PID_ENV]);
        crate::env::remove_var(TEMP_SERVER_ENV);
        crate::env::remove_var(SERVER_SCOPE_ENV);
        crate::env::set_var(OWNER_PID_ENV, "123");

        assert_eq!(temporary_server_policy_from_env(), None);
    }

    #[test]
    fn temporary_policy_reads_owner_and_timeout() {
        let _guard = EnvGuard::capture(&[TEMP_SERVER_ENV, SERVER_SCOPE_ENV, OWNER_PID_ENV, TEMP_IDLE_SECS_ENV]);
        crate::env::set_var(SERVER_SCOPE_ENV, "temporary");
        crate::env::set_var(OWNER_PID_ENV, "123");
        crate::env::set_var(TEMP_IDLE_SECS_ENV, "42");

        assert_eq!(
            temporary_server_policy_from_env(),
            Some(TemporaryServerPolicy {
                owner_pid: Some(123),
                idle_timeout_secs: 42,
            })
        );
    }

    #[test]
    fn temporary_metadata_path_is_socket_scoped() {
        assert_eq!(
            metadata_path(Path::new("/tmp/example/horon-agent.sock")),
            PathBuf::from("/tmp/example/horon-agent.sock.server.json")
        );
    }

    /// Exercises the `is_process_running` delegation on this platform, and the
    /// pid-0 screen in front of it: on Unix `kill(0, 0)` signals the caller's
    /// whole process group and reports "alive", which is why `process_alive`
    /// screens pid 0 out rather than trusting the platform helper.
    #[test]
    fn current_process_is_alive() {
        assert!(process_alive(std::process::id()));
        assert!(!process_alive(0));
    }

    #[test]
    fn cleanup_server_runtime_files_removes_sockets_metadata_and_hash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("horon-agent.sock");
        let debug_socket = dir.path().join("horon-agent-debug.sock");
        let metadata = metadata_path(&socket);
        let hash = PathBuf::from(format!("{}.hash", socket.display()));
        let unrelated = dir.path().join("keep.txt");

        for path in [&socket, &debug_socket, &metadata, &hash, &unrelated] {
            std::fs::write(path, "placeholder").expect("write fixture file");
        }

        cleanup_server_runtime_files(&socket, &debug_socket);

        assert!(!socket.exists(), "main socket should be removed");
        assert!(!debug_socket.exists(), "debug socket should be removed");
        assert!(!metadata.exists(), "temporary server metadata should be removed");
        assert!(!hash.exists(), "build-hash sidecar should be removed");
        assert!(unrelated.exists(), "unrelated files must stay untouched");
    }
}
