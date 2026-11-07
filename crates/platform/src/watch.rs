//! Filesystem change notification.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

#[cfg(all(unix, not(target_os = "macos")))]
use horon_agent_platform_linux::watch as os;
#[cfg(target_os = "macos")]
use horon_agent_platform_macos::watch as os;
#[cfg(windows)]
use horon_agent_platform_windows::watch as os;

/// Block until anything under `paths` changes, or until `timeout` elapses.
///
/// The wait is an event when the platform has one (`inotify` on Linux) and the
/// timeout itself everywhere else, so a caller never depends on the difference.
pub fn wait_for_any_change(paths: &[PathBuf], timeout: Duration) -> io::Result<()> {
    os::wait_for_any_change(paths, timeout)
}
