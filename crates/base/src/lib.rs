//! `horon-agent-base`: foundational layer of the horon-agent application core.
//!
//! This crate holds the downward-closed set of modules that the upper
//! server/tool/agent layer (`horon-agent-app-core`) depends on: provider, auth,
//! config, session, message, memory, and their supporting leaves.
//! Splitting it out lets the two halves compile as separate rustc units so the
//! largest compilation unit (and its peak memory) is roughly halved.
//!
//! `horon-agent-app-core` re-exports this crate via `pub use horon_agent_base::*`, so every
//! existing `crate::<module>` path in the upper layers keeps resolving.
// Tests hold the std env/home serialization lock across awaits on purpose.
#![cfg_attr(test, allow(clippy::await_holding_lock))]
#![allow(
    unknown_lints,
    clippy::collapsible_match,
    clippy::manual_checked_ops,
    clippy::unnecessary_sort_by,
    clippy::useless_conversion
)]

pub mod agent_defs;
pub mod artifacts;
pub mod auth;
pub mod aux_completion;
pub mod background;
pub mod bus;
pub mod cache_invalidation;
pub mod cache_tracker;
pub mod claude_live;
pub mod compaction;
pub mod config;
pub mod console;
pub mod context_import;
pub mod context_notes;
pub mod copilot_usage;
pub mod crash;
pub mod detected_emails;
pub mod durable_input;
pub mod durable_runtime;
#[cfg(feature = "embeddings")]
pub mod embedding;
pub mod embedding_backend;
#[cfg(not(feature = "embeddings"))]
pub mod embedding_stub;
pub mod env;
pub mod external_auth;
pub mod foreign_rules;
pub mod gateway;
pub mod git_clone_isolation;
pub mod git_worktree;
pub mod github;
pub mod goal;
pub mod hooks;
pub mod id;
pub mod image_normalize;
pub mod import;
pub mod jev;
pub mod kv_cache_monitor;
pub mod logging;
pub mod managed_skills;
pub mod mcp;
pub mod memory;
pub mod memory_agent;
pub mod memory_graph;
pub mod memory_inspect;
pub mod memory_jev;
pub mod memory_judge_metrics;
pub mod memory_log;
pub mod memory_rerank;
pub mod memory_types;
pub mod message;
pub mod model_pricing;
pub mod model_usage;
pub mod monitor;
pub mod open_url;
pub mod output_style;
pub mod plan;
pub mod power_inhibit;
pub mod process_memory;
pub mod process_title;
pub mod prompt;
pub mod protocol;
pub mod provider;
pub mod provider_activity;
pub mod provider_catalog;
pub mod recent_session_index;
pub mod registry;
pub mod runtime_memory_log;
pub mod safety;
pub mod scratch_maintenance;
pub mod secrets;
pub mod session;
pub mod session_list_cache;
pub mod session_metrics;
pub mod session_prefix;
pub mod sidecar;
pub mod skill;
pub mod skill_install;
pub mod soft_interrupt_store;
pub mod sponsors;
pub mod sqlite_read;
pub mod stdin_detect;
pub mod storage;
pub mod terminal_launch;
pub mod todo;
pub mod transcript_sample;
pub mod transport;
pub mod usage;
pub mod util;
pub mod workspace_trust;
#[cfg(not(feature = "embeddings"))]
pub use embedding_stub as embedding;
pub use horon_agent_core::{terminal_eprint, terminal_eprintln, terminal_print, terminal_println};
