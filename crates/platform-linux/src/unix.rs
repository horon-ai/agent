//! POSIX primitives shared by the macOS and Linux adapters.
//!
//! `platform-linux` owns this file and compiles it directly; `platform-macos`
//! pulls the same source in with `#[path = ...]`, so the two Unix adapters
//! cannot drift apart. The file never refers to `crate::` items: it is a
//! self-contained module compiled into whichever adapter includes it.

use horon_agent_platform_api::log::{PlatformLogLevel as Level, log as report};
use std::io;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use horon_agent_platform_api::{InhibitorState, ReadyChild, SleepInhibitor, StopRequest};

fn desired_nofile_soft_limit(current: u64, hard: u64, minimum: u64) -> Option<u64> {
    let desired = current.max(minimum).min(hard);
    (desired > current).then_some(desired)
}

/// Check if a process is running by PID.
///
/// `kill(pid, 0)` performs the permission and existence check without sending
/// a signal. Only `ESRCH` means the process is gone; `EPERM` still proves it
/// exists.
pub(crate) fn is_process_running(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        return true;
    }
    let err = io::Error::last_os_error();
    !matches!(err.raw_os_error(), Some(code) if code == libc::ESRCH)
}

/// Send a signal to an entire detached process group/session led by `pid`.
///
/// Detached tasks are spawned with `setsid()`, so the leader PID is also the
/// process-group/session ID. Signaling `-pid` reaches the full tree. A process
/// that leads no group is not reached this way, and the failure is reported
/// instead of being answered with a signal to the leader alone: a caller told
/// the group stopped would leave every child of that leader running.
pub(crate) fn signal_detached_process_group(pid: u32, signal: i32) -> io::Result<()> {
    let group = unsafe { libc::kill(-(pid as i32), signal) };
    if group == 0 {
        return Ok(());
    }
    Err(io::Error::last_os_error())
}

/// Send a signal to `pid` alone, whether or not it leads a group.
pub(crate) fn signal_process(pid: u32, signal: i32) -> io::Result<()> {
    let result = unsafe { libc::kill(pid as i32, signal) };
    if result == 0 {
        return Ok(());
    }
    Err(io::Error::last_os_error())
}

/// Stop a process group gracefully: send SIGTERM to the group led by `pid`,
/// wait up to `grace` for the leader to exit, then SIGKILL the group.
///
/// `pid` must be a process-group leader (spawned with `setpgid`/`setsid`), which
/// is how `bash` children are started. Returns whether the group is gone when
/// this returns.
pub(crate) fn terminate_process_group(pid: u32, grace: Duration) -> bool {
    stop_until_gone(pid, grace, signal_detached_process_group)
}

/// Stop `pid` itself: SIGTERM, wait up to `grace`, then SIGKILL.
///
/// A process that leads no group - one a supervisor started, or a daemon whose
/// exec changed its group - cannot be reached through `-pid`, and the group
/// signal says so. This stops that one process and reaches no child, so it is
/// the fallback for a caller that already learned the group is not there.
pub(crate) fn terminate_process(pid: u32, grace: Duration) -> bool {
    stop_until_gone(pid, grace, signal_process)
}

/// Send SIGTERM through `signal`, wait up to `grace`, then send SIGKILL. The
/// group and single-process forms differ only in that call, so the escalation
/// and the polling live here once.
fn stop_until_gone(pid: u32, grace: Duration, signal: fn(u32, i32) -> io::Result<()>) -> bool {
    if !is_process_running(pid) {
        return true;
    }
    // Best effort: the poll below decides whether the target actually exits.
    drop(signal(pid, libc::SIGTERM));
    let deadline = Instant::now() + grace;
    loop {
        if !is_process_running(pid) {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50).min(deadline - now));
    }
    drop(signal_detached_process_group(pid, libc::SIGKILL));
    // SIGKILL is immediate for the kernel, but the process entry lingers
    // until it is reaped. Poll briefly instead of claiming success blindly.
    for _ in 0..50 {
        if !is_process_running(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    !is_process_running(pid)
}

/// Best-effort non-blocking reap for a child process owned by the current process.
///
/// Returns:
/// - `Ok(Some(exit_code))` if the child exited and was reaped now
/// - `Ok(None)` if it is still running or is not our child
pub(crate) fn try_reap_child_process(pid: u32) -> io::Result<Option<i32>> {
    let mut status = 0;
    let rc = unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) };
    if rc == 0 {
        return Ok(None);
    }
    if rc == -1 {
        let err = io::Error::last_os_error();
        if matches!(err.raw_os_error(), Some(code) if code == libc::ECHILD) {
            return Ok(None);
        }
        return Err(err);
    }

    if libc::WIFEXITED(status) {
        Ok(Some(libc::WEXITSTATUS(status)))
    } else if libc::WIFSIGNALED(status) {
        Ok(Some(128 + libc::WTERMSIG(status)))
    } else {
        Ok(Some(-1))
    }
}

