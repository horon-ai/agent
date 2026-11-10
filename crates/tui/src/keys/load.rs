//! Reading `$HORON_HOME/keybindings.toml` and watching it for edits.
//!
//! The file is optional and holds one array of key names per action:
//!
//! ```toml
//! "app.submit" = ["ctrl+j"]
//! "app.quit" = []
//! ```
//!
//! A missing file means the defaults, which is the state of every installation
//! that never opened it, so a missing file is not a warning. A file that cannot
//! be parsed keeps the map that was in force and contributes one warning, the
//! same contract the settings files have.
//!
//! [`Loader::tick`] re-reads the file when its size or modification time moved,
//! on the settings reloader's one-second cadence, so a remapped key works
//! without restarting the client.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use toml::Table;

use super::bindings::Bindings;

/// What a reload produced: the map to apply and the warnings to show.
#[derive(Debug, Clone)]
pub struct Reload {
    /// The key map in force after the reload.
    pub bindings: Bindings,
    /// One sentence per problem the file earned.
    pub warnings: Vec<String>,
}

/// Size and modification time of the bindings file.
///
/// A missing file has no stamp, so creating or deleting it changes the stamp
/// and triggers a reload. A filesystem that cannot report a modification time
/// reports the epoch; a rewrite that changes the length still reloads there,
/// and one that does not is picked up on the next length change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: SystemTime,
}

impl Stamp {
    fn of(path: &Path) -> Option<Self> {
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(_) => return None,
        };
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        Some(Self {
            len: metadata.len(),
            modified,
        })
    }
}

/// Owns the key map and tracks the file it came from.
pub struct Loader {
    path: PathBuf,
    bindings: Bindings,
    warnings: Vec<String>,
    stamp: Option<Stamp>,
    interval: Duration,
    last_check: Instant,
}

impl Loader {
    /// A loader that reads `$HORON_HOME/keybindings.toml`.
    pub fn new() -> Self {
        Self::at(horon_agent_sdk::user_horon_agent_home().join("keybindings.toml"))
    }

    /// A loader that reads an explicit file, for tests.
    pub fn at(path: PathBuf) -> Self {
        let mut loader = Self {
            path,
            bindings: Bindings::default(),
            warnings: Vec::new(),
            stamp: None,
            interval: Duration::from_secs(1),
            last_check: Instant::now(),
        };
        let reload = loader.reload();
        loader.bindings = reload.bindings;
        loader.warnings = reload.warnings;
        loader
    }

    /// The file this loader reads.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The key map in force.
    pub fn bindings(&self) -> &Bindings {
        &self.bindings
    }

    /// Warnings from the initial load.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Check the file once per interval; nothing comes back when the interval
    /// has not elapsed or the file did not change.
    pub fn tick(&mut self) -> Option<Reload> {
        if self.last_check.elapsed() < self.interval {
            return None;
        }
        self.check_now()
    }

    /// Check the file immediately, for tests and forced reloads.
    pub fn check_now(&mut self) -> Option<Reload> {
        self.last_check = Instant::now();
        if Stamp::of(&self.path) == self.stamp {
            return None;
        }
        Some(self.reload())
    }

    /// Read the file and build the map it describes.
    ///
    /// A file that cannot be read or parsed keeps the map in force; a missing
    /// file returns to the defaults, because deleting the file is how a user
    /// asks for the defaults back.
    fn reload(&mut self) -> Reload {
        self.stamp = Stamp::of(&self.path);
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let (bindings, warnings) = Bindings::from_table(&Table::new());
                self.bindings = bindings.clone();
                self.warnings = self.located(warnings);
                return Reload {
                    bindings,
                    warnings: self.warnings.clone(),
                };
            }
            Err(error) => {
                let warnings = self.located(vec![format!(
                    "cannot read the file: {error}; keeping the last key bindings"
                )]);
                self.warnings = warnings.clone();
                return Reload {
                    bindings: self.bindings.clone(),
                    warnings,
                };
            }
        };
        let (bindings, warnings) = match toml::from_str::<Table>(&text) {
            Ok(table) => Bindings::from_table(&table),
            Err(error) => {
                let warnings = self.located(vec![format!(
                    "cannot parse the file: {error}; keeping the last key bindings"
                )]);
                self.warnings = warnings.clone();
                return Reload {
                    bindings: self.bindings.clone(),
                    warnings,
                };
            }
        };
        let warnings = self.located(warnings);
        self.bindings = bindings.clone();
        self.warnings = warnings.clone();
        Reload { bindings, warnings }
    }

    /// Name the file in front of every warning it caused.
    fn located(&self, warnings: Vec<String>) -> Vec<String> {
        warnings
            .into_iter()
            .map(|warning| format!("{}: {warning}", self.path.display()))
            .collect()
    }
}

impl Default for Loader {
    fn default() -> Self {
        Self::new()
    }
}
