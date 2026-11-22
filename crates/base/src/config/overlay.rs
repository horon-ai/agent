//! Explicit per-run TOML overlays for the configuration file.
//!
//! `horon run --config <file>` layers each file, in the order given, over
//! `config.toml`: tables merge key by key, and any other value in a later file
//! replaces the earlier one. Keys a file omits are inherited. The list lives in
//! this process only, so no other session or process sees it, and every file is
//! read and parsed before the run starts so a bad one names itself.

use super::Config;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};

static OVERLAYS: LazyLock<Mutex<Vec<PathBuf>>> = LazyLock::new(Default::default);

impl Config {
    /// Record the overlays for this process after checking that each one parses.
    pub fn set_run_overlays(paths: &[PathBuf]) -> anyhow::Result<()> {
        for path in paths {
            read_table(path)?;
        }
        *OVERLAYS.lock().unwrap_or_else(PoisonError::into_inner) = paths.to_vec();
        Self::invalidate_cache();
        // The ordinary loader falls back to defaults on a bad file. A run that
        // asked for overlays must fail before provider use instead.
        if let Err(error) = Self::load_file_layer() {
            OVERLAYS.lock().unwrap_or_else(PoisonError::into_inner).clear();
            Self::invalidate_cache();
            return Err(error);
        }
        Ok(())
    }
}

fn overlays() -> Vec<PathBuf> {
    OVERLAYS.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

pub(super) fn active() -> bool {
    !overlays().is_empty()
}

fn read_table(path: &Path) -> anyhow::Result<toml::Table> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| anyhow::anyhow!("Failed to read config overlay {}: {error}", path.display()))?;
    text.parse::<toml::Table>()
        .map_err(|error| anyhow::anyhow!("Failed to parse config overlay {}: {error}", path.display()))
}

/// Merge `top` into `base`: tables merge key by key, any other value replaces.
fn merge(base: &mut toml::Table, top: toml::Table) {
    for (key, value) in top {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(base)), toml::Value::Table(top)) => merge(base, top),
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

/// The config file (when it exists) with every overlay applied, parsed as a
/// whole, with the document that produced it.
pub(super) fn load(path: &Path) -> anyhow::Result<(Config, toml::Value)> {
    let mut table = if path.exists() {
        let text = std::fs::read_to_string(path)
            .map_err(|error| anyhow::anyhow!("Failed to read config file {}: {error}", path.display()))?;
        text.parse::<toml::Table>()
            .map_err(|error| anyhow::anyhow!("Failed to parse config file {}: {error}", path.display()))?
    } else {
        toml::Table::new()
    };
    for overlay in overlays() {
        merge(&mut table, read_table(&overlay)?);
    }
    let document = toml::Value::Table(table);
    let config: Config = document
        .clone()
        .try_into()
        .map_err(|error| anyhow::anyhow!("Invalid configuration after applying the run overlays: {error}"))?;
    config
        .prewalk
        .validate()
        .map_err(|error| anyhow::anyhow!("Invalid [prewalk] section after applying the run overlays: {error}"))?;
    Ok((config, document))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> toml::Table {
        text.parse().unwrap()
    }

    #[test]
    fn a_later_overlay_wins_per_leaf_and_omitted_keys_inherit() {
        let mut base = table("[tools]\nplan_mode = true\nirc = false\n\n[provider]\ndefault_model = \"a\"\n");
        merge(&mut base, table("[tools]\nirc = true\n"));
        merge(&mut base, table("[provider]\ndefault_model = \"b\"\n"));
        assert_eq!(
            base,
            table("[tools]\nplan_mode = true\nirc = true\n\n[provider]\ndefault_model = \"b\"\n")
        );
    }

    #[test]
    fn an_invalid_overlay_names_its_file_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad-overlay.toml");
        std::fs::write(&bad, "[tools\nirc = true\n").unwrap();
        let error = Config::set_run_overlays(std::slice::from_ref(&bad))
            .unwrap_err()
            .to_string();
        assert!(error.contains("bad-overlay.toml"), "{error}");
        assert!(!active());
    }
}
