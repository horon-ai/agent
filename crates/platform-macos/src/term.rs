//! macOS stdin-state detection through `libproc`.
//!
//! A process counts as reading standard input when fd 0 is a pipe or a vnode
//! (a terminal) and any of its threads is in a waiting state. macOS has no
//! syscall or wait-channel equivalent of Linux's `/proc`, so this is the
//! strongest answer the platform offers.

use std::mem;

use horon_agent_platform_api::StdinState;

// libproc bindings
unsafe extern "C" {
    fn proc_pidinfo(pid: i32, flavor: i32, arg: u64, buffer: *mut libc::c_void, buffersize: i32) -> i32;
}

const PROC_PIDLISTFDS: i32 = 1;

#[repr(C)]
struct proc_fdinfo {
    proc_fd: i32,
    proc_fdtype: u32,
}

const PROC_PIDTHREADINFO: i32 = 5;
const PROC_PIDLISTTHREADS: i32 = 6;

#[repr(C)]
struct proc_threadinfo {
    pth_user_time: u64,
    pth_system_time: u64,
    pth_cpu_usage: i32,
    pth_policy: i32,
    pth_run_state: i32,
    pth_flags: i32,
    pth_sleep_time: i32,
    pth_curpri: i32,
    pth_priority: i32,
    pth_maxpriority: i32,
    pth_name: [u8; 64],
}

// Taken from `libc` rather than hand-rolled: a local `2` is `TH_STATE_STOPPED`,
// so a thread blocked on `read(0)` never registered. Apple's
// `mach/thread_info.h` has RUNNING=1, STOPPED=2, WAITING=3, and `libc` already
// exposes the constant, so sourcing it removes the opportunity for it to drift.
use libc::TH_STATE_WAITING;

pub(crate) fn stdin_state(pid: u32) -> StdinState {
    if !stdin_is_interactive(pid as i32) {
        return StdinState::NotReading;
    }
    if is_thread_waiting(pid as i32) {
        return StdinState::Reading;
    }
    StdinState::NotReading
}

/// macOS answers about the process itself; there is no descendant walk.
pub(crate) fn process_tree_stdin_state(pid: u32) -> StdinState {
    stdin_state(pid)
}

/// macOS exposes no parent link that avoids scanning every process.
pub(crate) fn direct_children(_pid: u32) -> Vec<u32> {
    Vec::new()
}

fn stdin_is_interactive(pid: i32) -> bool {
    let fd_size = mem::size_of::<proc_fdinfo>() as i32;
    let buf_size = fd_size * 256; // up to 256 fds
    let mut buf = vec![0u8; buf_size as usize];

    let ret = unsafe { proc_pidinfo(pid, PROC_PIDLISTFDS, 0, buf.as_mut_ptr() as *mut libc::c_void, buf_size) };

    if ret <= 0 {
        return false;
    }

    let num_fds = ret / fd_size;
    let fds = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const proc_fdinfo, num_fds as usize) };

    // fd type 1 = vnode (a pty is one), 6 = pipe.
    for fd in fds {
        if fd.proc_fd == 0 {
            return fd.proc_fdtype == 1 || fd.proc_fdtype == 6;
        }
    }

    false
}

fn is_thread_waiting(pid: i32) -> bool {
    let mut thread_ids = vec![0u64; 64];
    let ret = unsafe {
        proc_pidinfo(
            pid,
            PROC_PIDLISTTHREADS,
            0,
            thread_ids.as_mut_ptr() as *mut libc::c_void,
            (thread_ids.len() * mem::size_of::<u64>()) as i32,
        )
    };

    if ret <= 0 {
        return false;
    }

    let num_threads = ret as usize / mem::size_of::<u64>();

    for &thread_id in thread_ids.iter().take(num_threads) {
        let mut tinfo: proc_threadinfo = unsafe { mem::zeroed() };
        let ret = unsafe {
            proc_pidinfo(
                pid,
                PROC_PIDTHREADINFO,
                thread_id,
                &mut tinfo as *mut _ as *mut libc::c_void,
                mem::size_of::<proc_threadinfo>() as i32,
            )
        };

        if ret > 0 && tinfo.pth_run_state == TH_STATE_WAITING {
            return true;
        }
    }

    false
}

pub use crate::unix_term::{
    Pty, detached_shell_command, passes_scratch_dir_to_shell, pty_supported, shell_env_overrides, shell_is_posix,
};
