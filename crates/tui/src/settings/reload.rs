//! Live reload of the settings files.
//!
//! The event loop calls [`Reloader::tick`] each pass. The tick is throttled to
//! one check per interval (one second by default): the check compares each
//! file's size and modification time, and only a change re-reads and re-resolves
//! the layers. A broken file keeps its last parsed values and contributes the
//! warning the caller shows in the transcript.

use std::time::{Duration, Instant};

use super::layers::{FileStamp, LayerSet, SettingValue, Settings, Sources, save_global};

/// What changed after a reload.
#[derive(Debug, Clone)]
pub struct Reload {
    /// The settings in force after the reload.
    pub settings: Settings,
    /// Warnings from this reload, for the transcript.
    pub warnings: Vec<String>,
}

/// Owns the layer cache and the settings in force.
pub struct Reloader {
    sources: Sources,
    layers: LayerSet,
    settings: Settings,
    stamps: Vec<Option<FileStamp>>,
    warnings: Vec<String>,
    interval: Duration,
    last_check: Instant,
}

impl Reloader {
    /// Load every layer and start watching with a one-second interval.
    pub fn new(sources: Sources) -> Self {
        Self::with_interval(sources, Duration::from_secs(1))
    }

    /// Load every layer and start watching with an explicit interval.
    pub fn with_interval(sources: Sources, interval: Duration) -> Self {
        let mut layers = LayerSet::new(&sources);
        let mut warnings = Vec::new();
        layers.refresh(&mut warnings);
        let (settings, resolve_warnings) = layers.resolve(resolve_process_env);
        warnings.extend(resolve_warnings);
        let stamps = layers.stamps();
        Self {
            sources,
            layers,
            settings,
            stamps,
            warnings,
            interval,
            last_check: Instant::now(),
        }
    }

    /// The files this reloader reads.
    pub fn sources(&self) -> &Sources {
        &self.sources
    }

    /// The settings in force now.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Warnings from the initial load.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Check once per interval; returns nothing when the interval has not
    /// elapsed or when no file changed.
    pub fn tick(&mut self) -> Option<Reload> {
        if self.last_check.elapsed() < self.interval {
            return None;
        }
        self.check_now()
    }

    /// Check immediately, ignoring the interval.
    ///
    /// This is the entry point for tests and for the `:reload` style command a
    /// later piece of work adds, so a test never sleeps.
    pub fn check_now(&mut self) -> Option<Reload> {
        self.last_check = Instant::now();
        let stamps = self.layers.stamps();
        if stamps == self.stamps {
            return None;
        }
        self.stamps = stamps;
        Some(self.reload())
    }

    /// Change one key in the global file and reload immediately.
    ///
    /// Returns the warnings of the reload that followed the write. The stamps
    /// move with it, so the next `tick` does not treat the file this save just
    /// wrote as an outside change and repeat the same warnings.
    pub fn save_global(&mut self, id: &str, value: &SettingValue) -> Result<Vec<String>, String> {
        save_global(&self.sources, id, value)?;
        let reload = self.reload();
        self.stamps = self.layers.stamps();
        Ok(reload.warnings)
    }

    /// Re-read every layer and resolve.
    fn reload(&mut self) -> Reload {
        let mut warnings = Vec::new();
        self.layers.refresh(&mut warnings);
        let (settings, resolve_warnings) = self.layers.resolve(resolve_process_env);
        warnings.extend(resolve_warnings);
        self.settings = settings.clone();
        self.warnings = warnings.clone();
        Reload { settings, warnings }
    }
}

/// Read an environment variable, treating a non-Unicode value as absent.
fn resolve_process_env(name: &str) -> Option<String> {
    let mut value = None;
    if let Ok(raw) = std::env::var(name) {
        value = Some(raw);
    }
    value
}
