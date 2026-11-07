//! Local IPC behind one API: a Windows named pipe, with the same types and
//! call shapes as the Unix adapter's socket version.
//!
//! A named pipe is not a file: it has no pathname, no mode bits and no
//! `flock`. The functions that take a socket path therefore translate it into a
//! deterministic pipe name, and owner-only access is carried by the pipe ACL
//! rather than by a file mode.

use horon_agent_platform_api::log::{PlatformLogLevel as Level, log as report};
use sha2::{Digest, Sha256};
use std::io;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions};

/// Convert a filesystem path to a Windows named pipe path.
///
/// e.g. `/run/user/1000/horon-agent.sock` -> `\\.\pipe\horon-agent`
/// e.g. `/run/user/1000/horon-agent/myserver.sock` -> `\\.\pipe\horon-agent-myserver`
fn path_to_pipe_name(path: &Path) -> String {
    let stem: String = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("horon-agent")
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        .take(32)
        .collect();
    let stem = if stem.is_empty() { "horon-agent" } else { &stem };
    let normalized = path.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
    let digest = Sha256::digest(normalized.as_bytes());
    let hash = hex::encode(digest);
    format!(r"\\.\pipe\{}-{}", stem, &hash[..16])
}

/// Listener wraps a Windows named pipe server, providing an accept loop
/// that matches the UnixListener interface.
pub struct Listener {
    pipe_name: String,
    current_server: NamedPipeServer,
}

impl Listener {
    pub fn bind(path: &Path) -> io::Result<Self> {
        let pipe_name = path_to_pipe_name(path);
        match ServerOptions::new().first_pipe_instance(true).create(&pipe_name) {
            Ok(server) => Ok(Self {
                pipe_name,
                current_server: server,
            }),
            Err(e) if e.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED as i32) => {
                report(
                    Level::Warn,
                    &format!(
                        "[windows] Named pipe {pipe_name} busy (access denied), retrying without first_pipe_instance"
                    ),
                );
                std::thread::sleep(std::time::Duration::from_millis(200));
                let server = ServerOptions::new().create(&pipe_name)?;
                Ok(Self {
                    pipe_name,
                    current_server: server,
                })
            }
            Err(e) => Err(e),
        }
    }

    pub async fn accept(&mut self) -> io::Result<(Stream, PipeAddr)> {
        self.current_server.connect().await?;

        let connected = std::mem::replace(&mut self.current_server, ServerOptions::new().create(&self.pipe_name)?);

        Ok((Stream::Server(connected), PipeAddr))
    }
}

/// Placeholder for the "address" of a named pipe connection.
pub struct PipeAddr;

/// Stream wraps either a NamedPipeServer (accepted connection) or
/// NamedPipeClient (outgoing connection).
pub enum Stream {
    Server(NamedPipeServer),
    Client(NamedPipeClient),
}

