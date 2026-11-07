use super::Client;
use crate::transport::Stream;
use anyhow::Result;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub fn socket_path() -> PathBuf {
    if let Ok(custom) = std::env::var("HORON_AGENT_SOCKET") {
        return PathBuf::from(custom);
    }
    default_socket_path()
}

/// The shared-daemon socket for this runtime dir, ignoring any
/// `--socket`/`HORON_AGENT_SOCKET` override.
pub fn default_socket_path() -> PathBuf {
    crate::storage::runtime_dir().join("horon-agent.sock")
}

/// Resolve a socket path to a comparable identity: absolute, with its parent
/// directory's symlinks collapsed.
///
/// `HORON_AGENT_SOCKET` carries a user-supplied *spelling*, and several spellings
/// name the same socket: `horon-agent.sock` relative to the runtime dir, a path
/// through a symlinked parent, a `..` segment. Comparing spellings would call
/// those "custom" and hand the shared socket to a process-scoped server that
/// then takes it down with the run, disconnecting every other client.
///
/// Only the *parent* is resolved; the final component is kept as written, even
/// when it currently exists as a symlink to another socket. Binding always
/// unlinks and recreates that last component (see `Server::run`), so the
/// socket a server ends up owning is the one named here: not whatever the
/// name happened to point at beforehand. Following the final link would pin
/// the daemon lock to the old target and then publish a different socket,
/// locking a path nobody is listening on (review of #1768, finding 2).
///
/// Falls back to the plain absolute path when the parent is absent too: two
/// absent paths still compare consistently with each other.
fn resolved_socket_identity(path: &std::path::Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };

    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(file_name)) => match std::fs::canonicalize(parent) {
            Ok(canonical_parent) => canonical_parent.join(file_name),
            Err(_) => absolute,
        },
        _ => absolute,
    }
}

/// True when `path` is the shared daemon socket for this runtime dir, however
/// it happens to be spelled.
pub fn is_shared_socket(path: &std::path::Path) -> bool {
    resolved_socket_identity(path) == resolved_socket_identity(&default_socket_path())
}

/// True when `--socket`/`HORON_AGENT_SOCKET` points somewhere other than the shared
/// daemon socket for this runtime dir.
///
/// A custom socket means "this process wants its own server", which is what
/// lets an isolated server be started for it without disturbing the shared
/// daemon (issue #1748).
pub fn socket_path_is_custom() -> bool {
    !is_shared_socket(&socket_path())
}

/// Debug socket path for testing/introspection
/// Derived from main socket path
pub fn debug_socket_path() -> PathBuf {
    let main_path = socket_path();
    let filename = main_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("horon-agent.sock");
    let debug_filename = filename.replace(".sock", "-debug.sock");
    main_path.with_file_name(debug_filename)
}

pub(super) fn sibling_socket_path(path: &std::path::Path) -> Option<PathBuf> {
    let filename = path.file_name()?.to_str()?;

    if let Some(base) = filename.strip_suffix("-debug.sock") {
        return Some(path.with_file_name(format!("{}.sock", base)));
    }

    if let Some(base) = filename.strip_suffix(".sock") {
        return Some(path.with_file_name(format!("{}-debug.sock", base)));
    }

    None
}

/// Remove a socket file and its sibling (main/debug) if present.
pub fn cleanup_socket_pair(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    if let Some(sibling) = sibling_socket_path(path) {
        let _ = std::fs::remove_file(sibling);
    }
}

/// Connect to a socket path.
///
/// Do not unlink the path on connection-refused here. A client-side cleanup can
/// strand a live daemon behind an unlinked Unix socket pathname, leaving the
/// process running with the daemon lock held while new clients can no longer
/// discover or connect to it.
pub async fn connect_socket(path: &std::path::Path) -> Result<Stream> {
    match Stream::connect(path).await {
        Ok(stream) => Ok(stream),
        Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused && path.exists() => {
            Err(anyhow::Error::new(err).context(format!(
                "Socket exists but refused the connection at {}. Retry, or remove it after confirming no horon-agent server is running.",
                path.display()
            )))
        }
        Err(err) if horon_agent_platform::process::is_too_many_open_files(&err) => Err(anyhow::anyhow!(
            "{} ({})",
            err,
            horon_agent_platform::process::fd_diagnostic_snapshot()
        )),
        Err(err) => Err(err.into()),
    }
}

pub(super) async fn socket_has_live_listener(path: &std::path::Path) -> bool {
    if !crate::transport::is_socket_path(path) {
        return false;
    }
    // Where a connection attempt can occupy the only published listener (a
    // Windows named pipe), the presence probe is the whole answer: a second
    // connect would wait inside the platform's busy-retry loop.
    if !horon_agent_platform::ipc::connect_probe_is_safe() {
        return true;
    }
    Stream::connect(path).await.is_ok()
}

