//! stdio JSONL RPC mode: a line-oriented protocol a host process drives.
//!
//! `horon-agent rpc` (or `horon-agent --mode rpc`) speaks this mode on stdin and
//! stdout: commands in, frames out, one JSON object per line, no chunking.
//! The protocol contract this implements, and the parts of it Horon cannot
//! serve, are in `docs/protocol/RPC.md`.
//!
//! The transport is version 1 only: one JSON object per line, no chunking, so a
//! frame over [`frame::MAX_FRAME_BYTES`] is reported as `rpc_frame_error` rather
//! than sent.

use anyhow::Result;

mod adapter;
mod command;
mod frame;
mod link;
mod mapper;
mod state;
#[cfg(test)]
mod tests;

/// Entry point shared by the `rpc` subcommand and the `--mode rpc` flag.
pub(crate) async fn run_rpc_command(
    provider: crate::cli::provider_init::ProviderChoice,
    model: Option<String>,
    provider_profile: Option<String>,
    session_dir: Option<String>,
    resume: Option<String>,
) -> Result<()> {
    crate::env::set_var("HORON_AGENT_NON_INTERACTIVE", "1");
    // The host does not open a terminal for this process, so tool output must
    // never be routed to one.
    if let Some(dir) = session_dir.as_deref().filter(|dir| !dir.trim().is_empty()) {
        // Accepted for compatibility only: sessions live under the Horon home.
        crate::logging::warn(&format!(
            "rpc: --session-dir {dir} is ignored; sessions are stored under the Horon home"
        ));
    }
    let cwd = std::env::current_dir().ok();
    adapter::run(adapter::Start {
        provider: &provider,
        model: model.as_deref(),
        provider_profile: provider_profile.as_deref(),
        cwd: cwd.as_deref(),
        resume: resume.as_deref(),
        connector: None,
        out: Box::new(tokio::io::stdout()),
        input: Box::new(tokio::io::BufReader::new(tokio::io::stdin())),
    })
    .await
}
