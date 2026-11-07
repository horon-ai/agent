//! Behaviour of the Linux stdin-state reader.
//!
//! The reader answers from `/proc`, so these tests run real child processes and
//! inspect the same interfaces the implementation does. They are Linux-only
//! because the whole module is.

use std::process::{Command, Stdio};

use horon_agent_platform_api::StdinState;

use super::term::{direct_children, process_tree_stdin_state, stdin_state};

/// Hold a socket on fd 0 for the duration of a check, restoring the previous
/// stdin on drop.
///
/// A cargo test harness always has a thread blocked somewhere, so with an
/// inherited terminal, pipe or regular file on fd 0 this process cannot be
/// distinguished from one that is reading stdin. A socket is not an interactive
/// stdin stream, so the check answers about the process instead of about the
/// caller's terminal.
struct SocketStdin {
    previous: i32,
}

impl SocketStdin {
    fn install() -> Self {
        let mut ends = [0 as libc::c_int; 2];
        let created = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, ends.as_mut_ptr()) };
        assert_eq!(created, 0, "socketpair failed: {}", std::io::Error::last_os_error());
        let previous = unsafe { libc::dup(0) };
        assert!(previous >= 0, "dup(0) failed: {}", std::io::Error::last_os_error());
        let installed = unsafe { libc::dup2(ends[0], 0) };
        assert!(installed >= 0, "dup2 failed: {}", std::io::Error::last_os_error());
        unsafe {
            libc::close(ends[0]);
            libc::close(ends[1]);
        }
        Self { previous }
    }
}

impl Drop for SocketStdin {
    fn drop(&mut self) {
        unsafe {
            libc::dup2(self.previous, 0);
            libc::close(self.previous);
        }
    }
}

#[test]
fn test_own_process_not_reading_stdin() {
    let _stdin = SocketStdin::install();
    let pid = std::process::id();
    assert_ne!(stdin_state(pid), StdinState::Reading);
}

#[test]
fn test_nonexistent_pid() {
    assert_ne!(stdin_state(u32::MAX), StdinState::Reading);
}

