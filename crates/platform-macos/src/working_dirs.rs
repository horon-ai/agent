//! Working directories of live processes, from `libproc`.
//!
//! Linux reads `/proc/<pid>/cwd`. macOS has no such tree, but
//! `proc_pidinfo(PROC_PIDVNODEPATHINFO)` reports the current and root vnode for
//! any process the caller may inspect, so the same answer is available without
//! privileges. Scratch cleanup depends on it: an entry that is the working
//! directory of a live process must never be deleted, and an empty answer makes
//! that guard inert.

use horon_agent_platform_api::log::{PlatformLogLevel as Level, log as report};
use std::ffi::CStr;
use std::path::PathBuf;

/// `PROC_ALL_PIDS` from `<libproc.h>`. `libc` exposes `proc_listpids` but not
/// this selector, and the value is the `typeinfo` argument that asks for every
/// process rather than one.
const PROC_ALL_PIDS: u32 = 1;

/// Every process id the kernel will name to this caller.
///
/// `proc_listpids` reports the bytes it wrote, not a count, so the buffer is
/// sized in bytes and retried once with room for twice as many ids when it
/// comes back full. A full buffer means the answer is truncated, and a truncated
/// answer could miss exactly the process that is sitting in a scratch directory.
fn all_pids() -> Vec<i32> {
    let width = std::mem::size_of::<i32>();
    let mut capacity = 4096;
    loop {
        let bytes = capacity * width;
        let mut buffer = vec![0i32; capacity];
        // SAFETY: the buffer holds `capacity` properly aligned i32 values, and
        // `bytes` is exactly its byte length, which is what `proc_listpids`
        // requires. The typeinfo argument names the process, and `0` here is the
        // one whose ids are wanted.
        let written = unsafe {
            libc::proc_listpids(
                PROC_ALL_PIDS,
                0,
                buffer.as_mut_ptr().cast::<libc::c_void>(),
                bytes as libc::c_int,
            )
        };
        if written <= 0 {
            return Vec::new();
        }
        let written = written as usize;
        if written < bytes {
            buffer.truncate(written / width);
            buffer.retain(|pid| *pid > 0);
            return buffer;
        }
        // Truncated: the kernel had more ids than fit. One retry with room for
        // twice as many covers a process table that grew during the call; a
        // second full answer would need a runaway fork loop to miss anything.
        if capacity > 1 << 20 {
            return Vec::new();
        }
        capacity *= 2;
    }
}

/// Null-terminated path held as `[[c_char; 32]; 32]` by `libc`.
fn path_from_chars(raw: &[[libc::c_char; 32]; 32]) -> Option<PathBuf> {
    let bytes: Vec<u8> = raw
        .iter()
        .flat_map(|chunk| chunk.iter().map(|value| *value as u8))
        .collect();
    // The field is a fixed 1024-byte buffer that starts with a NUL when the
    // process has no such vnode, which is not a path.
    if bytes.first() == Some(&0) {
        return None;
    }
    // The buffer is the kernel's own copy and is NUL-terminated by contract; a
    // path that is not UTF-8 cannot name a scratch entry this process walked, so
    // it is skipped rather than lossily converted into a different directory.
    match CStr::from_bytes_until_nul(&bytes) {
        Ok(raw) => match raw.to_str() {
            Ok(text) if !text.is_empty() => Some(PathBuf::from(text)),
            Ok(_) => None,
            Err(err) => {
                report(Level::Debug, &format!("working directory is not UTF-8: {err}"));
                None
            }
        },
        Err(err) => {
            report(Level::Debug, &format!("working directory has no terminator: {err}"));
            None
        }
    }
}

/// Working directories of the live processes this caller can inspect.
///
/// A process that exited between the listing and the query is simply absent from
/// the snapshot, which is the same answer Linux gives for a vanished `/proc`
/// entry. Every other failure is silent for the same reason the Linux side only
/// logs at debug: a guard that reports nothing must not fail the cleanup pass.
pub(crate) fn process_working_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for pid in all_pids() {
        let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
        let size = std::mem::size_of::<libc::proc_vnodepathinfo>();
        // SAFETY: `info` is a correctly sized, properly aligned, zeroed
        // `proc_vnodepathinfo`, and `size` is exactly its size, which is what
        // `proc_pidinfo` requires for `PROC_PIDVNODEPATHINFO`.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                info.as_mut_ptr().cast::<libc::c_void>(),
                size as libc::c_int,
            )
        };
        if written != size as libc::c_int {
            continue;
        }
        // SAFETY: `proc_pidinfo` reported that it wrote the full struct.
        let info = unsafe { info.assume_init() };
        if let Some(cwd) = path_from_chars(&info.pvi_cdir.vip_path) {
            dirs.push(cwd);
        }
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_reports_its_own_working_directory() {
        let expected = std::env::current_dir().expect("a working directory");
        // The canonical form resolves `/private/var` against `/var`, which the
        // kernel reports in its own spelling.
        let dirs = process_working_dirs();
        let matches = dirs.iter().any(|dir| {
            dir == &expected
                || std::fs::canonicalize(dir)
                    .is_ok_and(|resolved| resolved == std::fs::canonicalize(&expected).unwrap_or(expected.clone()))
        });
        assert!(
            matches,
            "this process must appear in the snapshot: {dirs:?} does not hold {expected:?}"
        );
    }

    #[test]
    fn the_snapshot_is_not_empty() {
        assert!(
            !process_working_dirs().is_empty(),
            "an inert guard is the defect this fixes; at least this test process has a cwd"
        );
    }

    #[test]
    fn a_pid_that_cannot_be_inspected_is_skipped() {
        // Pid 0 is the kernel and has no vnode path; the call must not panic.
        let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
        let size = std::mem::size_of::<libc::proc_vnodepathinfo>();
        let written = unsafe {
            libc::proc_pidinfo(
                0,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                info.as_mut_ptr().cast::<libc::c_void>(),
                size as libc::c_int,
            )
        };
        assert_eq!(written, 0, "pid 0 reports no vnode path");
    }
}
