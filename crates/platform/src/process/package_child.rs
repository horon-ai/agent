//! Starting one installed package executable as a supervised child.
//!
//! A package is user-supplied code, so it does not inherit this process's
//! environment: the launcher clears it and passes only the operating system's
//! minimum variables plus the names of the package, its installed root and its
//! private state directory. Its home and temporary directory point inside that
//! state directory, so nothing in its environment names the user's home or
//! credentials. The child still runs with that user's permissions: the same
//! user's files and network access remain reachable, and this launcher does not
//! isolate them.
//!
//! [`PackageChild`] is the one owner of the child's lifetime. It hands out the
//! standard streams (std handles, so a Tokio pipeline can adopt them without
//! another process layer), places the child in its own process group, and stops
//! that group as a unit when the caller cancels it. A descendant that survives
//! the leader is still reached, because the group is signalled while the leader
//! is unreaped: after reaping, the group id could belong to another process and
//! is never signalled again.
//!
//! Windows has no process group a signal can reach; the same guarantee needs a
//! dedicated Job Object, which issue #644 adds. Until then the Windows launcher
//! refuses to start a package child instead of running one it cannot stop.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::time::Duration;

use horon_agent_platform_api::log::{self, PlatformLogLevel};

/// Environment variable naming the installed package root.
///
/// The root is also the child's first argument; this variable lets a package
/// read the same value without parsing its argument list.
pub const PACKAGE_ROOT_ENV: &str = "HORON_PACKAGE_ROOT";
/// Environment variable naming the installed package ID.
pub const PACKAGE_ID_ENV: &str = "HORON_PACKAGE_ID";
/// Environment variable naming the package kind: `extension` or `plugin`.
pub const PACKAGE_KIND_ENV: &str = "HORON_PACKAGE_KIND";
/// Environment variable naming the package's private state directory.
pub const PACKAGE_STATE_ENV: &str = "HORON_PACKAGE_STATE";

/// How long a child gets between the graceful request and the hard kill.
///
/// The wire protocol's `shutdown` request is the graceful path a session uses;
/// this grace period only covers a child that is being cancelled without one.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(1);

/// The longest single pause while the grace period runs out.
#[cfg(unix)]
const STOP_POLL: Duration = Duration::from_millis(25);

/// The variables a package inherits from the host process.
///
/// These carry a search path and the locale the user chose, never a credential
/// or a home directory. Everything else is either supplied by the launcher or
/// absent.
#[cfg(unix)]
const INHERITED_ENVIRONMENT: &[&str] = &["PATH", "LANG", "LC_ALL", "LC_CTYPE", "TZ"];

/// The variables a Windows package inherits from the host process.
#[cfg(windows)]
const INHERITED_ENVIRONMENT: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "OS",
];

/// What to start: one installed executable with its package's own directories.
pub struct PackageChildSpec<'a> {
    /// Absolute path of the installed executable.
    pub program: &'a Path,
    /// Arguments after the program name; the first is the installed root.
    pub arguments: &'a [OsString],
    /// Installed copy the child runs from, used as its working directory.
    pub root: &'a Path,
    /// Package ID, for the diagnostics the package reports.
    pub id: &'a str,
    /// `extension` or `plugin`.
    pub kind: &'a str,
    /// Owner-private directory the package may write to.
    pub state_dir: &'a Path,
    /// Graceful-stop grace period; [`DEFAULT_GRACE`] unless tuned.
    pub grace: Duration,
}

/// Start `spec`'s executable as a child this process owns.
///
/// The child is placed in its own process group, receives piped standard
/// streams, and sees the activation environment described in the module
/// documentation.
pub fn spawn(spec: &PackageChildSpec<'_>) -> io::Result<PackageChild> {
    let mut command = activation_command(spec);
    let child = launch(&mut command)?;
    Ok(PackageChild {
        child,
        grace: spec.grace,
        status: None,
    })
}

/// One started package process and the only owner of its lifetime.
pub struct PackageChild {
    child: Child,
    grace: Duration,
    status: Option<ExitStatus>,
}

impl PackageChild {
    /// The leader's process ID; also its process-group ID on a platform with
    /// process groups.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// The grace period [`PackageChild::terminate`] gives the group.
    pub fn grace(&self) -> Duration {
        self.grace
    }

