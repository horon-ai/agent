use super::*;
use tempfile::TempDir;

fn write_auth_file(path: &std::path::Path, value: serde_json::Value) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, serde_json::to_string(&value).unwrap()).unwrap();
}

#[test]
fn novita_api_key_import_requires_trust() {
    let _guard = crate::storage::lock_test_env();
    let dir = TempDir::new().unwrap();
    let prev = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", dir.path());

    let path = ExternalAuthSource::OpenCode.path().unwrap();
    write_auth_file(
        &path,
        serde_json::json!({ "novita": { "type": "api", "key": "novita_test_secret" } }),
    );
    assert!(load_api_key_for_env("NOVITA_API_KEY").is_none());
    trust_external_auth_source(ExternalAuthSource::OpenCode).unwrap();
    assert_eq!(
        load_api_key_for_env("NOVITA_API_KEY").as_deref(),
        Some("novita_test_secret")
    );

    if let Some(prev) = prev {
        crate::env::set_var("HORON_HOME", prev);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
}

#[test]
fn opencode_api_key_imports_from_trusted_file() {
    let _guard = crate::storage::lock_test_env();
    let dir = TempDir::new().unwrap();
    let prev = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", dir.path());

    let path = ExternalAuthSource::OpenCode.path().unwrap();
    write_auth_file(
        &path,
        serde_json::json!({
            "opencode": { "type": "api", "key": "oc_test_secret" }
        }),
    );

    assert!(load_api_key_for_env("OPENCODE_API_KEY").is_none());
    trust_external_auth_source(ExternalAuthSource::OpenCode).unwrap();
    assert_eq!(
        load_api_key_for_env("OPENCODE_API_KEY").as_deref(),
        Some("oc_test_secret")
    );

    if let Some(prev) = prev {
        crate::env::set_var("HORON_HOME", prev);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
}

#[test]
fn pi_api_key_env_reference_uses_named_env_var() {
    let _guard = crate::storage::lock_test_env();
    let dir = TempDir::new().unwrap();
    let prev_home = std::env::var_os("HORON_HOME");
    let prev_key = std::env::var_os("PI_OPENAI_KEY");
    crate::env::set_var("HORON_HOME", dir.path());
    crate::env::set_var("PI_OPENAI_KEY", "sk-from-env-ref");

    let path = ExternalAuthSource::Pi.path().unwrap();
    write_auth_file(
        &path,
        serde_json::json!({
            "openai": { "type": "api_key", "key": "PI_OPENAI_KEY" }
        }),
    );

    trust_external_auth_source(ExternalAuthSource::Pi).unwrap();
    assert_eq!(
        load_api_key_for_env("OPENAI_API_KEY").as_deref(),
        Some("sk-from-env-ref")
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("HORON_HOME", prev_home);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
    if let Some(prev_key) = prev_key {
        crate::env::set_var("PI_OPENAI_KEY", prev_key);
    } else {
        crate::env::remove_var("PI_OPENAI_KEY");
    }
}

#[test]
fn pi_shell_command_api_keys_are_not_executed() {
    let _guard = crate::storage::lock_test_env();
    let dir = TempDir::new().unwrap();
    let prev = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", dir.path());

    let path = ExternalAuthSource::Pi.path().unwrap();
    write_auth_file(
        &path,
        serde_json::json!({
            "openai": { "type": "api_key", "key": "!security find-generic-password -ws openai" }
        }),
    );

    trust_external_auth_source(ExternalAuthSource::Pi).unwrap();
    assert!(load_api_key_for_env("OPENAI_API_KEY").is_none());

    if let Some(prev) = prev {
        crate::env::set_var("HORON_HOME", prev);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
}

#[test]
fn unconsented_source_detects_supported_api_key_files() {
    let _guard = crate::storage::lock_test_env();
    let dir = TempDir::new().unwrap();
    let prev = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", dir.path());

    let path = ExternalAuthSource::OpenCode.path().unwrap();
    write_auth_file(
        &path,
        serde_json::json!({
            "opencode": { "type": "api", "key": "oc_test_secret" }
        }),
    );

    assert_eq!(
        preferred_unconsented_api_key_source_for_env("OPENCODE_API_KEY"),
        Some(ExternalAuthSource::OpenCode)
    );

    if let Some(prev) = prev {
        crate::env::set_var("HORON_HOME", prev);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
}

#[test]
fn openclaw_api_key_imports_from_trusted_file() {
    let _guard = crate::storage::lock_test_env();
    let dir = TempDir::new().unwrap();
    let prev = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", dir.path());

    let path = ExternalAuthSource::OpenClaw.path().unwrap();
    write_auth_file(
        &path,
        serde_json::json!({
            "anthropic": { "type": "api_key", "key": "sk-ant-openclaw" }
        }),
    );

    assert!(load_api_key_for_env("ANTHROPIC_API_KEY").is_none());
    trust_external_auth_source(ExternalAuthSource::OpenClaw).unwrap();
    assert_eq!(
        load_api_key_for_env("ANTHROPIC_API_KEY").as_deref(),
        Some("sk-ant-openclaw")
    );

    if let Some(prev) = prev {
        crate::env::set_var("HORON_HOME", prev);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
}

#[test]
fn hermes_api_key_imports_from_credential_pool() {
    let _guard = crate::storage::lock_test_env();
    let dir = TempDir::new().unwrap();
    let prev = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", dir.path());

    let path = ExternalAuthSource::Hermes.path().unwrap();
    write_auth_file(
        &path,
        serde_json::json!({
            "version": 1,
            "active_provider": "anthropic",
            "credential_pool": {
                "anthropic": [
                    {
                        "id": "abc123",
                        "label": "manual",
                        "auth_type": "api_key",
                        "priority": 0,
                        "source": "manual:1",
                        "access_token": "sk-ant-hermes"
                    }
                ]
            }
        }),
    );

    assert!(load_api_key_for_env("ANTHROPIC_API_KEY").is_none());
    trust_external_auth_source(ExternalAuthSource::Hermes).unwrap();
    assert_eq!(
        load_api_key_for_env("ANTHROPIC_API_KEY").as_deref(),
        Some("sk-ant-hermes")
    );

    if let Some(prev) = prev {
        crate::env::set_var("HORON_HOME", prev);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
}

#[test]
fn openclaw_auth_profiles_store_resolves_and_flattens() {
    let _guard = crate::storage::lock_test_env();
    let dir = TempDir::new().unwrap();
    let prev = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", dir.path());

    // No legacy ~/.openclaw/agent/auth.json: the current per-agent
    // auth-profiles.json store must be discovered instead.
    let profiles_path = crate::storage::user_home_path(".openclaw/agents/main/agent/auth-profiles.json").unwrap();
    write_auth_file(
        &profiles_path,
        serde_json::json!({
            "version": 1,
            "profiles": {
                "openrouter:default": {
                    "type": "api_key",
                    "provider": "openrouter",
                    "key": "sk-or-openclaw"
                },
                "groq:work": {
                    "type": "api_key",
                    "provider": "groq",
                    "key": "gsk-openclaw-work"
                }
            }
        }),
    );

    assert_eq!(ExternalAuthSource::OpenClaw.path().unwrap(), profiles_path);
    trust_external_auth_source(ExternalAuthSource::OpenClaw).unwrap();

    assert_eq!(
        load_api_key_for_env("OPENROUTER_API_KEY").as_deref(),
        Some("sk-or-openclaw")
    );
    assert_eq!(
        load_api_key_for_env("GROQ_API_KEY").as_deref(),
        Some("gsk-openclaw-work")
    );

    if let Some(prev) = prev {
        crate::env::set_var("HORON_HOME", prev);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
}

#[test]
fn openclaw_legacy_flat_auth_json_still_wins_when_present() {
    let _guard = crate::storage::lock_test_env();
    let dir = TempDir::new().unwrap();
    let prev = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", dir.path());

    // Both layouts exist: the original pi-fork path must take precedence so
    // previously-recorded trust decisions stay bound to the same file.
    let legacy_path = crate::storage::user_home_path(".openclaw/agent/auth.json").unwrap();
    write_auth_file(
        &legacy_path,
        serde_json::json!({
            "anthropic": { "type": "api_key", "key": "sk-ant-legacy" }
        }),
    );
    let profiles_path = crate::storage::user_home_path(".openclaw/agents/main/agent/auth-profiles.json").unwrap();
    write_auth_file(&profiles_path, serde_json::json!({ "version": 1, "profiles": {} }));

    assert_eq!(ExternalAuthSource::OpenClaw.path().unwrap(), legacy_path);

    if let Some(prev) = prev {
        crate::env::set_var("HORON_HOME", prev);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
}
