#![cfg_attr(test, allow(clippy::await_holding_lock))]

use anyhow::{Context, Result};
use std::process::{Command as ProcessCommand, Stdio};
use std::time::Instant;

use super::args::{
    AmbientCommand, ApprovalVerdictArg, ApprovalsCommand, Args, Command, MemoryCommand, ModelCommand, ProviderCommand,
    ServerCommand, SessionCommand,
};
use crate::{agent, auth, build, provider_catalog, server, session, startup_profile};

use super::{acp, commands, debug, hot_exec, output, provider_init, terminal};
use provider_init::ProviderChoice;

fn is_file_controlled_debug_client() -> bool {
    std::env::var_os("HORON_AGENT_DEBUG_CMD_PATH").is_some()
}

/// Tie file-controlled debug clients to the process that launched them.
///
/// These clients are automation helpers, not user-owned terminals. Without a
/// parent-death signal they are reparented to init when a verification script
/// or debug server exits, retaining a full TUI and session history indefinitely.
fn arm_debug_client_parent_death_signal() {
    if !is_file_controlled_debug_client() {
        return;
    }
    // The platform arms its parent-death behaviour and re-checks the parent
    // afterwards, because the launcher can exit before the signal is armed.
    if horon_agent_platform::process::bind_process_lifetime_to_parent() {
        std::process::exit(0);
    }
}

