use super::*;
// These moved from cli::provider_init to crate::external_auth in the
// tui->cli layering refactor (a9a82827); provider_init.rs only re-imports the
// subset it uses, so `super::*` no longer re-exports them to this test module.
use crate::external_auth::{parse_external_auth_review_selection, pending_external_auth_review_candidates};
use crate::provider_catalog::{self, resolve_openai_compatible_profile};
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use tempfile::TempDir;

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    let mutex = ENV_LOCK.get_or_init(|| Mutex::new(()));
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[test]
#[allow(deprecated)]
fn test_provider_choice_arg_values() {
    assert_eq!(ProviderChoice::HoronAgent.as_arg_value(), "horon-agent");
    assert_eq!(ProviderChoice::Openai.as_arg_value(), "openai");
    assert_eq!(ProviderChoice::OpenaiApi.as_arg_value(), "openai-api");
    assert_eq!(ProviderChoice::Azure.as_arg_value(), "azure");
    assert_eq!(ProviderChoice::Opencode.as_arg_value(), "opencode");
    assert_eq!(ProviderChoice::OpencodeGo.as_arg_value(), "opencode-go");
    assert_eq!(ProviderChoice::Zai.as_arg_value(), "zai");
    assert_eq!(ProviderChoice::Groq.as_arg_value(), "groq");
    assert_eq!(ProviderChoice::Mistral.as_arg_value(), "mistral");
    assert_eq!(ProviderChoice::Perplexity.as_arg_value(), "perplexity");
    assert_eq!(ProviderChoice::TogetherAi.as_arg_value(), "togetherai");
    assert_eq!(ProviderChoice::Deepinfra.as_arg_value(), "deepinfra");
    assert_eq!(ProviderChoice::Fireworks.as_arg_value(), "fireworks");
    assert_eq!(ProviderChoice::Novita.as_arg_value(), "novita");
    assert_eq!(ProviderChoice::Minimax.as_arg_value(), "minimax");
    assert_eq!(ProviderChoice::Xai.as_arg_value(), "xai");
    assert_eq!(ProviderChoice::GrokBuild.as_arg_value(), "grok-build");
    assert_eq!(ProviderChoice::XiaomiMimo.as_arg_value(), "xiaomi-mimo");
    assert_eq!(ProviderChoice::MetaMuse.as_arg_value(), "meta-muse");
    assert_eq!(ProviderChoice::Celeris.as_arg_value(), "celeris");
    assert_eq!(ProviderChoice::Lmstudio.as_arg_value(), "lmstudio");
    assert_eq!(ProviderChoice::Ollama.as_arg_value(), "ollama");
    assert_eq!(ProviderChoice::Chutes.as_arg_value(), "chutes");
    assert_eq!(ProviderChoice::Cerebras.as_arg_value(), "cerebras");
    assert_eq!(ProviderChoice::AlibabaCodingPlan.as_arg_value(), "alibaba-coding-plan");
    assert_eq!(ProviderChoice::OpenaiCompatible.as_arg_value(), "openai-compatible");
    assert_eq!(ProviderChoice::Google.as_arg_value(), "google");
    assert_eq!(ProviderChoice::Auto.as_arg_value(), "auto");
}

