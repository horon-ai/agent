use anyhow::Result;
use clap::Parser;
use std::process::Command as ProcessCommand;

use crate::{build, logging, perf, server, setup_hints, startup_profile, storage, update};

use super::{
    args::{Args, Command},
    dispatch, hot_exec, output, terminal,
};

fn sync_output_style_from_config() {
    crate::output_style::set_emoji_enabled(crate::config::config().display.emoji);
}

pub async fn run() -> Result<()> {
    // Parse once, before startup side effects. Invalid arguments and --help
    // must not harden credential files or create configuration state.
    let args = Args::parse();
    crate::workspace_trust::set_cli_trust_workspace(args.trust_workspace);
    // Interactive login prompts are a client concern. The shared auth flows
    // reach them through this presenter, which prints the login URL as plain
    // text and reads secrets from stdin.
    super::login::install_login_presenter();
    // Snapshot user-supplied provider env before provider init rewrites it.
    super::provider_init::capture_user_openrouter_model_env();
    // Credential import and explicit MCP management do not need normal agent
    // startup hardening, migrations, or provider discovery.
    if args.ssh.is_none()
        && matches!(
            args.command,
            Some(
                Command::Auth(super::args::AuthCommand::Import { .. })
                    | Command::Mcp { .. }
                    | Command::Approvals { .. }
            )
        )
    {
        if let Some(cwd) = &args.cwd {
            std::env::set_current_dir(cwd)?;
        }
        return dispatch::run_main(args).await;
    }
    startup_profile::init();

    terminal::install_panic_hook();
    startup_profile::mark("panic_hook");

    logging::init();
    startup_profile::mark("logging_init");
    crate::crash::install_panic_hook();
    // Old log pruning now runs on a background thread inside logging::init(),
    // so it no longer blocks startup. Memory-event logs have a separate,
    // longer (14-day) retention, so prune them on their own background thread.
    std::thread::Builder::new()
        .name("horon-agent-memlog-cleanup".to_string())
        .spawn(crate::memory_log::cleanup_old_memory_logs)
        .ok();
    // Prune stale per-session `.bak` recovery copies (never the transcripts
    // themselves) so the sessions directory does not grow without bound.
    std::thread::Builder::new()
        .name("horon-agent-session-bak-prune".to_string())
        .spawn(crate::session::prune_old_session_backups)
        .ok();
    // Record which binary actually launched. Stale copies earlier on PATH (or a
    // shortcut pinned to an old install) otherwise look identical to the
    // updated launcher in logs, and keep re-offering the same update (#1626).
    logging::info(&format!(
        "horon-agent starting (version={}, exe={})",
        horon_agent_build_meta::version(),
        std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "unknown".to_string())
    ));

    // Wire config-reload reactions without making config depend on auth/bus:
    // when the config cache reloads, invalidate the auth-status cache and
    // broadcast a models-updated event.
    sync_output_style_from_config();
    crate::config::on_config_reloaded(sync_output_style_from_config);
    crate::config::on_config_reloaded(crate::auth::AuthStatus::invalidate_cache);
    crate::config::on_config_reloaded(|| crate::bus::Bus::global().publish_models_updated());

    // Invert the legacy provider_catalog -> auth dependency: provider_catalog
    // consults registered fallback resolvers, and auth (the higher layer)
    // registers its external-CLI credential scan here.
    crate::provider_catalog::register_api_key_fallback_resolver(crate::auth::external::load_api_key_for_env);

    // Invert the legacy safety -> notifications dependency: safety raises a
    // permission request and the notifications layer (which depends on safety
    // types) delivers it via the dispatcher registered here.
    crate::safety::register_permission_notifier(|action, description, request_id| {
        crate::notifications::NotificationDispatcher::new().dispatch_permission_request(
            action,
            description,
            request_id,
        );
    });

    // Report the outcome of every recorded permission decision (approved,
    // denied, or the timeout default) through the same channels. safety keeps
    // every registered callback, so this sits beside the request notifier and
    // the resume runner's callback without displacing either. Registration is
    // guarded by a OnceLock: a second startup in the same process must not
    // deliver each decision twice.
    crate::notifications::install_default_decision_notifier();

    // Invert the legacy memory -> skill dependency: memory collects synthetic
    // entries from registered providers, and skill (the higher layer that
    // depends on MemoryEntry) registers its registry->memory adapter here.
    // The shared snapshot holds global skills only; memory retrieval is
    // process-scoped, so compose the project overlay from the process cwd
    // (issue #457 keeps session overlays out of the shared registry).
    crate::memory::register_synthetic_entry_provider(|| {
        let global = crate::skill::SkillRegistry::shared_snapshot();
        crate::skill::SkillRegistry::effective_for_working_dir(&global, None)
            .list()
            .into_iter()
            .map(|skill| skill.as_memory_entry())
            .collect()
    });

    // Invert the legacy server -> cli dependency for shared-server spawning: the
    // CLI owns the provider-bootstrap spawn logic and registers it here, so the
    // TUI reconnect loop can request a replacement server via server_spawn
    // without referencing cli.
    crate::server_spawn::register_default_server_spawner(Box::new(|| {
        Box::pin(async { dispatch::spawn_server(&crate::cli::provider_init::ProviderChoice::Auto, None, None).await })
    }));

    crate::platform::raise_nofile_limit_best_effort(8_192);
    startup_profile::mark("nofile_limit");

    storage::harden_user_config_permissions();
    startup_profile::mark("perm_harden");

    perf::init_background();
    startup_profile::mark("perf_init");

    let args = parse_and_prepare_args(args)?;
    spawn_background_update_check(&args);

    if let Err(e) = dispatch::run_main(args).await {
        report_main_error(&e);
        return Err(e);
    }

    Ok(())
}

