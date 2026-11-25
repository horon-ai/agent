use anyhow::Result;
use serde::Serialize;

use crate::config::Config;
use crate::effective_settings::{EffectiveSource, effective_settings};
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub(crate) enum ConfigCommand {
    /// Show effective values and the layer behind each, without secrets
    Show {
        /// Emit JSON instead of plain text
        #[arg(long)]
        json: bool,
    },
    /// Validate config.toml with the startup loader; exits 1 on an invalid file
    Check {
        /// Emit JSON instead of plain text
        #[arg(long)]
        json: bool,
    },
}

#[derive(Serialize)]
struct Setting {
    key: String,
    value: String,
    source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<String>,
}

fn source_name(source: EffectiveSource) -> &'static str {
    match source {
        EffectiveSource::Default => "default",
        EffectiveSource::Cli => "cli",
        EffectiveSource::Env => "env",
        EffectiveSource::File => "file",
    }
}

pub(crate) fn run(command: &ConfigCommand) -> Result<()> {
    match command {
        ConfigCommand::Show { json } => show(*json),
        ConfigCommand::Check { json } => check(*json),
    }
}

/// The config file, parsed and validated by the strict startup check (including
/// unknown TOML keys and `[provider] default_provider`), with any failure
/// reported as the loader words it (file path, TOML position and key).
fn check(json: bool) -> Result<()> {
    let path = Config::path().map(|path| path.display().to_string());
    let failure = Config::check_strict().err().map(|error| format!("{error:#}"));
    if json {
        let report = serde_json::json!({ "ok": failure.is_none(), "path": path, "error": failure });
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if let Some(error) = &failure {
        println!("invalid");
        eprintln!("{error}");
    } else {
        println!("ok\t{}", path.as_deref().unwrap_or("(no config file)"));
    }
    if failure.is_some() {
        std::process::exit(1);
    }
    Ok(())
}

/// The allow-listed effective values with the layer behind each. The list is a
/// fixed table, so provider tables, headers and keys can never appear in it. A
/// config file that does not parse is an error here: the defaults startup would
/// fall back to are not what the file says.
fn show(json: bool) -> Result<()> {
    Config::load_strict()?;
    let settings: Vec<Setting> = effective_settings()
        .into_iter()
        .map(|row| Setting {
            key: row.key,
            value: row.value,
            source: source_name(row.source),
            origin: row.origin,
        })
        .collect();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "settings": settings }))?
        );
    } else {
        for setting in &settings {
            match &setting.origin {
                Some(origin) => println!("{}\t{}\t{}\t{origin}", setting.key, setting.value, setting.source),
                None => println!("{}\t{}\t{}", setting.key, setting.value, setting.source),
            }
        }
    }
    Ok(())
}
