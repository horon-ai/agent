//! `horon-agent api-bridge`: launching the harness API listener.
//!
//! Split out of `dispatch.rs` so the bridge's start-up path — stdio on Unix,
//! the shared daemon, the local socket and the optional authenticated TCP
//! listener — reads in one place, and so `dispatch.rs` stays inside its size
//! budget.
use crate::cli::provider_init::ProviderChoice;
use anyhow::Result;

/// The `api-bridge` flags, taken out of the parsed command.
pub(crate) struct Launch {
    /// `--api-socket`, or the environment when absent.
    pub api_socket: Option<String>,
    /// `--stdio`: one API connection on stdin/stdout instead of a socket.
    pub stdio: bool,
    /// `--api-bind`: also serve the API on this TCP address.
    pub api_bind: Option<String>,
    /// `--api-insecure-behind-proxy`: acknowledge a TLS-terminating proxy.
    pub api_insecure_behind_proxy: bool,
}

pub(crate) async fn run(
    provider: &ProviderChoice,
    model: Option<&str>,
    provider_profile: Option<&str>,
    launch: Launch,
) -> Result<()> {
    if launch.stdio {
        // The stdio transport pairs the bridge with an in-process socket,
        // which only the Unix adapter provides. Windows clients reach the same
        // API through the named pipe instead, so the flag is refused there
        // rather than silently starting a listener nobody asked for.
        #[cfg(unix)]
        {
            crate::env::set_var("HORON_AGENT_NON_INTERACTIVE", "1");
            tokio::time::timeout(
                std::time::Duration::from_secs(30),
                super::dispatch::spawn_server(provider, model, provider_profile),
            )
            .await
            .map_err(|_| anyhow::anyhow!("api --stdio: timed out starting the horon-agent server"))??;
            horon_agent_harness_api_server::run_bridge_stdio(horon_agent_harness_api_server::legacy_socket_path())
                .await?;
            return Ok(());
        }
        #[cfg(not(unix))]
        anyhow::bail!("api-bridge --stdio is Unix only; connect to the API socket instead (a named pipe on Windows)");
    }
    // The daemon must be up for the bridge to translate onto, and a user trying
    // the SDK has usually never started one. Starting it here avoids
    // "connection refused".
    //
    // Best-effort on purpose: a spawn can legitimately fail while a usable
    // daemon exists (another build already holds the runtime dir, say).
    // Aborting then would leave the SDK with no endpoint over a daemon that was
    // fine, so report and listen anyway. If the daemon really is absent,
    // per-client dials fail with a message naming the socket, which is the
    // smaller and more accurate error.
    if let Err(error) = super::dispatch::spawn_server(provider, model, provider_profile).await {
        eprintln!("api-bridge: could not start the horon-agent server: {error:#}");
        eprintln!("api-bridge: continuing; an already-running server will still be used");
    }
    let api_socket = launch
        .api_socket
        .map(std::path::PathBuf::from)
        .unwrap_or_else(horon_agent_harness_api_server::api_socket_path);
    // The global `--socket` (and `HORON_AGENT_SOCKET`) already selects the
    // daemon socket; `set_socket_path` exported it during startup.
    let daemon_socket = horon_agent_harness_api_server::legacy_socket_path();
    super::api_credentials::serve_bridge(
        api_socket,
        daemon_socket,
        launch.api_bind,
        launch.api_insecure_behind_proxy,
    )
    .await
}
