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
                "description": "Service and bg task ID (1-48 chars: letters, digits, ._-)."
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
