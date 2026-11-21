//! `horon package`: install and manage the extension and plugin catalog.
//!
//! Every command here is a local filesystem operation on `$HORON_HOME/packages`.
//! None of them loads a provider profile, checks for updates, starts the daemon,
//! or runs a package executable, so packages stay manageable on a machine that
//! has no model configured.

use std::path::PathBuf;

use anyhow::Result;

use super::args::PackageCommand;
use crate::packages::{InstallRequest, PackageKind, PackageRecord, PackageScope, PackageState, PackageStore};

pub(crate) fn run(action: PackageCommand) -> Result<()> {
    let store = PackageStore::for_current_user()?;
    match action {
        PackageCommand::Install {
            directory,
            project,
            replace,
        } => install(&store, directory, project, replace),
        PackageCommand::List { kind } => list(&store, kind.map(PackageKind::from)),
        PackageCommand::Verify => verify(&store),
        PackageCommand::Disable { id } => disable(&store, &id),
        PackageCommand::Remove { id } => remove(&store, &id),
    }
}

fn install(store: &PackageStore, directory: PathBuf, project: Option<PathBuf>, replace: bool) -> Result<()> {
    let scope = match project {
        Some(root) => PackageScope::project(root)?,
        None => PackageScope::Global,
    };
    let request = InstallRequest::new(directory, scope);
    let request = if replace { request.replacing() } else { request };
    let record = store.install(request)?;
    println!("Installed {}", describe(&record));
    Ok(())
}

fn list(store: &PackageStore, kind: Option<PackageKind>) -> Result<()> {
    let catalog = store.catalog()?;
    let mut printed = false;
    for record in catalog.packages() {
        if kind.is_some_and(|kind| record.kind != kind) {
            continue;
        }
        if !printed {
            println!("Installed packages (catalog generation {}):", catalog.generation());
            printed = true;
        }
        println!("  {}", describe(record));
    }
    if !printed {
        println!("No packages installed.");
    }
    Ok(())
}

fn verify(store: &PackageStore) -> Result<()> {
    let report = store.verify()?;
    let noun = if report.packages == 1 { "package" } else { "packages" };
    println!(
        "Verified {} {noun} at catalog generation {}.",
        report.packages, report.generation
    );
    Ok(())
}

fn disable(store: &PackageStore, id: &str) -> Result<()> {
    let generation = store.disable(id)?;
    println!("Disabled {id} (catalog generation {generation}).");
    Ok(())
}

fn remove(store: &PackageStore, id: &str) -> Result<()> {
    let generation = store.remove(id)?;
    println!("Removed {id} (catalog generation {generation}).");
    Ok(())
}

/// One catalog record as one line: ID, version, kind, state, scope and source.
fn describe(record: &PackageRecord) -> String {
    format!(
        "{} {} [{}, {}, {}] from {}",
        record.id,
        record.version,
        kind_label(record.kind),
        state_label(record.state),
        scope_label(&record.scope),
        record.source
    )
}

fn kind_label(kind: PackageKind) -> &'static str {
    match kind {
        PackageKind::Extension => "extension",
        PackageKind::Plugin => "plugin",
    }
}

fn state_label(state: PackageState) -> &'static str {
    match state {
        PackageState::Installed => "installed",
        PackageState::Disabled => "disabled",
        PackageState::Revoked => "removed",
    }
}

fn scope_label(scope: &PackageScope) -> String {
    match scope.project_root() {
        Some(root) => format!("project {}", root.display()),
        None => "global".to_owned(),
    }
}
