//! Filesystem change notification.
//!
//! Linux can block on `inotify` and wake the moment anything under a watched
//! path changes, which is what a reload handoff needs: the new server writes its
//! marker, and the waiting client should notice without a polling delay. The
//! other Unix targets this crate serves (FreeBSD and friends) have no inotify,
//! so they wait out the timeout instead.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

/// Block until anything under `paths` changes, or until `timeout` elapses.
///
/// A path that cannot be watched is skipped; when nothing can be watched the
/// call still waits out the timeout, so a caller always makes progress on its own
/// schedule.
#[cfg(target_os = "linux")]
pub fn wait_for_any_change(paths: &[PathBuf], timeout: Duration) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    if paths.is_empty() {
        std::thread::sleep(timeout);
        return Ok(());
    }

    let timeout_ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: `inotify_init1` takes only flags and returns a fresh descriptor or
    // -1. The descriptor is closed before this function returns.
    let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
    if fd < 0 {
        let error = io::Error::last_os_error();
        std::thread::sleep(timeout);
        return Err(error);
    }

    let mask = libc::IN_CREATE
        | libc::IN_MOVED_TO
        | libc::IN_ATTRIB
        | libc::IN_MODIFY
        | libc::IN_CLOSE_WRITE
        | libc::IN_DELETE
        | libc::IN_MOVE_SELF
        | libc::IN_DELETE_SELF;

    let mut watched = 0usize;
    for path in paths {
        let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
            continue;
        };
        // SAFETY: `fd` is a live inotify descriptor and `path` is a valid
        // null-terminated string for the duration of the call.
        if unsafe { libc::inotify_add_watch(fd, path.as_ptr(), mask) } >= 0 {
            watched += 1;
        }
    }

    if watched == 0 {
        // SAFETY: `fd` is the descriptor this function created and no other
        // owner exists.
        unsafe { libc::close(fd) };
        std::thread::sleep(timeout);
        return Ok(());
    }

    let mut poll_fd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: `poll_fd` points at one initialized entry for the duration of
        // the call.
        let ready = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
        if ready > 0 && (poll_fd.revents & libc::POLLIN) != 0 {
            let mut buf = [0u8; 512];
            // SAFETY: the buffer is valid for writes and `fd` is the descriptor
            // this function owns. A short or failed read only means the event
            // queue was drained or interrupted, and the caller rechecks state
            // either way.
            if unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, buf.len()) } < 0 {
                // Nothing to do: the wait is over either way.
            }
            break;
        }
        if ready == 0 {
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        // SAFETY: `fd` is still this function's descriptor.
        unsafe { libc::close(fd) };
        return Err(error);
    }

    // SAFETY: `fd` is still this function's descriptor and is closed once.
    unsafe { libc::close(fd) };
    Ok(())
}

/// Wait out `timeout`: this Unix has no `inotify`, and the caller rechecks its
/// own state on that schedule.
#[cfg(not(target_os = "linux"))]
pub fn wait_for_any_change(_paths: &[PathBuf], timeout: Duration) -> io::Result<()> {
    std::thread::sleep(timeout);
    Ok(())
}
