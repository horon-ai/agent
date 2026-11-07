//! Terminal and stdin state shared by the adapters.

/// Whether a process is currently blocked reading its standard input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StdinState {
    /// The process is blocked in a read on file descriptor 0.
    Reading,
    /// The process exists and is not blocked reading standard input.
    NotReading,
    /// The platform cannot answer, or the process is gone.
    Unknown,
}
