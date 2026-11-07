//! Native SSH attach is an interactive-TUI feature; this headless build keeps
//! only the resume-hint helper the CLI still uses.

use anyhow::{Result, bail};

use super::args::Args;
use super::provider_init::ProviderChoice;

fn validate(args: &Args) -> Result<()> {
    match args.command {
        None => {}
        _ => bail!("--ssh supports the interactive client only"),
    }
    if args
        .resume
        .as_deref()
        .is_some_and(|id| id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)))
    {
        bail!("--ssh --resume requires an explicit remote session ID; local session lookup is not used")
    }
    if args.provider != ProviderChoice::Auto
        || args.model.is_some()
        || args.provider_profile.is_some()
        || args.tool_profile.is_some()
        || args.tools.is_some()
        || args.disabled_tools.is_some()
        || args.disable_base_tools
        || args.mcp_tools.is_some()
        || args.mcp_tools_token_threshold.is_some()
    {
        bail!(
            "provider/tool startup flags cannot configure an existing SSH server; use the remote /model command or configure the remote host"
        )
    }
    Ok(())
}

pub(crate) async fn run(args: Args) -> Result<()> {
    validate(&args)?;
    bail!("native SSH attach is an interactive-TUI feature and is unavailable in this headless build")
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Return a remote-aware resume command, never a local session lookup.
pub(crate) fn resume_hint(session_id: &str) -> Option<String> {
    let host = std::env::var("HORON_AGENT_SSH_REMOTE").ok()?;
    let mut args = vec!["horon-agent".to_string(), "--ssh".to_string(), quote(&host)];
    for (flag, variable) in [
        ("--ssh-binary", "HORON_AGENT_SSH_BINARY"),
        ("--ssh-server-socket", "HORON_AGENT_SSH_SERVER_SOCKET"),
        ("--remote-working-dir", "HORON_AGENT_SSH_WORKING_DIR"),
    ] {
        if let Ok(value) = std::env::var(variable) {
            args.extend([flag.to_owned(), quote(&value)]);
        }
    }
    args.extend(["--resume".to_string(), quote(session_id)]);
    Some(args.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn remote_modes_accept_explicit_remote_ids_without_local_lookup() {
        for argv in [
            vec!["horon-agent", "--ssh", "dev"],
            vec!["horon-agent", "--ssh", "dev", "--resume", "session_remote_123"],
        ] {
            validate(&Args::try_parse_from(argv).unwrap()).unwrap();
        }
    }

    #[test]
    fn remote_modes_reject_local_only_operations_before_connecting() {
        for tail in [
            vec!["--resume"],
            vec!["run", "test"],
            vec!["--model", "local-model"],
            vec!["--tools", "bash"],
        ] {
            let mut argv = vec!["horon-agent", "--ssh", "dev"];
            argv.extend(tail);
            assert!(validate(&Args::try_parse_from(argv).unwrap()).is_err());
        }
    }

    #[test]
    fn resume_hint_retains_remote_identity_and_quotes_paths() {
        let _lock = crate::storage::lock_test_env();
        let names = [
            "HORON_AGENT_SSH_REMOTE",
            "HORON_AGENT_SSH_BINARY",
            "HORON_AGENT_SSH_WORKING_DIR",
            "HORON_AGENT_SSH_SERVER_SOCKET",
        ];
        let previous: Vec<_> = names.iter().map(std::env::var_os).collect();
        for name in names {
            crate::env::remove_var(name);
        }
        crate::env::set_var("HORON_AGENT_SSH_REMOTE", "dev");
        crate::env::set_var("HORON_AGENT_SSH_WORKING_DIR", "/srv/sam's repo");
        let hint = resume_hint("session_remote_1").unwrap();
        assert!(hint.contains("--ssh 'dev'"));
        assert!(hint.contains("'/srv/sam'\\''s repo'"));
        assert!(hint.contains("--resume 'session_remote_1'"));
        for (name, value) in names.into_iter().zip(previous) {
            match value {
                Some(value) => crate::env::set_var(name, value),
                None => crate::env::remove_var(name),
            }
        }
    }
}