/// Atomically swap a symlink by creating a temp symlink and renaming.
pub(crate) fn atomic_symlink_swap(src: &Path, dst: &Path, temp: &Path) -> io::Result<()> {
    // A leftover staging entry is expected on the first call and harmless.
    drop(std::fs::remove_file(temp));
    std::os::unix::fs::symlink(src, temp)?;
    std::fs::rename(temp, dst)
}

/// Spawn a process detached from the current client session.
///
/// This is used for launching new terminal windows (for `/resume`, `/split`,
/// crash restore, etc.) so the new client survives if the invoking horon-agent
/// process exits or its terminal closes.
pub(crate) fn spawn_detached(cmd: &mut Command) -> io::Result<Child> {
    use std::os::unix::process::CommandExt;

    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    cmd.spawn()
}

/// Spawn a fire-and-forget child that must never flash a console window.
///
/// Consoles are a Windows concept, so on Unix this is exactly
/// [`spawn_detached`].
pub(crate) fn spawn_detached_without_console_window(cmd: &mut Command) -> io::Result<Child> {
    spawn_detached(cmd)
}

/// Spawn a background child that keeps the caller's session.
///
/// Unix has no console to hide, and a host that already owns its own session
/// (the SDK launching the runtime) must not have its child moved out of it, so
/// this is a plain spawn.
pub(crate) fn spawn_background_consoleless(cmd: &mut Command) -> io::Result<Child> {
    cmd.spawn()
}

/// Reap a detached child without blocking the caller.
pub(crate) fn reap_detached(child: Child) {
    let mut child = child;
    // A thread that cannot start only leaves the child unreaped, which the OS
    // handles when this process exits.
    drop(
        std::thread::Builder::new()
            .name("horon-agent-detached-child".to_string())
            .spawn(move || {
                drop(child.wait());
            }),
    );
}

/// Stop a child process: ask politely, wait three seconds, then force it.
///
/// A child that ignores `SIGTERM` is killed, so the caller always regains
/// control; the child is reaped before this returns.
pub(crate) fn terminate_child(child: &mut Child) {
    use std::time::{Duration, Instant};

    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    // SAFETY: the id comes from a live `Child`, and kill only reads it.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    drop(child.kill());
    drop(child.wait());
}

/// Replace the current process with a new command (exec on Unix).
///
/// On success this never returns.
pub(crate) fn replace_process(cmd: &mut Command) -> io::Error {
    use std::os::unix::process::CommandExt;

    let err = cmd.exec();
    report(
        Level::Error,
        &format!("replace_process failed: {} ({})", err, process_fd_diagnostic_snapshot()),
    );
    err
}

/// Best-effort increase of the current process soft `RLIMIT_NOFILE`.
///
/// This helps horon-agent survive short-lived reload/connect spikes even when it was
/// launched from a shell with a conservative `ulimit -n` like 1024.
pub(crate) fn raise_nofile_limit_best_effort(minimum_soft_limit: u64) {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        report(
            Level::Warn,
            &format!("Failed to read RLIMIT_NOFILE: {}", io::Error::last_os_error()),
        );
        return;
    }

    // `rlim_cur`/`rlim_max` are `u64` on Linux/macOS but `i64` on some
    // platforms (e.g. FreeBSD), so cast explicitly to keep builds portable.
    // The cast is a no-op (and clippy-flagged) where the field is already
    // `u64`, hence the allow.
    #[allow(clippy::unnecessary_cast)]
    let current: u64 = limit.rlim_cur as u64;
    #[allow(clippy::unnecessary_cast)]
    let hard: u64 = limit.rlim_max as u64;
    let Some(desired) = desired_nofile_soft_limit(current, hard, minimum_soft_limit) else {
        return;
    };

    let updated = libc::rlimit {
        rlim_cur: desired as libc::rlim_t,
        rlim_max: limit.rlim_max,
    };
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &updated) } == 0 {
        report(
            Level::Info,
            &format!("Raised RLIMIT_NOFILE soft limit from {current} to {desired} (hard={hard})"),
        );
    } else {
        report(
            Level::Warn,
            &format!(
                "Failed to raise RLIMIT_NOFILE from {current} toward {desired} (hard={hard}): {}",
                io::Error::last_os_error()
            ),
        );
    }
}

/// Create a symlink.
pub(crate) fn symlink_or_copy(src: &Path, dst: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

/// Set file permissions to owner read/write/execute (0o755).
pub(crate) fn set_permissions_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let perms = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(path, perms)
}

