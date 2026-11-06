//! Process values the adapter returns to callers that never branch on the
//! operating system themselves.

use std::fs::File;
use std::process::Child;

/// How a stop request reached the process group it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopRequest {
    /// A catchable termination signal was delivered. The process may take a
    /// moment to exit, and a forced kill can still escalate it.
    Signaled,
    /// The process was terminated immediately, so there is nothing left to
    /// escalate. Windows has no catchable termination signal, only this.
    Terminated,
}

/// A spawned child plus, where the operating system passes descriptors to a
/// child, the read end of its readiness pipe.
pub struct ReadyChild {
    /// The spawned process.
    pub child: Child,
    /// The read end of the child's readiness pipe: the child holds the write
    /// end as the descriptor named by the caller and writes one byte when it is
    /// ready. `None` when this operating system cannot hand a child a
    /// descriptor, in which case the caller polls the child's own effects.
    pub ready: Option<File>,
}

/// The memory facts one diagnostic snapshot records for the current process.
///
/// Every field is `None` when the platform cannot measure it, which keeps the
/// snapshot honest instead of reporting a zero that looks like a measurement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessMemoryFacts {
    /// Resident set size.
    pub rss_bytes: Option<u64>,
    /// Peak resident set size since the process started.
    pub peak_rss_bytes: Option<u64>,
    /// Virtual address space size.
    pub virtual_bytes: Option<u64>,
    /// Number of threads in the process.
    pub thread_count: Option<u64>,
    /// Bytes committed to the main thread's stack, where the platform reports
    /// it separately.
    pub main_stack_bytes: Option<u64>,
    /// Open file descriptors, where the platform can count them cheaply.
    pub fd_count: Option<u64>,
}