pub(crate) async fn run_main(mut args: Args) -> Result<()> {
    arm_debug_client_parent_death_signal();
    if args.ssh.is_some() {
        // A remote session ID and working directory belong to the remote host.
        // Do not run local resume lookup or provider bootstrap.
        return super::ssh::run(args).await;
    }
    // Explicit MCP management is independent of provider profiles, sessions,
    // and the daemon. In particular, list must remain offline and scriptable.
    if matches!(args.command, Some(Command::Mcp { .. })) {
        let Some(Command::Mcp { action }) = args.command else {
            unreachable!("MCP command checked above");
        };
        return super::mcp::run(action).await;
    }
    // Release installation is a standalone filesystem operation. Installers run
    // it on machines with no configuration yet, and it must not load provider
    // profiles, migrate config, or start the daemon before it installs.
    if let Some(Command::InstallRelease { .. }) = &args.command {
        let Some(Command::InstallRelease {
            assets,
            agent_version,
            source_binary,
        }) = args.command
        else {
            unreachable!("install-release command checked above");
        };
        return super::install_release::run(assets, agent_version, source_binary).await;
    }
    // The standalone picker is the entire command, not a resume ID lookup.
    // In particular, cancelling it must not continue into provider/agent startup
    // with an empty session ID.
    if args.resume.as_deref() == Some("") {
        anyhow::bail!(
            "the interactive session picker is unavailable in this headless build; pass an explicit session id to --resume"
        );
    }
    resolve_resume_arg(&mut args)?;

    // One-time config migration: users whose config.toml still carries the old
    // baked-in `swarm_spawn_mode = "visible"` default get flipped to the
    // current `inline` default. Cheap (single file read, marker-gated), and it
    // must run before the config cache is first populated.
    crate::config::Config::migrate_legacy_swarm_spawn_mode_once();

    if let Some(profile_name) = args
        .provider_profile
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        provider_catalog::apply_named_provider_profile_env(profile_name)?;
        crate::env::set_var("HORON_AGENT_PROVIDER_PROFILE_NAME", profile_name);
        crate::env::set_var("HORON_AGENT_PROVIDER_PROFILE_ACTIVE", "1");
        args.provider = provider_choice_for_named_profile(profile_name);
    }

    if let Some(tool_profile) = args.tool_profile.as_deref() {
        crate::env::set_var("HORON_AGENT_TOOL_PROFILE", tool_profile);
    }
    if let Some(tools) = args.tools.as_deref() {
        crate::env::set_var("HORON_AGENT_TOOLS", tools);
    }
    if let Some(disabled_tools) = args.disabled_tools.as_deref() {
        crate::env::set_var("HORON_AGENT_DISABLED_TOOLS", disabled_tools);
    }
    if args.disable_base_tools {
        crate::env::set_var("HORON_AGENT_DISABLE_BASE_TOOLS", "1");
    }
    if let Some(mcp_tools) = args.mcp_tools.as_deref() {
        crate::env::set_var("HORON_AGENT_MCP_TOOLS", mcp_tools);
    }
    if let Some(threshold) = args.mcp_tools_token_threshold {
        crate::env::set_var("HORON_AGENT_MCP_TOOLS_TOKEN_THRESHOLD", threshold.to_string());
    }
    if args.tool_profile.is_some()
        || args.tools.is_some()
        || args.disabled_tools.is_some()
        || args.disable_base_tools
        || args.mcp_tools.is_some()
        || args.mcp_tools_token_threshold.is_some()
    {
        crate::config::invalidate_config_cache();
    }

    // Headless default tool profile. A plain `serve`/`run`/`api-bridge` launch
    // (or the bare no-subcommand run) that selects no profile exposes only the
    // core tool set. `--tool-profile`, `HORON_AGENT_TOOL_PROFILE`, and `[tools]
    // profile` in config.toml all still override it, so the set can be widened.
    let launches_agent = match &args.command {
        None | Some(Command::Serve { .. }) | Some(Command::Run { .. }) => true,
        #[cfg(unix)]
        Some(Command::ApiBridge { .. }) => true,
        _ => false,
    };
    if launches_agent {
        apply_default_tool_profile(args.tool_profile.as_deref());
    }

    match args.command {
        Some(Command::Mcp { action }) => super::mcp::run(action).await?,
        Some(Command::Serve {
            temporary_server,
            owner_pid,
            temp_idle_timeout_secs,
            server_name,
        }) => {
            let serve_start = Instant::now();
            crate::env::set_var("HORON_AGENT_NON_INTERACTIVE", "1");
            if temporary_server {
                server::configure_temporary_server(owner_pid, temp_idle_timeout_secs);
            }
            let provider_start = Instant::now();
            let provider = provider_init::init_provider_for_serve(&args.provider, args.model.as_deref()).await?;
            let provider_ms = provider_start.elapsed().as_millis();
            let server_new_start = Instant::now();
            let server = server::Server::new_with_name(provider, server_name);
            let server_new_ms = server_new_start.elapsed().as_millis();
            crate::logging::info(&format!(
                "[TIMING] serve bootstrap: provider_init={}ms, server_new={}ms, before_run={}ms",
                provider_ms,
                server_new_ms,
                serve_start.elapsed().as_millis()
            ));
            server.run().await?;
        }
        Some(Command::Acp) => {
            acp::run_acp_command(
                args.provider,
                args.model.clone(),
                args.provider_profile.clone(),
                args.tool_profile.is_some(),
            )
            .await?;
        }
        #[cfg(unix)]
        Some(Command::ApiBridge { api_socket, stdio }) => {
            if stdio {
                crate::env::set_var("HORON_AGENT_NON_INTERACTIVE", "1");
                tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    spawn_server(&args.provider, args.model.as_deref(), args.provider_profile.as_deref()),
                )
                .await
                .map_err(|_| anyhow::anyhow!("api --stdio: timed out starting the horon-agent server"))??;
                horon_agent_harness_api_server::run_bridge_stdio(horon_agent_harness_api_server::legacy_socket_path())
                    .await?;
                return Ok(());
            }
            // The daemon must be up for the bridge to translate onto, and a
            // user running this to try the SDK has usually never started one.
            // Starting it here turns "connection refused, good luck" into a
            // working socket.
            //
            // Best-effort on purpose: a spawn can legitimately fail while a
            // usable daemon exists (another build already holds the runtime
            // dir, say). Aborting then would leave the SDK with no endpoint
            // over a daemon that was fine, so report and listen anyway. If the
            // daemon really is absent, per-client dials fail with a message
            // naming the socket, which is the smaller and more accurate error.
            if let Err(error) =
                spawn_server(&args.provider, args.model.as_deref(), args.provider_profile.as_deref()).await
            {
                eprintln!("api-bridge: could not start the horon-agent server: {error:#}");
                eprintln!("api-bridge: continuing; an already-running server will still be used");
            }
            let api_socket = api_socket
                .map(std::path::PathBuf::from)
                .unwrap_or_else(horon_agent_harness_api_server::api_socket_path);
            // The global `--socket` (and `HORON_AGENT_SOCKET`) already selects the
            // daemon socket; `set_socket_path` exported it during startup.
            let legacy_socket = horon_agent_harness_api_server::legacy_socket_path();
            horon_agent_harness_api_server::run_bridge(api_socket, legacy_socket).await?;
        }
        Some(Command::Server { action }) => match action {
            #[cfg(unix)]
            ServerCommand::Stdio => {
                crate::env::set_var("HORON_AGENT_NON_INTERACTIVE", "1");
                tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    spawn_server_with_executable(
                        &args.provider,
                        args.model.as_deref(),
                        args.provider_profile.as_deref(),
                        Some(std::env::current_exe()?),
                    ),
                )
                .await
                .map_err(|_| anyhow::anyhow!("server stdio: timed out starting the remote daemon"))??;
                super::ssh_transport::run_stdio(server::socket_path()).await?;
            }
            ServerCommand::Start { json } => {
                spawn_server(&args.provider, args.model.as_deref(), args.provider_profile.as_deref()).await?;
                if json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": "running",
                        })
                    );
                } else {
                    println!("HoronAgent server is running.");
                }
            }
            ServerCommand::Keepalive => {
                run_server_keepalive(&args.provider, args.model.as_deref(), args.provider_profile.as_deref()).await?;
            }
            ServerCommand::Promote { version, json } => {
                commands::run_server_promote_command(version.as_deref(), json)?;
            }
            ServerCommand::Reload { force, json } => {
                commands::run_server_reload_command(force, json).await?;
            }
            ServerCommand::Stop { force, json } => {
                commands::run_server_stop_command(force, json).await?;
            }
        },
        Some(Command::Run { message, json, ndjson }) => {
            register_on_demand_server_spawner_for_run(
                args.provider.clone(),
                args.model.clone(),
                args.provider_profile.clone(),
            );
            commands::run_single_message_command(
                &args.provider,
                args.model.as_deref(),
                args.resume.as_deref(),
                &message,
                json,
                ndjson,
            )
            .await?;
        }
        Some(Command::Repl) => {
            let (provider, registry) =
                provider_init::init_provider_and_registry(&args.provider, args.model.as_deref()).await?;
            let mut agent = agent::Agent::new(provider, registry);
            agent.repl().await?;
        }
        Some(Command::Update) => {
            // The updater uses reqwest's blocking client, which must not run on a runtime thread.
            tokio::task::spawn_blocking(hot_exec::run_update).await??;
        }
        Some(Command::InstallRelease { .. }) => unreachable!("install-release handled before bootstrap"),
        Some(Command::Version { json }) => {
            commands::run_version_command(json)?;
        }
        Some(Command::Usage { json }) => {
            commands::run_usage_command(json).await?;
        }
        Some(Command::RuntimeState(command)) => super::runtime_state::run(command)?,
        Some(Command::Debug {
            command,
            arg,
            session,
            socket,
            wait,
        }) => {
            debug::run_debug_command(&command, &arg, session, socket, wait).await?;
        }
        Some(Command::Provider(subcmd)) => match subcmd {
            ProviderCommand::List { json } => {
                commands::run_provider_list_command(json)?;
            }
            ProviderCommand::Current { json } => {
                commands::run_provider_current_command(&args.provider, args.model.as_deref(), json).await?;
            }
            ProviderCommand::Add {
                name,
                base_url,
                model,
                context_window,
                api_key_env,
                api_key,
                api_key_stdin,
                no_api_key,
                auth,
                auth_header,
                env_file,
                set_default,
                overwrite,
                provider_routing,
                model_catalog,
                json,
            } => {
                commands::run_provider_add_command(commands::ProviderAddOptions {
                    name,
                    base_url,
                    model,
                    context_window,
                    api_key_env,
                    api_key,
                    api_key_stdin,
                    no_api_key,
                    auth,
                    auth_header,
                    env_file,
                    set_default,
                    overwrite,
                    provider_routing,
                    model_catalog,
                    json,
                })?;
            }
        },
        Some(Command::Memory(subcmd)) => {
            commands::run_memory_command(map_memory_subcommand(subcmd)).await?;
        }
        Some(Command::Session(subcmd)) => match subcmd {
            // `delete` talks to the daemon, so it cannot go through the
            // synchronous session dispatcher that handles every other
            // subcommand.
            SessionCommand::Delete { session, yes, json } => {
                commands::run_session_delete_command(&session, yes, json).await?;
            }
            other => commands::run_session_command(other)?,
        },
        Some(Command::Export { session, out, json }) => {
            super::sessions::export_markdown(&session, out.as_deref(), json)?;
        }
        Some(Command::Ambient(subcmd)) => {
            commands::run_ambient_command(map_ambient_subcommand(subcmd)).await?;
        }
        Some(Command::Cloud(subcmd)) => {
            super::cloud_move::run_cli(subcmd)?;
        }
        Some(Command::Pair { list, revoke }) => {
            commands::run_pair_command(list, revoke)?;
        }
        Some(Command::Permissions) => {
            anyhow::bail!("the interactive permissions editor is unavailable in this headless build");
        }
        Some(Command::Approvals { action }) => match action {
            ApprovalsCommand::Resolve {
                request_id,
                tool_call_id,
                verdict,
                note,
            } => {
                let store = crate::ambient::BlockedApprovalStore::open_default()?;
                resolve_blocked_approval(&store, &request_id, &tool_call_id, verdict, note.as_deref())?;
            }
        },
        Some(Command::Model(subcmd)) => match subcmd {
            ModelCommand::List { json, verbose } => {
                commands::run_model_command(&args.provider, args.model.as_deref(), json, verbose).await?;
            }
        },
        None => run_default_command(args).await?,
    }

    Ok(())
}

