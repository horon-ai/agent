//! `[storage]` section of the config: how firmly original session data is
//! confirmed on disk.

use serde::{Deserialize, Serialize};

/// The durability level original writes are confirmed with.
///
/// The value names a guarantee, not a syscall: the platform adapter picks the
/// cheapest call that provides it on the current operating system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StorageSyncMode {
    /// The strongest guarantee that a write survives a power loss (default).
    ///
    /// macOS also flushes the drive's write cache (`F_FULLFSYNC`), which costs
    /// several milliseconds per write.
    #[default]
    Full,
    /// The cheapest flush that still survives an operating-system crash.
    ///
    /// macOS uses a plain `fsync(2)`. Linux and Windows have no cheaper call
    /// than `Full`, so the value does not change their behaviour.
    Fast,
}

impl StorageSyncMode {
    /// Parse a mode name, returning `None` for unknown values.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "full" => Some(Self::Full),
            "fast" => Some(Self::Fast),
            _ => None,
        }
    }

    /// The value written back to `config.toml`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Fast => "fast",
        }
    }
}

impl<'de> Deserialize<'de> for StorageSyncMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match Self::parse(&value) {
            Some(mode) => mode,
            None => Self::Full,
        })
    }
}

/// Storage durability configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StorageConfig {
    /// Flush level for writes that have to survive a crash. Default: `full`.
    pub sync: StorageSyncMode,

    /// The value read from the file when it was not recognized.
    ///
    /// Kept so the loader can name the offending text in its one warning. It is
    /// never written back: the next save stores the resolved value.
    #[serde(skip)]
    pub unrecognized_sync: Option<String>,
}

impl StorageConfig {
    /// Resolve the durability level from the file and an optional env override.
    ///
    /// The environment wins. An unrecognized value in either place falls back
    /// to `Full` and returns the single warning line that names it, so a typo
    /// costs a warning instead of failing the whole config parse.
    pub fn resolve(&self, env_override: Option<&str>) -> (StorageSyncMode, Option<String>) {
        if let Some(value) = env_override {
            let value = value.trim();
            if !value.is_empty() {
                return match StorageSyncMode::parse(value) {
                    Some(mode) => (mode, None),
                    None => (
                        StorageSyncMode::Full,
                        Some(format!(
                            "[storage] sync: unrecognized HORON_AGENT_STORAGE_SYNC value \"{value}\"; using \"full\""
                        )),
                    ),
                };
            }
        }
        match &self.unrecognized_sync {
            Some(value) => (
                StorageSyncMode::Full,
                Some(format!(
                    "[storage] sync: unrecognized value \"{value}\" in config.toml; using \"full\""
                )),
            ),
            None => (self.sync, None),
        }
    }
}

impl<'de> Deserialize<'de> for StorageConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        /// The section as written, before the value is interpreted.
        #[derive(Default, Deserialize)]
        #[serde(default)]
        struct Raw {
            sync: Option<String>,
        }

        let raw = Raw::deserialize(deserializer)?;
        let mut config = Self::default();
        if let Some(value) = raw.sync {
            match StorageSyncMode::parse(&value) {
                Some(mode) => config.sync = mode,
                None => config.unrecognized_sync = Some(value),
            }
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values_parse_and_unknown_values_are_reported() {
        assert_eq!(StorageSyncMode::parse("full"), Some(StorageSyncMode::Full));
        assert_eq!(StorageSyncMode::parse(" FAST "), Some(StorageSyncMode::Fast));
        assert_eq!(StorageSyncMode::parse("bogus"), None);

        let config: StorageConfig = toml_like(r#"{"sync": "bogus"}"#);
        assert_eq!(config.sync, StorageSyncMode::Full);
        assert_eq!(config.unrecognized_sync.as_deref(), Some("bogus"));
    }

    #[test]
    fn recognized_value_leaves_no_warning_behind() {
        let config: StorageConfig = toml_like(r#"{"sync": "fast"}"#);
        assert_eq!(config.sync, StorageSyncMode::Fast);
        assert!(config.unrecognized_sync.is_none());
        assert_eq!(config.sync.as_str(), "fast");
    }

    fn toml_like(json: &str) -> StorageConfig {
        serde_json::from_str(json).expect("storage config parses")
    }
}