#[test]
fn test_blocked_process_detected() {
    let mut child = Command::new("cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("failed to spawn cat");

    let pid = child.id();
    std::thread::sleep(std::time::Duration::from_millis(200));

    let state = process_tree_stdin_state(pid);

    child.kill().ok();
    child.wait().ok();

    assert_eq!(state, StdinState::Reading, "cat should be waiting for stdin");
}

#[test]
fn test_running_process_not_reading() {
    let mut child = Command::new("sleep")
        .arg("10")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .expect("failed to spawn sleep");

    let pid = child.id();
    std::thread::sleep(std::time::Duration::from_millis(100));

    let state = stdin_state(pid);

    child.kill().ok();
    child.wait().ok();

    assert_eq!(state, StdinState::NotReading, "sleep should not be reading stdin");
}

#[test]
fn test_child_process_tree_detection() {
    // `bash -c "cat"` spawns bash which spawns cat; cat is the one reading.
    let mut child = Command::new("bash")
        .arg("-c")
        .arg("cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("failed to spawn bash");

    let pid = child.id();
    std::thread::sleep(std::time::Duration::from_millis(300));

    let state = process_tree_stdin_state(pid);

    child.kill().ok();
    child.wait().ok();

    assert_eq!(
        state,
        StdinState::Reading,
        "child cat should be detected via process tree"
    );
}

#[test]
fn test_grandchild_process_tree_detection() {
    // Wrapper chain: an outer bash spawns an inner `bash -c cat`, so the actual
    // reader (`cat`) is a grandchild of the tracked pid and the intermediate
    // bash is not itself reading. A trailing `; true` keeps each bash from
    // exec-optimizing itself away, so the nesting is preserved.
    let mut child = Command::new("bash")
        .arg("-c")
        .arg("bash -c 'cat; true'; true")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("failed to spawn bash");

    let pid = child.id();
    std::thread::sleep(std::time::Duration::from_millis(400));

    let state = process_tree_stdin_state(pid);

    child.kill().ok();
    child.wait().ok();

    assert_eq!(
        state,
        StdinState::Reading,
        "grandchild cat should be detected via recursive process-tree walk"
    );
}

#[test]
fn test_direct_children_lists_immediate_children() {
    // A compound command keeps bash alive as the parent of a `sleep` child
    // instead of exec-optimizing itself away.
    let mut child = Command::new("bash")
        .arg("-c")
        .arg("sleep 5; true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .expect("failed to spawn bash");

    let pid = child.id();
    std::thread::sleep(std::time::Duration::from_millis(200));

    let kids = direct_children(pid);

    // Verify parentage before killing the parent, otherwise the child reparents
    // to init (ppid 1) and the check races.
    let mut all_parented_by_pid = !kids.is_empty();
    for kid in &kids {
        let status = std::fs::read_to_string(format!("/proc/{}/status", kid)).unwrap_or_default();
        let ppid = status
            .lines()
            .find_map(|l| l.strip_prefix("PPid:\t"))
            .and_then(|v| v.trim().parse::<u32>().ok());
        if ppid != Some(pid) {
            all_parented_by_pid = false;
        }
    }

    child.kill().ok();
    child.wait().ok();

    assert!(
        !kids.is_empty(),
        "bash should have at least one direct child (the sleep)"
    );
    assert!(
        all_parented_by_pid,
        "every reported child should be parented by {pid}; got {kids:?}"
    );
}

#[test]
fn test_process_that_reads_then_exits() {
    use std::io::Write;

    let mut child = Command::new("head")
        .arg("-n1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("failed to spawn head");

    let pid = child.id();
    std::thread::sleep(std::time::Duration::from_millis(200));

    let state_before = stdin_state(pid);

    if let Some(ref mut stdin) = child.stdin {
        stdin.write_all(b"hello\n").ok();
        stdin.flush().ok();
    }

    let status = child.wait().expect("failed to wait");
    let state_after = stdin_state(pid);

    assert_eq!(state_before, StdinState::Reading, "head should be reading before input");
    assert_ne!(
        state_after,
        StdinState::Reading,
        "head should not be reading after exit"
    );
    assert!(status.success(), "head should exit successfully");
}

#[test]
fn test_process_with_closed_stdin_not_reading() {
    let mut child = Command::new("cat")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .expect("failed to spawn cat");

    let pid = child.id();
    // cat with /dev/null as stdin reads EOF immediately and exits.
    std::thread::sleep(std::time::Duration::from_millis(200));

    let state = stdin_state(pid);

    child.kill().ok();
    child.wait().ok();

    assert_ne!(state, StdinState::Reading);
}

#[test]
fn test_multiple_sequential_reads() {
    use std::io::Write;

    let mut child = Command::new("head")
        .arg("-n2")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("failed to spawn head");

    let pid = child.id();
    std::thread::sleep(std::time::Duration::from_millis(200));

    assert_eq!(
        stdin_state(pid),
        StdinState::Reading,
        "should be waiting for the first line"
    );

    if let Some(ref mut stdin) = child.stdin {
        stdin.write_all(b"line1\n").ok();
        stdin.flush().ok();
    }
    std::thread::sleep(std::time::Duration::from_millis(100));

    assert_eq!(
        stdin_state(pid),
        StdinState::Reading,
        "should be waiting for the second line"
    );

    if let Some(ref mut stdin) = child.stdin {
        stdin.write_all(b"line2\n").ok();
        stdin.flush().ok();
    }

    let status = child.wait().expect("failed to wait");
    assert!(status.success());
}

#[test]
fn direct_children_of_childless_process_does_not_scan_proc() {
    // A childless process must return an empty list through
    // `/proc/<pid>/task/<tid>/children` without falling back to the whole-/proc
    // scan. The syscall count cannot be observed here, but the interface itself
    // must be readable and empty for a leaf process we control, which is the
    // branch condition the fix keys on.
    let mut child = std::process::Command::new("sleep")
        .arg("5")
        .spawn()
        .expect("spawn sleep");
    let pid = child.id();

    let path = format!("/proc/{}/task/{}/children", pid, pid);
    let readable = std::fs::read_to_string(&path).is_ok();
    let children = direct_children(pid);

    let _ = child.kill();
    let _ = child.wait();

    assert!(
        readable,
        "kernel lacks CONFIG_PROC_CHILDREN; fallback scan is expected on this system"
    );
    assert!(children.is_empty(), "sleep should have no children, got {children:?}");
}
