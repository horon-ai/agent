//! `edit-bench`: an edit benchmark for the coding agent.
//!
//! Three stages, each a subcommand:
//!
//! 1. `generate` mutates one line of real `.rs` files into task fixtures
//!    (`prompt.md`, `input/`, `expected/`, `metadata.json`);
//! 2. `run` executes an arm (a config file) against every fixture through the
//!    real `horon-agent run --ndjson` CLI and appends JSONL result rows;
//! 3. `compare` pairs `(task, rep)` rows from two arms and reports whether the
//!    candidate is better, worse, or indistinguishable at 95% confidence.
//!
//! The comparison is a ratchet: `compare --fail-on-down` exits non-zero when the
//! candidate regresses.

mod rng;

pub mod compare;
pub mod generate;
pub mod mutate;
pub mod results;
pub mod run;
