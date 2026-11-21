//! Process primitives for clients that start a local program.
//!
//! The operating-system branch lives in the platform adapter; this module is
//! the SDK's public name for it, so an application links the SDK and not the
//! platform crate directly. [`shell_command`] builds the shell a command string
//! needs, [`isolate_process_group`] keeps its descendants reachable as one
//! group, and [`terminate_process_group`] stops the tree with the platform's
//! own polite-then-forced sequence.

pub use horon_agent_platform::process::{
    force_stop_process_group, isolate_process_group, kill_process_group, shell_command,
};
pub use horon_agent_platform::signal::{exit_signal, signal_name};
pub use horon_agent_platform::terminate_process_group;
