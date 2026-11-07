//! The platform adapter selected for this build.
//!
//! Core crates call these free functions and never branch on the operating
//! system themselves. `Current` is resolved at compile time (macOS, Windows,
//! and every other Unix target, where Linux handles FreeBSD as well), so no
//! dynamic dispatch is involved anywhere on this path.

use std::fs::File;
use std::io;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use rustls::ClientConfig;

pub use horon_agent_platform_api::{PlatformAdapter, SleepAssertion, SyncMode};

pub mod fs;
pub mod ipc;
pub mod paths;
pub mod power;
pub mod process;
pub mod signal;
pub mod term;
pub mod watch;

#[cfg(target_os = "macos")]
pub type Current = horon_agent_platform_macos::Adapter;

#[cfg(windows)]
pub type Current = horon_agent_platform_windows::Adapter;

#[cfg(all(unix, not(target_os = "macos")))]
pub type Current = horon_agent_platform_linux::Adapter;

/// Flush `file`'s contents according to `mode`.
pub fn sync_file(file: &File, mode: SyncMode) -> io::Result<()> {
    Current::sync_file(file, mode)
}

/// Flush a directory entry so a file created or renamed inside it survives a
/// crash.
///
/// A path that is not a directory is an error on every platform, before any
/// operating-system call happens. The check follows a symlink, so a home or a
/// session root that is a link to its directory is flushed through the link as
/// it always was. Windows then has nothing left to flush and returns `Ok(())`.
pub fn sync_dir(path: &Path, mode: SyncMode) -> io::Result<()> {
    if !std::fs::metadata(path)?.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotADirectory, "not a directory"));
    }
    Current::sync_dir(path, mode)
}

static TLS_CONFIG_BUILDS: AtomicU64 = AtomicU64::new(0);
static TLS_CONFIG: LazyLock<Arc<ClientConfig>> = LazyLock::new(|| {
    TLS_CONFIG_BUILDS.fetch_add(1, Ordering::Relaxed);
    Current::tls_client_config()
});

/// The process-wide client TLS configuration.
///
/// The trust set is the bundled webpki roots plus the roots the operating
/// system trusts, the client offers ALPN `h2` then `http/1.1`, and the
/// aws-lc-rs provider is named explicitly. The configuration is built on the
/// first call and every later call returns the same allocation, so a process
/// pays the system-verifier setup once.
pub fn tls_client_config() -> Arc<ClientConfig> {
    Arc::clone(&TLS_CONFIG)
}

/// How many configurations this process has built.
///
/// A process that shares the cached configuration reports `1`; the `tls_probe`
/// example prints this as the evidence that a second client reuses the first
/// build.
pub fn tls_config_builds() -> u64 {
    TLS_CONFIG_BUILDS.load(Ordering::Relaxed)
}

/// Build an adapter configuration that also trusts `extra_roots`.
///
/// Production code uses [`tls_client_config`]. This hook exists for tests: it
/// proves a root the operating system trusts and the bundled webpki roots do
/// not is accepted, without installing that root into the real trust store.
/// The result is not cached, so the process-wide configuration stays untouched.
#[doc(hidden)]
pub fn tls_client_config_with_extra_roots(
    extra_roots: Vec<rustls::pki_types::CertificateDer<'static>>,
) -> Result<Arc<ClientConfig>, rustls::Error> {
    Current::client_config_with_extra_roots(extra_roots).map(Arc::new)
}

/// Whether a process with `pid` exists.
pub fn is_process_running(pid: u32) -> bool {
    Current::is_process_running(pid)
}

/// Send `signal` to the detached process group or session led by `pid`.
pub fn signal_detached_process_group(pid: u32, signal: i32) -> io::Result<()> {
    Current::signal_detached_process_group(pid, signal)
}

/// Stop the process group led by `pid`: ask politely, wait up to `grace`, then
/// force it. Returns whether the group is gone.
pub fn terminate_process_group(pid: u32, grace: Duration) -> bool {
    Current::terminate_process_group(pid, grace)
}

/// Stop `pid` itself, for a process that leads no group.
pub fn terminate_process(pid: u32, grace: Duration) -> bool {
    Current::terminate_process(pid, grace)
}

/// Reap a child of the current process without blocking.
pub fn try_reap_child_process(pid: u32) -> io::Result<Option<i32>> {
    Current::try_reap_child_process(pid)
}

/// Atomically replace the symlink `dst` with one pointing at `src`, using
/// `temp` as the staging name.
pub fn atomic_symlink_swap(src: &Path, dst: &Path, temp: &Path) -> io::Result<()> {
    Current::atomic_symlink_swap(src, dst, temp)
}

/// Spawn a process detached from the current client session.
pub fn spawn_detached(cmd: &mut Command) -> io::Result<Child> {
    Current::spawn_detached(cmd)
}

/// Spawn a fire-and-forget child that must never flash a console window.
pub fn spawn_detached_without_console_window(cmd: &mut Command) -> io::Result<Child> {
    Current::spawn_detached_without_console_window(cmd)
}

/// Spawn a fire-and-forget child that must not flash a console window and keeps
/// the caller's session on Unix.
///
/// The SDK launches the runtime this way from a host that owns its own session;
/// the child is not reaped by the platform, so the caller drops it.
pub fn spawn_background_consoleless(cmd: &mut Command) -> io::Result<Child> {
    Current::spawn_background_consoleless(cmd)
}