/// Reap a provably-stale socket left behind by a dead daemon.
///
/// Background: after an upgrade or crash, the runtime socket file can survive
/// even though the daemon that owned it is gone. A client that finds the socket
/// path present but cannot connect/handshake gets wedged into a connect-retry
/// loop and never recovers on its own (see issues #277 and #291).
///
/// Safety: a live daemon holds an exclusive `flock` on this socket's daemon
/// lock ([`daemon_lock_path_for`]) for its entire lifetime. So if (a) the socket
/// has no live listener AND (b) we can acquire that exclusive lock, then no
/// daemon is running and the socket is provably stale. Only then do we unlink
/// the socket pair (main + debug) and the lock file. This can never strand a
/// live daemon, because a live daemon would either still be answering on the
/// socket or still be holding the lock.
///
/// Returns true if a stale socket was reaped.
pub async fn reap_stale_socket_if_dead(path: &std::path::Path) -> bool {
    // A platform whose listening endpoints vanish with their owner never leaves
    // a node to reap, and probing one would only consume a live instance.
    if !horon_agent_platform::ipc::socket_nodes_can_be_stale() {
        return false;
    }

    // Nothing to reap if the path isn't even present.
    if !crate::transport::is_socket_path(path) {
        return false;
    }

    // If a listener answers, the daemon is alive; never touch it.
    if socket_has_live_listener(path).await {
        return false;
    }

    // Try to grab this socket's daemon lock. If a live daemon holds it, this
    // fails and we leave everything alone (the daemon may just be slow to
    // answer).
    let lock_path = daemon_lock_path_for(path);
    let Ok(Some(_lock)) = try_acquire_daemon_lock(&lock_path) else {
        return false;
    };

    // Re-check the listener now that we hold the lock, to close the race where a
    // daemon bound the socket between our probe and acquiring the lock. (A real
    // daemon could not have given us the lock, but a brand-new daemon spawned in
    // the gap might be mid-startup before it acquired the lock itself.)
    if socket_has_live_listener(path).await {
        return false;
    }

    crate::logging::warn(&format!(
        "Reaping stale horon-agent socket with no live listener at {}",
        path.display()
    ));
    cleanup_socket_pair(path);
    // `_lock` (a DaemonLockGuard) removes the lock file when it drops at the end
    // of this scope, so the leftover lock is cleaned up too.
    true
}

/// Return true if a live server process is listening on the socket path.
///
/// This is intentionally weaker than [`is_server_ready`]: a live listener may
/// still be finishing startup or be temporarily too busy to answer a ping
/// within the short readiness timeout. Callers that must avoid spawning a
/// duplicate daemon should prefer this check over a ping-only probe.
pub async fn has_live_listener(path: &std::path::Path) -> bool {
    socket_has_live_listener(path).await
}

/// Lock path that enforces "one daemon per socket".
///
/// The shared socket keeps the historical runtime-dir-scoped name, so a daemon
/// from an older build that only knows that name is still respected across an
/// upgrade. A custom `--socket` path gets its own sibling lock, so an isolated
/// server can bind it while the shared daemon keeps running: a runtime-wide
/// lock made the second server impossible, which is why `run --socket X` could
/// never get a listener on `X` (issue #1748). The spawn lock
/// (`cli::dispatch::spawn_lock_path`) has always been socket-scoped; this
/// aligns the daemon lock with it.
pub(super) fn daemon_lock_path_for(socket: &std::path::Path) -> PathBuf {
    if is_shared_socket(socket) {
        return crate::storage::runtime_dir().join("horon-agent-daemon.lock");
    }

    // Derive the lock from the resolved identity, not the spelling: two
    // spellings of one custom socket must contend for the same lock, or two
    // daemons could bind the same socket.
    let resolved = resolved_socket_identity(socket);
    let filename = resolved
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("horon-agent.sock");
    resolved.with_file_name(format!("{filename}.daemon.lock"))
}

/// The daemon lock this process actually acquired.
///
/// Recorded at acquisition rather than recomputed at exit: the lock path is
/// derived from the socket, and the filesystem under that socket can change
/// while the server runs. Recomputing could name a different file and leave
/// the real lock behind (review of #1768, finding 2).
static HELD_DAEMON_LOCK: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Remove the daemon lock this process holds, on a path that exits without
/// unwinding and so skips [`DaemonLockGuard`]'s `Drop`.
///
/// Only the holder may do this: unlinking a lock another process holds would
/// let a third process lock a fresh inode and start a second daemon. `flock`
/// is released by process exit either way, so a file left behind is litter
/// rather than a stuck lock: but a per-run socket would leave one per run.
pub(super) fn cleanup_held_daemon_lock() {
    if let Some(path) = HELD_DAEMON_LOCK.get() {
        let _ = std::fs::remove_file(path);
    }
}

