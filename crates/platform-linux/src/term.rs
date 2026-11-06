//! Linux stdin-state detection.
//!
//! The answer comes from `/proc`, so it is exact where that interface exists and
//! `Unknown` where it does not: a script reading fd 0 is recognised by the
//! syscall it is blocked in, and a wrapper chain by walking the descendant tree
//! while requiring every hop to share the parent's stdin.

use horon_agent_platform_api::StdinState;

/// Whether `pid` itself is blocked reading standard input.
pub(crate) fn stdin_state(pid: u32) -> StdinState {
    check_inner(pid, false)
}

fn check_inner(pid: u32, strict: bool) -> StdinState {
    // `/proc/PID/syscall` is the exact answer: it names the syscall and the file
    // descriptor it is blocked on.
    if let Ok(contents) = std::fs::read_to_string(format!("/proc/{}/syscall", pid)) {
        // Format: "syscall_nr fd ...". `read` is 0 on x86_64 and 63 on aarch64,
        // and fd 0 is printed as 0x0.
        let parts: Vec<&str> = contents.split_whitespace().collect();
        if parts.len() >= 2 {
            let syscall_nr = parts[0];
            let fd = parts[1];
            let is_read = syscall_nr == "0" || syscall_nr == "63";
            let is_stdin = fd == "0x0";
            if is_read && is_stdin {
                return StdinState::Reading;
            }
        }
    }

    // `wchan` needs no special permission but is less exact, so it only counts
    // when fd 0 is a pipe or a terminal. For descendants the caller additionally
    // verifies the child shares the parent's stdin before using strict mode.
    if let Ok(wchan) = std::fs::read_to_string(format!("/proc/{}/wchan", pid)) {
        let wchan = wchan.trim();
        if (wchan == "n_tty_read"
            || wchan == "wait_woken"
            || wchan == "pipe_read"
            || wchan == "pipe_wait_readable"
            || wchan == "unix_stream_read_generic")
            && stdin_is_pipe_or_pty(pid)
        {
            return StdinState::Reading;
        }
        return StdinState::NotReading;
    }

    if strict {
        StdinState::NotReading
    } else {
        StdinState::Unknown
    }
}

fn stdin_is_pipe_or_pty(pid: u32) -> bool {
    if let Ok(link) = std::fs::read_link(format!("/proc/{}/fd/0", pid)) {
        let path = link.to_string_lossy();
        return path.contains("pipe") || path.contains("pts") || path.contains("ptmx");
    }
    false
}

/// Whether `pid` or a descendant sharing its stdin is blocked reading it.
pub(crate) fn process_tree_stdin_state(pid: u32) -> StdinState {
    let result = stdin_state(pid);
    if result == StdinState::Reading {
        return result;
    }

    // Walk the descendant tree (children, grandchildren, ...) so wrapper chains
    // like `sh -> wrapper -> reader` are detected even when the intermediate
    // process is not itself reading stdin. At each hop we keep the same
    // same-stdin gate: a descendant only counts if it shares fd 0 with `pid`, so
    // unrelated background processes reading their own stdin do not produce a
    // false positive.
    let parent_stdin_link = match std::fs::read_link(format!("/proc/{}/fd/0", pid)) {
        Ok(path) => Some(path.to_string_lossy().to_string()),
        Err(_) => None,
    };

    // Bound the traversal so a pathological tree cannot turn this
    // few-hundred-ms poll into an expensive scan, and guard against cycles.
    const MAX_DEPTH: usize = 32;
    const MAX_VISITED: usize = 512;
    let mut visited: std::collections::HashSet<u32> = std::collections::HashSet::new();
    visited.insert(pid);
    let mut queue: std::collections::VecDeque<(u32, usize)> =
        direct_children(pid).into_iter().map(|child| (child, 1usize)).collect();

    while let Some((child_pid, depth)) = queue.pop_front() {
        if !visited.insert(child_pid) || visited.len() > MAX_VISITED {
            continue;
        }

        if let Some(parent_link) = &parent_stdin_link {
            let child_link = match std::fs::read_link(format!("/proc/{}/fd/0", child_pid)) {
                Ok(path) => Some(path.to_string_lossy().to_string()),
                Err(_) => None,
            };
            if child_link.as_deref() != Some(parent_link) {
                continue;
            }
        }

        if check_inner(child_pid, true) == StdinState::Reading {
            return StdinState::Reading;
        }

        if depth < MAX_DEPTH {
            for grandchild in direct_children(child_pid) {
                queue.push_back((grandchild, depth + 1));
            }
        }
    }

    result
}

/// The direct child PIDs of `pid`.
///
/// `/proc/<pid>/task/<tid>/children` lists only the immediate children of each
/// thread, which avoids scanning every process in `/proc` and reading all of
/// their status files. Every thread is consulted because the kernel tracks
/// children per thread.
pub(crate) fn direct_children(pid: u32) -> Vec<u32> {
    let mut children = Vec::new();
    // Distinguish "the interface works and this process has no children" (the
    // common leaf case) from "the interface is unavailable". Falling back to the
    // whole-`/proc` scan on a merely empty result reintroduced the per-poll scan
    // this function exists to avoid: every polled childless process triggered a
    // scan of every PID's status file on each stdin poll.
    let mut children_interface_readable = false;
    if let Ok(threads) = std::fs::read_dir(format!("/proc/{}/task", pid)) {
        for thread in threads.flatten() {
            let tid = thread.file_name();
            let Some(tid) = tid.to_str() else { continue };
            if let Ok(list) = std::fs::read_to_string(format!("/proc/{}/task/{}/children", pid, tid)) {
                children_interface_readable = true;
                for child in list.split_whitespace() {
                    if let Ok(child_pid) = child.parse::<u32>() {
                        children.push(child_pid);
                    }
                }
            }
        }
    }

    if children_interface_readable {
        return children;
    }

    // Kernels built without CONFIG_PROC_CHILDREN get the original `/proc` scan.
    children_via_proc_scan(pid)
}

fn children_via_proc_scan(pid: u32) -> Vec<u32> {
    let mut children = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            if let Ok(name) = entry.file_name().into_string()
                && let Ok(child_pid) = name.parse::<u32>()
                && let Ok(status) = std::fs::read_to_string(format!("/proc/{}/status", child_pid))
            {
                for line in status.lines() {
                    if let Some(ppid_str) = line.strip_prefix("PPid:\t")
                        && matches!(ppid_str.trim().parse::<u32>(), Ok(found) if found == pid)
                    {
                        children.push(child_pid);
                        break;
                    }
                }
            }
        }
    }
    children
}

pub use crate::unix_term::{
    Pty, detached_shell_command, passes_scratch_dir_to_shell, pty_supported, shell_env_overrides, shell_is_posix,
};