/// Summarize the current process's file-descriptor usage for debugging reload or
/// connect failures such as EMFILE/`Too many open files`.
pub(crate) fn process_fd_diagnostic_snapshot() -> String {
    let pid = std::process::id();
    let fd_dir = Path::new("/proc/self/fd");
    let mut total = 0usize;
    let mut sockets = 0usize;
    let mut pipes = 0usize;
    let mut anon = 0usize;
    let mut chars = 0usize;
    let mut regs = 0usize;
    let mut dirs = 0usize;
    let mut other = 0usize;

    if let Ok(entries) = std::fs::read_dir(fd_dir) {
        for entry in entries.flatten() {
            total += 1;
            let target = match std::fs::read_link(entry.path()) {
                Ok(path) => path.to_string_lossy().into_owned(),
                Err(_) => String::new(),
            };
            if target.starts_with("socket:") {
                sockets += 1;
            } else if target.starts_with("pipe:") {
                pipes += 1;
            } else if target.starts_with("anon_inode:") {
                anon += 1;
            } else if target.starts_with("/dev/") {
                chars += 1;
            } else if target.starts_with('/') {
                match std::fs::metadata(&target) {
                    Ok(meta) if meta.is_file() => regs += 1,
                    Ok(meta) if meta.is_dir() => dirs += 1,
                    Ok(_) | Err(_) => other += 1,
                }
            } else {
                other += 1;
            }
        }
    }

    let (soft_limit, hard_limit) =
        read_max_open_files_limits().unwrap_or_else(|| ("unknown".to_string(), "unknown".to_string()));

    format!(
        "pid={} fds={} soft_limit={} hard_limit={} kinds={{socket:{}, pipe:{}, anon_inode:{}, char:{}, file:{}, dir:{}, other:{}}}",
        pid, total, soft_limit, hard_limit, sockets, pipes, anon, chars, regs, dirs, other
    )
}

fn read_max_open_files_limits() -> Option<(String, String)> {
    let contents = match std::fs::read_to_string("/proc/self/limits") {
        Ok(contents) => contents,
        Err(_) => return None,
    };
    contents.lines().find_map(|line| {
        let parts: Vec<_> = line.split_whitespace().collect();
        (parts.len() >= 5 && parts[0] == "Max" && parts[1] == "open" && parts[2] == "files")
            .then(|| (parts[3].to_string(), parts[4].to_string()))
    })
}

/// How `metadata` describes the file's type to a user.
pub(crate) fn describe_file_kind(metadata: &std::fs::Metadata) -> &'static str {
    use std::os::unix::fs::FileTypeExt;

    if metadata.is_dir() {
        return "directory";
    }
    let kind = metadata.file_type();
    if kind.is_socket() {
        return "socket";
    }
    if kind.is_fifo() {
        return "fifo";
    }
    if kind.is_block_device() {
        return "block device";
    }
    if kind.is_char_device() {
        return "character device";
    }
    "non-regular file"
}

/// Prepare `options` to open the private credential file at `path`.
///
/// `O_NOFOLLOW` refuses a symlink at the leaf and `O_NONBLOCK` keeps a FIFO the
/// opener did not expect from blocking the process forever; the mode is a
/// filter in the open call itself, so the file never exists world-readable.
pub(crate) fn harden_credential_open(options: &mut std::fs::OpenOptions, _path: &Path) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).mode(0o600);
    Ok(())
}

/// Repair the permissions of an already-open file through its descriptor.
pub(crate) fn set_permissions_owner_only_file(file: &std::fs::File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    file.set_permissions(std::fs::Permissions::from_mode(0o600))
}

/// Verify `metadata` describes a single-link regular file owned by this user.
pub(crate) fn verify_private_file_metadata(metadata: &std::fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    // SAFETY: `geteuid` has no preconditions and never fails.
    let euid = unsafe { libc::geteuid() };
    if !metadata.is_file() || metadata.uid() != euid || metadata.nlink() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "a credential file must be a single-link regular file owned by this user",
        ));
    }
    Ok(())
}

/// Verify `metadata` describes a directory owned by this user.
pub(crate) fn verify_private_dir_metadata(metadata: &std::fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    // SAFETY: `geteuid` has no preconditions and never fails.
    let euid = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.uid() != euid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "a credential directory must be a directory owned by this user",
        ));
    }
    Ok(())
}

/// Copy `src` to `dst` with the filesystem's copy-on-write primitive.
#[cfg(target_os = "macos")]
pub(crate) fn clone_tree(src: &Path, dst: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    unsafe extern "C" {
        // Declared here because the pinned `libc` does not expose `clonefile`.
        fn clonefile(src: *const libc::c_char, dst: *const libc::c_char, flags: u32) -> libc::c_int;
    }

    let src = CString::new(src.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source path contains a NUL byte"))?;
    let dst = CString::new(dst.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "destination path contains a NUL byte"))?;
    // SAFETY: both pointers are valid NUL-terminated strings for the duration of
    // the call, and `dst` does not exist (checked by the caller).
    let rc = unsafe { clonefile(src.as_ptr(), dst.as_ptr(), 0) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Copy `src` to `dst` with `cp`'s reflink request, which fails instead of
/// degrading to a full copy when the filesystem has no reflink support.
#[cfg(target_os = "linux")]
pub(crate) fn clone_tree(src: &Path, dst: &Path) -> io::Result<()> {
    let status = Command::new("cp")
        .arg("-a")
        .arg("--reflink=always")
        .arg(src)
        .arg(dst)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("cp -a --reflink=always exited with {status}")))
    }
}

