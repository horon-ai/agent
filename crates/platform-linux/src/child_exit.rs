//! Reporting a child's exit without reaping it.
//!
//! `platform-linux` owns this file and compiles it directly; `platform-macos`
//! pulls the same source in with `#[path = ...]`, so the two Unix adapters cannot
//! drift apart. The file never refers to `crate::` items: it is a self-contained
//! module compiled into whichever adapter includes it.

use std::io;

/// Wait for child process `pid` to exit without reaping it.
///
/// `waitid` with `WNOWAIT` reports the exit and leaves the child waitable, so the
/// process-group id it leads stays reserved: a descendant that outlived the
/// leader can still be signalled before the caller reaps it. With `nonblocking`
/// the call reports whether the child has exited and returns immediately.
///
/// The caller must not have reaped the child: `std::process::Child::wait`
/// consumes the wait state this reports on, and a reaped child makes the call
/// fail with `ECHILD`.
pub(crate) fn wait_child_exit(pid: u32, nonblocking: bool) -> io::Result<bool> {
    let mut options = libc::WEXITED | libc::WNOWAIT;
    if nonblocking {
        options |= libc::WNOHANG;
    }
    #[cfg(any(target_os = "freebsd", target_os = "dragonfly"))]
    let id = libc::id_t::from(pid);
    #[cfg(not(any(target_os = "freebsd", target_os = "dragonfly")))]
    let id = pid;
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `info` points at a valid `siginfo_t`, and the kernel fills it
        // only when the wait reports a child.
        let rc = unsafe { libc::waitid(libc::P_PID, id, &mut info, options) };
        if rc == 0 {
            if !nonblocking {
                return Ok(true);
            }
            // SAFETY: a successful `waitid` initializes the reported fields; a
            // child that has not exited leaves `si_pid` zero as POSIX requires,
            // and `P_PID` means a non-zero value can only be this child.
            return Ok(unsafe { info.si_pid() } != 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            // A signal interrupted the wait. The blocking form has to wait again
            // rather than report the interruption as the child's exit state.
            continue;
        }
        return Err(error);
    }
}
