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
        if paths.is_empty() {
            let mut guard = OVERLAYS.lock().unwrap_or_else(PoisonError::into_inner);
            if guard.is_empty() {
                return Ok(());
            }
            guard.clear();
            drop(guard);
            Self::invalidate_cache();
            return Ok(());
        }
        let mut tables = Vec::with_capacity(paths.len());
        for path in paths {
            match read_table(path) {
                Ok(table) => tables.push(table),
                Err(error) => {
                    clear_overlays_if_set();
                    return Err(error);
                }
            }
        }
        let validated = match Self::path() {
            Some(path) => match merge_and_parse(&path, tables) {
                Ok(config) => Some(config),
                Err(error) => {
                    clear_overlays_if_set();
                    return Err(error);
                }
            },
            None => None,
        };
        *OVERLAYS.lock().unwrap_or_else(PoisonError::into_inner) = paths.to_vec();
        if let Some(mut config) = validated {
            config.apply_env_overrides();
            publish_loaded_config(config);
        } else {
            Self::invalidate_cache();
        }
        Ok(())
    }
}

fn clear_overlays_if_set() {
    let mut guard = OVERLAYS.lock().unwrap_or_else(PoisonError::into_inner);
    if !guard.is_empty() {
        guard.clear();
        drop(guard);
        Config::invalidate_cache();
    }
}

fn publish_loaded_config(config: Config) {
    let published = super::super::leak_config(config);
    let mut cache = super::super::CONFIG_CACHE
        .write()
        .unwrap_or_else(PoisonError::into_inner);
    cache.config = published;
    cache.fingerprint = super::super::ConfigCacheFingerprint::current();
    cache.last_checked = std::time::Instant::now();
    cache.force_reload = false;
    drop(cache);
    super::super::notify_config_reloaded();
    super::super::populate_context_limits_from_config_ref(published);
}

pub(super) fn active() -> bool {
    !OVERLAYS.lock().unwrap_or_else(PoisonError::into_inner).is_empty()
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

fn read_base_table(path: &Path) -> anyhow::Result<toml::Table> {
    if path.exists() {
        let text = std::fs::read_to_string(path)
            .map_err(|error| anyhow::anyhow!("Failed to read config file {}: {error}", path.display()))?;
        text.parse::<toml::Table>()
            .map_err(|error| anyhow::anyhow!("Failed to parse config file {}: {error}", path.display()))
    } else {
        Ok(toml::Table::new())
    }
}

fn validate_merged_config(config: Config) -> anyhow::Result<Config> {
    config
        .prewalk
        .validate()
        .map_err(|error| anyhow::anyhow!("Invalid [prewalk] section after applying the run overlays: {error}"))?;
    Ok(config)
}

fn merge_and_parse(path: &Path, tables: impl IntoIterator<Item = toml::Table>) -> anyhow::Result<Config> {
    let mut table = read_base_table(path)?;
    for overlay_table in tables {
        merge(&mut table, overlay_table);
    }
    let config: Config = toml::Value::Table(table)
        .try_into()
        .map_err(|error| anyhow::anyhow!("Invalid configuration after applying the run overlays: {error}"))?;
    validate_merged_config(config)
}

fn read_merged_table_from_active_overlays(path: &Path) -> anyhow::Result<toml::Table> {
    let mut table = read_base_table(path)?;
    let guard = OVERLAYS.lock().unwrap_or_else(PoisonError::into_inner);
    for overlay in guard.iter() {
        merge(&mut table, read_table(overlay)?);
    }
    Ok(table)
}

/// The config file (when it exists) with every active overlay applied, without
/// retaining or cloning the intermediate TOML document.
pub(super) fn load_config(path: &Path) -> anyhow::Result<Config> {
    let table = read_merged_table_from_active_overlays(path)?;
    let config: Config = toml::Value::Table(table)
        .try_into()
        .map_err(|error| anyhow::anyhow!("Invalid configuration after applying the run overlays: {error}"))?;
    validate_merged_config(config)
}

/// The config file (when it exists) with every overlay applied, parsed as a
/// whole, with the document that produced it.
pub(super) fn load(path: &Path) -> anyhow::Result<(Config, toml::Value)> {
    let document = toml::Value::Table(read_merged_table_from_active_overlays(path)?);
    let config: Config = document
        .clone()
        .try_into()
        .map_err(|error| anyhow::anyhow!("Invalid configuration after applying the run overlays: {error}"))?;
    Ok((validate_merged_config(config)?, document))
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