/// Other Unix targets have no copy-on-write directory clone this process can
/// request portably.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn clone_tree(_src: &Path, _dst: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this platform has no copy-on-write directory clone",
    ))
}

/// Suppress a console window. Consoles are a Windows concept, so a Unix child
/// of a windowless process is already windowless.
pub(crate) fn hide_console_window(_cmd: &mut Command) {}

/// How long each spawned helper holds the lock before it has to be replaced.
///
/// Bounding this is what makes an orphaned lock self-heal after the daemon is
/// reloaded with `execv` or killed: the helper exits on its own.
const INHIBIT_TTL: Duration = Duration::from_secs(150);

/// Replace the helper once it has been held this long, well below
/// [`INHIBIT_TTL`] so coverage never lapses between reconcile ticks.
const INHIBIT_REFRESH_AFTER: Duration = Duration::from_secs(90);

/// A sleep inhibitor backed by a bounded helper process.
///
/// Linux runs `systemd-inhibit`, which blocks sleep and lid-switch handling
/// through logind; macOS runs `caffeinate -i -s`. The helper holds the lock
/// only for its own lifetime, so a crashed or reloaded daemon cannot leave the
/// machine unable to sleep.
pub struct UnixInhibitor {
    child: Child,
    started: Instant,
}

impl SleepInhibitor for UnixInhibitor {
    fn supported() -> bool {
        cfg!(any(target_os = "linux", target_os = "macos"))
    }

    fn start() -> io::Result<Self> {
        let child = inhibit_command(INHIBIT_TTL).spawn()?;
        Ok(Self {
            child,
            started: Instant::now(),
        })
    }

    fn state(&mut self) -> InhibitorState {
        match self.child.try_wait() {
            Ok(None) => InhibitorState::Holding,
            Ok(Some(status)) if status.success() => InhibitorState::ExitedCleanly,
            Ok(Some(_)) | Err(_) => InhibitorState::Failed,
        }
    }

    fn needs_refresh(&self) -> bool {
        self.started.elapsed() >= INHIBIT_REFRESH_AFTER
    }

    fn stop(&mut self) {
        match self.child.try_wait() {
            // `state` may already have reaped it; there is nothing left to stop.
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(error) => {
                report(
                    Level::Warn,
                    &format!("power_inhibit: failed to inspect inhibitor process: {error}"),
                );
                return;
            }
        }
        if let Err(error) = self.child.kill() {
            report(
                Level::Warn,
                &format!("power_inhibit: failed to stop inhibitor process: {error}"),
            );
        }
        if let Err(error) = self.child.wait() {
            report(
                Level::Warn,
                &format!("power_inhibit: failed to reap inhibitor process: {error}"),
            );
        }
    }
}

impl Drop for UnixInhibitor {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The helper command that holds the inhibition for `ttl`.
fn inhibit_command(ttl: Duration) -> Command {
    let mut command;
    #[cfg(target_os = "macos")]
    {
        command = Command::new("caffeinate");
        // -i prevents idle sleep. -s prevents system sleep while on AC power.
        // We intentionally do not use -d so the display can still sleep/turn
        // off. -t bounds the assertion so a crashed/reloaded daemon self-heals.
        command.arg("-i").arg("-s").arg("-t").arg(ttl.as_secs().to_string());
    }
    #[cfg(not(target_os = "macos"))]
    {
        command = Command::new("systemd-inhibit");
        command
            .arg("--what=sleep:handle-lid-switch")
            .arg("--who=horon-agent")
            .arg("--why=HoronAgent is streaming or processing active work")
            .arg("--mode=block")
            // Power inhibition is best-effort and must never trigger an
            // interactive Polkit authentication prompt from the daemon.
            .arg("--no-ask-password")
            .arg("sleep")
            .arg(ttl.as_secs().to_string());
    }
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    command
}

/// The command that runs `command` through the platform shell.
pub(crate) fn shell_command(command: &str, temp_dir: Option<&Path>) -> Command {
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(command);
    if let Some(dir) = temp_dir {
        cmd.env("TMPDIR", dir).env("HORON_AGENT_SCRATCH_DIR", dir);
    }
    cmd
}

/// [`shell_command`] with the command text passed as a normal argument.
///
/// A POSIX shell takes the command text as one argv element either way, so this
/// is the same call.
pub(crate) fn shell_command_escaped(command: &str, temp_dir: Option<&Path>) -> Command {
    shell_command(command, temp_dir)
}

/// Working directories of the processes this kernel exposes through `/proc`.
#[cfg(target_os = "linux")]
pub(crate) fn process_working_dirs() -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.bytes().all(|byte| byte.is_ascii_digit()))
        })
        .filter_map(|entry| {
            let path = entry.path().join("cwd");
            match std::fs::read_link(&path) {
                Ok(cwd) => Some(cwd),
                Err(err) => {
                    // The process exited between the `/proc` listing and this
                    // read, which the kernel reports as ENOENT; its working
                    // directory is simply not part of the snapshot.
                    if err.kind() != std::io::ErrorKind::NotFound {
                        report(Level::Debug, &format!("cannot read {}: {err}", path.display()));
                    }
                    None
                }
            }
        })
        .collect()
}

