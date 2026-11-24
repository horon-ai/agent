//! `skills list` and `skills install`: the existing managed-skill installer
//! from the command line.
//!
//! Install goes through the same code as `skill_manage install`: the source is
//! checked before anything is written, the clone lands in a staging directory,
//! and one rename puts the finished skill in place. No install script runs.
use anyhow::{Result, bail};
use serde::Serialize;

use crate::skill::SkillRegistry;
use crate::skill_install::{self, InstallRecord};

pub(crate) async fn run_skills_command(command: crate::cli::args::SkillsCommand) -> Result<()> {
    use crate::cli::args::SkillsCommand;
    match command {
        SkillsCommand::List { json } => run_skills_list_command(json),
        SkillsCommand::Install {
            source,
            git_ref,
            name,
            approve,
        } => run_skills_install_command(source, git_ref, name, approve).await,
    }
}

#[derive(Serialize)]
struct Row {
    name: String,
    description: String,
    path: String,
    /// Where an installed skill came from; absent for a skill written by hand.
    #[serde(skip_serializing_if = "Option::is_none")]
    provenance: Option<InstallRecord>,
}

fn run_skills_list_command(json: bool) -> Result<()> {
    let working_dir = std::env::current_dir()?;
    let registry = SkillRegistry::load_for_working_dir(Some(&working_dir))?;
    let locks = skill_install::read_lock()?;
    let mut rows: Vec<Row> = registry
        .list()
        .into_iter()
        .map(|skill| Row {
            name: skill.name.clone(),
            description: skill.description.clone(),
            path: skill.path.display().to_string(),
            provenance: locks.iter().find(|lock| lock.name == skill.name).cloned(),
        })
        .collect();
    rows.sort_by(|left, right| left.name.cmp(&right.name));
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("No skills found.");
    }
    for row in rows {
        println!("{}: {}", row.name, row.description);
        println!("  path: {}", row.path);
        match row.provenance {
            Some(lock) => {
                let reference = match &lock.git_ref {
                    Some(name) => format!(" ref {name}"),
                    None => String::new(),
                };
                println!(
                    "  source: {}{reference} at {}",
                    lock.source,
                    &lock.commit[..lock.commit.len().min(12)]
                );
            }
            None => println!("  source: local"),
        }
    }
    Ok(())
}

async fn run_skills_install_command(
    source: String,
    git_ref: Option<String>,
    name: Option<String>,
    approve: bool,
) -> Result<()> {
    if !approve {
        bail!(
            "Installing writes a skill into the managed skills directory. Review the source, then pass --approve to install from {source}."
        );
    }
    if !crate::config::config().tools.managed_skills {
        bail!("Managed skills are off. Set `managed_skills = true` under [tools] in config.toml to allow installs.");
    }
    let installed =
        tokio::task::spawn_blocking(move || skill_install::install(&source, git_ref.as_deref(), name.as_deref()))
            .await
            .map_err(|error| anyhow::anyhow!("the skill install task failed: {error}"))??;
    println!(
        "Installed skill '{}' from {} at {}\npath: {}",
        installed.name,
        installed.source,
        &installed.commit[..installed.commit.len().min(12)],
        installed.path.display()
    );
    Ok(())
}
