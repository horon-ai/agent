//! Kernel change waits shared by the real-process suites: a file or directory
//! write, and a process exit.
//!
//! BSD-family kernels report a vnode change through `kqueue`, so a wait wakes on
//! the write itself. Other unix targets have no kqueue equivalent in the standard
//! library, so the same wait polls one short slice at a time. Both forms obey the
//! caller's deadline and report nothing of their own, which keeps the guarantee
//! identical: a probe that passes on macOS proves the same thing on Linux.
#![allow(dead_code)]

use std::path::Path;
use std::time::{Duration, Instant};

/// How long a wait sleeps between two checks of the caller's predicate when no
/// usable change event source exists. Small enough that a probe does not add
/// measurable latency to a local daemon start, large enough not to spin a core
/// while a turn runs.
const POLL_SLICE: Duration = Duration::from_millis(10);

/// Whether the process table reports `pid` as gone. A missing entry means the
/// child was reaped; a zombie is the exit a process event reports before the
/// parent waits for it.
fn process_exited(pid: i32) -> bool {
    #[cfg(target_os = "linux")]
    {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => stat
                .rsplit_once(')')
                .is_some_and(|(_, rest)| matches!(rest.trim_start().chars().next(), Some('Z' | 'X'))),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // SAFETY: signal 0 performs the permission and existence checks only.
        let signalled = unsafe { libc::kill(pid, 0) };
        signalled != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
}

/// The wait used whenever no process event source is available: every non-BSD
/// target, and a BSD target whose kqueue cannot be created.
fn poll_for_pid_exit(pid: i32, deadline: Duration) -> Result<(), String> {
    let end = Instant::now() + deadline;
    loop {
        if process_exited(pid) {
            return Ok(());
        }
        let remaining = end.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("process {pid} still runs after {deadline:?}"));
        }
        std::thread::sleep(remaining.min(POLL_SLICE));
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
mod imp {
    use super::*;

    /// A registered vnode watch on every path the caller passed to
    /// [`ChangeWatch::arm`]. A queue that could not be created degrades to the
    /// polling slice rather than failing the probe: the caller's deadline and
    /// predicate still decide the outcome.
    pub struct ChangeWatch {
        queue: std::ffi::c_int,
        /// The opened files keep the watched vnodes alive: closing the last
        /// descriptor detaches the note, so they are held, never read, for as
        /// long as the watch lives.
        _files: Vec<std::fs::File>,
    }

    impl ChangeWatch {
        /// Registers a write, extension, delete, and rename watch on every
        /// readable path. Arming before the caller scans is what makes a line
        /// written between the scan and the wait still wake it.
        pub fn arm(paths: &[&Path]) -> Self {
            use std::os::fd::AsRawFd;

            let queue = unsafe { libc::kqueue() };
            let mut files = Vec::new();
            if queue >= 0 {
                for path in paths {
                    let Ok(file) = std::fs::File::open(path) else {
                        continue;
                    };
                    let change = libc::kevent {
                        ident: file.as_raw_fd() as libc::uintptr_t,
                        filter: libc::EVFILT_VNODE,
                        flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_CLEAR,
                        fflags: libc::NOTE_WRITE | libc::NOTE_EXTEND | libc::NOTE_DELETE | libc::NOTE_RENAME,
                        data: 0,
                        udata: std::ptr::null_mut(),
                    };
                    unsafe { libc::kevent(queue, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
                    files.push(file);
                }
            }
            Self { queue, _files: files }
        }

        /// Blocks until one watched path changes or the timeout expires.
        pub fn wait(&self, timeout: Duration) {
            if self.queue < 0 {
                std::thread::sleep(timeout.min(POLL_SLICE));
                return;
            }
            let timeout = libc::timespec {
                tv_sec: timeout.as_secs() as libc::time_t,
                tv_nsec: timeout.subsec_nanos() as libc::c_long,
            };
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            unsafe { libc::kevent(self.queue, std::ptr::null(), 0, &mut event, 1, &timeout) };
        }
    }

    impl Drop for ChangeWatch {
        fn drop(&mut self) {
            if self.queue >= 0 {
                unsafe { libc::close(self.queue) };
            }
        }
    }

    /// Waits for process `pid` to exit, on the kernel's exit event. A pid that is
    /// already gone reports success, because the exit the caller waits for has
    /// happened.
    pub fn wait_for_pid_exit(pid: i32, deadline: Duration) -> Result<(), String> {
        let queue = unsafe { libc::kqueue() };
        if queue < 0 {
            return super::poll_for_pid_exit(pid, deadline);
        }
        let change = libc::kevent {
            ident: pid as libc::uintptr_t,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        let registered = unsafe { libc::kevent(queue, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
        if registered < 0 {
            let error = std::io::Error::last_os_error();
            unsafe { libc::close(queue) };
            return if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(())
            } else {
                Err(format!("watch pid {pid}: {error}"))
            };
        }
        let timeout = libc::timespec {
            tv_sec: deadline.as_secs() as libc::time_t,
            tv_nsec: deadline.subsec_nanos() as libc::c_long,
        };
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        let ready = unsafe { libc::kevent(queue, std::ptr::null(), 0, &mut event, 1, &timeout) };
        unsafe { libc::close(queue) };
        if ready == 1 {
            Ok(())
        } else {
            Err(format!("process {pid} still runs after {deadline:?}"))
        }
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
)))]
mod imp {
    use super::*;

    /// Targets without `kqueue` re-check the caller's predicate once per slice.
    pub struct ChangeWatch;

    impl ChangeWatch {
        pub fn arm(_paths: &[&Path]) -> Self {
            Self
        }

        pub fn wait(&self, timeout: Duration) {
            std::thread::sleep(timeout.min(POLL_SLICE));
        }
    }

    pub fn wait_for_pid_exit(pid: i32, deadline: Duration) -> Result<(), String> {
        poll_for_pid_exit(pid, deadline)
    }
}

#[allow(unused_imports)]
pub use imp::{ChangeWatch, wait_for_pid_exit};
