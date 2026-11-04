use anyhow::Result;
use horon_agent::auth::AuthStatus;
use horon_agent::cli::provider_init::{ProviderChoice, apply_login_provider_profile_env, init_provider_for_validation};
use horon_agent::provider_catalog::{
    LoginProviderDescriptor, LoginProviderTarget, OpenAiCompatibleProfile, apply_openai_compatible_profile_env,
    openai_compatible_profiles, resolve_openai_compatible_profile, server_bootstrap_login_providers,
};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn lock_env() -> MutexGuard<'static, ()> {
    let mutex = ENV_LOCK.get_or_init(|| Mutex::new(()));
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn tracked_env_vars() -> Vec<String> {
    let mut keys: HashSet<String> = [
        "HORON_AGENT_HOME",
        "XDG_CONFIG_HOME",
        "HORON_AGENT_OPENROUTER_API_BASE",
        "HORON_AGENT_OPENROUTER_API_KEY_NAME",
        "HORON_AGENT_OPENROUTER_ENV_FILE",
        "HORON_AGENT_OPENROUTER_CACHE_NAMESPACE",
        "HORON_AGENT_OPENROUTER_PROVIDER_FEATURES",
        "HORON_AGENT_OPENROUTER_ALLOW_NO_AUTH",
        "HORON_AGENT_OPENROUTER_PROVIDER",
        "HORON_AGENT_OPENROUTER_NO_FALLBACK",
        "HORON_AGENT_OPENROUTER_MODEL",
        "HORON_AGENT_OPENROUTER_MODEL_CATALOG",
        "HORON_AGENT_OPENROUTER_STATIC_MODELS",
        "HORON_AGENT_OPENROUTER_AUTH_HEADER",
        "HORON_AGENT_OPENROUTER_AUTH_HEADER_NAME",
        "HORON_AGENT_OPENROUTER_DYNAMIC_BEARER_PROVIDER",
        "HORON_AGENT_OPENROUTER_THINKING",
        "HORON_AGENT_OPENAI_COMPAT_API_BASE",
        "HORON_AGENT_OPENAI_COMPAT_API_KEY_NAME",
        "HORON_AGENT_OPENAI_COMPAT_ENV_FILE",
        "HORON_AGENT_OPENAI_COMPAT_SETUP_URL",
        "HORON_AGENT_OPENAI_COMPAT_DEFAULT_MODEL",
        "HORON_AGENT_OPENAI_COMPAT_LOCAL_ENABLED",
        "HORON_AGENT_NAMED_PROVIDER_PROFILE",
        "HORON_AGENT_PROVIDER_PROFILE_ACTIVE",
        "HORON_AGENT_PROVIDER_PROFILE_NAME",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "COPILOT_GITHUB_TOKEN",
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "CURSOR_API_KEY",
        "GEMINI_API_KEY",
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
    temp: tempfile::TempDir,
}

impl TestEnv {
    fn new() -> Result<Self> {
        let lock = lock_env();
        let temp = tempfile::Builder::new()
            .prefix("horon-agent-provider-matrix-")
            .tempdir()?;
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

        let config_root = temp.path().join("config").join("horon-agent");
        std::fs::create_dir_all(&config_root)?;
        horon_agent::env::set_var("HORON_AGENT_HOME", temp.path());
        horon_agent::config::invalidate_config_cache();
        apply_openai_compatible_profile_env(None);
        AuthStatus::invalidate_cache();

        Ok(Self {
            _lock: lock,
            saved,
            temp,
        })
    }

    fn config_dir(&self) -> PathBuf {
        self.temp.path().join("config").join("horon-agent")
    }

    fn clear_profile_keys(&self) {
        horon_agent::env::remove_var("OPENROUTER_API_KEY");
        for profile in openai_compatible_profiles() {
            horon_agent::env::remove_var(profile.api_key_env);
        }
        AuthStatus::invalidate_cache();
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        apply_openai_compatible_profile_env(None);
        AuthStatus::invalidate_cache();
        horon_agent::config::invalidate_config_cache();
        for (key, value) in &self.saved {
            if let Some(value) = value {
                horon_agent::env::set_var(key, value);
            } else {
                horon_agent::env::remove_var(key);
            }
        }
        AuthStatus::invalidate_cache();
        horon_agent::config::invalidate_config_cache();
    }
}

fn openai_compatible_login_providers(
    providers: impl IntoIterator<Item = LoginProviderDescriptor>,
) -> Vec<LoginProviderDescriptor> {
    providers
        .into_iter()
        .filter(|provider| matches!(provider.target, LoginProviderTarget::OpenAiCompatible(_)))
        .collect()
}

fn non_compatible_login_providers(
    providers: impl IntoIterator<Item = LoginProviderDescriptor>,
) -> Vec<LoginProviderDescriptor> {
    providers
        .into_iter()
        .filter(|provider| {
            !matches!(
                provider.target,
                LoginProviderTarget::OpenAiCompatible(_)
                    | LoginProviderTarget::AutoImport
                    | LoginProviderTarget::Google
            )
        })
        .collect()
}

fn login_provider_profile(provider: LoginProviderDescriptor) -> OpenAiCompatibleProfile {
    match provider.target {
        LoginProviderTarget::OpenAiCompatible(profile) => profile,
        _ => panic!("{} is not an OpenAI-compatible login provider", provider.id),
    }
}

fn write_profile_api_key_file(env: &TestEnv, profile: OpenAiCompatibleProfile, value: &str) -> Result<()> {
    let resolved = resolve_openai_compatible_profile(profile);
    let path = env.config_dir().join(&resolved.env_file);
    std::fs::create_dir_all(env.config_dir())?;
    std::fs::write(&path, format!("{}={value}\n", resolved.api_key_env))?;
    horon_agent::env::remove_var(&resolved.api_key_env);
    AuthStatus::invalidate_cache();
    Ok(())
}

fn write_profile_login_material(env: &TestEnv, profile: OpenAiCompatibleProfile, label: &str) -> Result<()> {
    let resolved = resolve_openai_compatible_profile(profile);
    if resolved.requires_api_key {
        write_profile_api_key_file(env, profile, &format!("sk-{label}-{}", resolved.id))?;
    }
    Ok(())
}

fn assert_runtime_profile_env(profile: OpenAiCompatibleProfile, context: &str) {
    let resolved = resolve_openai_compatible_profile(profile);
    assert_eq!(
        std::env::var("HORON_AGENT_OPENROUTER_API_BASE").ok().as_deref(),
        Some(resolved.api_base.as_str()),
        "runtime api base mismatch for {context}"
    );
    assert_eq!(
        std::env::var("HORON_AGENT_OPENROUTER_API_KEY_NAME").ok().as_deref(),
        Some(resolved.api_key_env.as_str()),
        "runtime api key env mismatch for {context}"
    );
    assert_eq!(
        std::env::var("HORON_AGENT_OPENROUTER_ENV_FILE").ok().as_deref(),
        Some(resolved.env_file.as_str()),
        "runtime env file mismatch for {context}"
    );
    assert_eq!(
        std::env::var("HORON_AGENT_OPENROUTER_CACHE_NAMESPACE").ok().as_deref(),
        Some(resolved.id.as_str()),
        "runtime cache namespace mismatch for {context}"
    );
    assert_eq!(
        std::env::var("HORON_AGENT_OPENROUTER_ALLOW_NO_AUTH").ok().as_deref(),
        (!resolved.requires_api_key).then_some("1"),
        "runtime no-auth flag mismatch for {context}"
    );
}

fn assert_no_compatible_runtime_profile_env(context: &str) {
    for key in [
        "HORON_AGENT_OPENROUTER_API_BASE",
        "HORON_AGENT_OPENROUTER_API_KEY_NAME",
        "HORON_AGENT_OPENROUTER_ENV_FILE",
        "HORON_AGENT_OPENROUTER_CACHE_NAMESPACE",
        "HORON_AGENT_OPENROUTER_PROVIDER_FEATURES",
        "HORON_AGENT_OPENROUTER_ALLOW_NO_AUTH",
        "HORON_AGENT_OPENROUTER_STATIC_MODELS",
        "HORON_AGENT_PROVIDER_PROFILE_ACTIVE",
        "HORON_AGENT_PROVIDER_PROFILE_NAME",
        "HORON_AGENT_NAMED_PROVIDER_PROFILE",
    ] {
        assert!(
            std::env::var_os(key).is_none(),
            "{key} should be cleared for {context}; value={:?}",
            std::env::var_os(key)
        );
    }
}

fn assert_no_active_compatible_profile_lock(context: &str) {
    for key in [
        "HORON_AGENT_PROVIDER_PROFILE_ACTIVE",
        "HORON_AGENT_PROVIDER_PROFILE_NAME",
    ] {
        assert!(
            std::env::var_os(key).is_none(),
            "{key} should not stay locked for {context}; value={:?}",
            std::env::var_os(key)
        );
    }
}

fn seed_non_compatible_auto_auth(provider: LoginProviderDescriptor) -> bool {
    match provider.target {
        LoginProviderTarget::Claude => {
            horon_agent::env::set_var("ANTHROPIC_API_KEY", "test-anthropic-key");
            true
        }
        LoginProviderTarget::OpenAiApiKey => {
            horon_agent::env::set_var("OPENAI_API_KEY", "sk-test-openai-key");
            true
        }
        LoginProviderTarget::OpenRouter => {
            horon_agent::env::set_var("OPENROUTER_API_KEY", "sk-test-openrouter-key");
            true
        }
        LoginProviderTarget::Copilot => {
            horon_agent::env::set_var("COPILOT_GITHUB_TOKEN", "gho_test-copilot-token");
            true
        }
        LoginProviderTarget::Cursor => {
            horon_agent::env::set_var("CURSOR_API_KEY", "sk-test-cursor-key");
            true
        }
        _ => false,
    }
}

#[tokio::test]
async fn provider_matrix_non_compatible_bootstrap_login_clears_stale_compatible_profile_state_space() -> Result<()> {
    let stale_provider = openai_compatible_login_providers(server_bootstrap_login_providers())
        .into_iter()
        .next()
        .expect("compatible bootstrap provider");
    let stale_profile = login_provider_profile(stale_provider);
    let providers = non_compatible_login_providers(server_bootstrap_login_providers());
    assert!(
        !providers.is_empty(),
        "server bootstrap login surface should include non-compatible providers"
    );

    for provider in providers {
        let env = TestEnv::new()?;
        env.clear_profile_keys();
        write_profile_login_material(&env, stale_profile, "stale-non-compatible")?;
        apply_login_provider_profile_env(stale_provider);
        let context = format!(
            "non-compatible bootstrap provider={} stale_profile={}",
            provider.id, stale_profile.id
        );
        assert_runtime_profile_env(stale_profile, &context);

        apply_login_provider_profile_env(provider);
        AuthStatus::invalidate_cache();
        assert_no_compatible_runtime_profile_env(&context);

        if seed_non_compatible_auto_auth(provider) {
            AuthStatus::invalidate_cache();
            // A non-compatible provider has no direct OpenAI-compatible
            // endpoint in this build, so auto init must stop with setup
            // instructions rather than silently locking onto the stale
            // compatible profile.
            let error = init_provider_for_validation(&ProviderChoice::Auto, None)
                .await
                .err()
                .unwrap_or_else(|| panic!("auto init must fail without a configured provider for {context}"));
            let message = error.to_string();
            assert!(message.contains("openai-compatible"), "{context}: {message}");
            assert!(!message.contains("router"), "{context}: {message}");
            assert_no_active_compatible_profile_lock(&context);
        }
    }

    Ok(())
}

#[tokio::test]
async fn provider_matrix_concurrent_auto_init_preserves_bootstrap_compatible_profile() -> Result<()> {
    let providers = openai_compatible_login_providers(server_bootstrap_login_providers());
    assert!(!providers.is_empty(), "compatible bootstrap providers");

    for provider in providers {
        let selected = login_provider_profile(provider);
        let env = TestEnv::new()?;
        env.clear_profile_keys();
        write_profile_login_material(&env, selected, "concurrent-auto")?;
        apply_login_provider_profile_env(provider);
        AuthStatus::invalidate_cache();

        let context = format!("concurrent auto init provider={} selected={}", provider.id, selected.id);
        let (a, b, c, d) = tokio::join!(
            init_provider_for_validation(&ProviderChoice::Auto, None),
            init_provider_for_validation(&ProviderChoice::Auto, None),
            init_provider_for_validation(&ProviderChoice::Auto, None),
            init_provider_for_validation(&ProviderChoice::Auto, None),
        );
        for result in [a, b, c, d] {
            result.unwrap_or_else(|err| panic!("auto init failed for {context}: {err}"));
        }
        assert_runtime_profile_env(selected, &context);
    }

    Ok(())
}
