//! The destructive-command gate for the `bash` tool (issue #604).
//!
//! Kept in its own file so the policy seam is easy to find and review: this is
//! the only thing standing between a model's `rm -rf` and the user's data.

/// Apply the deterministic destructive-command gate, returning refusal text
/// when the command must not run as-issued.
///
/// Stage 1 is a pure blast-radius assessment; stage 2 turns a `Confirm` verdict
/// into a reflection prompt that a blind retry cannot satisfy. Catastrophic
/// targets (`/`, `$HOME`, credential stores, device nodes) are denied outright.
/// See issue #604.
pub(super) fn destructive_command_refusal(
    command: &str,
    justification: Option<&str>,
    working_dir: Option<std::path::PathBuf>,
) -> Option<String> {
    let mut risk_ctx = horon_agent_command_risk::RiskContext::from_env(working_dir);
    // Assess the same scratch path that the child shell actually receives. A
    // platform whose shells get no scratch directory leaves this unset, and the
    // shell then sees the same `None`.
    if horon_agent_platform::term::passes_scratch_dir_to_shell() {
        risk_ctx.scratch_dir = super::tool_scratch_dir();
    }
    let assessment = horon_agent_command_risk::assess(command, &risk_ctx);
    if assessment.level.runs_immediately() {
        return None;
    }

    let justification = horon_agent_command_risk::Justification {
        text: justification.map(str::to_string),
    };
    match horon_agent_command_risk::gate(&assessment, &justification) {
        horon_agent_command_risk::GateOutcome::Allow => None,
        horon_agent_command_risk::GateOutcome::Deny { reason } => {
            crate::logging::warn(&format!("[bash] denied destructive command: {command}"));
            Some(reason)
        }
        horon_agent_command_risk::GateOutcome::Reflect { prompt } => {
            crate::logging::info(&format!("[bash] destructive command held for justification: {command}"));
            Some(prompt)
        }
    }
}

/// Validate configured tool-selection, plan-mode, approval, interceptor, git-guard,
/// and destructive-command policies for direct `bash` execution outside a model turn.
pub async fn validate_direct_bash_policy(
    session_id: &str,
    command: &str,
    cwd: &std::path::Path,
    force_plan_mode: bool,
) -> anyhow::Result<()> {
    let cfg = crate::config::config();
    let selection = cfg.tools.selection();
    if let Some(allowed) = selection.allowed_tools.as_ref()
        && !crate::tool::tool_name_is_allowed(allowed, "bash")
    {
        anyhow::bail!("Tool 'bash' is not allowed");
    }
    if crate::tool::tool_name_is_disabled(&selection.disabled_tools, "bash") {
        anyhow::bail!("Tool 'bash' is disabled");
    }

    let effects = crate::tool::ToolEffects::write().union(crate::tool::ToolEffects::process());
    if force_plan_mode || !crate::plan_mode::allows_effects(session_id, effects) {
        anyhow::bail!("{}", crate::plan_mode::block_message("bash"));
    }

    let input = serde_json::json!({ "command": command });
    let bash_pattern = crate::approval::bash_pattern_verdict(command);
    if let Some(verdict) = bash_pattern.as_ref()
        && verdict.approval == crate::config::BashPatternApproval::Deny
    {
        anyhow::bail!("Blocked by bash pattern '{}' (deny).", verdict.pattern);
    }

    let approval_mode = crate::tool::run_approval::stricter(
        session_id,
        crate::tool::approval_override::get(session_id).unwrap_or(cfg.tools.approval_mode),
    );
    let danger = crate::approval::danger_class("bash", &input, Some(cwd));
    let approval_paths = crate::tool::path_policy::named_paths("bash", &input, Some(cwd));
    let restricted = crate::approval_grants::restricted_match("bash", &input, Some(cwd));
    let tool_call_id = format!("direct-bash-{}", horon_agent_time::Timestamp::now().timestamp_millis());
    let evaluation = crate::approval::evaluate_and_audit(
        crate::approval::ApprovalCall {
            tool: "bash",
            effects,
            mode: approval_mode,
            dual_confirm: &cfg.tools.dual_confirm_classes,
            danger,
            bash_pattern: bash_pattern.as_ref(),
            paths: &approval_paths,
            restricted: restricted.as_deref(),
        },
        &tool_call_id,
    );
    let evaluation = match evaluation {
        crate::approval::ApprovalEvaluation::RequiresApproval {
            is_dual_confirm: false, ..
        } if restricted.is_none()
            && bash_pattern.is_none()
            && crate::approval_grants::covered("bash", &input, Some(cwd), effects) =>
        {
            crate::approval::audit_granted_call("bash", approval_mode, &tool_call_id);
            crate::approval::ApprovalEvaluation::AutoApproved {
                reason: "grant".to_string(),
            }
        }
        other => other,
    };
    if let crate::approval::ApprovalEvaluation::RequiresApproval { reason, .. } = evaluation {
        anyhow::bail!("Tool 'bash' needs approval before it runs: {reason}. Nothing was executed.");
    }

    if cfg.tools.bash_interceptor == crate::config::BashInterceptorMode::Block
        && let Some(interception) = crate::tool::bash_interceptor::intercept(command, false)
    {
        anyhow::bail!("Blocked: {}\n\nOriginal command: {}", interception.message, command);
    }

    if cfg.tools.git_guard {
        let wd = cwd.to_path_buf();
        let cmd = command.to_string();
        if let Ok(Some(refusal)) = tokio::task::spawn_blocking(move || crate::tool::git_guard::review(&cmd, &wd)).await
        {
            anyhow::bail!("{refusal}");
        }
    }

    if let Some(refusal) = destructive_command_refusal(command, None, Some(cwd.to_path_buf())) {
        anyhow::bail!("{refusal}");
    }

    Ok(())
}

