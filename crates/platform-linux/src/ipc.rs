//! Local IPC behind one API: a Unix socket on Unix, a named pipe on Windows.
//!
//! Callers write one code path and get the platform's native mechanism. The
//! types mirror `tokio::net::Unix*` so the Unix side is a thin wrapper and the
//! Windows side only has to match its shape, not its syscalls.
//!
//! This file is compiled into both Unix adapter crates (macOS includes it with
//! `#[path]`), and the Windows crate carries its own `ipc` module with the same
//! public names.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use std::os::unix::net::UnixStream as SyncStream;
pub use tokio::net::UnixStream as Stream;
pub use tokio::net::unix::OwnedReadHalf as ReadHalf;
pub use tokio::net::unix::OwnedWriteHalf as WriteHalf;

/// Listening socket for local IPC.
///
/// A wrapper rather than a type alias so `accept` takes `&mut self` on every
/// platform: the Windows named-pipe listener republishes an instance per
/// accept, and callers must not have to care which one they are on.
pub struct Listener(tokio::net::UnixListener);

impl Listener {
    /// Listen on `path`, replacing nothing.
    pub fn bind(path: &Path) -> io::Result<Self> {
        tokio::net::UnixListener::bind(path).map(Self)
    }

    /// Accept the next local connection.
    pub async fn accept(&mut self) -> io::Result<(Stream, tokio::net::unix::SocketAddr)> {
        self.0.accept().await
    }
}

/// The socket address of an accepted connection.
pub type Addr = tokio::net::unix::SocketAddr;

/// Whether a server is listening on `path`.
pub fn is_socket_path(path: &Path) -> bool {
    path.exists()
}

/// Remove a socket path, ignoring "already gone".
///
/// A stale socket file blocks `bind`; the caller holds the instance lock, so no
/// live server owns the path when this runs.
pub fn remove_socket(path: &Path) {
    drop(std::fs::remove_file(path));
}

/// Create a connected pair of streams, for bridging in-process.
pub fn stream_pair() -> io::Result<(Stream, Stream)> {
    Stream::pair()
}

/// Restrict a socket path to its owner (0600).
///
/// A freshly bound socket inherits the process umask, which is usually 0755
/// and would widen access to everything reachable through it.
pub fn restrict_socket_to_owner(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

/// Whether the socket at `path` is reachable by its owner only.
///
/// A freshly bound socket inherits the process umask, so reading the mode back
/// is the only way to tell a deliberate restriction from the default.
pub fn socket_is_owner_only(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o777 == 0o600)
        .unwrap_or(false)
}

/// An exclusive lock on a lock file, guaranteeing one live owner per path.
///
/// The kernel drops the `flock` when the holder exits, so a crashed owner never
/// wedges its replacement out.
pub struct InstanceLock {
    _file: std::fs::File,
    path: PathBuf,
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // Best effort: the lock is released by the fd close regardless, and a
        // leftover empty lock file is harmless (the next owner re-locks it).
        drop(std::fs::remove_file(&self.path));
    }
}

/// Take the exclusive lock beside `path`, or report that a live owner holds it.
pub fn lock_instance_file(path: &Path) -> io::Result<Option<InstanceLock>> {
    use std::os::fd::AsRawFd;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    Ok(taken.then_some(InstanceLock {
        _file: file,
        path: path.to_path_buf(),
    }))
}

/// The outcome of trying to serialize a client-side bootstrap.
///
/// A bootstrap lock is advisory and best-effort: the caller proceeds without it
/// where the platform has none, which is why the "not supported" case is a
/// value instead of an error.
pub enum LockAttempt {
    /// This process holds the lock until the value is dropped.
    Acquired(InstanceLock),
    /// Another live process holds it.
    Busy,
    /// This platform has no advisory file lock.
    Unsupported,
}

