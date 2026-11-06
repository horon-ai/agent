//! Sleep inhibition: keep the machine awake while work is running.

use std::io;

/// How a started inhibitor currently stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InhibitorState {
    /// The assertion is held.
    Holding,
    /// The helper exited cleanly, so it no longer holds anything.
    ExitedCleanly,
    /// The helper failed, or the mechanism was lost.
    Failed,
}

/// One operating system's sleep inhibitor.
///
/// The mechanism differs per platform - a `systemd-inhibit` child on Linux, a
/// `caffeinate` child on macOS, an execution-state thread on Windows - and each
/// one can lose the assertion, so the caller polls [`state`](SleepInhibitor::state)
/// and replaces a helper whose bounded lifetime is about to end.
pub trait SleepInhibitor: Sized {
    /// Whether this platform can inhibit sleep in this process at all.
    fn supported() -> bool;

    /// Start holding the assertion.
    fn start() -> io::Result<Self>;

    /// How the assertion stands now.
    fn state(&mut self) -> InhibitorState;

    /// Whether the assertion must be replaced before its bounded lifetime ends.
    ///
    /// Only a bounded helper can expire; an in-process assertion cannot.
    fn needs_refresh(&self) -> bool;

    /// Release the assertion. Dropping the value does the same.
    fn stop(&mut self);
}