/// The daemon lock, held through the platform's exclusive instance lock.
///
/// The lock file is unlinked when the guard drops, and the kernel releases the
/// lock when the process exits, so a crashed daemon never wedges its
/// replacement out. Where the platform keeps no lock file (its listening
/// endpoint is a single-owner named pipe), the guard holds nothing and the pipe
/// keeps the single-owner rule.
pub(super) struct DaemonLockGuard {
    _lock: Option<horon_agent_platform::ipc::InstanceLock>,
}

pub(super) fn try_acquire_daemon_lock(path: &std::path::Path) -> Result<Option<DaemonLockGuard>> {
    if !horon_agent_platform::ipc::daemon_lock_file_is_used() {
        return Ok(None);
    }
    Ok(horon_agent_platform::ipc::lock_instance_file(path)?.map(|lock| DaemonLockGuard { _lock: Some(lock) }))
}

/// Take this socket's daemon lock, or report the live daemon that holds it.
pub(super) fn acquire_daemon_lock_for(socket: &std::path::Path) -> Result<DaemonLockGuard> {
    if !horon_agent_platform::ipc::daemon_lock_file_is_used() {
        return Ok(DaemonLockGuard { _lock: None });
    }
    let path = daemon_lock_path_for(socket);
    let guard = try_acquire_daemon_lock(&path)?.ok_or_else(|| {
        anyhow::anyhow!(
            "Another horon-agent server process is already running on socket {}",
            socket.display()
        )
    })?;
    let _ = HELD_DAEMON_LOCK.set(path);
    Ok(guard)
}

pub fn set_socket_path(path: &str) {
    crate::env::set_var("HORON_AGENT_SOCKET", path);
}

/// Environment variable through which a spawned server learns the descriptor it
/// reports readiness on.
const READY_FD_ENV: &str = "HORON_AGENT_READY_FD";

/// Longest the parent waits for the readiness byte before falling back to
/// polling the socket itself.
const READY_SIGNAL_TIMEOUT: Duration = Duration::from_secs(10);

/// Longest the parent polls for a listening socket when the platform cannot
/// pass the child a readiness descriptor.
///
/// A server bootstrap can legitimately take tens of seconds on slow hosts (auth
/// preflights and provider init were observed at 15-60s on a Windows Server
/// VPS, issue #503). The child's liveness is checked every poll, so a generous
/// budget only delays the error for a genuinely hung server, while a crashed
/// server still fails fast with its stderr.
const READY_POLL_TIMEOUT: Duration = Duration::from_secs(120);
const READY_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Spawn a server child process and wait until it signals readiness.
///
/// Where the platform can pass a descriptor to the child, the parent hands over
/// a pipe write end as `HORON_AGENT_READY_FD` and awaits a single byte on the
/// read end. The server calls `signal_ready_fd()` once its accept loops are
/// spawned, so the future resolves only after the daemon can start servicing
/// client requests.
///
/// Falls back to polling the socket when the pipe read times out (a server
/// built without ready-fd support, or one that crashed before bind), and uses
/// the poll path outright on a platform that cannot pass the descriptor.
pub async fn spawn_server_notify(cmd: &mut std::process::Command) -> Result<std::process::Child> {
    let mut spawned = horon_agent_platform::spawn_with_ready_pipe(cmd, READY_FD_ENV)?;
    match spawned.ready.take() {
        Some(ready) => await_ready_descriptor(&mut spawned.child, ready).await?,
        None => await_listening_socket(&mut spawned.child).await?,
    }

    // The shared daemon outlives the spawning client. Keep draining the stderr
    // pipe after startup so later reloads cannot die on SIGPIPE when they emit
    // provider/model selection notices during boot.
    if let Some(mut stderr) = spawned.child.stderr.take() {
        std::thread::spawn(move || {
            let mut sink = std::io::sink();
            let _ = std::io::copy(&mut stderr, &mut sink);
        });
    }

    Ok(spawned.child)
}