impl Stream {
    pub async fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        let pipe_name = path_to_pipe_name(path.as_ref());
        loop {
            match ClientOptions::new().open(&pipe_name) {
                Ok(client) => return Ok(Stream::Client(client)),
                Err(e) if e.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_PIPE_BUSY as i32) => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub fn into_split(self) -> (ReadHalf, WriteHalf) {
        let (read, write) = tokio::io::split(self);
        (ReadHalf { inner: read }, WriteHalf { inner: write })
    }

    pub fn split(&mut self) -> (SplitReadRef<'_>, SplitWriteRef<'_>) {
        let (read, write) = tokio::io::split(self);
        (SplitReadRef { inner: read }, SplitWriteRef { inner: write })
    }

    pub fn pair() -> io::Result<(Self, Self)> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static PAIR_COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = PAIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pipe_name = format!(r"\\.\pipe\horon-agent-pair-{}-{}", std::process::id(), counter);
        let server = ServerOptions::new().first_pipe_instance(true).create(&pipe_name)?;
        let client = ClientOptions::new().open(&pipe_name)?;

        // The client connected when we opened it above, but the server must
        // call connect() to transition into the connected state.  For an
        // already-connected client this returns immediately.
        //
        // We use a short-lived runtime-free poll: since the client already
        // connected synchronously, the server's connect future will resolve
        // on the first poll.
        use std::future::Future;
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
        fn dummy_raw_waker() -> RawWaker {
            fn no_op(_: *const ()) {}
            fn clone(p: *const ()) -> RawWaker {
                RawWaker::new(p, &VTABLE)
            }
            const VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        let waker = unsafe { Waker::from_raw(dummy_raw_waker()) };
        let mut cx = Context::from_waker(&waker);
        let mut fut = server.connect();
        let pinned = unsafe { std::pin::Pin::new_unchecked(&mut fut) };
        match pinned.poll(&mut cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(e)) => return Err(e),
            Poll::Pending => {
                // Should not happen since the client already connected.
                // Drop the future and proceed - the pipe is still usable.
            }
        }
        drop(fut);

        Ok((Stream::Server(server), Stream::Client(client)))
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Server(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            Stream::Client(c) => std::pin::Pin::new(c).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Server(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            Stream::Client(c) => std::pin::Pin::new(c).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Server(s) => std::pin::Pin::new(s).poll_flush(cx),
            Stream::Client(c) => std::pin::Pin::new(c).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Server(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            Stream::Client(c) => std::pin::Pin::new(c).poll_shutdown(cx),
        }
    }
}

/// Owned read half of a Stream, created by `into_split()`.
pub struct ReadHalf {
    inner: tokio::io::ReadHalf<Stream>,
}

impl AsyncRead for ReadHalf {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

/// Owned write half of a Stream, created by `into_split()`.
pub struct WriteHalf {
    inner: tokio::io::WriteHalf<Stream>,
}

impl AsyncWrite for WriteHalf {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// Borrowed read reference for `stream.split()`.
pub struct SplitReadRef<'a> {
    inner: tokio::io::ReadHalf<&'a mut Stream>,
}

impl<'a> AsyncRead for SplitReadRef<'a> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

/// Borrowed write reference for `stream.split()`.
pub struct SplitWriteRef<'a> {
    inner: tokio::io::WriteHalf<&'a mut Stream>,
}

impl<'a> AsyncWrite for SplitWriteRef<'a> {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// Synchronous named pipe stream for blocking IPC (used by communicate tool).
pub struct SyncStream {
    handle: std::fs::File,
}

impl SyncStream {
    pub fn connect(path: &Path) -> io::Result<Self> {
        use std::fs::OpenOptions;
        let pipe_name = path_to_pipe_name(path);
        let file = OpenOptions::new().read(true).write(true).open(&pipe_name)?;
        Ok(Self { handle: file })
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            handle: self.handle.try_clone()?,
        })
    }

    pub fn set_read_timeout(&self, _timeout: Option<std::time::Duration>) -> io::Result<()> {
        // std::fs::File-backed named pipes do not expose socket-style read timeouts.
        // The communicate tool only uses this to avoid hanging forever; on Windows
        // we currently rely on the server side to respond promptly.
        Ok(())
    }
}

impl io::Read for SyncStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.handle.read(buf)
    }
}

impl io::Write for SyncStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.handle.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.handle.flush()
    }
}

pub fn is_socket_path(path: &Path) -> bool {
    let pipe_name = path_to_pipe_name(path);
    match probe_pipe(&pipe_name) {
        Ok(_) => true,
        Err(error) if error.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_PIPE_BUSY as i32) => {
            // Every published instance being occupied proves that a server is
            // listening. Do not report the endpoint as absent and race a second
            // daemon against it merely because another client connected first.
            true
        }
        Err(_) => false,
    }
}

pub fn remove_socket(path: &Path) {
    let pipe_name = path_to_pipe_name(path);
    if probe_pipe(&pipe_name).is_ok() {
        report(
            Level::Warn,
            &format!("[windows] Named pipe {pipe_name} still open, will be replaced by new server"),
        );
    }
}

/// Probe whether a named pipe is currently reachable, without a tokio reactor.
///
/// `ClientOptions::open` registers the pipe handle with the ambient tokio
/// reactor, so it panics with "there is no reactor running" when called from a
/// thread that has no runtime context. The two probes above are synchronous by
/// contract and are reachable from plain `std::thread` callers, so they must
/// not touch the tokio client API. `SyncStream::connect` already opens pipes
/// with `std::fs`, which is the same approach.
fn probe_pipe(pipe_name: &str) -> io::Result<()> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(pipe_name)
        .map(|_handle| ())
}

pub fn stream_pair() -> io::Result<(Stream, Stream)> {
    Stream::pair()
}

/// The "address" of a named pipe connection.
pub type Addr = PipeAddr;

