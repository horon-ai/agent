//! End-to-end tests for horon-agent using a mock provider
//!
//! These tests verify the full flow from user input to response
//! without making actual API calls.

#[cfg(unix)]
mod lifecycle_measurements;
mod mock_provider;
mod test_support;

mod ambient;
mod autonomous_approval_failover;
mod binary_integration;
mod burst_spawn;
mod disconnect;
mod durable_runtime_e2e;
mod irc;
#[cfg(unix)]
mod loading_e2e;
mod provider_behavior;
mod reload_multiclient;
mod safety;
mod session_flow;
mod soak_attach_detach;
mod text_framing;
mod transport;
#[cfg(windows)]
mod windows_lifecycle;
