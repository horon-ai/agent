//! Host side of the user-installed Rust package protocol.
//!
//! The shared contract — frames, typed payloads, protocol ranges, capability
//! sets and the declaration digest check — lives in `horon-agent-extension-types`
//! so an author SDK can depend on it without the host. This module owns the
//! parts that need a runtime: the bounded newline reader, the write deadline,
//! request ids, the pending/queue caps, cancellation and shutdown.

pub mod wire;
