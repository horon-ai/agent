use anyhow::Result;
use horon_agent::auth::AuthStatus;
use horon_agent::provider_catalog::{OPENAI_COMPAT_LOGIN_PROVIDER, login_providers, openai_compatible_profiles};
use std::collections::HashSet;
use std::sync::{Mutex, MutexGuard, OnceLock};

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn lock_env() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn tracked_env_vars() -> Vec<String> {
    let mut keys: HashSet<String> = [
        "HOME",
        "APPDATA",
        "XDG_CONFIG_HOME",
        "HORON_AGENT_HOME",
        "NO_PROXY",
        "no_proxy",
        "HORON_AGENT_OPENROUTER_API_BASE",
        "HORON_AGENT_OPENROUTER_API_KEY_NAME",
        "HORON_AGENT_OPENROUTER_ENV_FILE",
        "HORON_AGENT_OPENROUTER_CACHE_NAMESPACE",
        "HORON_AGENT_OPENROUTER_PROVIDER_FEATURES",
        "HORON_AGENT_OPENROUTER_ALLOW_NO_AUTH",
        "HORON_AGENT_OPENROUTER_MODEL_CATALOG",
        "HORON_AGENT_OPENROUTER_MODEL",
        "HORON_AGENT_OPENROUTER_STATIC_MODELS",
        "HORON_AGENT_OPENROUTER_AUTH_HEADER",
        "HORON_AGENT_OPENROUTER_AUTH_HEADER_NAME",
        "HORON_AGENT_OPENROUTER_DYNAMIC_BEARER_PROVIDER",
        "HORON_AGENT_OPENROUTER_PROVIDER",
        "HORON_AGENT_OPENROUTER_NO_FALLBACK",
        "OPENROUTER_API_KEY",
        "AUTH_FLOW_TEST_KEY",
    ]
    .into_iter()
    .map(ToString::to_string)
    .collect();

    for profile in openai_compatible_profiles() {
        keys.insert(profile.api_key_env.to_string());
    }

    let mut keys: Vec<_> = keys.into_iter().collect();
    keys.sort();
    keys
}

struct TestEnv {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(String, Option<String>)>,
    // Held so the temporary home directory outlives the test.
    _temp: tempfile::TempDir,
}

impl TestEnv {
    fn new() -> Result<Self> {
        let lock = lock_env();
        let temp = tempfile::Builder::new().prefix("horon-agent-auth-flow-").tempdir()?;
        let saved = tracked_env_vars()
            .into_iter()
            .map(|key| {
                let value = std::env::var(&key).ok();
                (key, value)
            })
            .collect::<Vec<_>>();

        for (key, _) in &saved {
            horon_agent::env::remove_var(key);
        }

        horon_agent::env::set_var("HOME", temp.path());
        horon_agent::env::set_var("XDG_CONFIG_HOME", temp.path().join("config"));
        horon_agent::env::set_var("APPDATA", temp.path().join("AppData").join("Roaming"));
        horon_agent::env::set_var("HORON_AGENT_HOME", temp.path().join("horon-agent-home"));
        horon_agent::env::set_var("NO_PROXY", "127.0.0.1,localhost");
        horon_agent::env::set_var("no_proxy", "127.0.0.1,localhost");
        AuthStatus::invalidate_cache();

        Ok(Self {
            _lock: lock,
            saved,
            _temp: temp,
        })
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        AuthStatus::invalidate_cache();
        for (key, value) in &self.saved {
            if let Some(value) = value {
                horon_agent::env::set_var(key, value);
            } else {
                horon_agent::env::remove_var(key);
            }
        }
        AuthStatus::invalidate_cache();
    }
}

#[test]
fn every_login_provider_has_auth_status_and_setup_copy_metadata() -> Result<()> {
    let _env = TestEnv::new()?;
    let status = AuthStatus::default();

    for provider in login_providers() {
        assert!(!provider.id.trim().is_empty(), "provider id must be set");
        assert!(
            !provider.display_name.trim().is_empty(),
            "{} display name must be set",
            provider.id
        );
        assert!(
            !provider.menu_detail.trim().is_empty(),
            "{} setup copy must be set",
            provider.id
        );
        assert!(
            !provider.auth_kind.label().trim().is_empty(),
            "{} auth kind label must be set",
            provider.id
        );
        assert!(
            !status.method_detail_for_provider(*provider).trim().is_empty(),
            "{} detected setup copy must be renderable",
            provider.id
        );
    }

    Ok(())
}

#[test]
fn openai_compatible_login_provider_exposes_setup_copy() -> Result<()> {
    let _env = TestEnv::new()?;
    let status = AuthStatus::default();

    let provider = login_providers()
        .iter()
        .find(|provider| provider.id == OPENAI_COMPAT_LOGIN_PROVIDER.id)
        .expect("openai-compatible login provider is registered");

    assert_eq!(provider.id, "openai-compatible");
    assert_eq!(
        provider.display_name, OPENAI_COMPAT_LOGIN_PROVIDER.display_name,
        "the registered entry keeps the catalog display name"
    );
    assert_eq!(
        provider.auth_kind.label(),
        "API key / CLI",
        "the openai-compatible auth kind keeps its login label"
    );
    assert_eq!(
        provider.menu_detail,
        "custom endpoint setup: base URL first, then API key"
    );
    assert!(
        !status.method_detail_for_provider(*provider).trim().is_empty(),
        "detected setup copy must be renderable"
    );

    Ok(())
}