fn parse_and_prepare_args(args: Args) -> Result<Args> {
    startup_profile::mark("args_parse");

    if let Some(chord) = args.spawn_hotkey.as_deref() {
        setup_hints::record_launch_hotkey_use(chord);
    }

    output::set_quiet_enabled(args.quiet);

    if let Some(cwd) = &args.cwd {
        std::env::set_current_dir(cwd)?;
        logging::info(&format!("Changed working directory to: {}", cwd));
    }

    validate_remote_working_dir(args.remote_working_dir.as_deref())?;

    if args.trace {
        crate::env::set_var("HORON_AGENT_TRACE", "1");
    }

    if let Some(ref socket) = args.socket {
        server::set_socket_path(socket);
    }

    crate::cli::proctitle::set_initial_title(&args);

    Ok(args)
}

fn validate_remote_working_dir(remote_working_dir: Option<&str>) -> Result<()> {
    if let Some(remote_working_dir) = remote_working_dir
        && !remote_working_dir_is_absolute(remote_working_dir)
    {
        anyhow::bail!("--remote-working-dir must be an absolute path");
    }
    Ok(())
}

fn remote_working_dir_is_absolute(path: &str) -> bool {
    if path.starts_with('/') || path.starts_with('\\') {
        return true;
    }

    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'/' || bytes[2] == b'\\') && bytes[0].is_ascii_alphabetic()
}

