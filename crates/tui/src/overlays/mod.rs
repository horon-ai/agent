//! Modal screens drawn over the transcript.
//!
//! Each overlay is a self-contained state machine: it consumes keystrokes and
//! returns what the caller should do, so the app state stays synchronous and a
//! test can drive the screen without a terminal.

pub mod settings;

#[cfg(test)]
mod tests;