/// This process's memory facts from `/proc`, where the kernel reports them.
#[cfg(target_os = "linux")]
pub(crate) fn process_memory_facts() -> horon_agent_platform_api::ProcessMemoryFacts {
    use horon_agent_platform_api::ProcessMemoryFacts;

    match std::fs::read_to_string("/proc/self/status") {
        Ok(status) => ProcessMemoryFacts {
            rss_bytes: proc_status_bytes(&status, "VmRSS:"),
            peak_rss_bytes: proc_status_bytes(&status, "VmHWM:"),
            virtual_bytes: proc_status_bytes(&status, "VmSize:"),
            thread_count: proc_status_count(&status, "Threads:"),
            main_stack_bytes: proc_status_bytes(&status, "VmStk:"),
            fd_count: fd_count(),
        },
        Err(_) => ProcessMemoryFacts {
            fd_count: fd_count(),
            ..ProcessMemoryFacts::default()
        },
    }
}

/// Open descriptors, from the entries in `/proc/self/fd`.
#[cfg(target_os = "linux")]
fn fd_count() -> Option<u64> {
    let Ok(entries) = std::fs::read_dir("/proc/self/fd") else {
        return None;
    };
    let count = entries.flatten().count();
    // Drop the directory handle's own entry so two samples taken the same way
    // compare equal.
    Some(u64::try_from(count.saturating_sub(1)).unwrap_or(0))
}

/// Parse a `/proc` byte counter such as `VmRSS:\t  1234 kB`.
#[cfg(target_os = "linux")]
fn proc_status_bytes(status: &str, key: &str) -> Option<u64> {
    status.lines().find_map(|line| {
        let trimmed = line.trim_start();
        let value = trimmed.strip_prefix(key)?.trim();
        let mut parts = value.split_whitespace();
        let number = match parts.next()?.parse::<u64>() {
            Ok(number) => number,
            Err(_) => return None,
        };
        let unit = parts.next().unwrap_or("kB");
        Some(match unit {
            "kB" | "KB" | "kb" => number.saturating_mul(1024),
            "mB" | "MB" | "mb" => number.saturating_mul(1024 * 1024),
            "gB" | "GB" | "gb" => number.saturating_mul(1024 * 1024 * 1024),
            _ => number,
        })
    })
}

/// Parse a unit-less `/proc` counter such as `Threads:\t10`.
#[cfg(target_os = "linux")]
fn proc_status_count(status: &str, key: &str) -> Option<u64> {
    status.lines().find_map(|line| {
        let rest = line.trim_start().strip_prefix(key)?;
        match rest.split_whitespace().next()?.parse::<u64>() {
            Ok(count) => Some(count),
            Err(err) => {
                report(Level::Debug, &format!("cannot parse {key} from /proc status: {err}"));
                None
            }
        }
    })
}

/// Set the short process name the kernel reports for this process.
///
/// The Linux name buffer is 16 bytes including its terminator, so a longer name
/// is truncated.
#[cfg(target_os = "linux")]
pub(crate) fn set_process_name(name: &str) {
    let mut comm = [0u8; 16];
    let bytes = name.as_bytes();
    let len = bytes.len().min(comm.len().saturating_sub(1));
    comm[..len].copy_from_slice(&bytes[..len]);
    // SAFETY: `comm` is a NUL-terminated 16-byte buffer, which is exactly what
    // `PR_SET_NAME` reads. A refused name change only costs a cosmetic `ps`
    // label, so it is reported and not turned into a failure.
    let result = unsafe { libc::prctl(libc::PR_SET_NAME, comm.as_ptr(), 0, 0, 0) };
    if result < 0 {
        report(
            Level::Debug,
            &format!("cannot set the process name: {}", std::io::Error::last_os_error()),
        );
    }
}

/// Set the short process name this process reports.
///
/// The other Unix targets take the process name from the executable, so there
/// is nothing to set.
#[cfg(not(target_os = "linux"))]
pub(crate) fn set_process_name(_name: &str) {}

