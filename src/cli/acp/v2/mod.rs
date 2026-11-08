//! ACP v2: per-connection version negotiation, the message builders for every
//! outbound v2 shape, and the prompt lifecycle and foreground state of a v2
//! session, and the mapper that turns its daemon events into v2 updates.
//!
//! `negotiate` and `shapes` touch no connection: each function takes plain
//! values and returns one JSON value, and `tests.rs` validates every builder
//! against the vendored `schema-v2.0.0-alpha.13` fixture. `prompt` and `state`
//! run a v2 session's prompts and its ordered update stream, `foreground` holds
//! the state rules those updates follow, `replay` rebuilds a stored
//! conversation for `session/resume`, `cancel` serves `tool_call/cancel`
//! against the calls this connection advertises, and `background` keeps the
//! session's background work list and serves `terminal/cancel`.

pub(super) mod background;
#[cfg(test)]
mod background_tests;
pub(super) mod cancel;
#[cfg(test)]
mod cancel_tests;
pub(super) mod foreground;
#[cfg(test)]
mod interaction_tests;
pub(super) mod mapper;
#[cfg(test)]
mod mapper_tests;
pub(super) mod negotiate;
pub(super) mod prompt;
#[cfg(test)]
mod prompt_tests;
pub(super) mod replay;
#[cfg(test)]
mod replay_tests;
pub(super) mod shapes;
pub(super) mod state;
#[cfg(test)]
mod tests;
