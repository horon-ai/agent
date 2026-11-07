//! Behaviour of the macOS stdin-state reader.
//!
//! The reader answers from `libproc` and uses thread wait states, so the tests
//! hold a non-interactive fd 0 for the negative case and a real blocked reader
//! for the positive one.

use horon_agent_platform_api::StdinState;

use super::term::stdin_state;

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

/// The positive half, which the own-process test above cannot cover because it
/// detaches stdin to get a deterministic answer: a process whose fd 0 is a pipe
/// and whose thread blocks in read(0) must be reported `Reading`.
#[test]
fn test_blocked_pipe_reader_is_detected() {
    let mut child = std::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn cat");

    let pid = child.id();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let state = loop {
        let state = stdin_state(pid);
        if state == StdinState::Reading || std::time::Instant::now() >= deadline {
            break state;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };

    child.kill().ok();
    child.wait().ok();

    assert_eq!(state, StdinState::Reading, "cat should be waiting for stdin");
}
