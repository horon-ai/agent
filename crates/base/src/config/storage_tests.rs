//! Tests for `[storage]` durability settings and their env override.

use super::{Config, StorageSyncMode};
use horon_agent_platform::SyncMode;
use std::ffi::OsString;

fn restore_env_var(key: &str, previous: Option<OsString>) {
    if let Some(previous) = previous {
        crate::env::set_var(key, previous);
    } else {
        crate::env::remove_var(key);
    }
}

fn parse(text: &str) -> Config {
    toml::from_str(text).expect("an unrecognized storage mode must not fail the whole config")
}

#[test]
fn storage_sync_defaults_to_full() {
    let _guard = crate::storage::lock_test_env();
    let previous = std::env::var_os("HORON_AGENT_STORAGE_SYNC");
    crate::env::remove_var("HORON_AGENT_STORAGE_SYNC");

    let config = Config::default();
    assert_eq!(config.storage.sync, StorageSyncMode::Full);
    assert!(config.storage.unrecognized_sync.is_none());
    assert_eq!(config.storage_sync_mode(), SyncMode::Full);
    let (mode, warning) = config.storage_sync_resolution();
    assert_eq!(mode, SyncMode::Full);
    assert!(warning.is_none(), "the default is not a warning: {warning:?}");

    restore_env_var("HORON_AGENT_STORAGE_SYNC", previous);
}

#[test]
fn storage_sync_env_overrides_config() {
    let _guard = crate::storage::lock_test_env();
    let previous = std::env::var_os("HORON_AGENT_STORAGE_SYNC");

    let config = parse("[storage]\nsync = \"fast\"\n");
    assert_eq!(config.storage.sync, StorageSyncMode::Fast);
    assert_eq!(config.storage_sync_mode(), SyncMode::Fast);

    crate::env::set_var("HORON_AGENT_STORAGE_SYNC", "full");
    assert_eq!(config.storage_sync_mode(), SyncMode::Full);

    crate::env::set_var("HORON_AGENT_STORAGE_SYNC", "fast");
    assert_eq!(config.storage_sync_mode(), SyncMode::Fast);

    // An empty value means "not set", not "unrecognized".
    crate::env::set_var("HORON_AGENT_STORAGE_SYNC", "  ");
    assert_eq!(config.storage_sync_mode(), SyncMode::Fast);

    restore_env_var("HORON_AGENT_STORAGE_SYNC", previous);
}

#[test]
fn storage_sync_unknown_value_falls_back_to_full_with_warning() {
    let _guard = crate::storage::lock_test_env();
    let previous = std::env::var_os("HORON_AGENT_STORAGE_SYNC");
    crate::env::remove_var("HORON_AGENT_STORAGE_SYNC");

    let config = parse("[storage]\nsync = \"bogus\"\n");
    assert_eq!(config.storage.sync, StorageSyncMode::Full);
    assert_eq!(config.storage.unrecognized_sync.as_deref(), Some("bogus"));
    assert_eq!(config.storage_sync_mode(), SyncMode::Full);

    let (mode, warning) = config.storage_sync_resolution();
    assert_eq!(mode, SyncMode::Full);
    let warning = warning.expect("an unrecognized file value is reported");
    assert!(warning.contains("\"bogus\""), "{warning}");
    assert!(warning.contains("config.toml"), "{warning}");

    // The environment is reported by name, because the file may be fine.
    crate::env::set_var("HORON_AGENT_STORAGE_SYNC", "bogus");
    let (mode, warning) = config.storage_sync_resolution();
    assert_eq!(mode, SyncMode::Full);
    let warning = warning.expect("an unrecognized env value is reported");
    assert!(warning.contains("HORON_AGENT_STORAGE_SYNC"), "{warning}");

    restore_env_var("HORON_AGENT_STORAGE_SYNC", previous);
}