/// Transport for `--provider-profile <name>` (issue #1560: this used to force
/// OpenAI-compatible unconditionally). Anthropic-compatible profiles go
/// through `Auto`, the same path as `default_provider = "<name>"`: the env
/// applied by `apply_named_provider_profile_env` already selects the
/// Anthropic Messages runtime with the profile's base URL and auth, while
/// `AnthropicApi` would pin an Anthropic API-key credential the profile may
/// not have (e.g. `auth = "none"`).
/// Decide the tool profile a launch should use when the user selected none.
///
/// Returns [`crate::config::CORE_TOOL_PROFILE`] only when no profile was chosen
/// by the `--tool-profile` flag, the `HORON_AGENT_TOOL_PROFILE` environment variable,
/// or `[tools] profile` in config.toml. Otherwise returns `None`, so an explicit
/// selection always wins and the tool set can be widened.
fn default_tool_profile_for_launch(
    flag_profile: Option<&str>,
    env_profile: Option<&str>,
    config_profile: &str,
) -> Option<&'static str> {
    let any_selected = flag_profile.map(|value| !value.trim().is_empty()).unwrap_or(false)
        || env_profile.map(|value| !value.trim().is_empty()).unwrap_or(false)
        || !config_profile.trim().is_empty();
    if any_selected {
        None
    } else {
        Some(crate::config::CORE_TOOL_PROFILE)
    }
}

/// Apply the default tool profile for a launch that selected none.
///
/// Sets `HORON_AGENT_TOOL_PROFILE` so later readers see the same value, then
/// patches the already-loaded config. The launch has resolved the config by
/// this point, and invalidating the cache here would make the next `config()`
/// parse the file a second time for a value we just chose.
fn apply_default_tool_profile(flag_profile: Option<&str>) {
    let env_profile = std::env::var("HORON_AGENT_TOOL_PROFILE").ok();
    let config_profile = crate::config::config().tools.profile.clone();
    if let Some(default_profile) =
        default_tool_profile_for_launch(flag_profile, env_profile.as_deref(), &config_profile)
    {
        crate::env::set_var("HORON_AGENT_TOOL_PROFILE", default_profile);
        crate::config::Config::set_loaded_tool_profile(default_profile);
    }
}

fn provider_choice_for_named_profile(profile_name: &str) -> ProviderChoice {
    // Same source as `apply_named_provider_profile_env`, which already
    // validated that the profile exists.
    match crate::config::Config::load_strict() {
        Ok(config) => provider_choice_for_named_profile_in(profile_name, &config),
        Err(_) => provider_choice_for_named_profile_in(profile_name, crate::config::config()),
    }
}

