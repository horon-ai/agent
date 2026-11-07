//! POSIX terminal and shell primitives shared by the macOS and Linux
//! adapters.
//!
//! `platform-linux` owns this file and compiles it directly; `platform-macos`
//! pulls the same source in with `#[path = ...]`, so the two Unix adapters
//! cannot drift apart. The file never refers to `crate::` items: it is a
//! self-contained module compiled into whichever adapter includes it.

use std::fs::File;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

/// Whether this platform can allocate a POSIX pseudo-terminal.
pub fn pty_supported() -> bool {
    true
}

/// A freshly allocated pseudo-terminal.
///
/// The slave end is consumed by [`Pty::configure`] so the parent keeps no copy
/// of it open: a lingering slave descriptor keeps the master end from reaching
/// end-of-file after the child exits.
pub struct Pty {
    master: std::os::fd::OwnedFd,
    slave: Option<std::os::fd::OwnedFd>,
}

impl Pty {
    /// Allocate a terminal with the given window size.
    pub fn open(columns: u16, rows: u16) -> io::Result<Pty> {
        use std::os::fd::{FromRawFd, OwnedFd};

        let mut winsize = libc::winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let mut master_fd: libc::c_int = -1;
        let mut slave_fd: libc::c_int = -1;
        // SAFETY: `openpty` writes two descriptors on success; `name` is unused
        // (null), the termios pointer is null for kernel defaults, and `winsize`
        // points at an initialized struct. glibc declares the window-size
        // parameter as `*const`, macOS as `*mut`, so the raw pointer is built
        // from a mutable borrow that is valid for both ABIs.
        let rc = unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut winsize,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: on success both descriptors are valid, distinct, and owned by
        // nothing else, so each `OwnedFd` is the unique owner.
        let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master_fd), OwnedFd::from_raw_fd(slave_fd)) };
        Ok(Pty {
            master,
            slave: Some(slave),
        })
    }

    /// Point `command`'s stdin, stdout, and stderr at the terminal and make it
    /// the child's controlling terminal.
    pub fn configure(&mut self, command: &mut Command) -> io::Result<()> {
        let slave = self
            .slave
            .take()
            .ok_or_else(|| io::Error::other("terminal slave end already consumed"))?;
        command
            .stdin(std::process::Stdio::from(slave.try_clone()?))
            .stdout(std::process::Stdio::from(slave.try_clone()?))
            .stderr(std::process::Stdio::from(slave));
        // SAFETY: `pre_exec` runs in the forked child before `exec`, so every
        // call below only changes the child's own session, terminal, signal
        // mask, and dispositions; none can affect the parent.
        //
        // `setsid` detaches the child from the daemon's process group so a
        // terminal interrupt cannot reach the daemon. `TIOCSCTTY` then makes the
        // terminal this session's controlling terminal, which is what lets the
        // line discipline generate SIGINT for `^C` at all (an open slave
        // descriptor alone never does) and what interactive programs such as
        // ssh and vim require. The interrupt dispositions are reset because a
        // long-lived agent process commonly ignores or blocks SIGINT, and the
        // child would otherwise inherit that and ignore `^C`.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                libc::sigaddset(&mut set, libc::SIGINT);
                libc::sigaddset(&mut set, libc::SIGQUIT);
                libc::sigprocmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                libc::signal(libc::SIGQUIT, libc::SIG_DFL);
                Ok(())
            });
        }
        Ok(())
    }

    /// A blocking reader on the terminal's master end.
    pub fn reader(&self) -> io::Result<File> {
        Ok(File::from(self.master.try_clone()?))
    }

    /// A blocking writer on the terminal's master end.
    pub fn writer(&self) -> io::Result<File> {
        Ok(File::from(self.master.try_clone()?))
    }
}

/// Whether the default shell reports POSIX `sh` syntax.
pub fn shell_is_posix() -> bool {
    true
}

/// A command that runs `command` text detached from the current session and
/// records its final working directory in `cwd_file` when one is given.
///
/// Returns `None` on platforms with no local shell that can perform the
/// capture.
pub fn detached_shell_command(command: &str, cwd_file: Option<&Path>) -> Option<Command> {
    let mut cmd = Command::new("bash");
    let script = if cwd_file.is_some() {
        r#"eval "$HORON_AGENT_RELOAD_DETACH_COMMAND"; status=$?; pwd > "$HORON_AGENT_CWD_FILE" 2>/dev/null; printf '\n--- Command finished with exit code: %s ---\n' "$status"; exit "$status""#
    } else {
        r#"eval "$HORON_AGENT_RELOAD_DETACH_COMMAND"; status=$?; printf '\n--- Command finished with exit code: %s ---\n' "$status"; exit "$status""#
    };
    cmd.arg("-lc")
        .arg(script)
        .env("HORON_AGENT_RELOAD_DETACH_COMMAND", command);
    if let Some(path) = cwd_file {
        cmd.env("HORON_AGENT_CWD_FILE", path);
    }
    Some(cmd)
}

/// Extra environment entries a POSIX shell needs beyond the shared set.
pub fn shell_env_overrides() -> Vec<(&'static str, String)> {
    Vec::new()
}

/// Whether the platform passes a scratch directory to child shells through
/// `TMPDIR`.
pub fn passes_scratch_dir_to_shell() -> bool {
    true
}
