//! Test support shared by the memory test modules.
//!
//! `TempHome` points `HORON_HOME` at a private directory for the length of one
//! closure and restores every process-wide setting it touched afterwards, so a
//! test never reads or writes the real `~/.horon` and never leaks an environment
//! change into the next test.

use std::path::{Path, PathBuf};
use std::sync::MutexGuard;

/// A private agent home for one test.
pub(crate) struct TempHome {
    root: PathBuf,
    _lock: MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
}

impl TempHome {
    /// Run `body` with `HORON_HOME` pointed at a fresh private directory.
    pub(crate) fn with<T>(body: impl FnOnce(&TempHome) -> T) -> T {
        let home = Self::enter();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&home)));
        drop(home);
        match result {
            Ok(value) => value,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    /// Take the environment lock and point `HORON_HOME` at a new directory.
    pub(crate) fn enter() -> Self {
        let lock = crate::storage::lock_test_env();
        let root = std::env::temp_dir().join(format!(
            "horon-memory-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        if let Err(error) = std::fs::create_dir_all(&root) {
            panic!("cannot create the private home {}: {error}", root.display());
        }
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", &root);
        crate::config::invalidate_config_cache();
        Self {
            root,
            _lock: lock,
            previous,
        }
    }

    /// The private home.
    pub(crate) fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::invalidate_config_cache();
        // The store keeps open handles and cached graphs keyed by path; a home
        // recreated at the same path must not answer from the deleted one.
        crate::memory::backend::forget_all();
        if let Err(error) = std::fs::remove_dir_all(&self.root) {
            crate::logging::debug(&format!(
                "cannot remove the private home {}: {error}",
                self.root.display()
            ));
        }
    }
}