fn provider_choice_for_named_profile_in(profile_name: &str, config: &crate::config::Config) -> ProviderChoice {
    match config.providers.get(profile_name) {
        Some(profile)
            if matches!(
                profile.provider_type,
                crate::config::NamedProviderType::AnthropicCompatible
            ) =>
        {
            ProviderChoice::Auto
        }
        _ => ProviderChoice::OpenaiCompatible,
    }
}

fn resolve_resume_arg(args: &mut Args) -> Result<()> {
    if let Some(ref resume_id) = args.resume {
        let resume_id = resume_id.clone();
        match resolve_resume_id(&resume_id) {
            Ok(full_id) => {
                args.resume = Some(full_id);
            }
            Err(e) => {
                match resume_resolution_failure_action(&resume_id, |key| std::env::var_os(key)) {
                    // During a reload/update/restart handoff the client re-execs
                    // itself with `--resume <id>` and `HORON_AGENT_RESUMING=1`. In the
                    // client/server architecture the shared server is the authority
                    // for session lifecycle, so an id that is not in the local store
                    // can still be valid server-side. Hard-exiting here dumped the
                    // user back to a shell with "No session found matching ...",
                    // making horon-agent unusable after an auto-update (issue #328).
                    // Instead, keep the raw id and let the remote connection resolve
                    // it; if the server cannot find it either, the TUI surfaces a
                    // recoverable message and falls back to a fresh session rather
                    // than killing the process.
                    ResumeResolutionFailureAction::DeferToServer => {
                        crate::logging::warn(&format!(
                            "Resume id '{}' not found locally during reload handoff ({}); deferring resolution to the server instead of exiting",
                            resume_id, e
                        ));
                        // Leave args.resume as the raw id for the server to resolve.
                    }
                    ResumeResolutionFailureAction::Exit => {
                        eprintln!("Error: {}", e);
                        if !output::quiet_enabled() {
                            eprintln!("\nUse `horon-agent --resume` to list available sessions.");
                        }
                        std::process::exit(1);
                    }
                }
            }
        }
    }

    Ok(())
}

/// What to do when a `--resume <id>` cannot be resolved from the local session
/// store. Extracted as a pure function so the reload-handoff recovery path can
/// be unit-tested without invoking `std::process::exit` (issue #328).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResumeResolutionFailureAction {
    /// Keep the raw id and let the shared server resolve it (reload handoff).
    DeferToServer,
    /// No live handoff in progress; the id is genuinely bad, so exit.
    Exit,
}

fn resume_resolution_failure_action<F, V>(_resume_id: &str, var_os: F) -> ResumeResolutionFailureAction
where
    F: Fn(&str) -> Option<V>,
{
    if var_os("HORON_AGENT_RESUMING").is_some() {
        ResumeResolutionFailureAction::DeferToServer
    } else {
        ResumeResolutionFailureAction::Exit
    }
}

fn resolve_resume_id(resume_id: &str) -> Result<String> {
    match session::find_session_by_name_or_id(resume_id) {
        Ok(full_id) => Ok(full_id),
        Err(native_err) => match crate::import::import_external_resume_id(resume_id)? {
            Some(imported_id) => Ok(imported_id),
            None => Err(native_err),
        },
    }
}

mod subcommand_mapping;
use subcommand_mapping::*;

async fn run_default_command(args: Args) -> Result<()> {
    startup_profile::mark("run_main_none_branch");
    // The auth snapshot is read by every client surface, so probe it now on a
    // side thread while the server check below runs.
    crate::auth::AuthStatus::prewarm_fast_in_background();

    let explicit_provider_or_model =
        args.provider != ProviderChoice::Auto || args.model.is_some() || args.provider_profile.is_some();
    let explicit_tool_options =
        args.tool_profile.is_some() || args.tools.is_some() || args.disabled_tools.is_some() || args.disable_base_tools;

    if args.resume.is_none() {
        terminal::show_crash_resume_hint();
    }
    startup_profile::mark("crash_resume_hint");

    startup_profile::mark("client_mode_start");
    let mut server_running = server_is_running().await;
    startup_profile::mark("server_check");

    if !server_running {
        server_running = wait_for_existing_reload_server("client startup").await;
    }

    if !server_running && std::env::var("HORON_AGENT_RESUMING").is_ok() {
        server_running = wait_for_resuming_server(
            "client startup without reload marker",
            std::time::Duration::from_secs(5),
        )
        .await;
    }

    if server_running && explicit_provider_or_model {
        output::stderr_info("Server already running; provider/model flags only apply when starting a new server.");
        output::stderr_info(format!(
            "Current server settings control `/model`. Restart server to apply: --provider {}{}",
            args.provider.as_arg_value(),
            args.model
                .as_ref()
                .map(|m| format!(" --model {}", m))
                .unwrap_or_default()
        ));
    }

    if server_running && explicit_tool_options {
        output::stderr_info(
            "Server already running; tool flags only apply when starting a new server. Restart server or edit [tools] in config.toml to change the active toolset.",
        );
    }

    if !server_running {
        // No live server and no in-flight reload/resume. If a dead socket was
        // left behind by a crashed or upgraded daemon, reap it now so the spawn
        // below binds cleanly instead of wedging the client in a connect-retry
        // loop against a stale socket (issues #277/#291). This only removes a
        // socket that has no live listener AND whose daemon lock is free, so it
        // can never disturb a running server.
        if server::reap_stale_socket_if_dead(&server::socket_path()).await {
            output::stderr_info("Removed a stale horon-agent socket from a previous server.");
        }

        maybe_prompt_server_bootstrap_login(&args.provider).await?;
        spawn_server(&args.provider, args.model.as_deref(), args.provider_profile.as_deref()).await?;
    }

    startup_profile::mark("tui_handoff");
    // The interactive client is a separate binary. On a terminal, and with one
    // installed beside this executable, this process becomes that client, with
    // a resumed session passed along; every other shape (a pipe, `HORON_NO_TUI`,
    // no sibling) keeps the headless message so scripts and CI still see it.
    super::tui_handoff::run(super::tui_handoff::decide_for_process(), args.resume.as_deref())
}