/// The `bash` tool's JSON schema, including the `justification` field the
/// destructive-command gate consumes.
///
/// Lives beside the gate so the schema and the policy that reads it stay in
/// sync, and so bash.rs stays inside the code-size budget.
pub(super) fn bash_parameters_schema() -> serde_json::Value {
    let cmd_desc = if !horon_agent_platform::term::shell_is_posix() {
        "The Windows command to execute via cmd.exe. Use cmd.exe syntax and quoting, not Bash syntax."
    } else {
        "Shell command. Temp files: $HORON_AGENT_SCRATCH_DIR, not /tmp. rg skips hidden and ignored by default."
    };
    serde_json::json!({
        "type": "object",
        "required": ["command"],
        "properties": {
            "intent": crate::tool::intent_schema_property(),
            "command": {
                "type": "string",
                "description": cmd_desc
            },
            "cwd": {
                "type": "string",
                "description": "Call directory; defaults to the previous call's directory."
            },
            "timeout": {
                "type": "integer",
                "description": "Timeout in ms (default 120000, max 600000, 0 disables)."
            },
            "run_in_background": {
                "type": "boolean",
                "description": "Run asynchronously; `HORON_AGENT_PROGRESS {json}` reports progress."
            },
            "name": {
                "type": "string",
                "description": "Background task name (1-48 of a-z0-9._-); naming runs it in background, ignoring timeout."
            },
            "ready": {
                "type": "object",
                "description": "Named service readiness (log and/or port).",
                "properties": {
                    "log": {
                        "type": "string",
                        "description": "Regex over the last 64 KiB."
                    },
                    "port": {
                        "type": "integer",
                        "description": "TCP port to probe."
                    },
                    "host": {
                        "type": "string",
                        "description": "Probe host (default 127.0.0.1)."
                    },
                    "timeout_secs": {
                        "type": "number",
                        "description": "Wait seconds (default 30, max 3600)."
                    }
                }
            },
            "notify": {
                "type": "boolean",
                "description": "Notify on completion."
            },
            "notify_pattern": {
                "type": "string",
                "description": "Live line regex for unnamed background jobs."
            },
            "wake": {
                "type": "boolean",
                "description": "Wake on completion."
            },
            "stall_wake_seconds": {
                "type": "integer",
                "description": "Wake a background job after N idle seconds (min 30)."
            },
            "justification": {
                "type": "string",
                "description": "After a destructive refusal, state the user request served by this command."
            },
            "pty": {
                "type": "string",
                "enum": ["off", "auto", "always"],
                "description": "Terminal: auto if interactive, always, or off (pipes)."
            }
        }
    })
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::destructive_command_refusal;

    #[test]
    fn scratch_log_and_backup_commands_do_not_require_justification() {
        let cwd = std::env::current_dir().ok();
        for command in [
            "cargo test --lib > \"$HORON_AGENT_SCRATCH_DIR/tests.log\" 2>&1",
            "git diff > \"${HORON_AGENT_SCRATCH_DIR}/before.patch\"",
            "env | grep HORON_AGENT",
            "command -v sudo && sudo -n true",
            "find /sys -type l -exec readlink {} \\;",
            "find /etc -type f -exec sed -n '1,10p' {} \\;",
        ] {
            assert!(
                destructive_command_refusal(command, None, cwd.clone()).is_none(),
                "{command}"
            );
        }
    }

    #[test]
    fn protected_writes_and_unknown_variables_remain_blocked() {
        for command in [
            "rm -rf /etc",
            "echo bad > /etc/passwd",
            "find /etc -type f -exec rm {} \\;",
            "echo test > \"$UNKNOWN/tests.log\"",
        ] {
            assert!(
                destructive_command_refusal(command, None, std::env::current_dir().ok()).is_some(),
                "{command}"
            );
        }
    }
}
