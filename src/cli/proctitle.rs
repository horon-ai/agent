//! Mapping from parsed CLI arguments to an initial process title.
//!
//! This logic depends on the clap `Args`/`Command` types defined in `cli`, so
//! it lives in the CLI layer. The low-level title-setting primitives it uses
//! (`compact_process_title`, `session_name`, `set_title`) live in the
//! `process_title` core module.

use crate::cli::args::{AmbientCommand, Args, Command};
use crate::process_title::{compact_process_title, session_name, set_title};

pub(crate) fn initial_title(args: &Args) -> String {
    match &args.command {
        Some(Command::Serve { .. }) => "horon:server".to_string(),
        Some(Command::Acp) => "horon-agent acp".to_string(),
        Some(Command::Server { .. }) => "horon-agent server".to_string(),
        Some(Command::Connect) => "horon:client".to_string(),
        #[cfg(unix)]
        Some(Command::ApiBridge { .. }) => "horon-agent api-bridge".to_string(),
        Some(Command::Run { .. }) => "horon-agent run".to_string(),
        Some(Command::Login { .. }) => "horon-agent login".to_string(),
        Some(Command::Account { .. }) => "horon-agent account".to_string(),
        Some(Command::Mcp { .. }) => "horon-agent mcp".to_string(),
        Some(Command::Repl) => "horon-agent repl".to_string(),
        Some(Command::Update) => "horon-agent update".to_string(),
        Some(Command::InstallRelease { .. }) => "horon-agent install-release".to_string(),
        Some(Command::Version { .. }) => "horon-agent version".to_string(),
        Some(Command::Usage { .. }) => "horon-agent usage".to_string(),
        Some(Command::RuntimeState(_)) => "horon-agent runtime-state".to_string(),
        Some(Command::Debug { .. }) => "horon-agent debug".to_string(),
        Some(Command::Auth(_)) => "horon-agent auth".to_string(),
        Some(Command::Provider(_)) => "horon-agent provider".to_string(),
        Some(Command::Memory(_)) => "horon-agent memory".to_string(),
        Some(Command::Session(_)) => "horon-agent session".to_string(),
        Some(Command::Ambient(subcommand)) => match subcommand {
            AmbientCommand::RunVisible => "horon-agent ambient visible".to_string(),
            _ => "horon-agent ambient".to_string(),
        },
        Some(Command::Cloud(_)) => "horon-agent cloud".to_string(),
        Some(Command::Pair { .. }) => "horon-agent pair".to_string(),
        Some(Command::Permissions) => "horon-agent permissions".to_string(),
        Some(Command::Approvals { .. }) => "horon-agent approvals".to_string(),
        Some(Command::Transcript { .. }) => "horon-agent transcript".to_string(),
        Some(Command::Companion { .. }) => "horon-agent companion".to_string(),
        Some(Command::Dictate { .. }) => "horon-agent dictate".to_string(),
        Some(Command::SetupHotkey {
            listen_macos_hotkey,
            notify_cli_launch,
            listen_windows_hotkey,
            uninstall,
        }) => {
            if *listen_macos_hotkey || *listen_windows_hotkey {
                "horon-agent hotkey listener".to_string()
            } else if notify_cli_launch.is_some() {
                "horon-agent shortcut reminder".to_string()
            } else if *uninstall {
                "horon-agent hotkey uninstall".to_string()
            } else {
                "horon-agent hotkey setup".to_string()
            }
        }
        Some(Command::Replay { .. }) => "horon-agent replay".to_string(),
        Some(Command::Model(_)) => "horon-agent model".to_string(),
        Some(Command::ProviderTestCoverage { .. }) => "horon-agent provider-test-coverage".to_string(),
        Some(Command::ProviderDoctor { .. }) => "horon-agent provider-doctor".to_string(),
        Some(Command::AuthTest { .. }) => "horon-agent auth-test".to_string(),
        Some(Command::Restart { .. }) => "horon-agent restart".to_string(),
        Some(Command::Menubar { .. }) => "horon-agent menubar".to_string(),
        Some(Command::SetupLauncher) => "horon-agent setup-launcher".to_string(),
        None => {
            if let Some(resume) = args.resume.as_deref().filter(|resume| !resume.is_empty()) {
                // The `horon:` colon labels share Linux's 15-char comm budget with
                // the session name, so they use the short brand form rather than
                // `horon-agent`.
                compact_process_title("horon:c:", Some(&session_name(resume)))
            } else {
                "horon:client".to_string()
            }
        }
    }
}

pub(crate) fn set_initial_title(args: &Args) {
    set_title(initial_title(args));
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn initial_title_labels_server() {
        let args = Args::parse_from(["horon-agent", "serve"]);
        assert_eq!(initial_title(&args), "horon:server");
    }

    #[test]
    fn initial_title_labels_resume_client_with_short_name() {
        let args = Args::parse_from(["horon-agent", "--resume", "session_fox_123"]);
        assert_eq!(initial_title(&args), "horon:c:fox");
    }

    #[test]
    fn initial_title_labels_windows_hotkey_listener() {
        let args = Args::parse_from(["horon-agent", "setup-hotkey", "--listen-windows-hotkey"]);
        assert_eq!(initial_title(&args), "horon-agent hotkey listener");
    }

    #[test]
    fn initial_title_labels_hotkey_uninstall() {
        let args = Args::parse_from(["horon-agent", "setup-hotkey", "--uninstall"]);
        assert_eq!(initial_title(&args), "horon-agent hotkey uninstall");
    }
}