/// Spawn `cmd` with a readiness pipe the child inherits where the platform can
/// pass a descriptor.
///
/// Re-exported from [`process`], where the rest of the spawn family lives: the
/// daemon spawn path reads it at the crate root next to `spawn_detached`.
pub use process::spawn_with_ready_pipe;

/// Reap a detached child without blocking the caller.
pub fn reap_detached(child: Child) {
    Current::reap_detached(child);
}

/// Replace the current process with `cmd` (exec on Unix).
///
/// Returns the error when the replacement fails; on Unix success never
/// returns.
pub fn replace_process(cmd: &mut Command) -> io::Error {
    Current::replace_process(cmd)
}

/// Best-effort increase of the soft `RLIMIT_NOFILE` limit on Unix.
pub fn raise_nofile_limit_best_effort(minimum_soft_limit: u64) {
    Current::raise_nofile_limit_best_effort(minimum_soft_limit);
}

/// Create a symlink (Unix) or copy the file (Windows).
pub fn symlink_or_copy(src: &Path, dst: &Path) -> io::Result<()> {
    Current::symlink_or_copy(src, dst)
}

/// Make a file executable (0o755). A no-op on Windows.
pub fn set_permissions_executable(path: &Path) -> io::Result<()> {
    Current::set_permissions_executable(path)
}

/// Create a directory its owner alone can enter, creating missing parents when
/// `recursive` is set.
///
/// Every level this call creates is private, not just the leaf: a parent made
/// with the process umask would expose the names below it, and a session tree
/// is meant to be closed at every level. An already existing directory is
/// accepted as it is: callers use this to reach a known state, not to detect
/// that a directory is new.
pub fn create_private_dir(path: &Path, recursive: bool) -> io::Result<()> {
    if recursive
        && let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        create_missing_private_levels(parent)?;
    }
    match Current::create_private_dir(path) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        other => other,
    }
}

/// Create the deepest missing ancestor of `path`, one private level at a time.
fn create_missing_private_levels(path: &Path) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if !parent.as_os_str().is_empty() {
                create_missing_private_levels(parent)?;
            }
            match Current::create_private_dir(path) {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
                other => other,
            }
        }
        Err(error) => Err(error),
    }
}

/// Restrict an existing directory to its owner, whether or not this process
/// created it.
pub fn set_permissions_private_dir(path: &Path) -> io::Result<()> {
    Current::set_dir_permissions_owner_only(path)
}

/// A sleep-prevention assertion held for as long as the value lives.
///
/// The wrapper keeps the operating-system type behind the adapter while giving
/// core code a plain type with inherent methods.
pub struct PowerAssertion(<Current as PlatformAdapter>::PowerAssertion);

impl PowerAssertion {
    /// Acquire the assertion. `reason` is shown by the operating system.
    pub fn prevent_user_idle_system_sleep(reason: &str) -> Self {
        Self(<Current as PlatformAdapter>::PowerAssertion::prevent_user_idle_system_sleep(reason))
    }

    /// Whether an operating-system assertion is currently held.
    pub fn is_active(&self) -> bool {
        SleepAssertion::is_active(&self.0)
    }

    /// Release the underlying assertion early; dropping the value does the
    /// same.
    pub fn release(&mut self) {
        SleepAssertion::release(&mut self.0);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn sync_file_full_then_reopen_reads_bytes() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary file");
        file.write_all(b"platform sync\n").expect("write bytes");
        file.flush().expect("flush bytes");
        sync_file(file.as_file(), SyncMode::Full).expect("full sync");
        sync_file(file.as_file(), SyncMode::Fast).expect("fast sync");

        let contents = std::fs::read(file.path()).expect("read back");
        assert_eq!(contents, b"platform sync\n");
    }

    #[test]
    fn sync_dir_accepts_a_directory() {
        let directory = tempfile::tempdir().expect("temporary directory");
        sync_dir(directory.path(), SyncMode::Full).expect("full directory sync");
        sync_dir(directory.path(), SyncMode::Fast).expect("fast directory sync");
    }

    #[test]
    fn sync_dir_rejects_non_directory() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("file.txt");
        std::fs::write(&path, b"data").expect("write file");

        let error = sync_dir(&path, SyncMode::Full).expect_err("a file path must be rejected");
        eprintln!("sync_dir(file) -> {error:?}");
        assert!(
            matches!(error.kind(), io::ErrorKind::NotADirectory | io::ErrorKind::InvalidInput),
            "unexpected error kind: {error:?}"
        );
    }

    #[test]
    fn sync_dir_rejects_a_missing_path() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let missing = directory.path().join("missing");
        let error = sync_dir(&missing, SyncMode::Full).expect_err("a missing path must be rejected");
        eprintln!("sync_dir(missing) -> {error:?}");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn tls_client_config_is_built_once_per_process() {
        let first = tls_client_config();
        let second = tls_client_config();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(tls_config_builds(), 1);
        assert_eq!(first.alpn_protocols, vec![b"h2".to_vec(), b"http/1.1".to_vec()]);
    }

    #[cfg(windows)]
    #[test]
    fn sync_dir_is_a_no_op_on_windows() {
        let directory = tempfile::tempdir().expect("temporary directory");
        // Windows keeps the identity check above and then has nothing to flush.
        sync_dir(directory.path(), SyncMode::Full).expect("windows directory sync is Ok");
        sync_dir(directory.path(), SyncMode::Fast).expect("windows directory sync is Ok");
    }
}
