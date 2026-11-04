//! Top-level `install-release`: verify and install a downloaded release from a local asset directory.
//!
//! The installers fetch a release's assets and then hand the directory to this command, so the
//! checksum check, archive extraction, immutable version directory, and channel activation all run
//! through `update::install_release_from_directory` instead of being re-implemented in shell. A
//! failed verification returns the error unchanged, so the installer reports it and leaves the
//! previous installation in place.

use anyhow::{Context, Result};
use std::path::PathBuf;

pub(crate) async fn run(assets: PathBuf, agent_version: String, source_binary: Option<PathBuf>) -> Result<()> {
    let label = assets.display().to_string();
    // Verification and staging are blocking filesystem work that must not run on a runtime thread.
    let outcome = tokio::task::spawn_blocking(move || {
        crate::update::install_release_from_directory(&assets, &agent_version, source_binary.as_deref())
    })
    .await?;
    let installed =
        outcome.with_context(|| format!("failed to install a release from the assets directory {label}"))?;
    println!("installed complete release at {}", installed.display());
    Ok(())
}
