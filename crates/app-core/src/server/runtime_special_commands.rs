//! Specialist runtime workflows for non-terminal clients.
//! Never mutate a client's filesystem or replace a workflow with a model prompt.
use crate::agent::Agent;
use anyhow::{Context, Result, bail};
use horon_agent_harness_api::RuntimeCommand;
use serde_json::{Value, json};

fn status_args(args: &[String]) -> Result<()> {
    match args {
        [] => Ok(()),
        [arg] if matches!(arg.as_str(), "status" | "show") => Ok(()),
        _ => bail!("This command accepts only status or show"),
    }
}

fn initiatives_reply() -> (Value, String) {
    (
        json!({"retired": true, "replacement": "goal tool"}),
        "Legacy initiatives are retired. The goal tool now carries this work, and it is registered as `goal`. Existing initiative records are preserved and are not migrated or re-enabled by this command. Use the goal tool for current tracked work."
            .to_string(),
    )
}

/// Find the attached session's latest run, rather than the host's latest run.
fn session_manifest(session_id: &str) -> Result<Option<crate::overnight::OvernightManifest>> {
    let root = crate::overnight::runs_dir()?;
    if !root.exists() {
        return Ok(None);
    }
    let mut latest = None;
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path().join("manifest.json");
        let Ok(manifest) = crate::storage::read_json::<crate::overnight::OvernightManifest>(&path) else {
            continue;
        };
        if manifest.parent_session_id != session_id && manifest.coordinator_session_id != session_id {
            continue;
        }
        if latest
            .as_ref()
            .is_none_or(|old: &crate::overnight::OvernightManifest| old.started_at < manifest.started_at)
        {
            latest = Some(manifest);
        }
    }
    Ok(latest)
}

pub(super) async fn execute(agent: &mut Agent, command: RuntimeCommand, args: &[String]) -> Result<(Value, String)> {
    match command {
        RuntimeCommand::Auth => {
            status_args(args)?;
            let status = tokio::task::spawn_blocking(crate::auth::AuthStatus::check_fast).await?;
            let profiles: Vec<_> = crate::config::config()
                .providers
                .iter()
                .map(|(name, profile)| {
                    json!({
                        "profile": name,
                        "base_url": profile.base_url,
                        "default_model": profile.default_model,
                        "api_key_env": profile.api_key_env,
                    })
                })
                .collect();
            let data = json!({
                "scope": "runtime host",
                "openai_compatible_credentials": status.openai_compatible_any,
                "profiles": profiles,
                "note": "Model requests go directly to the OpenAI-compatible profiles in config.toml. Secrets and raw credential files are never returned.",
            });
            Ok((
                data.clone(),
                format!(
                    "# Runtime authentication\n\n```json\n{}\n```",
                    serde_json::to_string_pretty(&data)?
                ),
            ))
        }
        RuntimeCommand::Logout => logout(args),
        RuntimeCommand::Remote => remote(args),
        RuntimeCommand::Overnight => overnight(agent, args).await,
        RuntimeCommand::Productivity => {
            status_args(args)?;
            let output = tokio::task::spawn_blocking(horon_agent_productivity_core::generate).await??;
            use base64::Engine;
            let png = base64::engine::general_purpose::STANDARD.encode(&output.png);
            Ok((
                json!({"report": output.report, "png_path": output.png_path,
                "image": {"media_type": "image/png", "base64": png}, "scope": "runtime host"}),
                output.markdown,
            ))
        }
        RuntimeCommand::Initiatives => {
            status_args(args)?;
            Ok(initiatives_reply())
        }
        _ => bail!("Not a specialist runtime command"),
    }
}

