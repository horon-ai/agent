use anyhow::Result;

use crate::cli::args::Command;

/// What a command that runs before provider setup left for the caller to do.
pub(crate) enum Early {
    /// Not one of these commands; continue the normal startup.
    Pass,
    /// Finished; nothing more to run.
    Done,
    /// Continue as this command, with the usual provider setup and approvals.
    Rewrite(Box<Command>),
}

/// Commands that read local state and need no model, daemon or session.
pub(crate) async fn run_early_command(command: &Option<Command>) -> Result<Early> {
    match command {
        Some(Command::Stats { from, through, json }) => {
            super::run_stats_command(from.as_deref(), through.as_deref(), *json)?;
            Ok(Early::Done)
        }
        Some(Command::Preview { tool, input, json }) => {
            super::run_preview_command(tool, input, *json).await?;
            Ok(Early::Done)
        }
        Some(Command::Diagnostics {
            files,
            json,
            select,
            delegate,
        }) => diagnostics(files, *json, select, *delegate).await,
        Some(Command::Config { command }) => {
            super::config_cli::run(command)?;
            Ok(Early::Done)
        }
        _ => Ok(Early::Pass),
    }
}

/// List diagnostics, or hand the selected ones to a normal one-shot run. The
/// run goes through the usual startup, so its approvals and isolation apply.
async fn diagnostics(files: &[String], json: bool, select: &[usize], delegate: bool) -> Result<Early> {
    use crate::tool::diagnostics_list::{collect, handoff, render};
    let reports = collect(files, &std::env::current_dir()?).await;
    if !delegate {
        if json {
            println!("{}", serde_json::to_string_pretty(&reports)?);
        } else {
            print!("{}", render(&reports));
        }
        return Ok(Early::Done);
    }
    let (paths, brief) = handoff(&reports, select)?;
    eprintln!("Delegating {} diagnostic(s) in: {}", select.len(), paths.join(", "));
    Ok(Early::Rewrite(Box::new(Command::one_shot(brief))))
}
