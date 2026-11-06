//! Restore channel links and metadata together when any activation step fails.
use super::digest::file_sha256;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

enum Previous {
    Missing,
    Link(PathBuf),
    File {
        backup: PathBuf,
        permissions: std::fs::Permissions,
        digest: String,
    },
}

pub(super) fn with_rollback<T>(paths: &[PathBuf], activate: impl FnOnce() -> Result<T>) -> Result<T> {
    let backups = tempfile::tempdir()?;
    let mut previous = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        let state = match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Previous::Missing,
            Err(error) => return Err(error.into()),
            Ok(metadata) if metadata.file_type().is_symlink() => Previous::Link(std::fs::read_link(path)?),
            Ok(metadata) if metadata.is_file() => {
                let backup = backups.path().join(index.to_string());
                std::fs::copy(path, &backup)?;
                let digest = file_sha256(&backup)?;
                Previous::File {
                    backup,
                    permissions: metadata.permissions(),
                    digest,
                }
            }
            Ok(_) => bail!("activation target {} is not a file or link", path.display()),
        };
        previous.push((path, state));
    }
    match activate() {
        Ok(value) => Ok(value),
        Err(error) => {
            let mut failures = Vec::new();
            for (path, state) in previous.iter().rev() {
                if let Err(restore) = restore(path, state) {
                    failures.push(format!("{}: {restore:#}", path.display()));
                }
            }
            if !failures.is_empty() {
                let preserved = backups.keep();
                bail!(
                    "activation failed: {error:#}; rollback errors: {}; backups preserved at {}",
                    failures.join("; "),
                    preserved.display()
                );
            }
            Err(error)
        }
    }
}

fn restore(path: &Path, previous: &Previous) -> Result<()> {
    let current = match std::fs::symlink_metadata(path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if current
        .as_ref()
        .is_some_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
    {
        bail!("refusing to replace a directory during rollback");
    }
    match previous {
        Previous::Missing => {
            if current.is_some() {
                std::fs::remove_file(path)?;
            }
        }
        Previous::Link(target) => {
            if current
                .as_ref()
                .is_some_and(|metadata| metadata.file_type().is_symlink())
                && std::fs::read_link(path)? == *target
            {
                return Ok(());
            }
            let parent = path.parent().context("activation target has no parent")?;
            let temporary = tempfile::NamedTempFile::new_in(parent)?.into_temp_path();
            horon_agent_platform::atomic_symlink_swap(target, path, &temporary)?;
        }
        Previous::File {
            backup,
            permissions,
            digest,
        } => {
            if current
                .as_ref()
                .is_some_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
                && file_sha256(path)? == *digest
            {
                return Ok(());
            }
            let parent = path.parent().context("activation target has no parent")?;
            let temporary = tempfile::NamedTempFile::new_in(parent)?;
            std::fs::copy(backup, temporary.path())?;
            temporary.as_file().set_permissions(permissions.clone())?;
            temporary.as_file().sync_all()?;
            temporary.persist(path).map_err(|error| error.error)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_late_metadata_failure_restores_prior_bytes_and_absent_paths() {
        let home = tempfile::tempdir().unwrap();
        let marker = home.path().join("current-version");
        let metadata = home.path().join("update_metadata.json");
        let fresh = home.path().join("new-marker");
        std::fs::write(&marker, b"old").unwrap();
        std::fs::write(&metadata, b"original-metadata").unwrap();
        let result: Result<()> = with_rollback(&[marker.clone(), metadata.clone(), fresh.clone()], || {
            std::fs::write(&marker, b"new")?;
            std::fs::write(&metadata, b"partial")?;
            std::fs::write(&fresh, b"new")?;
            bail!("injected last-step failure");
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(marker).unwrap(), b"old");
        assert_eq!(std::fs::read(metadata).unwrap(), b"original-metadata");
        assert!(!fresh.exists());
    }

    #[test]
    fn rollback_preserves_relative_symlink_target_without_touching_its_binary() {
        let home = tempfile::tempdir().unwrap();
        let old = home.path().join("old");
        let new = home.path().join("new");
        let link = home.path().join("current");
        std::fs::write(&old, b"old-binary").unwrap();
        std::fs::write(&new, b"new-binary").unwrap();
        if horon_agent_platform::fs::symlink_file(std::path::Path::new("old"), &link).is_err() {
            // This platform cannot create a relative symlink to test with.
            return;
        }
        let result: Result<()> = with_rollback(std::slice::from_ref(&link), || {
            std::fs::remove_file(&link)?;
            std::os::unix::fs::symlink("new", &link)?;
            bail!("later switch failed");
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("old"));
        assert_eq!(std::fs::read(&link).unwrap(), b"old-binary");
        assert_eq!(std::fs::read(new).unwrap(), b"new-binary");
    }
}