fn spawn_background_update_check(args: &Args) {
    let check_updates = should_spawn_background_update_check(args);
    let auto_update = should_auto_install_update(args);

    if !check_updates {
        return;
    }

    if update::is_release_build() {
        std::thread::spawn(move || {
            use crate::bus::{Bus, BusEvent, ClientMaintenanceAction, SessionUpdateStatus};
            match update::check_and_maybe_update(auto_update) {
                update::UpdateCheckResult::UpdateAvailable { current, latest, .. } => {
                    logging::info(&format!("Update available: {} -> {}", current, latest));
                }
                update::UpdateCheckResult::UpdateInstalled { version, path } => {
                    // When an interactive TUI session is running, hand the switch
                    // to the app's graceful reload path (saves the input line,
                    // waits for the current turn, resumes the session) instead of
                    // exec-ing over the live UI, which visibly resets the screen.
                    if let Some(session_id) = terminal::get_current_session() {
                        logging::info(&format!(
                            "Updated to {}. Requesting graceful session reload...",
                            version
                        ));
                        Bus::global().publish(BusEvent::SessionUpdateStatus(SessionUpdateStatus::ReadyToReload {
                            session_id,
                            action: ClientMaintenanceAction::Update,
                            version,
                        }));
                        return;
                    }
                    logging::info(&format!("Updated to {}. Restarting...", version));
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    let args: Vec<String> = std::env::args().skip(1).collect();
                    let exec_path = build::client_update_candidate(false).map(|(p, _)| p).unwrap_or(path);
                    let err = crate::platform::replace_process(
                        ProcessCommand::new(&exec_path).args(&args).arg("--no-update"),
                    );
                    eprintln!("Failed to exec new binary: {}", err);
                }
                update::UpdateCheckResult::Error(e) => {
                    logging::info(&format!("Update check failed: {}", e));
                }
                update::UpdateCheckResult::NoUpdate => {}
            }
        });
    } else {
        std::thread::spawn(move || {
            use crate::bus::{Bus, BusEvent, UpdateStatus};

            let start = std::time::Instant::now();
            Bus::global().publish(BusEvent::UpdateStatus(UpdateStatus::Checking));
            let status = source_update_check_status(hot_exec::check_for_updates());
            if matches!(status, UpdateStatus::Available { .. }) {
                // A checkout with local commits can never fast-forward, so the
                // pull below would always fail and surface a noisy "Update
                // diverged. Press Ctrl+Y..." card in every new session.
                // Developers with local work expect divergence; log it once
                // and stay quiet in the UI (no Available/Error cards).
                if hot_exec::local_commits_ahead_of_upstream() == Some(true) {
                    logging::info(
                        "Auto-update skipped: local commits are ahead of upstream (diverged). \
                         Merge or rebase manually when ready.",
                    );
                    Bus::global().publish(BusEvent::UpdateStatus(UpdateStatus::UpToDate));
                } else {
                    Bus::global().publish(BusEvent::UpdateStatus(status));
                    if auto_update {
                        logging::info("Update available - auto-updating...");
                        Bus::global().publish(BusEvent::UpdateStatus(UpdateStatus::Installing {
                            version: "latest source".to_string(),
                        }));
                        if let Err(e) = hot_exec::run_auto_update() {
                            Bus::global().publish(BusEvent::UpdateStatus(UpdateStatus::Error(e.to_string())));
                            logging::error(&format!("Auto-update failed: {}. Continuing with current version.", e));
                        }
                    } else {
                        logging::info("Update available! Run `horon-agent update` or `/reload` to update.");
                    }
                }
            } else {
                match &status {
                    UpdateStatus::Error(message) => logging::info(message),
                    UpdateStatus::Skipped { reason } => {
                        logging::info(&format!("Source update check skipped: {reason}"));
                    }
                    _ => {}
                }
                Bus::global().publish(BusEvent::UpdateStatus(status));
            }
            logging::info(&format!(
                "[TIMING] background_update_check: auto_update={}, total={}ms",
                auto_update,
                start.elapsed().as_millis()
            ));
        });
    }
}

fn source_update_check_status(result: anyhow::Result<Option<bool>>) -> crate::bus::UpdateStatus {
    use crate::bus::UpdateStatus;

    match result {
        Ok(Some(true)) => UpdateStatus::Available {
            current: horon_agent_build_meta::version().to_string(),
            latest: "latest source".to_string(),
        },
        Ok(Some(false)) => UpdateStatus::UpToDate,
        Ok(None) => UpdateStatus::Skipped {
            reason: "no upstream configured for the source checkout (local branch or detached HEAD)".to_string(),
        },
        Err(error) => UpdateStatus::Error(format!("Source update check failed: {error:#}")),
    }
}

fn should_spawn_background_update_check(args: &Args) -> bool {
    should_spawn_background_update_check_with_config(args, crate::config::config().features.check_updates)
}

fn should_spawn_background_update_check_with_config(args: &Args, check_updates: bool) -> bool {
    check_updates
        && args.ssh.is_none()
        && !args.quiet
        && !args.no_update
        && !matches!(
            args.command,
            Some(Command::Update)
                | Some(Command::Serve { .. })
                | Some(Command::Server { .. })
                | Some(Command::Acp { .. })
                | Some(Command::Approvals { .. })
        )
        && args.resume.is_none()
}

fn should_auto_install_update(args: &Args) -> bool {
    args.auto_update
}

