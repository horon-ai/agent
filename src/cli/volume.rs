//! `horon volume`: export, import, quiesce, resume, key management and
//! `delete_user` for one user volume.
//!
//! The behaviour lives in `crate::memory::export`; this module is the
//! command surface and its report. Every command acts on the `HORON_HOME` the
//! process resolved, so a hosted Machine's own volume is the volume it names and
//! no argument can point at another user's.
//!
//! The commands are offline: none of them needs a provider, a daemon or a
//! session, and `delete_user` reaches the control plane with the token the
//! operator names rather than with a stored credential.

use crate::cli::args::{VolumeCommand, VolumeKeyCommand};
use crate::memory::export::{
    VolumeLayout, archive,
    control_plane::{DEFAULT_ENDPOINT, HttpControlPlane, Secret},
    delete, fence, key,
};
use anyhow::{Context, Result};
use serde_json::json;

/// Resolve the volume this process serves.
fn layout() -> Result<VolumeLayout> {
    let home = crate::storage::horon_agent_dir().context("cannot resolve HORON_HOME")?;
    Ok(VolumeLayout::for_home(home))
}

/// Run one `volume` subcommand.
pub(crate) fn run(command: VolumeCommand) -> Result<()> {
    let layout = layout()?;
    match command {
        VolumeCommand::Export { output, json } => export(&layout, &output, json),
        VolumeCommand::Import { input, json } => import(&layout, &input, json),
        VolumeCommand::Quiesce { json } => quiesce(&layout, json),
        VolumeCommand::Resume { json } => resume(&layout, json),
        VolumeCommand::Key(action) => key_command(&layout, action),
        VolumeCommand::DeleteUser {
            user_id,
            endpoint,
            token_env,
            token_file,
            status,
            json,
        } => delete_user(&layout, &user_id, endpoint, token_env, token_file, status, json),
        VolumeCommand::DeletionStatus { json } => deletion_status(&layout, json),
    }
}

fn export(layout: &VolumeLayout, output: &std::path::Path, json: bool) -> Result<()> {
    let report = archive::export(layout, output)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "Exported {} page(s) ({} bytes) to {} in {} byte(s).",
        report.pages,
        report.page_bytes,
        report.path.display(),
        report.archive_bytes
    );
    Ok(())
}

fn import(layout: &VolumeLayout, input: &std::path::Path, json: bool) -> Result<()> {
    let report = archive::import(layout, input)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "Imported {} page(s) ({} bytes); the index was rebuilt at revision {}.",
        report.pages, report.page_bytes, report.store_revision
    );
    Ok(())
}

fn quiesce(layout: &VolumeLayout, json: bool) -> Result<()> {
    let report = fence::quiesce(layout)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "Quiesced the memory volume at revision {} ({} page file(s)); checkpoint {}.",
        report.fence.revision, report.fence.page_files, report.fence.token
    );
    println!("Writes are fenced. Take the snapshot, then run `horon volume resume`.");
    Ok(())
}

fn resume(layout: &VolumeLayout, json: bool) -> Result<()> {
    let was_fenced = fence::resume(layout)?;
    if json {
        println!("{}", json!({ "was_fenced": was_fenced }));
        return Ok(());
    }
    if was_fenced {
        println!("Lifted the write fence; the memory volume accepts writes again.");
    } else {
        println!("The memory volume was not fenced; nothing to lift.");
    }
    Ok(())
}

fn key_command(layout: &VolumeLayout, action: VolumeKeyCommand) -> Result<()> {
    match action {
        VolumeKeyCommand::Create { key_id, algorithm } => {
            let record = key::create_key(layout, &key_id, &algorithm)?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        VolumeKeyCommand::Show => match key::read_key(layout)? {
            Some(record) => println!("{}", serde_json::to_string_pretty(&record)?),
            None => println!("No key record; this volume has no user data key."),
        },
        VolumeKeyCommand::Revoke => {
            match key::revoke_key(layout)? {
                Some(record) => println!("{}", serde_json::to_string_pretty(&record)?),
                None => println!("No key record to revoke."),
            }
            let wiped = key::wipe_staged_material(layout)?;
            if wiped > 0 {
                println!("Wiped {wiped} staged key file(s).");
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn delete_user(
    layout: &VolumeLayout,
    user_id: &str,
    endpoint: Option<String>,
    token_env: Option<String>,
    token_file: Option<std::path::PathBuf>,
    status: bool,
    json: bool,
) -> Result<()> {
    if status {
        return deletion_status(layout, json);
    }
    let endpoint = endpoint.unwrap_or_else(|| DEFAULT_ENDPOINT.to_string());
    let token = Secret::resolve(token_env.as_deref(), token_file.as_ref())?;
    let control = HttpControlPlane::new(endpoint, token)?;
    let report = delete::delete_user(layout, &control, user_id)?;
    print_delete_report(&report, json)?;
    if !report.is_deleted() {
        anyhow::bail!(
            "delete_user did not finish: step `{}` failed ({}). Re-run the command to retry it.",
            report.failed_step.as_deref().unwrap_or("unknown"),
            report.failure.as_deref().unwrap_or("no reason recorded")
        );
    }
    Ok(())
}

fn deletion_status(layout: &VolumeLayout, json: bool) -> Result<()> {
    match delete::read_job(layout)? {
        Some(job) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&job)?);
            } else {
                println!("Deletion job {}: {}", job.job_id, job.state().as_str());
                println!("  finished: {}", job.done().join(", "));
                let remaining = job.remaining();
                if !remaining.is_empty() {
                    println!("  remaining: {}", remaining.join(", "));
                }
                if let Some(completed_at) = &job.completed_at {
                    println!("  completed: {completed_at}");
                }
            }
            Ok(())
        }
        None => {
            if json {
                println!("{}", json!({ "job": null }));
            } else {
                println!("No deletion job for this volume.");
            }
            Ok(())
        }
    }
}

fn print_delete_report(report: &delete::DeleteReport, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    println!("Deletion job {}: {}", report.job_id, report.state.as_str());
    println!("  finished: {}", report.steps_done.join(", "));
    if let Some(step) = &report.failed_step {
        println!("  failed at: {step}");
    }
    if let Some(failure) = &report.failure {
        println!("  reason: {failure}");
    }
    if !report.remaining.is_empty() {
        println!("  remaining: {}", report.remaining.join(", "));
    }
    Ok(())
}