/// Whether this desktop can launch a browser for the user.
pub(crate) fn browser_launcher_state() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        // `open` ships with the operating system.
        Some(true)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let has_display = ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|key| std::env::var(key).is_ok_and(|value| !value.trim().is_empty()));
        if !has_display {
            // A launcher with no display server cannot show anything.
            return Some(false);
        }
        let has_launcher = std::env::var("PATH")
            .is_ok_and(|path| std::env::split_paths(&path).any(|dir| dir.join("xdg-open").is_file()));
        // A display but no launcher is unusual; stay optimistic rather than
        // downgrading a user who has some other mechanism.
        has_launcher.then_some(true)
    }
}

/// Whether this process runs inside a container that hides the desktop.
pub(crate) fn is_container() -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new("/.dockerenv").exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Ask the process group led by `pid` to stop with `SIGTERM`.
pub(crate) fn request_graceful_stop(pid: u32) -> io::Result<StopRequest> {
    signal_detached_process_group(pid, libc::SIGTERM)?;
    Ok(StopRequest::Signaled)
}

/// Force the process group led by `pid` to exit with `SIGKILL`.
pub(crate) fn force_stop_process_group(pid: u32) -> io::Result<()> {
    signal_detached_process_group(pid, libc::SIGKILL)
}

/// Whether the parent of this process is already gone, after arming the
/// parent-death signal where this kernel supports one.
///
/// Only Linux can tie a child's death to its parent's. The other Unix targets
/// report `false`: they keep the process alive and leave shutdown to the caller.
#[cfg(target_os = "linux")]
pub(crate) fn bind_process_lifetime_to_parent() -> bool {
    // Capture the parent first, then check it again after `prctl`. This closes
    // the race where the launcher exits immediately before the signal is armed.
    // Safety: `getppid` has no preconditions and does not dereference pointers.
    let parent_pid = unsafe { libc::getppid() };
    // Safety: `PR_SET_PDEATHSIG` accepts a signal number as its scalar argument.
    let armed = unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) } == 0;
    // Safety: `getppid` has no preconditions and does not dereference pointers.
    let current_parent_pid = unsafe { libc::getppid() };
    armed
        && (parent_is_orphan_adopter(parent_pid)
            || current_parent_pid != parent_pid
            || parent_is_orphan_adopter(current_parent_pid))
}

/// Whether the parent of this process is already gone, after arming the
/// parent-death signal where this kernel supports one.
#[cfg(not(target_os = "linux"))]
pub(crate) fn bind_process_lifetime_to_parent() -> bool {
    false
}

/// The processes that adopt an orphan on Linux: pid 1 in a container, and the
/// user-session managers that take over when a launcher dies.
#[cfg(target_os = "linux")]
fn is_orphan_adopter_name(name: &str) -> bool {
    matches!(name.trim(), "init" | "systemd")
}

/// Whether `parent_pid` is one of the processes that adopt an orphan.
#[cfg(target_os = "linux")]
fn parent_is_orphan_adopter(parent_pid: libc::pid_t) -> bool {
    if parent_pid <= 1 {
        return true;
    }
    // `/proc/<pid>/comm` is the process's own short name for itself, including
    // the newline the kernel appends.
    std::fs::read_to_string(format!("/proc/{parent_pid}/comm")).is_ok_and(|name| is_orphan_adopter_name(&name))
}

/// Create a pipe whose ends never leak into any other child this process
/// spawns.
///
/// `pipe2(O_CLOEXEC)` sets close-on-exec on both ends in the same operation
/// that creates them, so there is no window in which a sibling thread's
/// `fork` can capture a descriptor this call is still setting up. Where the
/// kernel has no `pipe2`, the two `fcntl` calls that follow are the best
/// available: they shrink the window but cannot close it, so the leak is only
/// repaired where the platform allows it to be prevented.
fn cloexec_pipe() -> io::Result<[libc::c_int; 2]> {
    let mut fds = [0; 2];
    #[cfg(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "netbsd"
    ))]
    {
        // Safety: `fds` is a valid two-element array for the kernel to fill.
        let created = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
        if created == 0 {
            return Ok(fds);
        }
        let error = io::Error::last_os_error();
        // `ENOSYS` means the kernel predates `pipe2`; every other error is real.
        if error.raw_os_error() != Some(libc::ENOSYS) {
            return Err(error);
        }
    }
    // Safety: `fds` is a valid two-element array for the kernel to fill.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // Safety: both descriptors were just created by this process and are not
    // used anywhere else yet.
    for fd in fds {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
            let error = io::Error::last_os_error();
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return Err(error);
        }
    }
    Ok(fds)
}