pub(crate) async fn server_is_running() -> bool {
    server_is_running_at(&server::socket_path()).await
}

async fn wait_for_existing_reload_server(context: &str) -> bool {
    if let Some(state) = server::recent_reload_state(std::time::Duration::from_secs(30)) {
        match state.phase {
            server::ReloadPhase::Starting => {
                crate::logging::info(&format!(
                    "Reload state=starting during {}; waiting for existing server to return",
                    context
                ));
                return wait_for_reloading_server().await;
            }
            server::ReloadPhase::Failed => {
                crate::logging::warn(&format!(
                    "Reload state=failed during {} on {}: {}; recent_state={}",
                    context,
                    server::socket_path().display(),
                    state.detail.unwrap_or_else(|| "unknown reload failure".to_string()),
                    server::reload_state_summary(std::time::Duration::from_secs(60))
                ));
            }
            server::ReloadPhase::SocketReady => {}
        }
    }

    false
}

pub(crate) async fn wait_for_resuming_server(context: &str, timeout: std::time::Duration) -> bool {
    let socket_path = server::socket_path();
    let start = std::time::Instant::now();
    let mut announced = false;

    while start.elapsed() < timeout {
        if server_is_running_at(&socket_path).await {
            crate::logging::info(&format!(
                "Server became available during resume wait for {} after {}ms",
                context,
                start.elapsed().as_millis()
            ));
            return true;
        }

        if !announced {
            crate::logging::info(&format!(
                "Server not ready during {}; waiting up to {}ms for a resumed/reloading server before spawning a replacement",
                context,
                timeout.as_millis()
            ));
            announced = true;
        }

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    false
}

pub(crate) async fn wait_for_reloading_server() -> bool {
    match server::await_reload_handoff(&server::socket_path(), std::time::Duration::from_secs(30)).await {
        server::ReloadWaitStatus::Ready => true,
        server::ReloadWaitStatus::Failed(detail) => {
            crate::logging::warn(&format!(
                "Reload handoff failed while waiting for server on {}: {}; recent_state={}",
                server::socket_path().display(),
                detail.unwrap_or_else(|| "unknown reload failure".to_string()),
                server::reload_state_summary(std::time::Duration::from_secs(60))
            ));
            false
        }
        server::ReloadWaitStatus::Idle => false,
        server::ReloadWaitStatus::Waiting { .. } => false,
    }
}

async fn server_is_running_at(path: &std::path::Path) -> bool {
    // Check liveness before performing a protocol handshake. On Windows the
    // named pipe may be busy while another client is connecting; that already
    // proves a daemon exists, while a handshake connect can otherwise wait in
    // the transport's ERROR_PIPE_BUSY retry loop and block server startup.
    server::has_live_listener(path).await || server::is_server_ready(path).await
}

fn spawn_lock_path(socket_path: &std::path::Path) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("{}.spawning", socket_path.display()))
}