    /// Whether the leader has already been reaped.
    pub fn exited(&self) -> bool {
        self.status.is_some()
    }

    /// Take the child's standard input.
    ///
    /// The handle is a std pipe, so a Tokio pipeline can wrap it with
    /// `tokio::process::ChildStdin::from_std` without a second process layer.
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    /// Take the child's standard output.
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    /// Take the child's standard error.
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    /// Whether the leader has exited, without blocking.
    ///
    /// An exit is reported only after the group has been swept and the leader
    /// reaped, so a descendant that outlived the leader is already gone when this
    /// answers `Some`. The exit is remembered: a second call reports the same
    /// status instead of asking the operating system about a reaped process.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.status {
            return Ok(Some(status));
        }
        if !self.leader_exited()? {
            return Ok(None);
        }
        self.reap_exited().map(Some)
    }

    /// Wait for the leader to exit, sweep its group and reap it.
    ///
    /// The exit is observed without reaping the leader, so its process-group id
    /// is still reserved when the group is killed: a descendant that outlived the
    /// leader does not survive this call.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        let pid = self.child.id();
        super::wait_child_exit(pid, false)?;
        self.reap_exited()
    }

    /// Stop the child's process group and reap the leader.
    ///
    /// The group gets `grace` to exit after the graceful request and is then
    /// killed, so a child that ignores the request, and any descendant that
    /// outlives the leader, still leave no process behind. The leader is signalled
    /// and reaped only while it is still this child's unreaped process: a
    /// process-group id can be reused once its leader is reaped, and it is never
    /// signalled after that. Calling this a second time reports the status the
    /// leader was reaped with.
    #[cfg(unix)]
    pub fn terminate(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        let pid = self.child.id();
        // Asking the group to exit is best effort; a group that refuses the
        // request is still killed below, so the refusal is reported and not
        // treated as the outcome of this call. The leader is unreaped while every
        // signal is sent, even when it already exited on its own, so a descendant
        // that ignored the request is still reached.
        match self.request_stop(pid) {
            Ok(()) => {}
            Err(error) => log::log(
                PlatformLogLevel::Debug,
                &format!("package child {pid} did not accept the graceful stop request: {error}"),
            ),
        }
        // Give the group the grace period, but stop waiting as soon as the leader
        // exits: the hard kill below then reaches whatever ignored the request.
        let deadline = std::time::Instant::now() + self.grace;
        loop {
            if self.leader_exited()? {
                return self.reap_exited();
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                break;
            }
            std::thread::sleep(STOP_POLL.min(deadline - now));
        }
        // The leader is still unreaped, so its group id is reserved and the kill
        // reaches every member, including a descendant that ignored the request.
        self.kill_group(pid)?;
        // The kill is immediate, so this reports as soon as the leader's exit is
        // waitable; the leader is still not reaped, so no signal follows the reap.
        super::wait_child_exit(pid, false)?;
        let status = self.child.wait()?;
        self.status = Some(status);
        Ok(status)
    }

    /// Sweep the group of a leader that exited but is not reaped yet, then reap it.
    ///
    /// Every signal from this owner happens while `std::process::Child` still
    /// holds the leader's wait state, which is what keeps the process-group id
    /// reserved. After the reap below that id may name another process, so it is
    /// never signalled again.
    fn reap_exited(&mut self) -> io::Result<ExitStatus> {
        match <crate::Current as crate::PlatformAdapter>::force_stop_exited_process_group(self.child.id()) {
            Ok(()) => {}
            #[cfg(unix)]
            Err(error) if group_already_gone(&error) => {}
            Err(error) => return Err(error),
        }
        let status = self.child.wait()?;
        self.status = Some(status);
        Ok(status)
    }

    /// Whether the leader has exited, without reaping it.
    fn leader_exited(&self) -> io::Result<bool> {
        super::wait_child_exit(self.child.id(), true)
    }

    /// Stop a package child.
    ///
    /// A Windows child needs the Job Object that issue #644 adds before it can
    /// be stopped as a tree; the launcher never starts one, so reaching this is
    /// a programming error and fails closed.
    #[cfg(windows)]
    pub fn terminate(&mut self) -> io::Result<ExitStatus> {
        Err(io::Error::new(io::ErrorKind::Unsupported, WINDOWS_WITHOUT_JOB))
    }

    /// Ask the process group led by `pid` to exit.
    #[cfg(unix)]
    fn request_stop(&self, pid: u32) -> io::Result<()> {
        match super::request_graceful_stop(pid) {
            Ok(_request) => Ok(()),
            Err(error) if group_already_gone(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Kill the process group led by `pid`.
    #[cfg(unix)]
    fn kill_group(&self, pid: u32) -> io::Result<()> {
        match super::force_stop_process_group(pid) {
            Ok(()) => Ok(()),
            Err(error) if group_already_gone(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }

}

impl Drop for PackageChild {
    /// Stop the child when its owner lets go without a shutdown.
    ///
    /// The platform type is the lifetime owner, so dropping the value returned by
    /// [`spawn`] must not leave a package process behind even when the caller
    /// never called [`PackageChild::terminate`].
    fn drop(&mut self) {
        if self.status.is_some() {
            return;
        }
        if let Err(error) = self.terminate() {
            log::log(
                PlatformLogLevel::Warn,
                &format!(
                    "package child {} could not be stopped on drop: {error}",
                    self.child.id(),
                ),
            );
        }
    }
}

/// `ESRCH` is 3 on every supported Unix and is the only error that means the
/// group holds no process any more; the leader is still this child, so the
/// group id has not been handed to somebody else.
#[cfg(unix)]
fn group_already_gone(error: &io::Error) -> bool {
    error.raw_os_error() == Some(3)
}

/// Build the launch command with its pipes, clears environment and group.
fn activation_command(spec: &PackageChildSpec<'_>) -> Command {
    let mut command = Command::new(spec.program);
    command
        .args(spec.arguments)
        .current_dir(spec.root)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in activation_environment(spec) {
        command.env(key, value);
    }
    super::isolate_process_group(&mut command);
    super::hide_console_window(&mut command);
    command
}

/// The exact environment a package child receives.
fn activation_environment(spec: &PackageChildSpec<'_>) -> Vec<(OsString, OsString)> {
    let mut environment: Vec<(OsString, OsString)> = Vec::new();
    for key in INHERITED_ENVIRONMENT {
        if let Some(value) = std::env::var_os(key) {
            environment.push((OsString::from(key), value));
        }
    }
    environment.push((OsString::from(PACKAGE_ID_ENV), OsString::from(spec.id)));
    environment.push((OsString::from(PACKAGE_KIND_ENV), OsString::from(spec.kind)));
    environment.push((OsString::from(PACKAGE_ROOT_ENV), spec.root.as_os_str().to_owned()));
    environment.push((OsString::from(PACKAGE_STATE_ENV), spec.state_dir.as_os_str().to_owned()));
    environment.extend(package_home_environment(spec.state_dir));
    environment
}

/// The home and temporary directory a package sees: its own state directory.
#[cfg(unix)]
fn package_home_environment(state_dir: &Path) -> Vec<(OsString, OsString)> {
    vec![
        (OsString::from("HOME"), state_dir.as_os_str().to_owned()),
        (OsString::from("TMPDIR"), state_dir.join("tmp").into_os_string()),
    ]
}

/// The Windows equivalents; the Windows launcher refuses to start a child until
/// issue #644 binds it to a Job Object.
#[cfg(windows)]
fn package_home_environment(state_dir: &Path) -> Vec<(OsString, OsString)> {
    let temporary = state_dir.join("tmp").into_os_string();
    vec![
        (OsString::from("USERPROFILE"), state_dir.as_os_str().to_owned()),
        (OsString::from("TEMP"), temporary.clone()),
        (OsString::from("TMP"), temporary),
    ]
}

/// Start the prepared command.
#[cfg(unix)]
fn launch(command: &mut Command) -> io::Result<Child> {
    command.spawn()
}

/// Refuse to start a package child without the Job Object of issue #644.
#[cfg(windows)]
fn launch(_command: &mut Command) -> io::Result<Child> {
    Err(io::Error::new(io::ErrorKind::Unsupported, WINDOWS_WITHOUT_JOB))
}

/// Why the Windows launcher fails closed until issue #644 lands.
#[cfg(windows)]
const WINDOWS_WITHOUT_JOB: &str =
    "package children need the dedicated Job Object (issue #644); refusing to start one before it exists";