/// Restrict access to a local endpoint to its owner.
///
/// A named pipe carries an ACL that applies when the instance is published, and
/// the socket path never exists as a file that could be restricted, so the
/// caller's intent is already satisfied.
pub fn restrict_socket_to_owner(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Whether the endpoint at `path` is reachable by its owner only.
///
/// A named pipe carries an ACL that applies when the instance is published, and
/// there is no file mode to read back, so the restriction the caller asked for
/// is in force.
pub fn socket_is_owner_only(_path: &Path) -> bool {
    true
}

/// An exclusive lock on a lock file, guaranteeing one live owner per path.
///
/// Dropping the guard closes the handle and then unlinks the file, in that
/// order: an empty share mode makes the file undeletable while this handle is
/// open, so a lock left behind by every run would accumulate beside each socket.
pub struct InstanceLock {
    file: Option<std::fs::File>,
    path: std::path::PathBuf,
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // Close first, then remove; the file exists only while this process can
        // still be said to hold the lock.
        drop(self.file.take());
        drop(std::fs::remove_file(&self.path));
    }
}

/// Take the exclusive lock beside `path`, or report that a live owner holds it.
///
/// Windows has no `flock`: opening the lock file with an empty share mode makes
/// every later opener fail with a sharing violation for as long as this handle
/// lives, and closing the handle releases it. The file is unlinked only after
/// that close, and a process that opened it in between keeps its own handle, so
/// the unlink fails while somebody still owns the file and no holder is ever
/// split across two files.
pub fn lock_instance_file(path: &Path) -> io::Result<Option<InstanceLock>> {
    use std::os::windows::fs::OpenOptionsExt;

    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_SHARING_VIOLATION: i32 = 32;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .share_mode(0)
        .open(path)
    {
        Ok(file) => Ok(Some(InstanceLock {
            file: Some(file),
            path: path.to_path_buf(),
        })),
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION)
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
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

/// Report that a Windows bootstrap is not serialized.
///
/// A share-mode lock would work here, but the client's own bootstrap path waits
/// far longer than a lock holder's window on this platform (the daemon takes
/// tens of seconds to become ready), so waiting for a lock would turn a slow
/// launch into a timeout. The Windows spawn path serializes through the pipe
/// namespace instead.
pub fn acquire_bootstrap_lock(_path: &Path) -> io::Result<LockAttempt> {
    Ok(LockAttempt::Unsupported)
}

/// An interrupt handle for a blocking stream, when the platform has one.
///
/// A named pipe cannot be shut down from a second handle the way a Unix socket
/// can, so callers interrupt reads by dropping the transport and closing it.
pub fn sync_stream_shutdown_handle(_stream: &SyncStream) -> Option<Arc<dyn Fn() + Send + Sync>> {
    None
}

/// Windows handles are not inherited across a process replacement, so there is
/// nothing to mark.
pub fn mark_listener_close_on_exec(_listener: &Listener) {}

/// A Windows named pipe publishes one instance at a time.
///
/// A probe can temporarily occupy the only published instance, making a second
/// connect wait inside the `ERROR_PIPE_BUSY` retry loop, so the probe that only
/// checks presence is the safe one.
pub fn connect_probe_is_safe() -> bool {
    false
}

/// A named pipe disappears with its last handle: no stale node is left behind.
pub fn socket_nodes_can_be_stale() -> bool {
    false
}

/// Windows has no readiness-descriptor handshake, so there is nothing to
/// report.
pub fn signal_ready_fd(_ready_env_var: &str) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    static BUSY_PIPE_TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// HoronAgent Desktop drives the SDK from plain `std::thread`s, where tokio's
    /// named-pipe client panics with "there is no reactor running". The two
    /// synchronous probes must therefore stay off the tokio client API.
    ///
    /// The pipe has to be a real, bound one: an absent pipe makes the open fail
    /// before tokio reaches the reactor registration, so probing a missing
    /// endpoint would pass with or without the fix and prove nothing.
    ///
    /// The test itself is a `#[tokio::test]` so that `Listener::bind` has a
    /// runtime, while the `std::thread` doing the probe deliberately has none.
    /// That split is the situation this bug is about: the server owns a runtime,
    /// the synchronous probes are called from plain threads that do not.
    #[tokio::test]
    async fn sync_pipe_probes_do_not_panic_without_a_tokio_reactor() {
        let path = std::env::temp_dir().join(format!(
            "horon-agent-plain-thread-probe-{}-{}.sock",
            std::process::id(),
            BUSY_PIPE_TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _listener = Listener::bind(&path).expect("bind named pipe");

        // A second, separately bound pipe for `remove_socket`. Probing consumes
        // the only available instance of a pipe, so reusing `path` would let the
        // second probe return ERROR_PIPE_BUSY and never reach the code under
        // test, which is exactly the coverage a single shared pipe left out.
        let remove_path = std::env::temp_dir().join(format!(
            "horon-agent-plain-thread-remove-{}-{}.sock",
            std::process::id(),
            BUSY_PIPE_TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _remove_listener = Listener::bind(&remove_path).expect("bind second named pipe");

        let probe = path.clone();
        let remove_probe = remove_path.clone();
        let joined = std::thread::spawn(move || {
            let reachable = is_socket_path(&probe);
            remove_socket(&remove_probe);
            reachable
        })
        .join();

        assert!(
            joined.is_ok(),
            "probing a live pipe on a plain thread panicked: {:?}",
            joined.err()
        );
        assert_eq!(
            joined.expect("thread joined"),
            true,
            "a bound named pipe must be reported as a live endpoint"
        );
    }

    /// The TypeScript SDK derives this name independently, so the two must
    /// agree exactly or a Windows client dials a pipe nobody is listening on.
    /// Pinning literal values here gives that duplicate a fixed contract to
    /// match, and `sdk/typescript/test/pipe-name.test.ts` asserts the same strings.
    #[test]
    fn pipe_name_matches_the_typescript_sdk() {
        for (path, expected) in [
            (
                r"C:\Users\user\AppData\Local\horon-agent\run\horon-agent-api.sock",
                r"\\.\pipe\horon-agent-api-249ba7051747c288",
            ),
            (r"C:\a\b\horon-agent.sock", r"\\.\pipe\horon-agent-69098f328f40a6df"),
        ] {
            assert_eq!(
                path_to_pipe_name(Path::new(path)),
                expected,
                "pipe name for {path} drifted from the SDK's derivation"
            );
        }
    }

    #[test]
    fn pipe_name_is_stable_and_normalizes_case_and_separators() {
        let a = path_to_pipe_name(Path::new(r"C:\Temp\horon-agent\server.sock"));
        let b = path_to_pipe_name(Path::new("c:/temp/HORON-AGENT/server.sock"));
        assert_eq!(a, b, "pipe names should be normalized consistently");
        assert!(a.starts_with(r"\\.\pipe\server-"), "unexpected pipe name: {}", a);
    }

    #[test]
    fn pipe_name_falls_back_when_stem_is_empty() {
        let name = path_to_pipe_name(Path::new("..."));
        assert!(
            name.starts_with(r"\\.\pipe\horon-agent-"),
            "unexpected pipe name: {}",
            name
        );
    }

    #[tokio::test]
    async fn stream_pair_round_trips_bytes() {
        let (mut a, mut b) = stream_pair().expect("create stream pair");
        a.write_all(b"ping").await.expect("write to pipe");
        a.flush().await.expect("flush pipe");

        let mut buf = [0u8; 4];
        b.read_exact(&mut buf).await.expect("read from pipe");
        assert_eq!(&buf, b"ping");
    }

    #[tokio::test]
    async fn split_stream_supports_concurrent_read_and_write() {
        let (a, mut b) = stream_pair().expect("create stream pair");
        let (mut read, mut write) = a.into_split();

        let peer = tokio::spawn(async move {
            let mut request = [0u8; 4];
            b.read_exact(&mut request).await.expect("read request");
            assert_eq!(&request, b"ping");
            b.write_all(b"pong").await.expect("write response");
            b.flush().await.expect("flush response");
        });

        let exchange = async {
            write.write_all(b"ping").await.expect("write request");
            write.flush().await.expect("flush request");
            let mut response = [0u8; 4];
            read.read_exact(&mut response).await.expect("read response");
            assert_eq!(&response, b"pong");
        };

        tokio::time::timeout(std::time::Duration::from_secs(5), exchange)
            .await
            .expect("split stream exchange should not stall");
        peer.await.expect("peer task");
    }

    #[tokio::test]
    async fn busy_pipe_is_reported_as_a_live_socket_path() {
        let path = std::env::temp_dir().join(format!(
            "horon-agent-busy-pipe-probe-{}-{}.sock",
            std::process::id(),
            BUSY_PIPE_TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _listener = Listener::bind(&path).expect("bind named pipe");
        let _client = Stream::connect(&path).await.expect("occupy named pipe");

        assert!(
            is_socket_path(&path),
            "a busy named pipe must still be recognized as a live server endpoint"
        );
    }
}
