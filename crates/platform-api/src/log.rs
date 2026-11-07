//! The log sink the adapter crates report through.
//!
//! The adapters sit below the workspace logging crate, which depends on the
//! application foundation, and the foundation depends on the adapter. Calling
//! the logging crate from an adapter would therefore close a dependency cycle.
//! Adapters report through this sink instead, and `horon-agent-logging`
//! installs its own functions as the sink when it initialises, so the messages
//! reach the same log file at the same levels.
//!
//! Nothing is written before a sink is installed, which matches a process that
//! has not initialised logging yet.

use std::sync::OnceLock;

/// The severity of a platform report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlatformLogLevel {
    /// A failure on this path that the caller could not recover from.
    Error,
    /// A recoverable problem worth recording.
    Warn,
    /// A one-off state change worth recording.
    Info,
    /// Development detail.
    Debug,
}

/// Where the reports go: one function per level, chosen by the sink.
type Sink = fn(PlatformLogLevel, &str);

static SINK: OnceLock<Sink> = OnceLock::new();

/// Install the sink that receives platform reports.
///
/// The first installation wins and later ones are ignored, so the logging
/// crate's own initialisation is the only one that decides where reports go.
pub fn set_log_sink(sink: Sink) {
    // The first installation wins: the sink is fixed for the life of the
    // process, so a late caller cannot move reports that already went out.
    SINK.get_or_init(|| sink);
}

/// Report one message through the installed sink.
pub fn log(level: PlatformLogLevel, message: &str) {
    if let Some(sink) = SINK.get() {
        sink(level, message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU8, Ordering};

    static SEEN: AtomicU8 = AtomicU8::new(0);

    fn record(level: PlatformLogLevel, message: &str) {
        assert_eq!(message, "platform report");
        SEEN.store(
            match level {
                PlatformLogLevel::Error => 1,
                PlatformLogLevel::Warn => 2,
                PlatformLogLevel::Info => 3,
                PlatformLogLevel::Debug => 4,
            },
            Ordering::Relaxed,
        );
    }

    /// A report reaches the installed sink with its own level. Reports sent
    /// before any sink exists are dropped rather than buffered, because the
    /// adapter has nowhere to put them.
    #[test]
    fn a_report_reaches_the_installed_sink() {
        set_log_sink(record);
        log(PlatformLogLevel::Warn, "platform report");
        assert_eq!(SEEN.load(Ordering::Relaxed), 2);
    }
}
