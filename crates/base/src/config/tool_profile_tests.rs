//! Tests for applying a tool profile to the loaded config.
//!
//! The headless default profile used to be applied by invalidating the config
//! cache, which made the next `config()` call parse `config.toml` a second
//! time. These tests pin the replacement: one load, the environment variable
//! still set, and the stored fingerprint synchronized with it.

use super::{CORE_TOOL_PROFILE, Config, config};
use std::ffi::OsString;

struct TestHome {
    _lock: std::sync::MutexGuard<'static, ()>,
    _dir: tempfile::TempDir,
    previous_home: Option<OsString>,
    previous_profile: Option<OsString>,
}

impl TestHome {
    fn new() -> Self {
        let lock = crate::storage::lock_test_env();
        let dir = tempfile::tempdir().expect("temp home");
        let previous_home = std::env::var_os("HORON_HOME");
        let previous_profile = std::env::var_os("HORON_AGENT_TOOL_PROFILE");
        crate::env::set_var("HORON_HOME", dir.path());
        crate::env::remove_var("HORON_AGENT_TOOL_PROFILE");
        Config::invalidate_cache();
        Self {
            _lock: lock,
            _dir: dir,
            previous_home,
            previous_profile,
        }
    }

    fn write_config(&self, body: &str) {
        let path = Config::path().expect("config path");
        std::fs::create_dir_all(path.parent().expect("config parent")).expect("create config parent");
        std::fs::write(path, body).expect("write config");
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        match self.previous_profile.take() {
            Some(value) => crate::env::set_var("HORON_AGENT_TOOL_PROFILE", value),
            None => crate::env::remove_var("HORON_AGENT_TOOL_PROFILE"),
        }
        match self.previous_home.take() {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
        Config::invalidate_cache();
    }
}

#[test]
fn tool_profile_override_does_not_reload_config() {
    let home = TestHome::new();
    home.write_config("[display]\nemoji = false\n");
    assert_eq!(config().tools.profile, "");

    // What the CLI does for a headless launch that selected no profile: set
    // the environment variable, then patch the already-loaded config.
    crate::env::set_var("HORON_AGENT_TOOL_PROFILE", "full");
    Config::set_loaded_tool_profile("full");

    {
        let cache = super::CONFIG_CACHE
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(
            cache.fingerprint,
            super::ConfigCacheFingerprint::current(),
            "the stored fingerprint must include the profile the caller just set"
        );
        assert!(!cache.force_reload, "patching must not force a reload");
    }

    // Observing the patched value must not parse the config again.
    let loads_before = Config::config_load_count();
    assert_eq!(config().tools.profile, "full");
    assert_eq!(
        Config::config_load_count(),
        loads_before,
        "reading the patched config must not reload it"
    );
}

#[test]
fn default_tool_profile_survives_config_reload() {
    let home = TestHome::new();
    home.write_config("[display]\nemoji = false\n");
    assert_eq!(config().tools.profile, "");

    crate::env::set_var("HORON_AGENT_TOOL_PROFILE", CORE_TOOL_PROFILE);
    Config::set_loaded_tool_profile(CORE_TOOL_PROFILE);
    assert_eq!(config().tools.profile, CORE_TOOL_PROFILE);

    // A manual edit changes the file fingerprint: the source of a real reload.
    // The environment override carries the default profile across it.
    home.write_config("[display]\nemoji = true\n# edited\n");
    let reloaded = config();
    assert!(reloaded.display.emoji, "the file edit must be picked up");
    assert_eq!(
        reloaded.tools.profile, CORE_TOOL_PROFILE,
        "the default tool profile must survive a config reload"
    );
}
