//! On-disk state shared by every horon-agent process that checks for updates.
//!
//! Update checks are cheap but rate limited, and several horon-agent processes can run
//! on one machine at once, so the cadence and backoff state live in a single
//! metadata file rather than per-process memory.

use super::UPDATE_CHECK_INTERVAL;
use crate::storage;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateMetadata {
    pub last_check: SystemTime,
    pub installed_version: Option<String>,
    pub installed_from: Option<String>,
    /// When set, automatic update checks are suppressed until this time
    /// because GitHub reported the API rate limit was exhausted. Shared via
    /// the metadata file so every horon-agent process on the machine backs off, not
    /// just the one that saw the 403.
    #[serde(default)]
    pub rate_limited_until: Option<SystemTime>,
    /// Last `releases/latest` response with its ETag, so repeat checks send a
    /// conditional request and an unchanged release costs a cheap 304.
    #[serde(default)]
    pub latest_release_cache: Option<CachedRelease>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedRelease {
    pub url: String,
    pub etag: String,
    pub release: horon_agent_update_core::GitHubRelease,
}

impl Default for UpdateMetadata {
    fn default() -> Self {
        Self {
            last_check: SystemTime::UNIX_EPOCH,
            installed_version: None,
            installed_from: None,
            latest_release_cache: None,
            rate_limited_until: None,
        }
    }
}

impl UpdateMetadata {
    pub fn load() -> Result<Self> {
        let path = metadata_path()?;
        if path.exists() {
            let content = fs::read_to_string(&path)?;
            Ok(serde_json::from_str(&content)?)
        } else {
            Ok(Self::default())
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = metadata_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let previous_permissions = match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Some(metadata.permissions()),
            Ok(_) => anyhow::bail!("update metadata is not a regular file"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let content = serde_json::to_string_pretty(self)?;
        let mut temporary = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
        use std::io::Write;
        temporary.write_all(content.as_bytes())?;
        if let Some(permissions) = previous_permissions {
            temporary.as_file().set_permissions(permissions)?;
        }
        temporary.as_file().sync_all()?;
        temporary.persist(&path).map_err(|error| error.error)?;
        Ok(())
    }

    pub fn should_check(&self) -> bool {
        if self.rate_limited_backoff_remaining().is_some() {
            return false;
        }
        match self.last_check.elapsed() {
            Ok(elapsed) => elapsed > UPDATE_CHECK_INTERVAL,
            Err(_) => true,
        }
    }

    /// Reserve the current check window before any network I/O, so a failed
    /// check and concurrently launched processes all wait UPDATE_CHECK_INTERVAL.
    pub fn claim_check() -> bool {
        let mut metadata = Self::load().unwrap_or_default();
        if !metadata.should_check() {
            return false;
        }
        metadata.last_check = SystemTime::now();
        if let Err(error) = metadata.save() {
            crate::logging::warn(&format!("update: could not record check time: {error:#}"));
        }
        true
    }

    /// Remaining rate-limit backoff, if an earlier check was rate limited and
    /// the backoff window has not elapsed yet.
    pub fn rate_limited_backoff_remaining(&self) -> Option<Duration> {
        let until = self.rate_limited_until?;
        until
            .duration_since(SystemTime::now())
            .ok()
            .filter(|remaining| !remaining.is_zero())
    }
}

pub(super) fn metadata_path() -> Result<PathBuf> {
    Ok(storage::horon_agent_dir()?.join("update_metadata.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respects_rate_limit_backoff() {
        let mut metadata = UpdateMetadata {
            last_check: SystemTime::UNIX_EPOCH,
            ..Default::default()
        };
        assert!(metadata.should_check());
        metadata.rate_limited_until = Some(SystemTime::now() + Duration::from_secs(600));
        assert!(!metadata.should_check());
        assert!(metadata.rate_limited_backoff_remaining().is_some());
        // An elapsed backoff no longer blocks checks.
        metadata.rate_limited_until = Some(SystemTime::now() - Duration::from_secs(1));
        assert!(metadata.rate_limited_backoff_remaining().is_none());
        assert!(metadata.should_check());
    }

    #[test]
    fn enforces_check_interval() {
        let metadata = UpdateMetadata {
            last_check: SystemTime::now(),
            ..Default::default()
        };
        assert!(!metadata.should_check());
    }

    #[test]
    fn claim_check_reserves_the_window() {
        let _guard = crate::storage::lock_test_env();
        let previous = std::env::var_os("HORON_AGENT_HOME");
        let temp = tempfile::TempDir::new().expect("create temp dir");
        horon_agent_base::env::set_var("HORON_AGENT_HOME", temp.path());

        let first = UpdateMetadata::claim_check();
        let second = UpdateMetadata::claim_check();

        match previous {
            Some(value) => horon_agent_base::env::set_var("HORON_AGENT_HOME", value),
            None => horon_agent_base::env::remove_var("HORON_AGENT_HOME"),
        }
        assert!(first, "an empty metadata file must allow the first check");
        assert!(!second, "the first claim must block a second check in the same window");
    }
}
