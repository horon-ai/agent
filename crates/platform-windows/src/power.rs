//! Sleep inhibition through the thread execution-state API.
//!
//! Windows ties a request to prevent idle sleep to the calling thread: setting
//! `ES_CONTINUOUS | ES_SYSTEM_REQUIRED` keeps the system awake until the same
//! thread clears it, and the request disappears with the process. The guard
//! therefore runs on a dedicated thread and waits there for the stop request.

use std::io;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use horon_agent_platform_api::{InhibitorState, SleepInhibitor};

use horon_agent_platform_api::log::{PlatformLogLevel as Level, log as report};

/// The request held while streaming or processing active work.
fn execution_state_flags() -> u32 {
    use windows_sys::Win32::System::Power::{ES_CONTINUOUS, ES_SYSTEM_REQUIRED};

    ES_CONTINUOUS | ES_SYSTEM_REQUIRED
}

/// Clearing the request keeps `ES_CONTINUOUS` set, which is the documented way
/// to return the thread to the system's normal power behaviour.
fn clear_execution_state_flags() -> u32 {
    windows_sys::Win32::System::Power::ES_CONTINUOUS
}

/// A held execution-state request, released when the value drops.
pub struct PowerGuard {
    stop_tx: Option<Sender<()>>,
    done_rx: Option<Receiver<io::Result<()>>>,
    thread: Option<JoinHandle<()>>,
}

impl SleepInhibitor for PowerGuard {
    fn supported() -> bool {
        true
    }

    fn start() -> io::Result<Self> {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (stop_tx, stop_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("horon-agent-power-inhibit".to_string())
            .spawn(move || run_power_guard(ready_tx, stop_rx, done_tx))?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                stop_tx: Some(stop_tx),
                done_rx: Some(done_rx),
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                if thread.join().is_err() {
                    return Err(io::Error::other("power guard thread panicked"));
                }
                Err(error)
            }
            Err(error) => {
                if thread.join().is_err() {
                    return Err(io::Error::other("power guard thread panicked"));
                }
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    format!("power guard exited before acquisition: {error}"),
                ))
            }
        }
    }

    fn state(&mut self) -> InhibitorState {
        match self.thread.as_ref() {
            // The guard thread only stops when it is asked to, so a finished
            // thread means the request was lost.
            Some(thread) if !thread.is_finished() => InhibitorState::Holding,
            Some(_) => InhibitorState::Failed,
            None => InhibitorState::ExitedCleanly,
        }
    }

    fn needs_refresh(&self) -> bool {
        // The request lasts as long as the thread and the process do.
        false
    }

    fn stop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            drop(stop_tx.send(()));
        }

        let clear_result = match self.done_rx.take() {
            Some(done_rx) => done_rx
                .recv()
                .map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        format!("power guard exited before cleanup: {error}"),
                    )
                })
                .and_then(|cleared| cleared),
            None => Ok(()),
        };

        let joined = match self.thread.take() {
            Some(thread) => thread.join().is_ok(),
            None => true,
        };

        let result = match (clear_result, joined) {
            (Ok(()), true) => Ok(()),
            (Err(error), _) => Err(error),
            (Ok(()), false) => Err(io::Error::other("power guard thread panicked")),
        };
        if let Err(error) = result {
            report(Level::Warn, &format!("failed to stop the Windows power guard: {error}"));
        }
    }
}

impl Drop for PowerGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_power_guard(
    ready_tx: mpsc::SyncSender<io::Result<()>>,
    stop_rx: Receiver<()>,
    done_tx: mpsc::SyncSender<io::Result<()>>,
) {
    use windows_sys::Win32::System::Power::SetThreadExecutionState;

    let acquired = unsafe { SetThreadExecutionState(execution_state_flags()) };
    if acquired == 0 {
        drop(ready_tx.send(Err(io::Error::last_os_error())));
        return;
    }

    if ready_tx.send(Ok(())).is_err() {
        if unsafe { SetThreadExecutionState(clear_execution_state_flags()) } == 0 {
            report(
                Level::Warn,
                "failed to clear the Windows power guard after the receiver disconnected",
            );
        }
        return;
    }

    drop(stop_rx.recv());
    let cleared = unsafe { SetThreadExecutionState(clear_execution_state_flags()) };
    let result = if cleared == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    };
    drop(done_tx.send(result));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_requests_system_sleep_prevention_only() {
        use windows_sys::Win32::System::Power::{
            ES_AWAYMODE_REQUIRED, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
        };

        let flags = execution_state_flags();
        assert_eq!(flags, ES_CONTINUOUS | ES_SYSTEM_REQUIRED);
        assert_eq!(flags & ES_DISPLAY_REQUIRED, 0);
        assert_eq!(flags & ES_AWAYMODE_REQUIRED, 0);
    }

    #[test]
    fn guard_holds_until_stopped() {
        let mut guard = PowerGuard::start().expect("acquire the power guard");
        assert_eq!(guard.state(), InhibitorState::Holding);
        assert!(!guard.needs_refresh());
        guard.stop();
        assert_eq!(guard.state(), InhibitorState::ExitedCleanly);
    }
}
