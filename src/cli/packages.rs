//! `horon package`: install and manage the extension and plugin catalog.
//!
//! A running daemon owns the catalog, so a mutation goes to it and returns only
//! after the daemon published the resulting generation; open sessions then pick
//! that generation up at a turn boundary. When no daemon is reachable, the same
//! store's file lock publishes the change directly, so managing packages never
//! requires a daemon. Listing and verifying always read the catalog on disk: an
//! install is one atomic rename, so a reader sees a complete generation, not a
//! half-written one.
//!
//! Nothing here loads a provider profile, starts the daemon, checks for updates
//! or runs a package executable.

mod daemon;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::args::PackageCommand;
use crate::packages::{InstallRequest, PackageKind, PackageRecord, PackageScope, PackageState, PackageStore};
use crate::protocol::{PackageControlOperation, PackageControlScope, PackageControlState, PackageSummary};

pub(crate) async fn run(action: PackageCommand) -> Result<()> {
    let store = PackageStore::for_current_user()?;
    match action {
        PackageCommand::Install {
            directory,
            project,
            replace,
        } => install(&store, directory, project, replace).await,
        PackageCommand::List { kind } => list(&store, kind.map(PackageKind::from)),
        PackageCommand::Verify => verify(&store),
        PackageCommand::Disable { id } => disable(&store, &id).await,
        PackageCommand::Remove { id } => remove(&store, &id).await,
    }
}

async fn install(store: &PackageStore, directory: PathBuf, project: Option<PathBuf>, replace: bool) -> Result<()> {
    let scope = match project {
        Some(root) => PackageScope::project(root)?,
        None => PackageScope::Global,
    };
    let operation = PackageControlOperation::Install {
        directory: canonical_text(&directory)?,
        scope: wire_scope(&scope),
        replace,
    };
    if let Some(receipt) = daemon::apply(operation).await? {
        let summary = receipt
            .single()
            .context("the daemon reported no installed package; check `horon package list`")?;
        println!(
            "Installed {} (catalog generation {}).",
            describe_summary(summary),
            receipt.generation
        );
        return Ok(());
    }
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

async fn disable(store: &PackageStore, id: &str) -> Result<()> {
    if let Some(receipt) = daemon::apply(PackageControlOperation::Disable { id: id.to_owned() }).await? {
        println!("Disabled {id} (catalog generation {}).", receipt.generation);
        return Ok(());
    }
    let generation = store.disable(id)?;
    println!("Disabled {id} (catalog generation {generation}).");
    Ok(())
}

async fn remove(store: &PackageStore, id: &str) -> Result<()> {
    if let Some(receipt) = daemon::apply(PackageControlOperation::Remove { id: id.to_owned() }).await? {
        println!("Removed {id} (catalog generation {}).", receipt.generation);
        return Ok(());
    }
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

/// One record of the daemon's reply: the same line without the host-local
/// source path, which the wire contract does not carry.
fn describe_summary(summary: &PackageSummary) -> String {
    format!(
        "{} {} [{}, {}, {}]",
        summary.id,
        summary.version,
        kind_label(summary.kind),
        control_state_label(summary.state),
        control_scope_label(&summary.scope)
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

fn control_state_label(state: PackageControlState) -> &'static str {
    match state {
        PackageControlState::Installed => "installed",
        PackageControlState::Disabled => "disabled",
        PackageControlState::Revoked => "removed",
    }
}

fn scope_label(scope: &PackageScope) -> String {
    match scope.project_root() {
        Some(root) => format!("project {}", root.display()),
        None => "global".to_owned(),
    }
}

fn control_scope_label(scope: &PackageControlScope) -> String {
    match scope {
        PackageControlScope::Global => "global".to_owned(),
        PackageControlScope::Project { root } => format!("project {root}"),
    }
}

/// A catalog scope as the daemon receives it.
fn wire_scope(scope: &PackageScope) -> PackageControlScope {
    match scope.project_root() {
        Some(root) => PackageControlScope::Project {
            root: root.display().to_string(),
        },
        None => PackageControlScope::Global,
    }
}

/// The canonical, UTF-8 spelling of a package source directory.
///
/// The daemon resolves the same directory from its own working directory, so a
/// relative path would name a different place there. Resolving it here also
/// reports a missing directory before any work starts.
fn canonical_text(path: &Path) -> Result<String> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("resolve the package directory {}", path.display()))?;
    match canonical.to_str() {
        Some(text) => Ok(text.to_owned()),
        None => bail!(
            "package directory is not valid UTF-8 and cannot be named on the daemon wire: {}",
            canonical.display()
        ),
    }
}