fn logout(args: &[String]) -> Result<(Value, String)> {
    let (name, confirmed) = match args {
        [] => {
            return Ok((
                json!({"confirmation_required": true, "scope": "runtime host"}),
                "Logout requires an explicit profile and confirmation. Use `/logout <profile> confirm`. Supported ids are the OpenAI-compatible profiles in `config.toml` and the built-in profile ids. No credential was removed."
                    .into(),
            ));
        }
        [name] => (name.as_str(), false),
        [name, confirmation] if confirmation == "confirm" => (name.as_str(), true),
        _ => bail!("Usage: `/logout <profile> confirm`. Nothing was removed."),
    };
    let Some((display_name, api_key_env, env_file)) = logout_profile(name) else {
        bail!(
            "Unknown provider profile `{name}`. Add it with `horon-agent provider add` or use a built-in OpenAI-compatible profile id. Nothing was removed."
        );
    };
    if !confirmed {
        return Ok((
            json!({"confirmation_required": true, "provider": name, "scope": "runtime host"}),
            format!(
                "To remove the saved API key for {display_name}, run `/logout {name} confirm`. External environment keys and other apps' credentials are not revoked. No credential was removed."
            ),
        ));
    }
    let cleared = match (api_key_env.as_deref(), env_file.as_deref()) {
        (Some(env_key), Some(env_file)) => {
            crate::provider_catalog::save_env_value_to_env_file(env_key, env_file, None)?;
            true
        }
        _ => false,
    };
    crate::auth::AuthStatus::invalidate_cache();
    let data = json!({
        "provider": name,
        "scope": "runtime host",
        "saved_api_key_cleared": cleared,
        "external_credentials_revoked": false,
    });
    let message = if cleared {
        format!(
            "{display_name} stored API key cleared. Existing provider instances may still hold in-memory credentials until reconnect/reinitialization. External credentials and upstream tokens were not revoked."
        )
    } else {
        format!("{display_name} has no saved API key in horon-agent's provider env files. Nothing was removed.")
    };
    Ok((data, message))
}

fn logout_profile(name: &str) -> Option<(String, Option<String>, Option<String>)> {
    if let Some(profile) = crate::config::config().providers.get(name) {
        return Some((name.to_string(), profile.api_key_env.clone(), profile.env_file.clone()));
    }
    let profile = crate::provider_catalog::openai_compatible_profile_by_id(name)?;
    let resolved = crate::provider_catalog::resolve_openai_compatible_profile(profile);
    Some((
        resolved.display_name.to_string(),
        Some(resolved.api_key_env),
        Some(resolved.env_file),
    ))
}

fn remote(args: &[String]) -> Result<(Value, String)> {
    use crate::gateway::control::{self, RemoteCommand, RemoteStatus, ToggleOutcome};
    let input = format!("/remote {}", args.join(" "));
    let command = control::parse_remote_command(&input)
        .context("Invalid remote command")?
        .map_err(anyhow::Error::msg)?;
    match command {
        RemoteCommand::Cloud => bail!("HoronAgent Cloud activation belongs to the client's native account/cloud dialog. No local gateway setting was changed. Use Desktop's cloud setup UI."),
        RemoteCommand::Help => Ok((json!({"scope": "runtime host"}),
            "Usage: `/remote status`, `/remote on`, `/remote off`, `/remote pair`, `/remote revoke <device-id-or-name>`. These commands manage the attached runtime host's gateway. Gateway enable/disable settings take effect after a coordinated server restart; no server is restarted automatically. `/remote cloud` uses the client's Cloud setup dialog.".into())),
        RemoteCommand::Status => {
            let status = RemoteStatus::load();
            let devices: Vec<_> = status.devices.iter().map(|device| json!({"id": device.id, "name": device.name, "paired_at": device.paired_at, "last_seen": device.last_seen})).collect();
            Ok((json!({"enabled": status.enabled, "bind_addr": status.bind_addr, "port": status.port, "dial_address": status.dial_address(), "devices": devices, "scope": "runtime host", "listener_health_checked": false}),
                format!("{}\nThis is the runtime host's gateway configuration snapshot, not a live listener health check. Changed settings require a coordinated server restart.", status.to_markdown())))
        }
        RemoteCommand::On | RemoteCommand::Off => {
            let enabled = command == RemoteCommand::On;
            let outcome = control::set_gateway_enabled(enabled)?;
            let changed = matches!(outcome, ToggleOutcome::Changed { .. });
            Ok((json!({"enabled": enabled, "changed": changed, "restart_required": changed, "scope": "runtime host"}),
                if changed { format!("Gateway {} saved on the runtime host. A coordinated server restart is required before this change takes effect. No server was restarted.", if enabled { "enabled" } else { "disabled" }) }
                else { format!("Gateway is already configured {}. No setting was changed or server restarted.", if enabled { "on" } else { "off" }) }))
        }
        RemoteCommand::Pair => {
            let invite = control::create_pairing_invite()?;
            let message = format!("Pairing code: `{}`\n\nDial: `{}`\n\nPairing URI: `{}`\n\nThis code expires in five minutes. Share it only with the device you intend to authorize.{}",
                invite.spaced_code(), invite.dial_address, invite.uri,
                if invite.gateway_enabled { "" } else { " The gateway is not enabled. Run `/remote on` and coordinate a server restart before pairing." });
            Ok((json!({"code": invite.code, "dial_address": invite.dial_address, "uri": invite.uri, "gateway_enabled": invite.gateway_enabled, "expires_in_seconds": 300, "scope": "runtime host"}), message))
        }
        RemoteCommand::Revoke(target) => {
            let removed = control::revoke_device(&target)?;
            let message = if removed.is_empty() { format!("No paired device matched `{target}`. Nothing was revoked.") }
                else { format!("Revoked paired device(s): {}.", removed.join(", ")) };
            Ok((json!({"target": target, "removed": removed, "scope": "runtime host"}), message))
        }
    }
}

