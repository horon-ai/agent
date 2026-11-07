//! Contract shared by the platform adapter crates.
//!
//! `horon-agent-platform` compiles exactly one implementation - macOS, Linux,
//! or Windows - and exposes it as free functions, so core crates never branch
//! on the operating system. The trait deliberately stays object-free: every
//! caller is compiled against a concrete `Current` adapter and nothing needs
//! dynamic dispatch.

use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::Duration;

use rustls::ClientConfig;

mod fs;
pub mod log;
mod power;
mod process;
mod term;

pub use fs::{FileIds, FileStamp};
pub use power::{InhibitorState, SleepInhibitor};
pub use process::{ProcessMemoryFacts, ReadyChild, StopRequest};
pub use term::StdinState;

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

    /// The process-wide sleep inhibitor this operating system provides.
    type Inhibitor: SleepInhibitor;

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

    /// Whether flushing a directory's entry confirms a file created inside it.
    ///
    /// Unix flushes the entry table itself, so a creation that precedes this
    /// flush survives a power failure even when the file's own bytes were never
    /// flushed. That is enough for a file whose only meaning is its name: the
    /// catalog dirty marker is empty, readers test its existence, and a lost
    /// timestamp or size costs nothing.
    ///
    /// Windows cannot open a directory for a flush and leaves the entry to NTFS
    /// journaling, so `false` there means a creation is confirmed only by
    /// flushing the file itself.
    fn directory_sync_is_effective() -> bool;

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

    /// Stop `pid` itself: ask politely, wait up to `grace`, then force it.
    ///
    /// This is the form for a pid that leads no process group, where the group
    /// call reaches nothing. Unix signals the process alone and leaves its
    /// children to it; Windows has no process groups and terminates the tree, so
    /// both calls are the same there. Returns whether the process is gone.
    fn terminate_process(pid: u32, grace: Duration) -> bool;

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

    /// Spawn a fire-and-forget child that must not flash a console window and
    /// must not start a new session on Unix.
    ///
    /// The SDK launches the runtime this way from a host that owns its own
    /// session, so the child keeps the caller's session and the caller does not
    /// wait for it. Windows starts it in a new process group with a hidden
    /// console, which keeps the host's Ctrl+C or exit from reaching it.
    fn spawn_background_consoleless(cmd: &mut Command) -> io::Result<Child>;

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

    /// Restrict a file to read/write by its owner.
    ///
    /// Unix sets mode `0o600`. Windows replaces the DACL with a protected
    /// full-control entry for the current process user, so an inherited or
    /// explicit grant cannot expose a secret-bearing file.
    fn set_file_permissions_owner_only(path: &Path) -> io::Result<()>;

    /// Restrict a directory to read/write/execute by its owner.
    ///
    /// Unix sets mode `0o700`. Windows installs a DACL the directory's children
    /// inherit, so everything created inside stays owner-only.
    fn set_dir_permissions_owner_only(path: &Path) -> io::Result<()>;

    /// Narrow a secret-bearing file to its owner without failing the caller.
    ///
    /// Unix chmods synchronously. Windows defers the ACL replacement to a
    /// coalescing worker: credential readers call this on the startup and
    /// render paths, and a security product can stretch one ACL call to
    /// seconds, so read-time repair must never block the caller.
    fn harden_credential_file_permissions(path: &Path);

    /// Narrow a secret-bearing directory to its owner without failing the
    /// caller. See [`PlatformAdapter::harden_credential_file_permissions`].
    fn harden_credential_dir_permissions(path: &Path);

    /// Narrow a directory to its owner when the platform has to, ignoring
    /// failure.
    ///
    /// Unix chmods `0o700`. Windows does nothing: a directory created under the
    /// user-private tree already inherits that ACL, and replacing it on a path
    /// this cheap would cost an ACL call.
    fn harden_dir_permissions(path: &Path);

    /// Whether `metadata` describes a directory only its owner can enter.
    ///
    /// Windows reports `true` for any directory: the owner-only property lives
    /// in the inherited ACL, which the metadata surface does not expose.
    fn is_private_dir(metadata: &Metadata) -> bool;

    /// Whether `metadata` describes a directory only its owner can enter and
    /// that `owner` names the same user as.
    fn is_private_dir_owned_by(metadata: &Metadata, owner: &Metadata) -> bool;

    /// Whether `metadata` describes a single-link regular file only its owner
    /// can read or write.
    fn is_private_file(metadata: &Metadata) -> bool;

    /// Whether `path` names a file this process may execute.
    ///
    /// Unix requires the file type and an execute bit. Windows only requires a
    /// regular file, because executability comes from the extension.
    fn is_executable(path: &Path) -> bool;

    /// Open options that refuse to follow a symlink at the leaf.
    ///
    /// Unix requests `O_NOFOLLOW`. Windows has no such flag and checks the leaf
    /// identity after opening instead.
    fn no_follow_open_options() -> OpenOptions;

    /// Request owner-only (`0o600`) permissions for a file `options` creates.
    ///
    /// A no-op on Windows, where the parent's inherited ACL decides it.
    fn request_owner_only_mode(options: &mut OpenOptions);

    /// Create a directory only its owner can enter.
    ///
    /// Unix creates it with mode `0o700` and refuses to reuse an existing leaf
    /// silently through the caller's own retry. Windows creates the whole path
    /// and inherits the parent's ACL.
    fn create_private_dir(path: &Path) -> io::Result<()>;

    /// Verify that `file` is the private regular file `path` names.
    ///
    /// Unix compares the opened handle with the leaf entry: same device and
    /// inode, one link, mode `0o600`, and an owner matching the parent
    /// directory. Windows cannot read mode bits, link count or inode, so it
    /// requires a regular file whose creation time matches the opened handle,
    /// which still catches a file swapped in between the open and the check.
    fn verify_private_file(path: &Path, file: &File) -> io::Result<()>;

    /// The identity and timestamps of `metadata` for a cache stamp.
    fn file_stamp(metadata: &Metadata) -> FileStamp;

    /// The ownership identity of `metadata`.
    fn file_ids(metadata: &Metadata) -> FileIds;

    /// `metadata`'s modification time in nanoseconds since the Unix epoch.
    fn mtime_ns(metadata: &Metadata) -> i128;

    /// Keep `primary`'s current contents at `backup` while leaving `primary` in
    /// place.
    ///
    /// Unix hard-links the inode, so a concurrent reader sees either the old or
    /// the new contents and never a missing path. Windows renames, because it
    /// cannot hard-link over the file that is about to be replaced.
    fn rotate_backup(primary: &Path, backup: &Path) -> io::Result<()>;

    /// Flush the directory entry of `path`'s parent, if any.
    ///
    /// Windows journals directory entries itself and returns `Ok(())`.
    fn sync_parent_directory(path: &Path) -> io::Result<()>;

    /// Kill the process group led by `pid` immediately.
    fn kill_process_group(pid: u32);

    /// Place `command`'s child in its own process group, so the whole tree can
    /// be signalled as a unit.
    ///
    /// A no-op on Windows, which has no process groups; job objects cover the
    /// same ground and are not created here.
    fn isolate_process_group(command: &mut Command);

    /// Summarize this process's file-descriptor usage for debugging `EMFILE`
    /// and similar failures.
    fn fd_diagnostic_snapshot() -> String;

    /// Whether `pid` is blocked reading its standard input.
    fn stdin_state(pid: u32) -> StdinState;

    /// Whether `pid` or a descendant that shares its standard input is blocked
    /// reading it.
    fn process_tree_stdin_state(pid: u32) -> StdinState;

    /// The immediate children of `pid`.
    ///
    /// Empty on platforms that do not expose the parent link without scanning
    /// every process.
    fn direct_children(pid: u32) -> Vec<u32>;

    /// A short filesystem-safe discriminator for the current user.
    fn user_discriminator() -> String;

    /// The per-user temporary directory this platform provides, when it has one
    /// that is cheaper than the shared temporary directory.
    fn user_runtime_dir() -> Option<PathBuf>;

    /// How `metadata` describes the file's type to a user.
    ///
    /// Unix distinguishes sockets, FIFOs and device nodes; Windows, whose
    /// filesystem surface has no such entries, reports a directory or a
    /// non-regular file.
    fn describe_file_kind(metadata: &Metadata) -> &'static str;

    /// Prepare `options` to open the credential file at `path`.
    ///
    /// Unix asks for mode `0o600` and refuses to follow a symlink, blocking on
    /// a FIFO or opening a device. Windows has none of those flags and rejects
    /// a symlink at `path` instead, so the open cannot be redirected.
    fn harden_credential_open(options: &mut OpenOptions, path: &Path) -> io::Result<()>;

    /// Repair the permissions of the already-open `file` to owner-only.
    ///
    /// Unix chmods through the descriptor, which cannot be redirected by a
    /// path swap between the open and the repair. Windows has no mode bits and
    /// does nothing here; its files inherit the directory ACL instead.
    fn set_permissions_owner_only_file(file: &File) -> io::Result<()>;

    /// Verify `metadata` describes a private regular file.
    ///
    /// Unix requires a regular file owned by this process's user with a single
    /// link, so a hard link cannot expose the contents to another owner.
    /// Windows cannot read an owner or a link count and requires a regular file.
    fn verify_private_file_metadata(metadata: &Metadata) -> io::Result<()>;

    /// Verify `metadata` describes a directory owned by this process's user.
    ///
    /// Windows reports success: the owner-only property lives in the inherited
    /// ACL, which the metadata surface does not expose.
    fn verify_private_dir_metadata(metadata: &Metadata) -> io::Result<()>;

    /// Copy the directory `src` to `dst` with the filesystem's copy-on-write
    /// primitive, so the copy costs no data blocks.
    ///
    /// `dst` must not exist.
    fn clone_tree(src: &Path, dst: &Path) -> io::Result<()>;

    /// Suppress the console window a spawned child would otherwise get.
    ///
    /// A no-op on Unix, where a child of a windowless process stays windowless.
    fn hide_console_window(cmd: &mut Command);

    /// The command that runs `command` through this platform's shell.
    ///
    /// Unix runs `bash -c` and, when `temp_dir` is given, points `TMPDIR` and
    /// `HORON_AGENT_SCRATCH_DIR` at it. Windows runs `cmd.exe /D /S /C` and
    /// ignores `temp_dir`, whose variables are read by POSIX tools only.
    ///
    /// The command text reaches `cmd.exe` verbatim, because cmd.exe does not
    /// decode its command line with the C runtime's rules.
    fn shell_command(command: &str, temp_dir: Option<&Path>) -> Command;

    /// [`PlatformAdapter::shell_command`], but the command text is passed as a
    /// normal argument.
    ///
    /// Windows re-encodes such an argument for `CommandLineToArgvW`, so nested
    /// quotes can change; the process monitor has always spawned its configured
    /// command this way and keeps doing so. Unix has no such distinction.
    fn shell_command_escaped(command: &str, temp_dir: Option<&Path>) -> Command;

    /// Working directories of the processes this platform can inspect.
    ///
    /// Empty where no cheap process table with working directories exists.
    fn process_working_dirs() -> Vec<PathBuf>;

    /// This process's memory facts for the diagnostic snapshot.
    fn process_memory_facts() -> ProcessMemoryFacts;

    /// Set the short process name the operating system reports for this
    /// process, bounded by what the kernel accepts.
    ///
    /// A no-op where the process name comes from the executable alone.
    fn set_process_name(name: &str);

    /// Whether this desktop can launch a browser for the user.
    ///
    /// `Some(true)` when a launcher is known to exist, `Some(false)` when the
    /// platform proves it cannot show anything, and `None` when it cannot tell.
    fn browser_launcher_state() -> Option<bool>;

    /// Whether this process runs inside a container that hides the desktop.
    fn is_container() -> bool;

    /// Tune the system memory allocator for a long-running server.
    ///
    /// glibc keeps freed large blocks in its arenas and raises its own mmap
    /// threshold unless `malloc` is told otherwise, which shows up as permanent
    /// RSS. The Linux implementation calls `mallopt`; every other platform has
    /// nothing to tune. `use_system_allocator` is false when the caller linked a
    /// different global allocator, where this is a no-op.
    fn configure_system_allocator(use_system_allocator: bool);

    /// The main-thread stack size the entry point must request.
    ///
    /// Windows starts the main thread with a much smaller stack than the Unix
    /// environments this project is developed on, and the CLI's provider and
    /// config work can overflow it before Tokio takes over. `None` means the
    /// platform default is big enough and the entry point runs directly.
    fn main_thread_stack_size() -> Option<usize>;

    /// Arm this platform's parent-death behaviour for the current process and
    /// report whether the parent is already gone.
    ///
    /// Linux arms `PR_SET_PDEATHSIG(SIGTERM)` and compares the parent before and
    /// after arming, because the launcher can exit in between, and treats an
    /// init/systemd parent as gone. Windows has no parent-death signal, and
    /// macOS has no way to arm one, so both report `false` and leave the
    /// decision to the caller's own shutdown.
    fn bind_process_lifetime_to_parent() -> bool;

    /// Ask the process group led by `pid` to stop.
    ///
    /// Unix sends `SIGTERM` to the group, which the daemon can catch; Windows
    /// has no catchable termination signal and terminates the tree at once.
    fn request_graceful_stop(pid: u32) -> io::Result<StopRequest>;

    /// Force the process group led by `pid` to exit now.
    ///
    /// Unix sends `SIGKILL` to the group. Windows has no escalation step of its
    /// own, so this repeats the tree termination.
    fn force_stop_process_group(pid: u32) -> io::Result<()>;

    /// Spawn `cmd` and, where the platform passes descriptors to a child, hand
    /// it a readiness pipe.
    ///
    /// The child inherits the write end as the descriptor number named by
    /// `ready_env_var` and writes one byte to it once it can serve requests; the
    /// parent keeps the read end. Windows cannot pass that descriptor through
    /// process creation, so it spawns plainly and reports `ready: None`.
    fn spawn_with_ready_pipe(cmd: &mut Command, ready_env_var: &str) -> io::Result<ReadyChild>;

    /// The name this platform reports for a signal that ended a command, or
    /// `SIG<n>` when its exit-status table has no name for it.
    ///
    /// A platform without POSIX signals reports `"unknown"`.
    fn signal_name(signal: i32) -> String;

    /// A sentence describing a process that `signal` terminated.
    fn signal_crash_reason(signal: i32) -> String;

    /// Spawn a watcher that calls `on_signal` for every termination signal this
    /// platform delivers to the process.
    ///
    /// Unix watches `SIGHUP`, `SIGTERM`, `SIGINT` and `SIGQUIT`, one task per
    /// signal, and logs a signal it cannot watch. Windows delivers these through
    /// the console instead and installs nothing, so the callback never runs.
    fn spawn_termination_signal_watchers<F>(on_signal: F)
    where
        F: Fn(i32) + Send + Sync + 'static;

    /// Replace `to` with `from` in one step.
    ///
    /// A reader observes either the old file or the new one, never a missing or
    /// half-written path. Unix renames; Windows uses `MoveFileExW` with
    /// replacement and write-through.
    fn atomic_replace(from: &Path, to: &Path) -> io::Result<()>;

    /// Create `dst` as a symbolic link to `src`.
    ///
    /// A copy would stop tracking the file the link points at, so a platform
    /// that cannot create a link reports the error rather than copying.
    fn symlink_file(src: &Path, dst: &Path) -> io::Result<()>;

    /// Stop a child process and reap it.
    ///
    /// Unix sends `SIGTERM`, waits up to three seconds, then kills. Windows has
    /// no catchable termination signal and terminates at once.
    fn terminate_child(child: &mut Child);

    /// The base directory this platform keeps application configuration in.
    ///
    /// macOS returns `~/Library/Application Support`, Windows `%APPDATA%`, and
    /// the other Unix systems `$XDG_CONFIG_HOME` or `~/.config`. `None` when the
    /// environment names no home directory at all.
    fn user_config_home() -> Option<PathBuf>;
}