/// Spawn `cmd` with a readiness pipe the child inherits.
///
/// The write end survives `exec` so the child can report that it is ready; the
/// parent keeps only the read end and sees end-of-file when the child dies
/// first. The child also becomes its own session leader, as a detached daemon
/// must.
///
/// The child is told the number the write end already has rather than a number
/// chosen here. Writing to a descriptor the child inherits works with any
/// number, but a shell redirecting in the child (`>&N`) can only name a
/// single-digit one, so a program that reports readiness through a shell must
/// read `HORON_AGENT_*_FD` and address it as `/dev/fd/$FD`.
pub(crate) fn spawn_with_ready_pipe(cmd: &mut Command, ready_env_var: &str) -> io::Result<ReadyChild> {
    use std::os::fd::FromRawFd;
    use std::os::unix::process::CommandExt;

    let fds = cloexec_pipe()?;
    let read_fd = fds[0];
    let write_fd = fds[1];

    // Safety: the closure runs in the child between fork and exec, where only
    // async-signal-safe calls are permitted, and both calls are.
    //
    // Clearing close-on-exec here, rather than when the pipe was created, is
    // what limits the flag's absence to the child: in this process the write
    // end keeps it, so no sibling thread's `fork` can leave it open in a child
    // that never clears it.
    unsafe {
        cmd.pre_exec(move || {
            let flags = libc::fcntl(write_fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(write_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(io::Error::last_os_error());
            }
            libc::setsid();
            Ok(())
        });
    }
    cmd.env(ready_env_var, write_fd.to_string());

    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            // Safety: both descriptors belong to this process and nothing else
            // holds them.
            unsafe {
                libc::close(read_fd);
                libc::close(write_fd);
            }
            return Err(error);
        }
    };

    // Close our copy of the write end so the read end reports end-of-file if the
    // child dies before signalling readiness.
    // Safety: the descriptor belongs to this process and is not used again.
    unsafe { libc::close(write_fd) };
    // Safety: `read_fd` is owned by this process and nothing else wraps it.
    let ready = unsafe { std::fs::File::from_raw_fd(read_fd) };
    Ok(ReadyChild {
        child,
        ready: Some(ready),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desired_nofile_soft_limit_only_raises_when_possible() {
        assert_eq!(desired_nofile_soft_limit(1024, 524_288, 8192), Some(8192));
        assert_eq!(desired_nofile_soft_limit(8192, 524_288, 8192), None);
        assert_eq!(desired_nofile_soft_limit(1024, 4096, 8192), Some(4096));
    }

    #[test]
    fn spawn_detached_creates_new_session() {
        let parent_sid = unsafe { libc::getsid(0) };
        assert!(parent_sid > 0, "the test process should belong to a session");

        // The child must outlive the check: `getsid(pid)` answers only while the
        // process exists. `trap '' HUP` keeps this heredoc-free `sh` alive for the
        // check without making it depend on the terminal it just left.
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c")
            .arg("trap '' HUP; sleep 5")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        let mut child = spawn_detached(&mut cmd).expect("spawn detached child");

        // `spawn_detached` calls `setsid()` in the pre-exec hook, so the child is
        // its own session leader: its session id equals its pid, and never the
        // parent's session.
        let child_sid = Instant::now();
        let mut observed = unsafe { libc::getsid(child.id() as libc::pid_t) };
        while observed == -1 && child_sid.elapsed() < Duration::from_secs(1) {
            std::thread::sleep(Duration::from_millis(5));
            observed = unsafe { libc::getsid(child.id() as libc::pid_t) };
        }
        let _ = child.kill();
        // The kill above makes the exit status deliberate; the spawn itself already
        // succeeded, which is what this test asserts.
        let _ = child.wait();

        assert_eq!(
            observed,
            child.id() as i32,
            "detached child should lead its own session"
        );
        assert_ne!(observed, parent_sid, "detached child should not share parent session");
    }

    #[test]
    fn is_process_running_false_for_reaped_pid() {
        // `wait()` reaps the child, so the pid no longer names any process and
        // `kill(pid, 0)` answers ESRCH. The pid is not reused while the
        // assertion runs, because nothing on this host allocates pids that fast.
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn a short-lived child");
        let pid = child.id();
        child.wait().expect("reap the child");

        assert!(!is_process_running(pid), "a reaped pid must not be reported as running");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn init_and_systemd_are_recognized_as_orphan_adopters() {
        assert!(is_orphan_adopter_name("init\n"));
        assert!(is_orphan_adopter_name("systemd\n"));
        assert!(!is_orphan_adopter_name("bash\n"));
        assert!(!is_orphan_adopter_name("horon-agent\n"));
    }

    #[test]
    fn graceful_stop_reaches_a_live_process_group() {
        // A child in its own session is the shape `request_graceful_stop` and
        // `force_stop_process_group` address: the group id is the leader pid.
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c")
            .arg("trap '' TERM; sleep 30")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = spawn_detached(&mut cmd).expect("spawn detached child");
        let pid = child.id();

        assert_eq!(
            request_graceful_stop(pid).expect("SIGTERM reaches the group"),
            StopRequest::Signaled
        );
        // The trap ignores SIGTERM, so the process is still there and the force
        // path has something to end.
        force_stop_process_group(pid).expect("SIGKILL reaches the group");

        let deadline = Instant::now() + Duration::from_secs(5);
        while is_process_running(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = child.wait();
        assert!(!is_process_running(pid), "a forced stop must leave no process behind");
    }

    #[test]
    fn ready_pipe_reports_eof_when_the_child_dies_first() {
        use std::io::Read as _;

        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg("exit 3");
        let mut spawned = spawn_with_ready_pipe(&mut cmd, "HORON_AGENT_TEST_READY_FD").expect("spawn with pipe");
        assert!(spawned.ready.is_some(), "unix hands the child a descriptor");

        let mut ready = spawned.ready.take().expect("read end");
        let mut byte = [0u8; 1];
        assert_eq!(
            ready.read(&mut byte).expect("read readiness"),
            0,
            "child died without signalling"
        );
        let status = spawned.child.wait().expect("reap the child");
        assert_eq!(status.code(), Some(3));
    }

    #[test]
    fn cloexec_pipe_marks_both_ends_close_on_exec() {
        let fds = cloexec_pipe().expect("create a close-on-exec pipe");
        for fd in fds {
            // Safety: the descriptor is open and owned by this call.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            assert!(flags >= 0, "descriptor {fd} should be open");
            assert_eq!(
                flags & libc::FD_CLOEXEC,
                libc::FD_CLOEXEC,
                "descriptor {fd} must be close-on-exec so a concurrent sibling spawn cannot inherit it"
            );
        }
        // Safety: both descriptors belong to this test and are not used again.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    #[test]
    fn ready_pipe_delivers_the_child_signal() {
        use std::io::Read as _;

        // The child addresses the descriptor through `/dev/fd` rather than
        // through `>&$FD`: `sh` here is `dash` on Debian and Ubuntu, whose
        // `>&N` redirection is a syntax error for any number at or above ten,
        // and the number `pipe` returns depends on how many descriptors this
        // process happens to hold. Production's own consumer parses the number
        // and wraps it, so the number is not part of the contract; sending the
        // byte is.
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c")
            .arg("printf %s \"$HORON_AGENT_TEST_READY_FD\"; printf x > /dev/fd/$HORON_AGENT_TEST_READY_FD");
        cmd.stdout(std::process::Stdio::piped());
        let mut spawned = spawn_with_ready_pipe(&mut cmd, "HORON_AGENT_TEST_READY_FD").expect("spawn with pipe");

        let mut ready = spawned.ready.take().expect("read end");
        let mut byte = [0u8; 1];
        assert_eq!(ready.read(&mut byte).expect("read readiness"), 1);
        assert_eq!(&byte, b"x");

        let output = spawned.child.wait_with_output().expect("read the child's report");
        let reported = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let fd: libc::c_int = reported.parse().expect("the child should receive a descriptor number");
        assert!(
            fd > 2,
            "the readiness descriptor must not be a standard stream, got {fd}"
        );
    }

    /// The pipe must not survive into any child this process did not spawn for
    /// it. A leak turns "the daemon died without signalling" into "the pipe
    /// stays silent", so the parent waits out its whole timeout and then falls
    /// back to slow polling.
    #[cfg(target_os = "linux")]
    #[test]
    fn ready_pipe_ends_do_not_reach_an_unrelated_child() {
        use std::io::Read as _;

        let fds = cloexec_pipe().expect("create a close-on-exec pipe");
        let inode = |fd: libc::c_int| -> String {
            std::fs::read_link(format!("/proc/self/fd/{fd}"))
                .expect("the descriptor should be open")
                .to_string_lossy()
                .into_owned()
        };
        let read_target = inode(fds[0]);
        let write_target = inode(fds[1]);

        // A child that never touches the pipe: its descriptor table is the
        // only evidence needed, and it is what a concurrently spawned sibling
        // would have held.
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg("ls -l /proc/self/fd");
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::null());
        let mut child = cmd.spawn().expect("spawn an unrelated child");
        let mut listing = String::new();
        child
            .stdout
            .take()
            .expect("piped stdout")
            .read_to_string(&mut listing)
            .expect("read the child's descriptor table");
        let _ = child.wait();

        assert!(
            !listing.contains(&read_target),
            "the read end ({read_target}) leaked into a child:\n{listing}"
        );
        assert!(
            !listing.contains(&write_target),
            "the write end ({write_target}) leaked into a child:\n{listing}"
        );
        // Safety: both descriptors belong to this test and are not used again.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }
}