async fn overnight(agent: &mut Agent, args: &[String]) -> Result<(Value, String)> {
    use crate::overnight::{self, OvernightCommand, OvernightRunStatus};
    let input = format!("/overnight {}", args.join(" "));
    let command = overnight::parse_overnight_command(&input)
        .context("Invalid overnight command")?
        .map_err(anyhow::Error::msg)?;
    if command == OvernightCommand::Help {
        return Ok((
            json!({"usage": overnight::overnight_usage()}),
            overnight::overnight_usage().into(),
        ));
    }
    let session_id = agent.session_id().to_owned();
    if let OvernightCommand::Start { duration, mission } = command {
        if let Some(existing) = session_manifest(&session_id)?
            && matches!(
                existing.status,
                OvernightRunStatus::Running | OvernightRunStatus::CancelRequested
            )
        {
            bail!(
                "This session already has an active overnight run {}. Inspect status or cancel it before starting another.",
                existing.run_id
            );
        }
        let launch = overnight::start_overnight_run(overnight::OvernightStartOptions {
            duration,
            mission,
            parent_session: agent.session_for_split().clone(),
            provider: agent.provider_fork(),
            // Deliberately the plain clone: this registry builds a NEW coordinator
            // session, and `Agent::new_with_session` seeds its `CompactionManager`
            // from that child session and the child provider's window. Sharing the
            // launching session's manager would re-count the launcher's budget.
            registry: agent.registry(),
            working_dir: agent.working_dir().map(std::path::PathBuf::from),
            use_current_session: false,
        })?;
        let message = format!(
            "Started supervised overnight coordinator `{}` in session `{}`. Target wake: {}. Use `/overnight status`, `/overnight log`, `/overnight review`, or `/overnight cancel`.",
            launch.manifest.run_id,
            launch.manifest.coordinator_session_id,
            launch.manifest.target_wake_at.to_rfc3339()
        );
        return Ok((serde_json::to_value(launch.manifest)?, message));
    }
    let Some(mut manifest) = session_manifest(&session_id)? else {
        return Ok((json!({"run": null}), "No overnight runs belong to this session.".into()));
    };
    match command {
        OvernightCommand::Status => {
            let message = overnight::format_status_markdown(&manifest);
            Ok((serde_json::to_value(manifest)?, message))
        }
        OvernightCommand::Log => {
            let message = overnight::format_log_markdown(&manifest, 100);
            Ok((
                json!({"run_id": manifest.run_id, "events": overnight::read_events(&manifest)?}),
                message,
            ))
        }
        OvernightCommand::Review => {
            // Return the actual review document, not an opener on the remote host.
            overnight::render_review_html(&manifest)?;
            let html = std::fs::read_to_string(&manifest.review_path)?;
            let cards = overnight::read_task_cards(&manifest)?;
            let mut message = format!(
                "# Overnight review: {}\n\n{}\n\n## Task outcomes\n",
                manifest.run_id,
                overnight::format_status_markdown(&manifest)
            );
            if cards.is_empty() {
                message.push_str("\nNo task cards have been recorded yet.\n");
            }
            for card in &cards {
                message.push_str(&format!("\n### {}\n\nStatus: {}\n", card.title, card.status));
                if let Some(outcome) = &card.outcome {
                    message.push_str(&format!("\n{outcome}\n"));
                }
                if let Some(result) = &card.validation.result {
                    message.push_str(&format!("\nValidation: {result}\n"));
                }
                for evidence in &card.validation.evidence {
                    message.push_str(&format!("- Evidence: {evidence}\n"));
                }
                for followup in &card.followups {
                    message.push_str(&format!("- Follow-up: {followup}\n"));
                }
            }
            Ok((
                json!({"run_id": manifest.run_id, "html": html, "path": manifest.review_path, "tasks": cards}),
                message,
            ))
        }
        OvernightCommand::Cancel => {
            if !matches!(
                manifest.status,
                OvernightRunStatus::Completed | OvernightRunStatus::Failed
            ) {
                manifest.status = OvernightRunStatus::CancelRequested;
                manifest.cancel_requested_at = Some(chrono::Utc::now());
                overnight::save_manifest(&manifest)?;
                overnight::record_event(
                    &manifest,
                    "cancel_requested",
                    "User requested overnight cancellation".into(),
                    json!({}),
                    true,
                )?;
                overnight::render_review_html(&manifest)?;
            }
            let message = overnight::format_status_markdown(&manifest);
            Ok((serde_json::to_value(manifest)?, message))
        }
        OvernightCommand::Start { .. } | OvernightCommand::Help => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn logout_requires_explicit_supported_provider_and_confirmation() {
        let (data, _) = logout(&[]).unwrap();
        assert_eq!(data["confirmation_required"], true);
        let (data, message) = logout(&["deepseek".into()]).unwrap();
        assert_eq!(data["confirmation_required"], true);
        assert!(message.contains("No credential was removed"));
        assert!(logout(&["deepseek".into(), "yes".into()]).is_err());
        assert!(logout(&["invalid-provider".into(), "confirm".into()]).is_err());
        assert!(logout(&["copilot".into(), "confirm".into()]).is_err());
    }

    #[test]
    fn remote_help_and_cloud_are_side_effect_free() {
        let (data, message) = remote(&["help".into()]).unwrap();
        assert_eq!(data["scope"], "runtime host");
        assert!(message.contains("No") || message.contains("no server"));
        assert!(remote(&["cloud".into()]).is_err());
        assert!(remote(&["on".into(), "extra".into()]).is_err());
        assert!(remote(&["revoke".into()]).is_err());
    }

    #[test]
    fn read_only_commands_reject_mutation_arguments() {
        assert!(status_args(&[]).is_ok());
        assert!(status_args(&["status".into()]).is_ok());
        assert!(status_args(&["resume".into()]).is_err());
        assert!(status_args(&["delete".into()]).is_err());
    }

    #[test]
    fn initiatives_reply_points_at_the_registered_goal_tool() {
        let (data, message) = initiatives_reply();
        assert_eq!(data["retired"], true);
        assert_eq!(data["replacement"], "goal tool");
        assert!(message.contains("`goal`"), "{message}");
        assert!(message.contains("registered"), "{message}");
        assert!(message.contains("preserved"), "{message}");
        assert!(!message.contains("not registered"), "{message}");
    }
}