fn report_main_error(error: &anyhow::Error) {
    let error_str = format!("{:?}", error);
    logging::error(&error_str);

    if let Some(session_id) = terminal::get_current_session() {
        output::stderr_blank_line();
        output::stderr_info("\x1b[33mTo restore this session, run:\x1b[0m");
        output::stderr_info(format!("  horon-agent --resume {}", session_id));
        output::stderr_blank_line();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::{Args, Command};
    use clap::Parser;

    fn parse_args(argv: &[&str]) -> Args {
        Args::parse_from(argv)
    }

    #[test]
    fn source_update_check_preserves_git_error() {
        let crate::bus::UpdateStatus::Error(message) =
            source_update_check_status(Err(anyhow::anyhow!("git fetch: offline")))
        else {
            panic!("an indeterminate source comparison must report an error");
        };
        assert_eq!(message, "Source update check failed: git fetch: offline");
    }

    #[test]
    fn source_update_check_false_reports_up_to_date() {
        assert!(matches!(
            source_update_check_status(Ok(Some(false))),
            crate::bus::UpdateStatus::UpToDate
        ));
    }

    #[test]
    fn source_update_check_true_reports_available() {
        let crate::bus::UpdateStatus::Available { current, latest } = source_update_check_status(Ok(Some(true))) else {
            panic!("a source update must remain available");
        };
        assert_eq!(current, horon_agent_build_meta::version());
        assert_eq!(latest, "latest source");
    }

    #[test]
    fn source_update_check_real_git_upstream_states() {
        let repo = tempfile::tempdir().expect("temporary source checkout");
        let git = |args: &[&str]| {
            let output = ProcessCommand::new("git")
                .args([
                    "-c",
                    "user.name=Update Test",
                    "-c",
                    "user.email=update-test@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "core.hooksPath=/dev/null",
                ])
                .args(args)
                .current_dir(repo.path())
                .output()
                .expect("run git fixture command");
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-b", "source"]);
        git(&["commit", "--allow-empty", "-m", "initial"]);

        // A local branch without tracking is skipped before claiming a fetch slot.
        let result = hot_exec::check_for_updates_in(repo.path(), || {
            panic!("untracked checkouts must not fetch or claim the fetch slot")
        });
        assert_eq!(*result.as_ref().unwrap(), None);
        assert!(matches!(
            source_update_check_status(result),
            crate::bus::UpdateStatus::Skipped { .. }
        ));

        // Local branches supply tracking controls without fetching or networking.
        git(&["branch", "upstream"]);
        git(&["branch", "--set-upstream-to=upstream", "source"]);
        let result = hot_exec::check_for_updates_in(repo.path(), || false);
        assert_eq!(*result.as_ref().unwrap(), Some(false));
        assert!(matches!(
            source_update_check_status(result),
            crate::bus::UpdateStatus::UpToDate
        ));

        git(&["checkout", "upstream"]);
        git(&["commit", "--allow-empty", "-m", "upstream update"]);
        git(&["checkout", "source"]);
        let result = hot_exec::check_for_updates_in(repo.path(), || false);
        assert_eq!(*result.as_ref().unwrap(), Some(true));
        assert!(matches!(
            source_update_check_status(result),
            crate::bus::UpdateStatus::Available { .. }
        ));

        // Local commits alone are not updates.
        git(&["checkout", "upstream"]);
        git(&["branch", "--set-upstream-to=source", "upstream"]);
        let result = hot_exec::check_for_updates_in(repo.path(), || false);
        assert_eq!(result.unwrap(), Some(false));

        git(&["checkout", "--detach"]);
        let result = hot_exec::check_for_updates_in(repo.path(), || panic!("detached checkouts must not fetch"));
        assert!(matches!(
            source_update_check_status(result),
            crate::bus::UpdateStatus::Skipped { .. }
        ));

        // A configured but missing upstream must still report an error.
        git(&["checkout", "source"]);
        git(&["branch", "-D", "upstream"]);
        let result = hot_exec::check_for_updates_in(repo.path(), || false);
        assert!(matches!(
            source_update_check_status(result),
            crate::bus::UpdateStatus::Error(_)
        ));

        // Network/fetch failures on tracked branches must not become skips.
        git(&["remote", "add", "origin", "./missing-remote"]);
        git(&["update-ref", "refs/remotes/origin/source", "HEAD"]);
        git(&["config", "branch.source.remote", "origin"]);
        git(&["config", "branch.source.merge", "refs/heads/source"]);
        let result = hot_exec::check_for_updates_in(repo.path(), || true);
        let crate::bus::UpdateStatus::Error(message) = source_update_check_status(result) else {
            panic!("failed fetch must report an error");
        };
        assert!(message.contains("git fetch -q:"));
    }

    #[test]
    fn source_update_check_invalid_checkout_reports_error() {
        let directory = tempfile::tempdir().unwrap();
        let result = hot_exec::check_for_updates_in(directory.path(), || panic!("invalid checkouts must not fetch"));
        assert!(matches!(
            source_update_check_status(result),
            crate::bus::UpdateStatus::Error(_)
        ));
    }

    #[test]
    fn parses_mcp_tool_exposure_flags() {
        let args = parse_args(&[
            "horon-agent",
            "--mcp-tools",
            "deferred",
            "--mcp-tools-token-threshold",
            "4321",
            "run",
            "hello",
        ]);
        assert_eq!(args.mcp_tools.as_deref(), Some("deferred"));
        assert_eq!(args.mcp_tools_token_threshold, Some(4_321));
    }

    #[test]
    fn auto_install_allowed_without_live_terminal() {
        let args = parse_args(&["horon-agent", "login"]);
        assert!(should_auto_install_update(&args));
    }

    #[test]
    fn auto_install_allowed_with_live_terminal_attached() {
        let args = parse_args(&["horon-agent", "login"]);
        assert!(should_auto_install_update(&args));
    }

    #[test]
    fn auto_install_respects_explicit_disable_even_without_terminal() {
        let mut args = parse_args(&["horon-agent", "login"]);
        args.auto_update = false;
        assert!(!should_auto_install_update(&args));
    }

    #[test]
    fn remote_working_dir_validation_requires_absolute_path() {
        assert!(validate_remote_working_dir(Some("/home/agent/project")).is_ok());
        assert!(validate_remote_working_dir(Some("C:\\Users\\agent\\project")).is_ok());
        assert!(validate_remote_working_dir(None).is_ok());

        let error = validate_remote_working_dir(Some("relative/project")).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("--remote-working-dir must be an absolute path")
        );
    }

    #[test]
    fn update_command_still_skips_background_check_before_auto_install_logic() {
        let args = parse_args(&["horon-agent", "update"]);
        assert!(matches!(args.command, Some(Command::Update)));
        assert!(!should_spawn_background_update_check(&args));
        assert!(should_auto_install_update(&args));
    }

    #[test]
    fn config_can_permanently_disable_background_update_checks() {
        let args = parse_args(&["horon-agent", "login"]);
        assert!(should_spawn_background_update_check_with_config(&args, true));
        assert!(!should_spawn_background_update_check_with_config(&args, false));
    }

    #[test]
    fn hidden_spawn_hotkey_argument_is_global_and_preserves_canonical_text() {
        let args = parse_args(&["horon-agent", "--spawn-hotkey", "shift+cmd+'", "self-dev"]);
        assert_eq!(args.spawn_hotkey.as_deref(), Some("shift+cmd+'"));
        assert!(matches!(args.command, Some(Command::SelfDev { .. })));
    }
    #[test]
    fn no_external_provider_runtime_is_registered() {
        // Every external provider runtime was removed. Model requests run on the
        // direct OpenAI-compatible provider, so no factory may be registered for
        // any runtime key.
        assert!(
            !crate::provider::external::external_provider_registered(crate::provider::external::OPENAI_RUNTIME),
            "the OpenAI runtime must not be registered"
        );
        assert!(
            crate::provider::external::instantiate_external_provider(crate::provider::external::OPENAI_RUNTIME)
                .is_none(),
            "the OpenAI runtime must not instantiate"
        );

        // Every removed provider runtime must stay unregistered and fail to
        // instantiate in this build.
        for key in [
            crate::provider::external::GEMINI_RUNTIME,
            crate::provider::external::CURSOR_RUNTIME,
            crate::provider::external::ANTIGRAVITY_RUNTIME,
            crate::provider::external::COPILOT_RUNTIME,
            crate::provider::external::ANTHROPIC_RUNTIME,
        ] {
            assert!(
                !crate::provider::external::external_provider_registered(key),
                "{key} runtime must not be registered in this build"
            );
            assert!(
                crate::provider::external::instantiate_external_provider(key).is_none(),
                "{key} runtime must not instantiate in this build"
            );
        }
    }
}