async fn acquire_spawn_lock_or_wait(
    socket_path: &std::path::Path,
) -> Result<Option<horon_agent_platform::ipc::InstanceLock>> {
    let lock_path = spawn_lock_path(socket_path);
    // The client acquires this lock before the daemon creates its socket directory.
    if let Some(parent) = lock_path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create HoronAgent runtime directory {}", parent.display()))?;
    }
    let wait_start = std::time::Instant::now();
    let wait_timeout = std::time::Duration::from_secs(10);
    let mut announced_wait = false;

    loop {
        match horon_agent_platform::ipc::acquire_bootstrap_lock(&lock_path)? {
            horon_agent_platform::ipc::LockAttempt::Acquired(lock) => return Ok(Some(lock)),
            // No advisory lock on this platform, so this client spawns without
            // waiting for another one: its own readiness poll covers the race.
            horon_agent_platform::ipc::LockAttempt::Unsupported => return Ok(None),
            horon_agent_platform::ipc::LockAttempt::Busy => {}
        }

        if server_is_running_at(socket_path).await {
            return Ok(None);
        }

        if !announced_wait {
            output::stderr_info("Another client is starting the server, waiting...");
            announced_wait = true;
        }

        if wait_start.elapsed() >= wait_timeout {
            anyhow::bail!(
                "Timed out waiting for another client to start server at {}",
                socket_path.display()
            );
        }

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

pub(crate) async fn maybe_prompt_server_bootstrap_login(provider_choice: &ProviderChoice) -> Result<()> {
    startup_profile::mark("cred_check_start");

    // Normal interactive launches perform onboarding inside the TUI, and an
    // explicit provider choice never needs auto-detection here. Avoid probing
    // every credential backend unless the caller explicitly opted into the
    // legacy headless CLI bootstrap flow. On Windows those reads may trigger
    // expensive security-product inspection even when credentials are already
    // configured, delaying every cold launch before the server is spawned.
    let cli_bootstrap_requested = std::env::var_os("HORON_AGENT_CLI_BOOTSTRAP_LOGIN").is_some();
    if !should_detect_cli_bootstrap_credentials(provider_choice, cli_bootstrap_requested) {
        startup_profile::mark("cred_check_done");
        return Ok(());
    }

    let cred_state = detect_bootstrap_credentials().await;
    startup_profile::mark("cred_check_done");

    // Onboarding now happens entirely inside the TUI. We deliberately do *not*
    // run the blocking CLI "Approve sources" import prompt or the
    // "Choose a provider" selection menu here: a brand-new user launches
    // straight into the TUI, which detects the missing credentials and walks
    // them through login / external-auth import / model selection in the guided
    // first-run flow. The server is happy to spawn unauthenticated and the TUI
    // drives `/login` from there.
    //
    // The only thing left to honor at the CLI layer is an explicit headless
    // bootstrap (e.g. CI / non-interactive provisioning), which opts in via the
    // `HORON_AGENT_CLI_BOOTSTRAP_LOGIN` env var.
    if cred_state.has_any {
        return Ok(());
    }

    if auth::AuthStatus::has_any_untrusted_external_auth() {
        provider_init::maybe_run_external_auth_auto_import_flow().await?;
        if detect_bootstrap_credentials().await.has_any {
            return Ok(());
        }
    }

    // No interactive login exists in this build. Point the operator at the
    // config path instead of blocking a headless launch on a prompt.
    output::stderr_info(
        "No model provider is configured. Add a [providers.<name>] profile with type = \"openai-compatible\" to config.toml, then run again.",
    );

    Ok(())
}

fn should_detect_cli_bootstrap_credentials(provider_choice: &ProviderChoice, cli_bootstrap_requested: bool) -> bool {
    cli_bootstrap_requested && *provider_choice == ProviderChoice::Auto
}

struct BootstrapCredentialState {
    has_any: bool,
}

async fn detect_bootstrap_credentials() -> BootstrapCredentialState {
    let status = tokio::task::spawn_blocking(crate::auth::AuthStatus::check_fast)
        .await
        .unwrap_or_else(|error| {
            crate::logging::warn(&format!("Auth status probe failed: {error}"));
            crate::auth::AuthStatus::default()
        });
    BootstrapCredentialState {
        has_any: status.openai_compatible_any == crate::auth::AuthState::Available,
    }
}

pub(crate) async fn spawn_server(
    provider_choice: &ProviderChoice,
    model: Option<&str>,
    provider_profile: Option<&str>,
) -> Result<()> {
    spawn_server_with_executable(provider_choice, model, provider_profile, None).await
}

/// Lifecycle of a spawned `horon-agent serve` child.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ServerLifetime {
    /// The long-lived shared daemon: survives this process and serves later
    /// clients.
    #[default]
    Shared,
    /// A server owned by this process: `horon-agent serve --temporary-server
    /// --owner-pid <pid>` self-destructs once the owner exits, so a one-shot
    /// run leaves no daemon behind.
    OwnedByThisProcess,
}

async fn spawn_server_with_executable(
    provider_choice: &ProviderChoice,
    model: Option<&str>,
    provider_profile: Option<&str>,
    executable: Option<std::path::PathBuf>,
) -> Result<()> {
    spawn_server_with_lifetime(
        provider_choice,
        model,
        provider_profile,
        executable,
        ServerLifetime::Shared,
    )
    .await
}

/// Start an ephemeral server owned by this process on the socket it dials.
///
/// Headless `horon-agent run` runs its agent in-process, so `--socket X` selects a
/// path that nothing ever binds and the `swarm` tool has no server to talk to
/// (issue #1748). This gives that run a server for as long as it lives: the
/// child is marked `--temporary-server --owner-pid <our pid>`, so it shuts
/// itself down within seconds of the run exiting.
///
/// The current executable is used deliberately instead of the shared-server
/// update candidate: a run started from a locally built binary must be served
/// by that same binary, or `--socket`-isolated runtime checks would silently
/// measure the installed build (see AGENTS.md, "Verifying a change at
/// runtime").
pub(crate) async fn spawn_owned_server(
    provider_choice: &ProviderChoice,
    model: Option<&str>,
    provider_profile: Option<&str>,
) -> Result<()> {
    spawn_server_with_lifetime(
        provider_choice,
        model,
        provider_profile,
        std::env::current_exe().ok(),
        ServerLifetime::OwnedByThisProcess,
    )
    .await
}

/// Let a headless one-shot `horon-agent run` start a server on demand.
///
/// `horon-agent run` hosts its agent in-process, so there is no server for tools
/// that need one. Registering the spawner here (rather than at startup) keeps
/// it to the `run` command: interactive and client launches already bootstrap
/// a daemon through their own paths. Nothing is started by registering. The
/// first tool that cannot reach a server triggers the spawn, so runs that never
/// use `swarm` keep their current startup cost.
///
/// Which server gets started follows the dialed socket:
///
/// - Shared socket (no `--socket`): the long-lived daemon, exactly as
///   `horon-agent server start` or a TUI launch would start it. An ephemeral server
///   here would squat the path every later client uses and then vanish with
///   this run.
/// - Custom `--socket`: a server owned by this run. The caller asked for an
///   isolated socket, so nobody else is waiting on it, and the run must not
///   leave a daemon behind on a one-off path.
fn register_on_demand_server_spawner_for_run(
    provider_choice: ProviderChoice,
    model: Option<String>,
    provider_profile: Option<String>,
) {
    crate::server_spawn::register_on_demand_server_spawner(Box::new(move || {
        let model = model.clone();
        let provider_profile = provider_profile.clone();
        let provider_choice = provider_choice.clone();
        Box::pin(async move {
            let model = model.as_deref();
            let provider_profile = provider_profile.as_deref();
            if server::socket_path_is_custom() {
                spawn_owned_server(&provider_choice, model, provider_profile).await
            } else {
                spawn_server(&provider_choice, model, provider_profile).await
            }
        })
    }));
}