/// Wait for the readiness byte on `ready`, falling back to polling the socket.
async fn await_ready_descriptor(child: &mut std::process::Child, ready: std::fs::File) -> Result<()> {
    let mut async_file = tokio::fs::File::from_std(ready);
    let mut buf = [0u8; 1];
    match tokio::time::timeout(
        READY_SIGNAL_TIMEOUT,
        tokio::io::AsyncReadExt::read(&mut async_file, &mut buf),
    )
    .await
    {
        Ok(Ok(1)) => {
            crate::logging::info("Server signalled ready via pipe");
        }
        Ok(Ok(_)) => {
            if let Some(status) = child.try_wait()? {
                handle_server_start_exit(child, status).await?;
            }
            crate::logging::info("Server closed ready pipe without signalling; falling back to poll");
            wait_for_server_ready(&socket_path(), Duration::from_secs(5)).await?;
        }
        Ok(Err(e)) => {
            crate::logging::info(&format!("Ready pipe read error: {}; falling back to poll", e));
            wait_for_server_ready(&socket_path(), Duration::from_secs(5)).await?;
        }
        Err(_) => {
            crate::logging::info("Timed out waiting for server ready signal; falling back to poll");
            wait_for_server_ready(&socket_path(), Duration::from_secs(5)).await?;
        }
    }
    Ok(())
}

/// Wait until the socket has a live listener, failing fast when the child exits.
async fn await_listening_socket(child: &mut std::process::Child) -> Result<()> {
    let start = Instant::now();
    while start.elapsed() < READY_POLL_TIMEOUT {
        if has_live_listener(&socket_path()).await {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            let stderr_output = take_server_start_stderr(child);
            anyhow::bail!(format_server_start_error(status, &stderr_output));
        }
        tokio::time::sleep(READY_POLL_INTERVAL).await;
    }
    anyhow::bail!(
        "Timed out waiting for server to become ready at {} after {}ms",
        socket_path().display(),
        READY_POLL_TIMEOUT.as_millis()
    )
}

/// Wait until a server socket is connectable and responds to a ping.
pub async fn wait_for_server_ready(path: &std::path::Path, timeout: Duration) -> Result<()> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(Ok(mut client)) = tokio::time::timeout(
            Duration::from_millis(250),
            Client::connect_with_path(path.to_path_buf()),
        )
        .await
            && let Ok(Ok(true)) = tokio::time::timeout(Duration::from_millis(250), client.ping()).await
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("Timed out waiting for responsive server socket {}", path.display());
}

async fn probe_server_ready(path: &std::path::Path, ping_timeout: Duration) -> bool {
    let Ok(Ok(mut client)) = tokio::time::timeout(ping_timeout, Client::connect_with_path(path.to_path_buf())).await
    else {
        return false;
    };

    matches!(tokio::time::timeout(ping_timeout, client.ping()).await, Ok(Ok(true)))
}

pub async fn is_server_ready(path: &std::path::Path) -> bool {
    probe_server_ready(path, Duration::from_millis(50)).await
}

pub(super) fn take_server_start_stderr(child: &mut std::process::Child) -> String {
    use std::io::Read;

    child
        .stderr
        .take()
        .and_then(|mut stderr| {
            let mut buf = String::new();
            stderr.read_to_string(&mut buf).ok()?;
            Some(buf)
        })
        .unwrap_or_default()
}

pub(super) fn server_start_matches_existing_server(stderr_output: &str) -> bool {
    stderr_output.contains("Another horon-agent server process is already running")
        || stderr_output.contains("Refusing to replace active server socket")
}

pub(super) async fn wait_for_existing_server(path: &std::path::Path, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if is_server_ready(path).await || has_live_listener(path).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

pub(super) fn format_server_start_error(status: std::process::ExitStatus, stderr_output: &str) -> String {
    if stderr_output.trim().is_empty() {
        format!(
            "Server exited before signalling ready ({}). Check logs at ~/.horon-agent/logs/",
            status
        )
    } else {
        format!(
            "Server exited before signalling ready ({}):\n{}",
            status,
            stderr_output.trim()
        )
    }
}

pub(super) async fn handle_server_start_exit(
    child: &mut std::process::Child,
    status: std::process::ExitStatus,
) -> Result<()> {
    let stderr_output = take_server_start_stderr(child);
    if server_start_matches_existing_server(&stderr_output) {
        let socket_path = socket_path();
        if wait_for_existing_server(&socket_path, Duration::from_secs(5)).await {
            crate::logging::info("Server spawn raced with an existing daemon; treating startup as successful");
            return Ok(());
        }
    }

    anyhow::bail!(format_server_start_error(status, &stderr_output));
}

/// Report that the server can accept requests.
///
/// Where the platform passes a readiness descriptor, this writes one byte to
/// the descriptor named by `HORON_AGENT_READY_FD` and closes it; the variable is
/// cleared first so child processes (tool subprocesses) never inherit a stale
/// descriptor number. A platform without that handshake reports nothing, and
/// its parent polls the socket instead.
pub(super) fn signal_ready_fd() {
    horon_agent_platform::ipc::signal_ready_fd(READY_FD_ENV);
}
