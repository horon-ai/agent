//! Named launch profiles: one local namespace for config, sessions, credentials
//! and both daemon sockets, derived from a validated name.
use anyhow::{Result, bail};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub(crate) const PROFILE_ENV: &str = "HORON_PROFILE";

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Profile {
    pub(crate) name: String,
    pub(crate) home: PathBuf,
    pub(crate) runtime_dir: PathBuf,
}

pub(crate) fn validate_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 32
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if !valid {
        bail!("Invalid profile name '{name}'. Use 1-32 lowercase letters, digits or hyphens.");
    }
    Ok(())
}

pub(crate) fn resolve(name: &str, base_home: &Path, runtime_base: &Path) -> Result<Profile> {
    validate_name(name)?;
    let home = base_home.join("profiles").join(name);
    if let Ok(metadata) = std::fs::symlink_metadata(&home)
        && metadata.file_type().is_symlink()
    {
        bail!("Profile directory {} must not be a symbolic link", home.display());
    }
    // A short digest of the exact home keeps sockets under the Unix path limit
    // and gives every profile root its own endpoint names.
    let digest = Sha256::digest(home.to_string_lossy().as_bytes());
    let runtime_dir = runtime_base.join(format!("horon-profile-{}", hex_prefix(&digest)));
    Ok(Profile {
        name: name.to_owned(),
        home,
        runtime_dir,
    })
}

fn hex_prefix(digest: &[u8]) -> String {
    digest.iter().take(6).map(|byte| format!("{byte:02x}")).collect()
}

/// Export the profile to this process and every child it starts.
pub(crate) fn apply(profile: &Profile) -> Result<()> {
    std::fs::create_dir_all(&profile.home)?;
    std::fs::create_dir_all(&profile.runtime_dir)?;
    horon_agent_platform::set_permissions_private_dir(&profile.runtime_dir)?;
    crate::env::set_var("HORON_HOME", &profile.home);
    crate::env::set_var("HORON_AGENT_RUNTIME_DIR", &profile.runtime_dir);
    crate::env::set_var("HORON_AGENT_SOCKET", profile.runtime_dir.join("horon-agent.sock"));
    crate::env::set_var(
        "HORON_AGENT_API_SOCKET",
        profile.runtime_dir.join("horon-agent-api.sock"),
    );
    crate::env::set_var(PROFILE_ENV, &profile.name);
    Ok(())
}

/// Resolve the profile selected by `--isolated-profile`, from the current base paths.
pub(crate) fn select(name: &str) -> Result<Profile> {
    resolve(
        name,
        &crate::storage::horon_agent_dir()?,
        &crate::storage::runtime_dir(),
    )
}

pub(crate) fn describe(profile: &Profile) -> String {
    format!(
        "profile: {name}\nhome: {home}\nruntime: {runtime}\nalias: alias horon-{name}='horon --isolated-profile {name}'",
        name = profile.name,
        home = profile.home.display(),
        runtime = profile.runtime_dir.display(),
    )
}

#[cfg(test)]
mod tests;