/// The `serve` subcommand argv for a spawned server of the given lifetime.
///
/// Split out so the contract with `Command::Serve` is testable without
/// starting a process.
fn serve_subcommand_args(lifetime: ServerLifetime, owner_pid: u32) -> Vec<String> {
    let mut args = vec!["serve".to_string()];
    if lifetime == ServerLifetime::OwnedByThisProcess {
        args.push("--temporary-server".to_string());
        args.push("--owner-pid".to_string());
        args.push(owner_pid.to_string());
    }
    args
}

async fn spawn_server_with_lifetime(
    provider_choice: &ProviderChoice,
    model: Option<&str>,
    provider_profile: Option<&str>,
    executable: Option<std::path::PathBuf>,
    lifetime: ServerLifetime,
) -> Result<()> {
    let socket_path = server::socket_path();
    if server_is_running_at(&socket_path).await {
        startup_profile::mark("server_ready");
        return Ok(());
    }

    // The reload marker describes the shared daemon's handoff. An owned server
    // has no predecessor to wait for, and waiting on a marker written by the
    // shared daemon would stall this run for the handoff timeout.
    let awaits_reload_handoff = lifetime == ServerLifetime::Shared;

    if awaits_reload_handoff && wait_for_existing_reload_server("server spawn").await {
        startup_profile::mark("server_ready");
        return Ok(());
    }

    let _spawn_lock = acquire_spawn_lock_or_wait(&socket_path).await?;

    if server_is_running_at(&socket_path).await {
        startup_profile::mark("server_ready");
        return Ok(());
    }

    if awaits_reload_handoff && wait_for_existing_reload_server("server spawn after lock").await {
        startup_profile::mark("server_ready");
        return Ok(());
    }

    startup_profile::mark("server_spawn_start");
    match lifetime {
        ServerLifetime::Shared => output::stderr_info("Starting server..."),
        ServerLifetime::OwnedByThisProcess => output::stderr_info(format!(
            "Starting a temporary server on {} for this run...",
            socket_path.display()
        )),
    }
    let exe = executable
        .or_else(|| build::shared_server_update_candidate().map(|(path, _)| path))
        .or_else(|| std::env::current_exe().ok())
        .ok_or_else(|| anyhow::anyhow!("Could not determine executable path for server spawn"))?;
    let mut cmd = ProcessCommand::new(&exe);
    cmd.arg("--provider").arg(provider_choice.as_arg_value());
    // The interactive TUI owns first-run onboarding/login. Let the spawned
    // server boot with a deferred (credential-less) provider when nothing is
    // configured yet, instead of bailing; the TUI activates a provider via the
    // in-TUI `/login` flow. See init_provider_with_options.
    cmd.env("HORON_AGENT_DEFERRED_AUTH_BOOTSTRAP", "1");
    if let Some(provider_profile) = provider_profile {
        cmd.arg("--provider-profile").arg(provider_profile);
    }
    if let Some(model) = model {
        cmd.arg("--model").arg(model);
    }
    cmd.args(serve_subcommand_args(lifetime, std::process::id()));
    cmd.stdout(Stdio::null()).stderr(Stdio::piped());

    // The platform reports readiness through an inherited pipe where it can pass
    // a descriptor, and polls the listener where it cannot.
    let _child = server::spawn_server_notify(&mut cmd).await?;
    startup_profile::mark("server_ready");
    Ok(())
}