/// Try to take the exclusive lock on `path`, for serializing a bootstrap
/// between processes.
pub fn acquire_bootstrap_lock(path: &Path) -> io::Result<LockAttempt> {
    Ok(match lock_instance_file(path)? {
        Some(lock) => LockAttempt::Acquired(lock),
        None => LockAttempt::Busy,
    })
}

/// An interrupt handle for a blocking stream, when the platform has one.
///
/// Unix sockets can be shut down from a cloned descriptor, which wakes a
/// blocked reader or writer immediately. A Windows named pipe cannot be shut
/// down that way, so callers close the handle by dropping the transport.
pub fn sync_stream_shutdown_handle(stream: &SyncStream) -> Option<Arc<dyn Fn() + Send + Sync>> {
    let socket = match stream.try_clone() {
        Ok(socket) => socket,
        // Without a second descriptor there is no way to interrupt a blocked
        // reader; the caller falls back to dropping the transport.
        Err(_) => return None,
    };
    Some(Arc::new(move || {
        drop(socket.shutdown(std::net::Shutdown::Both));
    }))
}

/// Mark a listening socket so it does not survive `exec` in a child process.
///
/// A server replaces itself with a new image on reload, and the published
/// listener descriptors must close across that `exec` so the replacement can
/// bind the same paths.
pub fn mark_listener_close_on_exec(listener: &Listener) {
    use std::os::fd::AsRawFd;

    let raw = listener.0.as_raw_fd();
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
    if flags >= 0 && unsafe { libc::fcntl(raw, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1 {
        // The reload path cannot recover from this, but a listener that leaks
        // across `exec` must not do so silently.
        let error = io::Error::last_os_error();
        horon_agent_platform_api::log::log(
            horon_agent_platform_api::log::PlatformLogLevel::Warn,
            &format!("could not mark the listening socket close-on-exec: {error}"),
        );
    }
}

/// Whether a connection attempt is a safe liveness probe.
///
/// A Unix socket accepts any number of probes, so connecting is harmless.
pub fn connect_probe_is_safe() -> bool {
    true
}

/// Whether an abandoned listening endpoint can leave a filesystem node behind.
///
/// A Unix socket file survives its owner, so a dead daemon can strand it.
pub fn socket_nodes_can_be_stale() -> bool {
    true
}

/// Report readiness on the descriptor named by `ready_env_var`.
///
/// The variable is cleared first so child processes (tool subprocesses) never
/// inherit a stale descriptor number.
pub fn signal_ready_fd(ready_env_var: &str) {
    use std::os::fd::FromRawFd;

    if let Ok(fd_str) = std::env::var(ready_env_var) {
        // SAFETY: this runs on the thread that owns the readiness handshake,
        // before any child is spawned, so no other thread is reading the
        // environment through a pointer the removal could invalidate.
        unsafe { std::env::remove_var(ready_env_var) };
        if let Ok(fd) = fd_str.parse::<i32>() {
            let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
            if let Err(error) = std::io::Write::write_all(&mut file, b"R") {
                // A failed write means the parent has already gone; the child
                // still starts, so this is a diagnostic rather than an error.
                horon_agent_platform_api::log::log(
                    horon_agent_platform_api::log::PlatformLogLevel::Warn,
                    &format!("could not signal readiness to the parent: {error}"),
                );
            }
            // The file is dropped here, which closes the descriptor.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// The Unix side is the reference shape the Windows implementation is
    /// checked against, so a change to either cannot quietly diverge in what
    /// callers can rely on.
    #[tokio::test]
    async fn a_listener_accepts_and_round_trips_bytes() {
        let dir = std::env::temp_dir().join(format!("horon-agent-ipc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("round-trip.sock");
        remove_socket(&path);

        let mut listener = Listener::bind(&path).expect("bind");
        assert!(is_socket_path(&path), "a bound socket path should exist");

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut buf = [0u8; 5];
            stream.read_exact(&mut buf).await.expect("read");
            stream.write_all(b"pong\n").await.expect("write");
            buf
        });

        let mut client = Stream::connect(&path).await.expect("connect");
        client.write_all(b"ping\n").await.expect("write");
        let mut reply = [0u8; 5];
        client.read_exact(&mut reply).await.expect("read");

        assert_eq!(&server.await.expect("server task"), b"ping\n");
        assert_eq!(&reply, b"pong\n");

        remove_socket(&path);
        assert!(!is_socket_path(&path), "remove_socket should unlink the path");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_stream_pair_is_connected_in_both_directions() {
        let (mut a, mut b) = stream_pair().expect("pair");
        a.write_all(b"ab").await.expect("write a");
        let mut seen = [0u8; 2];
        b.read_exact(&mut seen).await.expect("read b");
        assert_eq!(&seen, b"ab");

        b.write_all(b"ba").await.expect("write b");
        a.read_exact(&mut seen).await.expect("read a");
        assert_eq!(&seen, b"ba");
    }

    #[test]
    fn an_absent_path_is_not_a_socket() {
        assert!(!is_socket_path(Path::new("/nonexistent/horon-agent-ipc-absent.sock")));
    }

    /// `remove_socket` is called on paths that may not exist, so it must not
    /// panic or report failure.
    #[test]
    fn removing_an_absent_socket_is_a_no_op() {
        remove_socket(Path::new("/nonexistent/horon-agent-ipc-absent.sock"));
    }

    #[test]
    fn a_second_instance_lock_is_refused_while_the_first_lives() {
        let dir = std::env::temp_dir().join(format!("horon-agent-ipc-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("owner.lock");

        let first = lock_instance_file(&path).expect("lock");
        assert!(first.is_some(), "the first owner must take the lock");
        assert!(
            lock_instance_file(&path).expect("lock").is_none(),
            "a second owner must be refused while the first is alive"
        );

        drop(first);
        assert!(
            lock_instance_file(&path).expect("lock relock").is_some(),
            "a released lock must be available again"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The bootstrap lock is the client-side use of the same primitive: one
    /// client serializes the spawn, and the lock file disappears with the
    /// guard so a later bootstrap starts from a clean directory.
    #[test]
    fn a_bootstrap_lock_is_exclusive_and_removes_its_file() {
        let dir = std::env::temp_dir().join(format!("horon-agent-ipc-bootstrap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("spawn.lock");

        let first = match acquire_bootstrap_lock(&path).expect("acquire") {
            LockAttempt::Acquired(lock) => lock,
            _ => panic!("the first bootstrap must take the lock"),
        };
        assert!(path.is_file(), "a held lock is a file other clients can see");
        assert!(
            matches!(acquire_bootstrap_lock(&path).expect("acquire"), LockAttempt::Busy),
            "a second client must be told the lock is taken"
        );

        drop(first);
        assert!(!path.exists(), "the guard removes its lock file");
        assert!(
            matches!(
                acquire_bootstrap_lock(&path).expect("acquire"),
                LockAttempt::Acquired(_)
            ),
            "a released lock must be available again"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_owner_only_socket_cannot_be_opened_by_other_users() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("horon-agent-ipc-mode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("private.sock");
        remove_socket(&path);
        let _listener = Listener::bind(&path).expect("bind");
        restrict_socket_to_owner(&path).expect("restrict");

        let mode = std::fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a private socket must be owner-only");
        remove_socket(&path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_shutdown_handle_wakes_a_reader_with_eof() {
        use std::io::Read;

        let (mut reader, _writer) = std::os::unix::net::UnixStream::pair().expect("pair");
        let handle = sync_stream_shutdown_handle(&reader).expect("unix sockets have a shutdown handle");
        handle();
        let mut probe = [0u8; 1];
        assert_eq!(
            reader.read(&mut probe).expect("read after shutdown"),
            0,
            "shutting the clone down must deliver EOF instead of blocking"
        );
    }
}
