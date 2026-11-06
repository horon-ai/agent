//! Contract shared by the platform adapter crates.
//!
//! `horon-agent-platform` compiles exactly one implementation - macOS, Linux,
//! or Windows - and exposes it as free functions, so core crates never branch
//! on the operating system. The trait deliberately stays object-free: every
//! caller is compiled against a concrete `Current` adapter and nothing needs
//! dynamic dispatch.

use std::fs::File;
use std::io;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::Duration;

use rustls::ClientConfig;

/// How hard a durability flush pushes the data to the device.
///
/// The setting describes a durability level, not a syscall: the adapter picks
/// the cheapest call on each operating system that meets it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SyncMode {
    /// The platform's strongest guarantee that the write survives a power loss.
    ///
    /// On macOS this is `F_FULLFSYNC` through std's `sync_all`, which also
    /// flushes the drive's write cache.
    #[default]
    Full,
    /// The cheapest flush that still survives an operating-system crash.
    ///
    /// On macOS this is a plain `fsync(2)`. Linux and Windows have no cheaper
    /// call than `Full`, so the mode does not change their behaviour.
    Fast,
}

/// A sleep-prevention assertion held for as long as the value lives.
///
/// macOS acquires an `IOPMAssertion`; the other platforms hand out an inert
/// value, so callers can hold one unconditionally.
pub trait SleepAssertion {
    /// Acquire the assertion. `reason` is shown by the operating system.
    fn prevent_user_idle_system_sleep(reason: &str) -> Self;

    /// Whether an operating-system assertion is currently held.
    fn is_active(&self) -> bool;

    /// Release the underlying assertion early; dropping the value does the
    /// same. Releasing twice is harmless.
    fn release(&mut self);
}

/// One operating system's implementation of the primitives core code shares.
///
/// The methods are associated functions: the adapter is a namespace for one
/// platform's syscalls, carries no state, and is never used behind a trait
/// object. The process-wide TLS configuration is cached by
/// `horon-agent-platform`.
pub trait PlatformAdapter {
    /// The sleep-prevention assertion this operating system provides.
    type PowerAssertion: SleepAssertion;

    /// Flush `file`'s contents according to `mode`.
    fn sync_file(file: &File, mode: SyncMode) -> io::Result<()>;

    /// Flush the directory entry of an existing directory so a file created or
    /// renamed inside it survives a crash.
    ///
    /// The caller has already checked that `path` names a directory. Windows
    /// cannot open a directory for a flush without backup semantics and NTFS
    /// journals directory entries itself, so its implementation returns
    /// `Ok(())` and leaves the identity check to the caller.
    fn sync_dir(path: &Path, mode: SyncMode) -> io::Result<()>;

    /// Build the client TLS configuration this operating system trusts.
    ///
    /// The trust set is the bundled webpki roots plus the roots the operating
    /// system trusts, and it is never narrower than that union.
    fn tls_client_config() -> Arc<ClientConfig>;

    /// Whether a process with `pid` exists.
    ///
    /// Unix probes with `kill(pid, 0)` and treats only `ESRCH` as "gone";
    /// Windows opens the process and reads its exit code.
    fn is_process_running(pid: u32) -> bool;

    /// Send `signal` to the detached process group or session led by `pid`.
    ///
    /// Unix signals `-pid` because detached children are started with
    /// `setsid()`. Windows has no catchable termination signal and terminates
    /// the whole process tree instead; `signal` is ignored there.
    fn signal_detached_process_group(pid: u32, signal: i32) -> io::Result<()>;

    /// Stop the process group led by `pid`: ask politely, wait up to `grace`,
    /// then force it. Returns whether the group is gone.
    fn terminate_process_group(pid: u32, grace: Duration) -> bool;

    /// Reap a child of the current process without blocking.
    ///
    /// `Ok(Some(code))` when the child exited and was reaped now, `Ok(None)`
    /// when it still runs or is not our child.
    fn try_reap_child_process(pid: u32) -> io::Result<Option<i32>>;

    /// Atomically replace the symlink `dst` with one pointing at `src`, using
    /// `temp` as the staging name.
    fn atomic_symlink_swap(src: &Path, dst: &Path, temp: &Path) -> io::Result<()>;

    /// Spawn a process detached from the current client session.
    fn spawn_detached(cmd: &mut Command) -> io::Result<Child>;

    /// Spawn a fire-and-forget child that must never flash a console window.
    ///
    /// On Unix this is [`PlatformAdapter::spawn_detached`]; Windows uses
    /// `CREATE_NO_WINDOW` rather than `DETACHED_PROCESS` so console-subsystem
    /// descendants inherit a windowless console instead of allocating one.
    fn spawn_detached_without_console_window(cmd: &mut Command) -> io::Result<Child>;

    /// Reap a detached child without blocking the caller.
    fn reap_detached(child: Child);

    /// Replace the current process with `cmd` (exec on Unix).
    ///
    /// Returns the error when the replacement fails; on Unix success never
    /// returns.
    fn replace_process(cmd: &mut Command) -> io::Error;

    /// Best-effort increase of the soft `RLIMIT_NOFILE` limit on Unix.
    fn raise_nofile_limit_best_effort(minimum_soft_limit: u64);

    /// Create a symlink (Unix) or copy the file (Windows).
    ///
    /// Windows symlinks need elevated privileges or Developer Mode, so the
    /// implementation falls back to copying.
    fn symlink_or_copy(src: &Path, dst: &Path) -> io::Result<()>;

    /// Make a file executable (0o755). A no-op on Windows, where executability
    /// comes from the file extension.
    fn set_permissions_executable(path: &Path) -> io::Result<()>;
}
