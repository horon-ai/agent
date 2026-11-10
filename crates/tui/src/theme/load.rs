//! Resolving the active theme and keeping it current.
//!
//! Lookup order is the built-in names (`dark`, `light`) and then
//! `$HORON_HOME/themes/<name>.json`; a user file that reuses a built-in name is
//! ignored, because a shipped theme a user file could shadow would make the
//! same setting render differently on two machines. A theme that fails to load
//! keeps the previous one and yields a warning for the transcript, so a typo in
//! a theme file never leaves the interface unstyled.
//!
//! [`ThemeLoader::tick`] re-checks the selected user file on the settings
//! reloader's cadence: an edit to the file, or a change of the `theme` or
//! `symbols` setting, re-resolves the active theme.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::settings::{Provenance, Settings};

use super::schema::{self, Theme};
use super::symbols::{SymbolPreset, Symbols};

/// What the caller last asked for, so a file change can re-apply it and an
/// unchanged request does not reload anything.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Request {
    name: String,
    preset: Option<SymbolPreset>,
}

/// Size and modification time of a theme file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl Stamp {
    fn of(path: &Path) -> Option<Self> {
        let Ok(metadata) = std::fs::metadata(path) else {
            return None;
        };
        let modified = match metadata.modified() {
            Ok(modified) => Some(modified),
            Err(_) => None,
        };
        Some(Self {
            len: metadata.len(),
            modified,
        })
    }
}

/// Owns the active theme and tracks the file it came from.
pub struct ThemeLoader {
    themes_dir: Option<PathBuf>,
    request: Option<Request>,
    active: Theme,
    symbols: Symbols,
    selected: Option<PathBuf>,
    stamp: Option<Stamp>,
    interval: Duration,
    last_check: Instant,
}

impl ThemeLoader {
    /// A loader that reads `$HORON_HOME/themes`.
    pub fn new() -> Self {
        Self::in_dir(horon_agent_sdk::user_horon_agent_home().join("themes"))
    }

    /// A loader that reads an explicit themes directory, for tests.
    pub fn in_dir(themes_dir: PathBuf) -> Self {
        Self {
            themes_dir: Some(themes_dir),
            request: None,
            active: Theme::default(),
            symbols: Symbols::default(),
            selected: None,
            stamp: None,
            interval: Duration::from_secs(1),
            last_check: Instant::now(),
        }
    }

    /// The theme in force.
    pub fn theme(&self) -> &Theme {
        &self.active
    }

    /// The glyphs in force.
    pub fn symbols(&self) -> &Symbols {
        &self.symbols
    }

    /// The name of the theme in force.
    pub fn name(&self) -> &str {
        &self.active.name
    }

    /// The user file the active theme was read from, if any.
    pub fn source_path(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    /// Apply the `theme` and `symbols` settings, returning any warnings.
    ///
    /// A `symbols` value only overrides the theme's preset when a layer other
    /// than the defaults set it: the registry default must not silently beat a
    /// theme that names its own preset.
    pub fn apply(&mut self, settings: &Settings) -> Vec<String> {
        let name = match settings.text("theme") {
            Some(name) => name.to_string(),
            None => self.active.name.clone(),
        };
        let preset = match settings.provenance("symbols") {
            Some(Provenance::Default) | None => None,
            Some(_) => settings.text("symbols").and_then(SymbolPreset::parse),
        };
        self.apply_choice(&name, preset)
    }

    /// Apply an explicit choice, returning any warnings.
    ///
    /// Repeating the request that is already in force does nothing, which is
    /// what keeps an unrelated settings reload from re-reading the theme file.
    pub fn apply_choice(&mut self, name: &str, preset: Option<SymbolPreset>) -> Vec<String> {
        let request = Request {
            name: name.to_string(),
            preset,
        };
        if self.request.as_ref() == Some(&request) {
            return Vec::new();
        }
        self.resolve(request)
    }

    /// Check the selected file once per interval.
    pub fn tick(&mut self) -> Vec<String> {
        if self.last_check.elapsed() < self.interval {
            return Vec::new();
        }
        self.check_now()
    }

    /// Check the selected file immediately, for tests and forced reloads.
    pub fn check_now(&mut self) -> Vec<String> {
        self.last_check = Instant::now();
        let Some(path) = self.selected.clone() else {
            return Vec::new();
        };
        if Stamp::of(&path) == self.stamp {
            return Vec::new();
        }
        let Some(request) = self.request.clone() else {
            return Vec::new();
        };
        self.resolve(request)
    }

    /// Load the requested theme, keeping the previous one on any failure.
    fn resolve(&mut self, request: Request) -> Vec<String> {
        let mut warnings = Vec::new();
        let file = self
            .themes_dir
            .as_ref()
            .map(|dir| dir.join(format!("{}.json", request.name)));
        let mut selected = None;
        let mut stamp = None;

        let loaded = match schema::builtin_source(&request.name) {
            Some(text) => {
                if let Some(path) = file.as_ref().filter(|path| path.exists()) {
                    warnings.push(format!(
                        "user theme {} is ignored: a built-in theme has that name",
                        path.display()
                    ));
                }
                schema::parse(text, &format!("built-in theme `{}`", request.name))
            }
            None => match file {
                None => Err("no themes directory; built-in themes are dark and light".to_string()),
                Some(path) => {
                    selected = Some(path.clone());
                    stamp = Stamp::of(&path);
                    match std::fs::read_to_string(&path) {
                        Ok(text) => schema::parse(&text, &path.display().to_string()),
                        Err(error) => Err(format!("{}: {error}", path.display())),
                    }
                }
            },
        };

        // Record the request even when it failed, so the next tick retries only
        // after the file or the setting actually changes instead of warning on
        // every settings reload.
        self.request = Some(request.clone());
        self.selected = selected;
        self.stamp = stamp;

        match loaded {
            Ok(theme) => {
                let preset = request.preset.or(theme.symbols).unwrap_or(SymbolPreset::Unicode);
                self.symbols = preset.symbols();
                self.active = theme;
            }
            Err(error) => warnings.push(format!(
                "theme `{}` not applied: {error}; keeping `{}`",
                request.name, self.active.name
            )),
        }
        warnings
    }
}

impl Default for ThemeLoader {
    fn default() -> Self {
        Self::new()
    }
}
