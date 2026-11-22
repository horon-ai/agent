//! Process liveness by command line, for the "child appeared" and "child gone" checks.
//!
//! The host starts an installed copy of the example from this test's scratch
//! home, and the installed path is unique to that home, so a process whose
//! command line contains it is a package child of this run and nothing else.
//! There is no portable library call for that question, so the test asks the
//! platform's own listing: `ps` on macOS and Linux, PowerShell's CIM view on
//! Windows. Both are present on every runner that runs this target.

use std::process::Command;
use std::time::{Duration, Instant};

/// Poll interval: the Windows listing costs a process start, so it is asked less often.
fn poll_interval() -> Duration {
    if std::env::consts::OS == "windows" {
        Duration::from_millis(500)
    } else {
        Duration::from_millis(100)
    }
}

/// The Windows listing, with the fragment in the environment.
///
/// The fragment cannot be an argument: this probe's own command line would then
/// contain it and the listing would report itself. `$PID` is the probe, which is
/// excluded as well so a PID-shaped coincidence cannot count.
const WINDOWS_LIST: &str = "Get-CimInstance Win32_Process | Where-Object { $_.ProcessId -ne $PID -and $_.CommandLine -ne $null -and $_.CommandLine.Contains($env:PKG_E2E_FRAGMENT) } | ForEach-Object { $_.ProcessId }";

/// PIDs whose command line contains `fragment`.
pub fn pids_running(fragment: &str) -> Vec<u32> {
    let listing = if std::env::consts::OS == "windows" {
        Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", WINDOWS_LIST])
            .env("PKG_E2E_FRAGMENT", fragment)
            .output()
    } else {
        // `-ww` keeps the command line from being truncated at the terminal
        // width, which would hide the install path on a long line.
        Command::new("ps").args(["-axww", "-o", "pid=,command="]).output()
    };
    let output = match listing {
        Ok(output) => output,
        Err(error) => {
            eprintln!("package end-to-end probe: listing processes failed: {error}");
            return Vec::new();
        }
    };
    let mut pids: Vec<u32> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if std::env::consts::OS == "windows" {
                return line.parse::<u32>().ok();
            }
            let (pid, command) = line.split_once(char::is_whitespace)?;
            command.contains(fragment).then(|| pid.parse::<u32>().ok())?
        })
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// Wait until at least one matching process exists, or the deadline passes.
///
/// An empty answer means the wait expired; the caller decides whether that is a
/// failure of the host or of the probe.
pub fn wait_for(fragment: &str, deadline: Duration) -> Vec<u32> {
    let end = Instant::now() + deadline;
    loop {
        let pids = pids_running(fragment);
        if !pids.is_empty() || Instant::now() >= end {
            return pids;
        }
        std::thread::sleep(poll_interval());
    }
}

/// Wait until no matching process is left.
pub fn wait_until_gone(fragment: &str, deadline: Duration) -> Result<(), String> {
    let end = Instant::now() + deadline;
    loop {
        let pids = pids_running(fragment);
        if pids.is_empty() {
            return Ok(());
        }
        if Instant::now() >= end {
            return Err(format!(
                "processes still running with {fragment} in their command line: {pids:?}"
            ));
        }
        std::thread::sleep(poll_interval());
    }
}
