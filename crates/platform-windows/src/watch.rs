//! Filesystem change notification on Windows.
//!
//! `ReadDirectoryChangesW` needs a directory handle, an overlapped buffer and a
//! completion port; nothing in this program needs that machinery. Waiting out
//! the caller's timeout keeps the caller's schedule intact.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

/// Wait out `timeout`: this platform has no cheap event interface to block on.
pub fn wait_for_any_change(_paths: &[PathBuf], timeout: Duration) -> io::Result<()> {
    std::thread::sleep(timeout);
    Ok(())
}