async fn run_server_keepalive(
    provider_choice: &ProviderChoice,
    model: Option<&str>,
    provider_profile: Option<&str>,
) -> Result<()> {
    let mut owner_closed = tokio::task::spawn_blocking(|| {
        let mut stdin = std::io::stdin();
        let mut buffer = [0u8; 256];
        loop {
            match std::io::Read::read(&mut stdin, &mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    });
    let mut client: Option<server::Client> = None;
    let mut first_attempt = true;

    loop {
        let delay = if first_attempt {
            first_attempt = false;
            std::time::Duration::ZERO
        } else if client.is_some() {
            std::time::Duration::from_secs(30)
        } else {
            std::time::Duration::from_secs(1)
        };
        tokio::select! {
            _ = &mut owner_closed => return Ok(()),
            _ = tokio::time::sleep(delay) => {
                if client.is_some() {
                    // A Ping is a one-shot control request, so sending it over
                    // the held connection would make the server close that
                    // connection after replying. Probe through a short-lived
                    // client instead and leave the counted keepalive connected.
                    let healthy = tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        async {
                            let mut probe = server::Client::connect().await?;
                            probe.ping().await
                        },
                    )
                    .await
                    .is_ok_and(|result| result.unwrap_or(false));
                    if healthy {
                        continue;
                    }
                    client = None;
                }
                if spawn_server(provider_choice, model, provider_profile).await.is_ok()
                    && let Ok(connected) = server::Client::connect().await
                {
                    client = Some(connected);
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod dispatch_tests;

/// Record a human verdict on the blocked approval `request_id`.
///
/// Three facts are durable: the request-level `outcome` (`human_retry` or
/// `human_skip`), which tells the resume runner the request is no longer
/// waiting, `tool_verdicts[tool_call_id]`, which names the exact call the
/// verdict applies to, and the `ApprovalResolved` event a client reads. One
/// request can cover several calls, so the per-call entry is what keeps the
/// other calls unaffected. The note is echoed back to the operator and written
/// to the log with the verdict.
///
/// The verdict is also written to the decision history, because the two readers
/// of a human verdict are different files. The resume runner reads the
/// blocked-approval record; a turn blocked inside
/// `request_permission(wait = true)` polls `history.json` through
/// [`crate::safety::SafetySystem::decision_for`] and never sees that record.
/// Writing only one of them leaves the live turn to apply its deny default while
/// the human verdict sits on disk.
fn resolve_blocked_approval(
    store: &crate::ambient::BlockedApprovalStore,
    request_id: &str,
    tool_call_id: &str,
    verdict: ApprovalVerdictArg,
    note: Option<&str>,
) -> Result<()> {
    let outcome = match verdict {
        ApprovalVerdictArg::Retry => "human_retry",
        ApprovalVerdictArg::Skip => "human_skip",
    };
    // A retry approves the gated action; a skip refuses it. The live waiter
    // renders the decision as "Permission approved" or "Permission denied", so
    // an approved skip would tell the model to go ahead with the very call the
    // operator told it not to re-run.
    let approved = matches!(verdict, ApprovalVerdictArg::Retry);
    let tool_call_id_owned = tool_call_id.to_string();
    let event = crate::protocol::ServerEvent::ApprovalResolved {
        request_id: request_id.to_string(),
        approved,
        via: outcome.to_string(),
        resolved_by: "human".to_string(),
    };
    let pending = serde_json::to_value(&event)?;
    let recorded = store.update_with(request_id, move |record| {
        // The first verdict closes the request; later verdicts only add their
        // own call entry. A `needs-human` stop is the runner asking for this
        // verdict, not a decision, so the first verdict replaces it. A decision
        // that already closed the request (`resumed`, `denied`, ...) stays.
        match record.outcome.as_deref() {
            None | Some("needs-human") => record.outcome = Some(outcome.to_string()),
            Some(_) => {}
        }
        record.tool_verdicts.insert(tool_call_id_owned, outcome.to_string());
        // This CLI runs in its own process, with no client sink to hand the
        // event to, so it records the event the runner replays and a client
        // acknowledges. It replaces an un-acked earlier event for the same
        // request: clients dedupe on `request_id`, and the verdict is the
        // later and more specific word on how that request was resolved.
        record.pending_event = Some(pending);
    })?;
    if !recorded {
        anyhow::bail!("no blocked approval record for request id '{request_id}'; nothing was recorded");
    }

    // The record is written first and the decision second. A waiter that
    // observes the decision can then rely on the durable verdict already
    // existing, and a crash between the two writes leaves "record says verdict,
    // history has nothing": the runner resumes the session with the exact
    // per-call verdicts, and re-running this command completes the decision.
    // The other order would leave "history says approved, record says waiting",
    // which a runner pass can close as a terminal denial (a skip) and lose the
    // verdict for good.
    let system = crate::safety::SafetySystem::new();
    let message = human_verdict_message(verdict, tool_call_id, note);
    match system.record_decision(request_id, approved, outcome, Some(message)) {
        Ok(crate::safety::RecordDecisionOutcome::Recorded(decision)) => {
            // A corrupt history records a denial whatever was asked for, so the
            // waiter must be told what it will actually receive rather than
            // what this command asked for.
            if decision.approved != approved {
                crate::logging::warn(&format!(
                    "human verdict for request {request_id} was recorded as a denial in the decision history ({})",
                    decision.decided_via
                ));
                println!(
                    "Note: the decision history recorded a denial for request {request_id} ({}); the approval record still carries {outcome}.",
                    decision.decided_via
                );
            }
        }
        Ok(crate::safety::RecordDecisionOutcome::AlreadyDecided { existing }) => {
            // A timeout or another approver got there first. The per-call
            // verdict is still the more specific word, and the runner reads it
            // before the request-level decision, so the record stands.
            crate::logging::warn(&format!(
                "human verdict for request {request_id} is in the approval record, but a decision already existed ({}); that decision stands in the history",
                existing.decided_via
            ));
            println!(
                "Note: a decision for request {request_id} was already recorded ({}); the approval record still carries this verdict.",
                existing.decided_via
            );
        }
        Err(error) => anyhow::bail!(
            "{outcome} is recorded in the approval record for request {request_id}, but the decision history could not be written, so a turn blocked on it will not wake: {error}. Re-run this command to complete the decision."
        ),
    }

    crate::logging::info(&format!(
        "human verdict recorded: request={request_id} tool_call={tool_call_id} verdict={outcome}{}",
        note.map(|note| format!(" note={note}")).unwrap_or_default()
    ));
    println!(
        "Recorded {outcome} for request {request_id} (tool call {tool_call_id}).{}",
        note.map(|note| format!("\nNote: {note}")).unwrap_or_default()
    );
    Ok(())
}

/// The text the live waiter hands the model as its tool result.
///
/// `RequestPermissionTool` renders this as `Permission approved: ...` or
/// `Permission denied: ...`, so the wording has to carry the operator's
/// instruction: a retry approves the action, and a skip refuses it while the
/// turn continues after it.
fn human_verdict_message(verdict: ApprovalVerdictArg, tool_call_id: &str, note: Option<&str>) -> String {
    let (word, instruction) = match verdict {
        ApprovalVerdictArg::Retry => ("retry", "run exactly this call again, then continue"),
        ApprovalVerdictArg::Skip => ("skip", "do not run this call again, continue after it"),
    };
    match note {
        Some(note) => format!("human {word} for tool call {tool_call_id}: {instruction} (note: {note})"),
        None => format!("human {word} for tool call {tool_call_id}: {instruction}"),
    }
}
