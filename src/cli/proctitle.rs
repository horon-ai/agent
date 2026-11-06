//! Mapping from parsed CLI arguments to an initial process title.
//!
//! This logic depends on the clap `Args`/`Command` types defined in `cli`, so
//! it lives in the CLI layer. The low-level title-setting primitives it uses
//! (`compact_process_title`, `session_name`, `set_title`) live in the
//! `process_title` core module.

use crate::cli::args::{Args, Command};
use crate::process_title::{compact_process_title, session_name, set_title};

pub(crate) fn initial_title(args: &Args) -> String {
    match &args.command {
        Some(Command::Serve { .. }) => "horon:server".to_string(),
        Some(Command::Acp) => "horon-agent acp".to_string(),
        Some(Command::Server { .. }) => "horon-agent server".to_string(),
        #[cfg(unix)]
        Some(Command::ApiBridge { .. }) => "horon-agent api-bridge".to_string(),
        Some(Command::Run { .. }) => "horon-agent run".to_string(),
        Some(Command::Account { .. }) => "horon-agent account".to_string(),
        Some(Command::Mcp { .. }) => "horon-agent mcp".to_string(),
        Some(Command::Repl) => "horon-agent repl".to_string(),
        Some(Command::Update) => "horon-agent update".to_string(),
        Some(Command::InstallRelease { .. }) => "horon-agent install-release".to_string(),
        Some(Command::Version { .. }) => "horon-agent version".to_string(),
        Some(Command::Usage { .. }) => "horon-agent usage".to_string(),
        Some(Command::RuntimeState(_)) => "horon-agent runtime-state".to_string(),
        Some(Command::Debug { .. }) => "horon-agent debug".to_string(),
        Some(Command::Provider(_)) => "horon-agent provider".to_string(),
        Some(Command::Memory(_)) => "horon-agent memory".to_string(),
        Some(Command::Session(_)) => "horon-agent session".to_string(),
        Some(Command::Ambient(_)) => "horon-agent ambient".to_string(),
        Some(Command::Cloud(_)) => "horon-agent cloud".to_string(),
        Some(Command::Pair { .. }) => "horon-agent pair".to_string(),
        Some(Command::Permissions) => "horon-agent permissions".to_string(),
        Some(Command::Approvals { .. }) => "horon-agent approvals".to_string(),
        Some(Command::Model(_)) => "horon-agent model".to_string(),
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
}
