//! Host side of the user-installed Rust package protocol.
//!
//! The shared contract — frames, typed payloads, protocol ranges, capability
//! sets and the declaration digest check — lives in `horon-agent-extension-types`
//! so an author SDK can depend on it without the host. This module owns the
//! parts that need a runtime: the bounded newline reader, the write deadline,
//! request ids, the pending/queue caps, cancellation and shutdown.
//!
//! It also owns the three session-facing pieces: the revocation registry the
//! daemon runs callbacks through, the per-turn package snapshot a session
//! captures, and the minimal observation events an installed package subscribes
//! to.

/// One call to an installed package child, and the child's lifetime.
pub mod invoke;

/// Revocation dispatch for live package holders.
pub mod lifecycle;

/// The package generation a session's turn observes.
///
/// The file sits beside the turn code under `agent/` because it is that
/// boundary's state. It is declared here so `agent.rs` stays inside its size
/// budget.
#[path = "agent/package_snapshot.rs"]
pub mod package_snapshot;

/// Minimal observation events for installed package children.
pub mod observer;

pub mod wire;
