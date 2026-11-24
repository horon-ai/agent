#![cfg_attr(test, allow(clippy::items_after_test_module))]

pub use horon_agent_storage::*;

use anyhow::Result;
use serde::de::DeserializeOwned;
use std::path::Path;

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    horon_agent_storage::read_json_with_recovery_handler(path, |event| match event {
        horon_agent_storage::StorageRecoveryEvent::CorruptPrimary { path, error } => {
            crate::logging::warn(&format!("Corrupt JSON at {}, trying backup: {}", path.display(), error));
        }
        horon_agent_storage::StorageRecoveryEvent::RecoveredFromBackup { backup_path } => {
            crate::logging::info(&format!("Recovered from backup: {}", backup_path.display()));
        }
    })
}

#[cfg(any(test, feature = "test-support"))]
use std::sync::{Mutex, MutexGuard, OnceLock};

#[cfg(any(test, feature = "test-support"))]
pub fn test_env_lock() -> &'static Mutex<()> {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    ENV_LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(any(test, feature = "test-support"))]
pub fn lock_test_env() -> MutexGuard<'static, ()> {
    test_env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Creates a directory the way the durable store expects one: owner-only where
/// the platform carries POSIX modes, and a plain create elsewhere, because the
/// store's `0700` requirement itself is unix-only.
#[cfg(any(test, feature = "test-support"))]
pub fn create_store_dir(path: &Path, recursive: bool) -> std::io::Result<()> {
    horon_agent_platform::create_private_dir(path, recursive)
}

/// Moves the process cwd for one test and puts it back when dropped, panics
/// included. The cwd is process-global, so hold `lock_test_env` first and
/// declare this guard after any temporary directory it points into, so it
/// drops, and restores the cwd, before that directory is deleted.
#[cfg(test)]
pub(crate) struct CwdGuard {
    original: std::path::PathBuf,
}

#[cfg(test)]
impl CwdGuard {
    pub(crate) fn enter(dir: &Path) -> std::io::Result<Self> {
        let original = std::env::current_dir()?;
        std::env::set_current_dir(dir)?;
        Ok(Self { original })
    }

    pub(crate) fn change(&self, dir: &Path) -> std::io::Result<()> {
        std::env::set_current_dir(dir)
    }
}

#[cfg(test)]
impl Drop for CwdGuard {
    fn drop(&mut self) {
        if let Err(error) = std::env::set_current_dir(&self.original) {
            eprintln!("could not restore cwd {}: {error}", self.original.display());
        }
    }
}

/// Points `HORON_HOME` at a directory for one test and restores the previous
/// value when dropped, panics included. Hold `lock_test_env` first.
#[cfg(test)]
pub(crate) struct HomeEnvGuard {
    previous: Option<std::ffi::OsString>,
}

#[cfg(test)]
impl HomeEnvGuard {
    pub(crate) fn set(dir: &Path) -> Self {
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", dir);
        Self { previous }
    }
}

#[cfg(test)]
impl Drop for HomeEnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
    }
}

#[cfg(test)]
mod tests;