#[test]
fn novita_cli_aliases_select_the_builtin_profile() {
    use clap::ValueEnum;
    for input in ["novita", "novita-ai", "novita.ai"] {
        let choice = ProviderChoice::from_str(input, false).unwrap();
        assert_eq!(choice, ProviderChoice::Novita);
        assert_eq!(profile_for_choice(&choice), Some(provider_catalog::NOVITA_PROFILE));
        assert_eq!(
            login_provider_for_choice(&choice),
            Some(provider_catalog::NOVITA_LOGIN_PROVIDER)
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::await_holding_lock,
    reason = "test env locks intentionally stay held across provider init to isolate process-global auth env"
)]
async fn auth_validation_refuses_builtin_execution_even_with_configured_credentials() {
    let _guard = lock_env();
    let _env_guard = crate::storage::lock_test_env();
    let dir = TempDir::new().expect("temp dir");
    let keys = [
        "HORON_AGENT_HOME",
        "OPENAI_API_KEY",
        "HORON_AGENT_PROVIDER_PROFILE_ACTIVE",
        "HORON_AGENT_PROVIDER_PROFILE_NAME",
        "HORON_AGENT_NAMED_PROVIDER_PROFILE",
        "HORON_AGENT_RUNTIME_PROVIDER",
        "HORON_AGENT_ACTIVE_PROVIDER",
        "HORON_AGENT_INITIAL_PROVIDER_EXPLICIT",
    ];
    let saved: Vec<(&str, Option<String>)> = keys.iter().map(|key| (*key, std::env::var(key).ok())).collect();

    crate::env::set_var("HORON_AGENT_HOME", dir.path());
    crate::env::set_var("OPENAI_API_KEY", "sk-openai-api-test");
    for key in keys.iter().skip(2) {
        crate::env::remove_var(key);
    }
    std::fs::write(
        dir.path().join("config.toml"),
        r#"
[provider]
default_provider = "local-gateway"
default_model = "gateway-default"

[providers.local-gateway]
type = "openai-compatible"
base_url = "http://localhost:1234/v1"
auth = "none"
default_model = "gateway-default"
requires_api_key = false
"#,
    )
    .expect("write competing configured provider default");
    crate::config::invalidate_config_cache();
    crate::auth::AuthStatus::invalidate_cache();

    let error = init_provider_for_validation(&ProviderChoice::OpenaiApi, Some("gpt-5.6-luna"))
        .await
        .err()
        .expect("auth validation must not bypass the direct provider configuration");

    let message = error.to_string();
    assert!(message.contains("openai-compatible"), "{message}");
    assert!(message.contains("[providers."), "{message}");
    assert!(!message.contains("router"), "{message}");
    assert!(std::env::var("HORON_AGENT_RUNTIME_PROVIDER").is_err());

    for (key, value) in saved {
        if let Some(value) = value {
            crate::env::set_var(key, value);
        } else {
            crate::env::remove_var(key);
        }
    }
    crate::config::invalidate_config_cache();
    crate::auth::AuthStatus::invalidate_cache();
}

#[test]
fn test_server_bootstrap_login_selection_preserves_order() {
    let providers = provider_catalog::server_bootstrap_login_providers();
    let ids: Vec<&str> = providers.iter().map(|provider| provider.id).collect();
    assert!(ids.contains(&"openai"), "openai must be selectable: {ids:?}");
    assert!(ids.contains(&"openai-api"), "openai-api must be selectable: {ids:?}");
    assert!(ids.contains(&"horon-agent"), "horon-agent must be selectable: {ids:?}");
    for removed in [
        "claude",
        "anthropic-api",
        "openrouter",
        "bedrock",
        "cursor",
        "copilot",
        "gemini",
        "antigravity",
    ] {
        assert!(!ids.contains(&removed), "{removed} must not be selectable: {ids:?}");
    }
}

#[test]
fn test_auto_init_login_selection_preserves_order() {
    let providers = provider_catalog::auto_init_login_providers();
    let ids: Vec<&str> = providers.iter().map(|provider| provider.id).collect();
    assert!(ids.contains(&"openai"), "openai must be selectable: {ids:?}");
    assert!(ids.contains(&"openai-api"), "openai-api must be selectable: {ids:?}");
    assert!(ids.contains(&"horon-agent"), "horon-agent must be selectable: {ids:?}");
    for removed in [
        "claude",
        "anthropic-api",
        "openrouter",
        "bedrock",
        "cursor",
        "copilot",
        "gemini",
        "antigravity",
    ] {
        assert!(!ids.contains(&removed), "{removed} must not be selectable: {ids:?}");
    }
}

#[tokio::test]
async fn a_builtin_provider_without_a_compatible_profile_is_refused_with_setup_instructions() {
    let _guard = lock_env();
    let keys = ["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "HORON_AGENT_RUNTIME_PROVIDER"];
    let saved: Vec<(&str, Option<String>)> = keys.iter().map(|key| (*key, std::env::var(key).ok())).collect();
    // Credentials that used to be probed first: the refusal must not depend on them.
    crate::env::set_var("OPENAI_API_KEY", "sk-refused-not-probed");
    for key in keys.iter().skip(1) {
        crate::env::remove_var(key);
    }

    for choice in [
        ProviderChoice::Openai,
        ProviderChoice::OpenaiApi,
        ProviderChoice::HoronAgent,
        ProviderChoice::Google,
    ] {
        let mut errors = Vec::new();
        errors.push(init_provider(&choice, None).await.err());
        errors.push(init_provider_for_run(&choice, None).await.err());
        errors.push(init_provider_for_serve(&choice, None).await.err());
        errors.push(init_provider_quiet(&choice, None).await.err());
        errors.push(init_provider_for_validation(&choice, None).await.err());
        for error in errors {
            let error = error.expect("a built-in provider must be refused on every model path");
            let message = error.to_string();
            assert!(message.contains(choice.as_arg_value()), "{message}");
            assert!(message.contains("openai-compatible"), "{message}");
            assert!(message.contains("[providers."), "{message}");
            assert!(!message.contains("router"), "{message}");
            assert!(
                !message.contains("No credentials configured"),
                "the refusal must not come from credential detection: {message}"
            );
        }
    }

    for (key, value) in saved {
        match value {
            Some(value) => crate::env::set_var(key, value),
            None => crate::env::remove_var(key),
        }
    }
}

#[tokio::test]
async fn auto_model_init_without_a_configured_provider_names_openai_compatible() {
    let _guard = lock_env();
    let _env_guard = crate::storage::lock_test_env();
    let dir = TempDir::new().expect("temp dir");
    let saved: Vec<(String, Option<String>)> = [
        "HORON_AGENT_HOME",
        "HORON_AGENT_NON_INTERACTIVE",
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "HORON_AGENT_PROVIDER_PROFILE_ACTIVE",
        "HORON_AGENT_PROVIDER_PROFILE_NAME",
        "HORON_AGENT_NAMED_PROVIDER_PROFILE",
        "HORON_AGENT_OPENROUTER_CACHE_NAMESPACE",
    ]
    .iter()
    .map(|k| (k.to_string(), std::env::var(k).ok()))
    .collect();
    crate::env::set_var("HORON_AGENT_HOME", dir.path());
    for key in saved.iter().skip(1) {
        crate::env::remove_var(&key.0);
    }
    crate::config::invalidate_config_cache();
    crate::auth::AuthStatus::invalidate_cache();

    for result in [
        init_provider(&ProviderChoice::Auto, None).await.err(),
        init_provider_for_run(&ProviderChoice::Auto, None).await.err(),
        init_provider_quiet(&ProviderChoice::Auto, None).await.err(),
        init_provider_for_validation(&ProviderChoice::Auto, None).await.err(),
    ] {
        let error = result.expect("an unconfigured model launch must fail");
        let message = error.to_string();
        assert!(message.contains("openai-compatible"), "{message}");
        assert!(message.contains("[providers."), "{message}");
        assert!(message.contains("default_provider"), "{message}");
        assert!(!message.contains("router"), "{message}");
        assert!(!message.contains("native plugin"), "{message}");
    }

    // A server launch stays up for ACP management while model calls fail with
    // the same setup instructions.
    let serve = init_provider_for_serve(&ProviderChoice::Auto, None)
        .await
        .expect("a management-only server launch survives");
    let reason = serve
        .unavailable_reason()
        .expect("the provider reports why it cannot model");
    assert!(reason.contains("openai-compatible"), "{reason}");
    assert!(!reason.contains("router"), "{reason}");
    assert!(serve.complete(&[], &[], "", None).await.is_err());

    for (key, value) in saved {
        match value {
            Some(value) => crate::env::set_var(&key, value),
            None => crate::env::remove_var(&key),
        }
    }
    crate::config::invalidate_config_cache();
    crate::auth::AuthStatus::invalidate_cache();
}

#[tokio::test]
async fn a_malformed_named_profile_is_a_named_configuration_error() {
    let _guard = lock_env();
    let _env_guard = crate::storage::lock_test_env();
    let dir = TempDir::new().expect("temp dir");
    let saved_home = std::env::var("HORON_AGENT_HOME").ok();
    crate::env::set_var("HORON_AGENT_HOME", dir.path());
    for key in [
        "HORON_AGENT_PROVIDER_PROFILE_ACTIVE",
        "HORON_AGENT_PROVIDER_PROFILE_NAME",
        "HORON_AGENT_NAMED_PROVIDER_PROFILE",
        "HORON_AGENT_OPENROUTER_CACHE_NAMESPACE",
    ] {
        crate::env::remove_var(key);
    }

    std::fs::write(
        dir.path().join("config.toml"),
        "[provider]\ndefault_provider = \"broken\"\n\n[providers.broken]\ntype = \"openai-compatible\"\nbase_url = \"\"\nauth = \"none\"\n",
    )
    .expect("write config");
    crate::config::invalidate_config_cache();
    let error = match init_provider_for_validation(&ProviderChoice::Auto, None).await {
        Ok(_) => panic!("an empty base_url must be a configuration error"),
        Err(error) => error,
    };
    let message = error.to_string();
    assert!(message.contains("broken"), "the configured profile is named: {message}");
    assert!(message.contains("base_url"), "{message}");
    assert!(!message.contains("router"), "{message}");

    std::fs::write(
        dir.path().join("config.toml"),
        "[provider]\ndefault_provider = \"missing\"\n",
    )
    .expect("write config");
    crate::config::invalidate_config_cache();
    let error = match init_provider_for_validation(&ProviderChoice::Auto, None).await {
        Ok(_) => panic!("an unknown default_provider must be an error"),
        Err(error) => error,
    };
    let message = error.to_string();
    assert!(message.contains("missing"), "{message}");
    assert!(message.contains("[providers.missing]"), "{message}");

    match saved_home {
        Some(home) => crate::env::set_var("HORON_AGENT_HOME", home),
        None => crate::env::remove_var("HORON_AGENT_HOME"),
    }
    crate::config::invalidate_config_cache();
}

#[test]
fn test_openai_compatible_profile_overrides() {
    let _guard = lock_env();
    let keys = [
        "HORON_AGENT_OPENAI_COMPAT_API_BASE",
        "HORON_AGENT_OPENAI_COMPAT_API_KEY_NAME",
        "HORON_AGENT_OPENAI_COMPAT_ENV_FILE",
        "HORON_AGENT_OPENAI_COMPAT_DEFAULT_MODEL",
    ];
    let saved: Vec<(String, Option<String>)> = keys.iter().map(|k| (k.to_string(), std::env::var(k).ok())).collect();

    crate::env::set_var("HORON_AGENT_OPENAI_COMPAT_API_BASE", "https://api.groq.com/openai/v1/");
    crate::env::set_var("HORON_AGENT_OPENAI_COMPAT_API_KEY_NAME", "GROQ_API_KEY");
    crate::env::set_var("HORON_AGENT_OPENAI_COMPAT_ENV_FILE", "groq.env");
    crate::env::set_var("HORON_AGENT_OPENAI_COMPAT_DEFAULT_MODEL", "openai/gpt-oss-120b");

    let resolved = resolve_openai_compatible_profile(provider_catalog::OPENAI_COMPAT_PROFILE);
    assert_eq!(resolved.api_base, "https://api.groq.com/openai/v1");
    assert_eq!(resolved.api_key_env, "GROQ_API_KEY");
    assert_eq!(resolved.env_file, "groq.env");
    assert_eq!(resolved.default_model.as_deref(), Some("openai/gpt-oss-120b"));

    for (key, value) in saved {
        if let Some(value) = value {
            crate::env::set_var(&key, value);
        } else {
            crate::env::remove_var(&key);
        }
    }
}

#[test]
fn test_openai_compatible_profile_rejects_invalid_overrides() {
    let _guard = lock_env();
    let keys = [
        "HORON_AGENT_OPENAI_COMPAT_API_BASE",
        "HORON_AGENT_OPENAI_COMPAT_API_KEY_NAME",
        "HORON_AGENT_OPENAI_COMPAT_ENV_FILE",
    ];
    let saved: Vec<(String, Option<String>)> = keys.iter().map(|k| (k.to_string(), std::env::var(k).ok())).collect();

    crate::env::set_var("HORON_AGENT_OPENAI_COMPAT_API_BASE", "http://example.com/v1");
    crate::env::set_var("HORON_AGENT_OPENAI_COMPAT_API_KEY_NAME", "bad-key-name");
    crate::env::set_var("HORON_AGENT_OPENAI_COMPAT_ENV_FILE", "../bad.env");

    let resolved = resolve_openai_compatible_profile(provider_catalog::OPENAI_COMPAT_PROFILE);
    assert_eq!(resolved.api_base, provider_catalog::OPENAI_COMPAT_PROFILE.api_base);
    assert_eq!(
        resolved.api_key_env,
        provider_catalog::OPENAI_COMPAT_PROFILE.api_key_env
    );
    assert_eq!(resolved.env_file, provider_catalog::OPENAI_COMPAT_PROFILE.env_file);

    for (key, value) in saved {
        if let Some(value) = value {
            crate::env::set_var(&key, value);
        } else {
            crate::env::remove_var(&key);
        }
    }
}

#[test]
fn parse_external_auth_review_selection_supports_all_and_deduped_indices() {
    assert_eq!(
        parse_external_auth_review_selection("", 3).unwrap(),
        Vec::<usize>::new()
    );
    assert_eq!(parse_external_auth_review_selection("a", 3).unwrap(), vec![0, 1, 2]);
    assert_eq!(parse_external_auth_review_selection("2,1,2", 3).unwrap(), vec![1, 0]);
    assert!(parse_external_auth_review_selection("4", 3).is_err());
    assert!(parse_external_auth_review_selection("nope", 3).is_err());
}

#[test]
fn parse_login_provider_selection_supports_skip_and_names() {
    let providers = provider_catalog::cli_login_providers();

    assert!(parse_login_provider_selection_input("", &providers).unwrap().is_none());
    assert!(
        parse_login_provider_selection_input("skip", &providers)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        parse_login_provider_selection_input("openai", &providers)
            .unwrap()
            .map(|provider| provider.id),
        Some("openai")
    );
    let first_provider = providers[0].id;
    assert_eq!(
        parse_login_provider_selection_input("1", &providers)
            .unwrap()
            .map(|provider| provider.id),
        Some(first_provider)
    );
    assert!(parse_login_provider_selection_input("not-a-provider", &providers).is_err());
}

#[test]
fn login_provider_menu_shows_autodetected_auth_and_skip() {
    let providers = vec![provider_catalog::OPENAI_LOGIN_PROVIDER];
    let status = auth::AuthStatus {
        openai: auth::AuthState::Available,
        openai_has_oauth: true,
        openai_oauth_state: auth::AuthState::Available,
        openai_has_api_key: false,
        ..Default::default()
    };

    let menu = render_login_provider_selection_menu("Choose a provider:", &providers, &status);
    assert!(menu.contains("Autodetected auth:"));
    assert!(menu.contains("[configured"));
    assert!(menu.contains("Skip: press Enter"));
}

#[test]
fn choice_for_login_provider_round_trips_openai_compatible_profiles() {
    assert_eq!(
        choice_for_login_provider(provider_catalog::OPENCODE_LOGIN_PROVIDER),
        Some(ProviderChoice::Opencode)
    );
    assert_eq!(
        choice_for_login_provider(provider_catalog::LMSTUDIO_LOGIN_PROVIDER),
        Some(ProviderChoice::Lmstudio)
    );
    assert_eq!(
        choice_for_login_provider(provider_catalog::OPENAI_COMPAT_LOGIN_PROVIDER),
        Some(ProviderChoice::OpenaiCompatible)
    );
}

#[test]
fn login_provider_choice_table_round_trips_catalog_providers() {
    let mut seen_choices = HashSet::new();
    let mut reverse_mapped_provider_ids = HashSet::new();

    for (choice, provider) in login_provider_choice_mappings() {
        assert!(
            seen_choices.insert(choice.as_arg_value()),
            "duplicate provider choice mapping for {}",
            choice.as_arg_value()
        );
        assert_eq!(
            login_provider_for_choice(choice).map(|candidate| candidate.id),
            Some(provider.id),
            "choice {} should resolve to {}",
            choice.as_arg_value(),
            provider.id
        );

        if reverse_mapped_provider_ids.insert(provider.id) {
            assert_eq!(
                choice_for_login_provider(*provider),
                Some(*choice),
                "provider {} should reverse-map to {}",
                provider.id,
                choice.as_arg_value()
            );
        }
    }

    for provider in provider_catalog::login_providers() {
        if matches!(provider.target, provider_catalog::LoginProviderTarget::AutoImport) {
            assert_eq!(choice_for_login_provider(*provider), None);
        } else {
            assert!(
                reverse_mapped_provider_ids.contains(provider.id),
                "provider {} is in the catalog but not the CLI choice table",
                provider.id
            );
        }
    }
}

#[test]
fn auth_integration_registry_matches_cli_choice_runtime_wiring() {
    for provider in provider_catalog::login_providers() {
        let integration = crate::auth::integration::auth_provider_integration(provider.id)
            .expect("catalog provider should have integration metadata");
        assert_eq!(integration.descriptor, *provider);

        if !matches!(provider.target, provider_catalog::LoginProviderTarget::AutoImport) {
            assert!(
                choice_for_login_provider(*provider).is_some(),
                "provider {} is missing a CLI choice mapping",
                provider.id
            );
        }

        let status = auth::AuthStatus::default();
        let assessment = status.assessment_for_provider(*provider);
        assert!(
            !assessment.method_detail.is_empty(),
            "provider {} should have non-empty auth status method detail",
            provider.id
        );
    }
}

#[test]
fn resolved_profile_default_model_uses_openai_compatible_override() {
    let _guard = lock_env();
    let _env_guard = crate::storage::lock_test_env();
    let saved: Vec<(String, Option<String>)> = [
        "HORON_AGENT_OPENAI_COMPAT_API_BASE",
        "HORON_AGENT_OPENAI_COMPAT_API_KEY_NAME",
        "HORON_AGENT_OPENAI_COMPAT_ENV_FILE",
        "HORON_AGENT_OPENAI_COMPAT_DEFAULT_MODEL",
    ]
    .iter()
    .map(|k| (k.to_string(), std::env::var(k).ok()))
    .collect();

    crate::env::set_var("HORON_AGENT_OPENAI_COMPAT_API_BASE", "http://localhost:11434/v1");
    crate::env::set_var("HORON_AGENT_OPENAI_COMPAT_DEFAULT_MODEL", "llama3.2");

    assert_eq!(
        resolved_profile_default_model(provider_catalog::OPENAI_COMPAT_PROFILE).as_deref(),
        Some("llama3.2")
    );

    for (key, value) in saved {
        if let Some(value) = value {
            crate::env::set_var(&key, value);
        } else {
            crate::env::remove_var(&key);
        }
    }
}

#[test]
fn apply_login_provider_profile_env_keeps_an_explicit_named_profile() {
    // Regression for #712: `auth-test --provider-profile <name>` cleared
    // HORON_AGENT_NAMED_PROVIDER_PROFILE before probing, so the probe evaluated the
    // built-in generic openai-compatible slot and false-negatived every custom
    // named profile.
    let _guard = lock_env();
    let _env_guard = crate::storage::lock_test_env();
    let saved: Vec<(String, Option<String>)> = [
        "HORON_AGENT_OPENROUTER_API_BASE",
        "HORON_AGENT_OPENROUTER_API_KEY_NAME",
        "HORON_AGENT_NAMED_PROVIDER_PROFILE",
        "HORON_AGENT_PROVIDER_PROFILE_ACTIVE",
        "HORON_AGENT_PROVIDER_PROFILE_NAME",
    ]
    .iter()
    .map(|k| (k.to_string(), std::env::var(k).ok()))
    .collect();
    for (key, _) in &saved {
        crate::env::remove_var(key);
    }

    crate::env::set_var("HORON_AGENT_NAMED_PROVIDER_PROFILE", "company-gateway");
    crate::env::set_var("HORON_AGENT_OPENROUTER_API_BASE", "https://gw.example.com/v1");
    crate::env::set_var("HORON_AGENT_OPENROUTER_API_KEY_NAME", "COMPANY_GATEWAY_KEY");

    apply_login_provider_profile_env(provider_catalog::OPENCODE_GO_LOGIN_PROVIDER);

    assert_eq!(
        std::env::var("HORON_AGENT_NAMED_PROVIDER_PROFILE").ok().as_deref(),
        Some("company-gateway"),
        "explicit named profile must survive the auth-test probe setup"
    );
    assert_eq!(
        std::env::var("HORON_AGENT_OPENROUTER_API_BASE").ok().as_deref(),
        Some("https://gw.example.com/v1"),
        "named profile runtime env must not be replaced by a builtin profile"
    );

    for (key, value) in saved {
        match value {
            Some(value) => crate::env::set_var(&key, value),
            None => crate::env::remove_var(&key),
        }
    }
}

#[test]
fn apply_login_provider_profile_env_preserves_compatible_profile_for_auto_spawn() {
    let _guard = lock_env();
    let _env_guard = crate::storage::lock_test_env();
    let saved: Vec<(String, Option<String>)> = [
        "HORON_AGENT_OPENROUTER_API_BASE",
        "HORON_AGENT_OPENROUTER_API_KEY_NAME",
        "HORON_AGENT_OPENROUTER_ENV_FILE",
        "HORON_AGENT_OPENROUTER_CACHE_NAMESPACE",
        "HORON_AGENT_OPENROUTER_PROVIDER_FEATURES",
        "HORON_AGENT_OPENROUTER_TRANSPORT_STATE",
        "HORON_AGENT_OPENROUTER_ALLOW_NO_AUTH",
        "HORON_AGENT_OPENROUTER_STATIC_MODELS",
        "HORON_AGENT_PROVIDER_PROFILE_ACTIVE",
        "HORON_AGENT_PROVIDER_PROFILE_NAME",
        "HORON_AGENT_NAMED_PROVIDER_PROFILE",
    ]
    .iter()
    .map(|k| (k.to_string(), std::env::var(k).ok()))
    .collect();

    for (key, _) in &saved {
        crate::env::remove_var(key);
    }

    apply_login_provider_profile_env(provider_catalog::OPENCODE_GO_LOGIN_PROVIDER);

    assert_eq!(
        std::env::var("HORON_AGENT_OPENROUTER_API_BASE").ok().as_deref(),
        Some("https://opencode.ai/zen/go/v1")
    );
    assert_eq!(
        std::env::var("HORON_AGENT_OPENROUTER_API_KEY_NAME").ok().as_deref(),
        Some("OPENCODE_GO_API_KEY")
    );
    assert_eq!(
        std::env::var("HORON_AGENT_OPENROUTER_ENV_FILE").ok().as_deref(),
        Some("opencode-go.env")
    );
    assert_eq!(
        std::env::var("HORON_AGENT_PROVIDER_PROFILE_ACTIVE").ok().as_deref(),
        Some("1")
    );

    // Mirrors the daemon child process starting with `--provider auto`: with the
    // active marker present, auto init must not erase the selected profile env.
    provider_catalog::apply_openai_compatible_profile_env(None);
    assert_eq!(
        std::env::var("HORON_AGENT_OPENROUTER_API_KEY_NAME").ok().as_deref(),
        Some("OPENCODE_GO_API_KEY")
    );

    // A later explicit compatible-provider selection in the same process must
    // still replace the active profile instead of being blocked by the marker.
    apply_login_provider_profile_env(provider_catalog::OPENCODE_LOGIN_PROVIDER);
    assert_eq!(
        std::env::var("HORON_AGENT_OPENROUTER_API_KEY_NAME").ok().as_deref(),
        Some("OPENCODE_API_KEY")
    );
    assert_eq!(
        std::env::var("HORON_AGENT_PROVIDER_PROFILE_ACTIVE").ok().as_deref(),
        Some("1")
    );

    for (key, value) in saved {
        if let Some(value) = value {
            crate::env::set_var(&key, value);
        } else {
            crate::env::remove_var(&key);
        }
    }
}

#[test]
fn pending_external_auth_review_candidates_include_shared_and_legacy_sources() {
    let _guard = lock_env();
    let _env_guard = crate::storage::lock_test_env();
    let dir = TempDir::new().expect("temp dir");
    let prev_home = std::env::var_os("HORON_AGENT_HOME");
    crate::env::set_var("HORON_AGENT_HOME", dir.path());

    let opencode_path = crate::auth::external::ExternalAuthSource::OpenCode
        .path()
        .expect("opencode path");
    std::fs::create_dir_all(opencode_path.parent().expect("opencode parent")).expect("create opencode dir");
    std::fs::write(
        &opencode_path,
        serde_json::json!({
            "openai": {
                "type": "oauth",
                "access": "sk-openai",
                "refresh": "refresh",
                "expires": chrono::Utc::now().timestamp_millis() + 60_000
            }
        })
        .to_string(),
    )
    .expect("write opencode auth");

    let codex_path = crate::auth::codex::legacy_auth_file_path().expect("codex path");
    std::fs::create_dir_all(codex_path.parent().expect("codex parent")).expect("create codex dir");
    std::fs::write(
        &codex_path,
        serde_json::json!({
            "tokens": {
                "access_token": "sk-codex",
                "refresh_token": "refresh",
                "expires_at": chrono::Utc::now().timestamp_millis() + 60_000
            }
        })
        .to_string(),
    )
    .expect("write codex auth");

    let candidates = pending_external_auth_review_candidates().expect("candidates");
    assert!(candidates.iter().any(|candidate| {
        candidate.source_name() == "OpenCode auth.json" && candidate.provider_summary().contains("Codex")
    }));
    assert!(
        candidates.iter().any(|candidate| {
            candidate.source_name() == "Codex auth.json" && candidate.provider_summary() == "Codex"
        })
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("HORON_AGENT_HOME", prev_home);
    } else {
        crate::env::remove_var("HORON_AGENT_HOME");
    }
}

#[test]
fn env_model_for_choice_only_targets_openrouter_slot_runtimes() {
    let env = Some("mistralai/mistral-large-4-0");
    assert_eq!(env_model_for_choice_from(&ProviderChoice::Deepseek, env), env);
    assert_eq!(env_model_for_choice_from(&ProviderChoice::Auto, env), None);
    assert_eq!(env_model_for_choice_from(&ProviderChoice::HoronAgent, env), None);
}
