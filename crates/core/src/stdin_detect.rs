//! Whether a process is blocked reading its standard input.
//!
//! The answer is a platform primitive: `/proc` on Linux answers exactly, macOS
//! answers through `libproc`, and Windows answers `Unknown`. The implementations
//! and their platform-specific tests live in the adapter crates, and this module
//! keeps the crate's long-standing names for the queries.

/// Whether `pid` is blocked reading its standard input.
pub use horon_agent_platform::term::stdin_state as is_waiting_for_stdin;

/// Whether `pid` or a descendant sharing its standard input is blocked reading
/// it.
pub use horon_agent_platform::term::process_tree_stdin_state as check_process_tree;

/// The immediate child PIDs of `pid`.
pub use horon_agent_platform::term::direct_children;

/// The answer a stdin check produced.
pub use horon_agent_platform::term::StdinState;
