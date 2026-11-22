//! The launch request and the lifetime contract one installed package child
//! shares across platforms.
//!
//! A package is user-supplied code, so the launcher that starts it is the one
//! owner of the child's lifetime: the child is started somewhere a single
//! handle can stop it together with everything it spawned, and dropping that
//! owner stops the tree even when the caller never asked. Each platform keeps
//! that promise its own way - a process group on Unix, a dedicated Job Object
//! on Windows - and this module is the shape both implementations answer to.
//! The environment, the arguments and the standard streams are decided by
//! `horon_agent_platform::process::package_child`, which is also the only
//! caller. One implementation per platform answers [`PackageChildProcess`] and
//! never chooses what a package sees.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::{ChildStderr, ChildStdin, ChildStdout, ExitStatus};
use std::time::Duration;

/// One installed executable with the exact environment it receives.
pub struct PackageChildSpawn<'a> {
    /// Absolute path of the installed executable.
    pub program: &'a Path,
    /// Arguments after the program name; the first is the installed root.
    pub arguments: &'a [OsString],
    /// The installed copy, used as the child's working directory.
    pub working_directory: &'a Path,
    /// The complete environment, in the order the launcher wrote it. The child
    /// inherits nothing else.
    pub environment: &'a [(OsString, OsString)],
}

/// A started package process that owns the lifetime of its whole tree.
///
/// The two guarantees a caller relies on are the same on every platform:
///
/// - the child is inside its own stop scope from before it can run any of its
///   own code, so a descendant it spawns cannot escape that scope; and
/// - reporting the leader's exit means the scope was already swept, so a
///   descendant that outlived the leader is gone by the time an exit status
///   comes back.
pub trait PackageChildProcess {
    /// The leader's process ID.
    fn id(&self) -> u32;

    /// Take the child's standard input, if it was not taken before.
    fn take_stdin(&mut self) -> Option<ChildStdin>;

    /// Take the child's standard output, if it was not taken before.
    fn take_stdout(&mut self) -> Option<ChildStdout>;

    /// Take the child's standard error, if it was not taken before.
    fn take_stderr(&mut self) -> Option<ChildStderr>;

    /// Whether the leader's exit was already observed and its tree swept.
    fn exited(&self) -> bool;

    /// Whether the leader has exited, without blocking.
    ///
    /// `Some` is reported only after the tree was swept, so a descendant that
    /// outlived the leader is already gone when this answers `Some`. The status
    /// is remembered: a second call reports it again instead of asking the
    /// operating system about a reaped process.
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>>;

    /// Wait for the leader to exit, sweep its tree and report its status.
    fn wait(&mut self) -> io::Result<ExitStatus>;

    /// Stop the child's tree and report the leader's status.
    ///
    /// A graceful request that was already delivered gets `grace` to finish,
    /// and whatever is still running after that is forced down, descendants
    /// included. Calling this after the leader's exit was observed reports that
    /// status and stops nothing that is already gone, which is what makes it
    /// safe to call from a drop.
    fn terminate(&mut self, grace: Duration) -> io::Result<ExitStatus>;
}
