use super::*;
use crate::external_auth::{parse_external_auth_review_selection, pending_external_auth_review_candidates};
use crate::provider_catalog::{self, resolve_openai_compatible_profile};
use std::sync::{LazyLock, Mutex};
use tempfile::TempDir;

static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    match ENV_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[test]
fn provider_choice_arg_values_round_trip_through_the_parser() {
    let choices = [
        ProviderChoice::Auto,
        ProviderChoice::OpenaiCompatible,
        ProviderChoice::Profile("deepseek".to_string()),
    ];
    for choice in choices {
        assert_eq!(parse_provider_choice(&choice.as_arg_value()), Ok(choice));
    }
}

#[test]
fn parse_provider_choice_accepts_auto_aliases_and_openai_compatible_profiles() {
    assert_eq!(parse_provider_choice("auto"), Ok(ProviderChoice::Auto));
    assert_eq!(parse_provider_choice(" Auto "), Ok(ProviderChoice::Auto));

    for alias in ["openai-compatible", "compat", "custom"] {
        assert_eq!(parse_provider_choice(alias), Ok(ProviderChoice::OpenaiCompatible));
    }

    let deepseek = parse_provider_choice("deepseek").expect("a built-in profile id is selectable");
    assert_eq!(deepseek, ProviderChoice::Profile("deepseek".to_string()));
    assert_eq!(deepseek.profile_id(), Some("deepseek"));
    assert_eq!(
        deepseek
            .profile_id()
            .and_then(provider_catalog::openai_compatible_profile_by_id)
            .map(|profile| profile.id),
        Some("deepseek")
    );

    assert_eq!(ProviderChoice::OpenaiCompatible.profile_id(), Some("openai-compatible"));
    assert_eq!(
        ProviderChoice::OpenaiCompatible
            .profile_id()
            .and_then(provider_catalog::openai_compatible_profile_by_id)
            .map(|profile| profile.id),
        Some(provider_catalog::OPENAI_COMPAT_PROFILE.id)
    );
    assert_eq!(ProviderChoice::Auto.profile_id(), None);
}

#[test]
fn parse_provider_choice_rejects_unknown_vendor_names() {
    let error = parse_provider_choice("").expect_err("an empty provider name is a usage error");
    assert!(error.contains("cannot be empty"), "{error}");

    let error = parse_provider_choice("claude").expect_err("a removed vendor login is not a provider");
    assert!(error.contains("unknown provider 'claude'"), "{error}");
    assert!(error.contains("openai-compatible"), "{error}");
    assert!(!error.contains("router"), "{error}");
}

#[tokio::test]
async fn auto_model_init_without_a_configured_provider_names_openai_compatible() {
    let _guard = lock_env();
    let _env_guard = crate::storage::lock_test_env();
    let dir = TempDir::new().expect("temp dir");
    let saved: Vec<(String, Option<String>)> = [
        "HORON_HOME",
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
    crate::env::set_var("HORON_HOME", dir.path());
    for key in saved.iter().skip(1) {
        crate::env::remove_var(&key.0);
    }
    crate::config::invalidate_config_cache();
    crate::auth::AuthStatus::invalidate_cache();

    for result in [
        init_provider(&ProviderChoice::Auto, None).await.err(),
        init_provider_for_run(&ProviderChoice::Auto, None).await.err(),
        init_provider_quiet(&ProviderChoice::Auto, None).await.err(),
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
    let saved_home = std::env::var("HORON_HOME").ok();
    crate::env::set_var("HORON_HOME", dir.path());
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
    let error = match init_provider_quiet(&ProviderChoice::Auto, None).await {
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
    let error = match init_provider_quiet(&ProviderChoice::Auto, None).await {
        Ok(_) => panic!("an unknown default_provider must be an error"),
        Err(error) => error,
    };
    let message = error.to_string();
    assert!(message.contains("missing"), "{message}");
    assert!(message.contains("[providers.missing]"), "{message}");

    match saved_home {
        Some(home) => crate::env::set_var("HORON_HOME", home),
        None => crate::env::remove_var("HORON_HOME"),
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
fn pending_external_auth_review_candidates_report_supported_api_key_sources() {
    let _guard = lock_env();
    let _env_guard = crate::storage::lock_test_env();
    let dir = TempDir::new().expect("temp dir");
    let prev_home = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", dir.path());

    let opencode_path = crate::auth::external::ExternalAuthSource::OpenCode
        .path()
        .expect("opencode path");
    std::fs::create_dir_all(opencode_path.parent().expect("opencode parent")).expect("create opencode dir");
    std::fs::write(
        &opencode_path,
        serde_json::json!({ "openai": { "type": "api", "key": "sk-opencode" } }).to_string(),
    )
    .expect("write opencode auth");

    let hermes_path = crate::auth::external::ExternalAuthSource::Hermes
        .path()
        .expect("hermes path");
    std::fs::create_dir_all(hermes_path.parent().expect("hermes parent")).expect("create hermes dir");
    std::fs::write(
        &hermes_path,
        serde_json::json!({
            "credential_pool": {
                "openai": [{ "auth_type": "api_key", "access_token": "sk-hermes" }]
            }
        })
        .to_string(),
    )
    .expect("write hermes auth");

    let candidates = pending_external_auth_review_candidates().expect("candidates");
    for source in ["OpenCode auth.json", "Hermes auth.json"] {
        assert!(
            candidates.iter().any(|candidate| {
                candidate.source_name() == source
                    && candidate.provider_summary() == "OpenAI-compatible API-key providers"
            }),
            "missing {source} in {candidates:?}"
        );
    }

    if let Some(prev_home) = prev_home {
        crate::env::set_var("HORON_HOME", prev_home);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
}
