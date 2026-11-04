use super::*;
use std::ffi::OsString;

fn restore_env_var(key: &str, previous: Option<OsString>) {
    if let Some(previous) = previous {
        crate::env::set_var(key, previous);
    } else {
        crate::env::remove_var(key);
    }
}

#[test]
fn command_candidates_adds_extension_on_windows() {
    crate::env::set_var("PATHEXT", ".EXE;.BAT");
    let candidates = command_candidates("testcmd");
    if cfg!(windows) {
        let normalized: Vec<String> = candidates
            .iter()
            .map(|c| c.to_string_lossy().to_ascii_lowercase())
            .collect();
        assert!(normalized.iter().any(|c| c == "testcmd"));
        assert!(normalized.iter().any(|c| c == "testcmd.exe"));
        assert!(normalized.iter().any(|c| c == "testcmd.bat"));
    } else {
        assert_eq!(candidates.len(), 1);
        assert!(candidates.iter().any(|c| c == "testcmd"));
    }
}

#[test]
fn auth_state_default_is_not_configured() {
    let state = AuthState::default();
    assert_eq!(state, AuthState::NotConfigured);
}

#[test]
fn auth_status_default_all_not_configured() {
    let status = AuthStatus::default();
    assert_eq!(status.openai, AuthState::NotConfigured);
    assert!(!status.openai_has_oauth);
    assert!(!status.openai_has_api_key);
}

#[test]
fn provider_auth_default() {
    let auth = ProviderAuth::default();
    assert_eq!(auth.state, AuthState::NotConfigured);
    assert!(!auth.has_oauth);
    assert!(!auth.has_api_key);
}

#[test]
fn provider_auth_assessment_predicates_reflect_state() {
    fn assessment_with_state(state: AuthState) -> ProviderAuthAssessment {
        ProviderAuthAssessment {
            state,
            readiness: AuthReadinessLevel::None,
            method_detail: "test".to_string(),
            credential_source: AuthCredentialSource::None,
            credential_source_detail: "not configured".to_string(),
            expiry_confidence: AuthExpiryConfidence::Unknown,
            refresh_support: AuthRefreshSupport::Unknown,
            validation_method: AuthValidationMethod::Unknown,
            last_validation: None,
            last_refresh: None,
        }
    }

    let not_configured = assessment_with_state(AuthState::NotConfigured);
    assert!(!not_configured.is_configured());
    assert!(!not_configured.is_available());

    let expired = assessment_with_state(AuthState::Expired);
    assert!(expired.is_configured());
    assert!(!expired.is_available());

    let available = assessment_with_state(AuthState::Available);
    assert!(available.is_configured());
    assert!(available.is_available());
}

#[test]
fn command_exists_for_known_binary() {
    if cfg!(windows) {
        assert!(command_exists("cmd") || command_exists("cmd.exe"));
    } else {
        assert!(command_exists("ls"));
    }
}

#[test]
fn command_exists_empty_string() {
    assert!(!command_exists(""));
    assert!(!command_exists("   "));
}

#[test]
fn command_exists_nonexistent() {
    assert!(!command_exists("surely_this_binary_does_not_exist_xyz"));
}

#[test]
fn command_exists_absolute_path() {
    if cfg!(windows) {
        assert!(command_exists(r"C:\Windows\System32\cmd.exe"));
    } else {
        assert!(command_exists("/bin/ls") || command_exists("/usr/bin/ls"));
    }
}

#[test]
fn command_exists_absolute_nonexistent() {
    assert!(!command_exists("/nonexistent/path/to/binary"));
}

#[test]
fn contains_path_separator_detection() {
    assert!(contains_path_separator("/usr/bin/test"));
    assert!(contains_path_separator("./test"));
    assert!(!contains_path_separator("test"));
}

#[test]
fn has_extension_detection() {
    assert!(has_extension(std::path::Path::new("test.exe")));
    assert!(!has_extension(std::path::Path::new("test")));
    assert!(has_extension(std::path::Path::new("test.sh")));
}

#[test]
fn dedup_preserves_order() {
    let input = vec![
        std::ffi::OsString::from("a"),
        std::ffi::OsString::from("b"),
        std::ffi::OsString::from("a"),
        std::ffi::OsString::from("c"),
    ];
    let result = dedup_preserve_order(input);
    assert_eq!(result.len(), 3);
    assert_eq!(result[0], "a");
    assert_eq!(result[1], "b");
    assert_eq!(result[2], "c");
}

#[test]
fn auth_state_equality() {
    assert_eq!(AuthState::Available, AuthState::Available);
    assert_eq!(AuthState::Expired, AuthState::Expired);
    assert_eq!(AuthState::NotConfigured, AuthState::NotConfigured);
    assert_ne!(AuthState::Available, AuthState::Expired);
    assert_ne!(AuthState::Available, AuthState::NotConfigured);
}

#[test]
fn is_wsl2_windows_path_matches_drive_mounts() {
    assert!(is_wsl2_windows_path(std::path::Path::new("/mnt/c")));
    assert!(is_wsl2_windows_path(std::path::Path::new("/mnt/d")));
    assert!(is_wsl2_windows_path(std::path::Path::new("/mnt/z")));
    assert!(is_wsl2_windows_path(std::path::Path::new("/mnt/c/Windows/System32")));
}

#[test]
fn is_wsl2_windows_path_rejects_non_drives() {
    // /mnt/wsl is a WSL-internal mount, not a Windows drive
    assert!(!is_wsl2_windows_path(std::path::Path::new("/mnt/wsl")));
    // /usr/bin is a plain Linux directory
    assert!(!is_wsl2_windows_path(std::path::Path::new("/usr/bin")));
    // /mnt alone is not a drive
    assert!(!is_wsl2_windows_path(std::path::Path::new("/mnt")));
    // empty
    assert!(!is_wsl2_windows_path(std::path::Path::new("")));
}

#[test]
fn command_exists_cached_on_second_call() {
    // Clear cache first to isolate this test
    if let Ok(mut cache) = COMMAND_EXISTS_CACHE.lock() {
        cache.remove("surely_this_binary_does_not_exist_xyz_cache_test");
    }
    // First call populates the cache
    let result1 = command_exists("surely_this_binary_does_not_exist_xyz_cache_test");
    assert!(!result1);
    // Second call must return same result (from cache)
    let result2 = command_exists("surely_this_binary_does_not_exist_xyz_cache_test");
    assert_eq!(result1, result2);
}

#[test]
fn auth_status_check_fast_ignores_expired_full_cache() {
    let _lock = crate::storage::lock_test_env();
    AuthStatus::invalidate_cache();

    let stale_status = AuthStatus {
        horon_agent: AuthState::Expired,
        ..Default::default()
    };
    let stale_when = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(AUTH_STATUS_CACHE_TTL_SECS + 1))
        .expect("stale cache timestamp");

    *AUTH_STATUS_CACHE.write().expect("auth cache lock") = Some((stale_status, stale_when, auth_cache_home_key()));
    *AUTH_STATUS_FAST_CACHE.write().expect("fast auth cache lock") = None;

    let status = AuthStatus::check_fast();
    assert_ne!(
        status.horon_agent,
        AuthState::Expired,
        "check_fast must not reuse an expired full auth cache forever"
    );

    AuthStatus::invalidate_cache();
}

#[test]
fn openrouter_like_status_is_provider_specific() {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("create temp dir");
    // Issue #1479: a horon-agent-launched shell with an active named provider
    // profile exports these, and the named-profile check short-circuits the
    // per-provider key lookup. Clear them so the test sees only its own env.
    let keys = [
        "HORON_AGENT_HOME",
        "CHUTES_API_KEY",
        "OPENCODE_API_KEY",
        "HORON_AGENT_NAMED_PROVIDER_PROFILE",
        "HORON_AGENT_OPENROUTER_ALLOW_NO_AUTH",
        "HORON_AGENT_OPENROUTER_API_KEY_NAME",
        "HORON_AGENT_OPENROUTER_ENV_FILE",
        "HORON_AGENT_OPENROUTER_API_BASE",
    ];
    let saved = keys
        .into_iter()
        .map(|key| (key, std::env::var_os(key)))
        .collect::<Vec<_>>();
    for key in keys {
        crate::env::remove_var(key);
    }

    crate::env::set_var("HORON_AGENT_HOME", temp.path());
    crate::env::set_var("CHUTES_API_KEY", "chutes-test-key");
    AuthStatus::invalidate_cache();

    let status = AuthStatus::check_fast();
    let chutes_assessment = status.assessment_for_provider(crate::provider_catalog::CHUTES_LOGIN_PROVIDER);
    let opencode_assessment = status.assessment_for_provider(crate::provider_catalog::OPENCODE_LOGIN_PROVIDER);
    assert!(chutes_assessment.is_available());
    assert_eq!(opencode_assessment.state, AuthState::NotConfigured);
    assert_eq!(
        chutes_assessment.method_detail,
        "API key (`CHUTES_API_KEY`)".to_string()
    );

    for (key, value) in saved {
        restore_env_var(key, value);
    }
    AuthStatus::invalidate_cache();
}

#[test]
fn azure_readiness_distinguishes_credentials_from_deployment_validation() {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("create temp dir");
    let saved = [
        "HORON_AGENT_HOME",
        crate::auth::azure::ENDPOINT_ENV,
        crate::auth::azure::API_KEY_ENV,
        crate::auth::azure::MODEL_ENV,
        crate::auth::azure::USE_ENTRA_ENV,
    ]
    .into_iter()
    .map(|key| (key, std::env::var_os(key)))
    .collect::<Vec<_>>();

    crate::env::set_var("HORON_AGENT_HOME", temp.path());
    crate::env::set_var(crate::auth::azure::ENDPOINT_ENV, "https://example.openai.azure.com");
    crate::env::set_var(crate::auth::azure::API_KEY_ENV, "azure-test-key");
    crate::env::set_var(crate::auth::azure::MODEL_ENV, "gpt-test-deployment");
    crate::env::remove_var(crate::auth::azure::USE_ENTRA_ENV);
    AuthStatus::invalidate_cache();

    let status = AuthStatus::check_fast();
    let assessment = status.assessment_for_provider(crate::provider_catalog::AZURE_LOGIN_PROVIDER);
    assert_eq!(assessment.state, AuthState::Available);
    assert_eq!(assessment.readiness, AuthReadinessLevel::CredentialPresent);
    assert!(assessment.health_summary().contains("readiness: credential present"));

    crate::auth::validation::save(
        "azure",
        crate::auth::validation::ProviderValidationRecord {
            checked_at_ms: chrono::Utc::now().timestamp_millis(),
            success: false,
            provider_smoke_ok: Some(false),
            tool_smoke_ok: None,
            summary: "provider_smoke: deployment not found".to_string(),
        },
    )
    .expect("save failed validation");
    let assessment = status.assessment_for_provider(crate::provider_catalog::AZURE_LOGIN_PROVIDER);
    assert_eq!(assessment.readiness, AuthReadinessLevel::CredentialPresent);

    crate::auth::validation::save(
        "azure",
        crate::auth::validation::ProviderValidationRecord {
            checked_at_ms: chrono::Utc::now().timestamp_millis(),
            success: true,
            provider_smoke_ok: Some(true),
            tool_smoke_ok: None,
            summary: "provider_smoke: ok".to_string(),
        },
    )
    .expect("save successful validation");
    let assessment = status.assessment_for_provider(crate::provider_catalog::AZURE_LOGIN_PROVIDER);
    assert_eq!(assessment.readiness, AuthReadinessLevel::DeploymentValid);
    assert!(assessment.health_summary().contains("readiness: deployment valid"));

    for (key, value) in saved {
        restore_env_var(key, value);
    }
    AuthStatus::invalidate_cache();
}

#[test]
fn configured_api_key_source_uses_valid_overrides() {
    let _lock = crate::storage::lock_test_env();
    let key_var = "HORON_AGENT_OPENAI_COMPAT_API_KEY_NAME";
    let file_var = "HORON_AGENT_OPENAI_COMPAT_ENV_FILE";
    let prev_key = std::env::var(key_var).ok();
    let prev_file = std::env::var(file_var).ok();

    crate::env::set_var(key_var, "GROQ_API_KEY");
    crate::env::set_var(file_var, "groq.env");

    let source =
        crate::provider_catalog::configured_api_key_source(key_var, file_var, "OPENAI_COMPAT_API_KEY", "compat.env");
    assert_eq!(source, Some(("GROQ_API_KEY".to_string(), "groq.env".to_string())));

    if let Some(v) = prev_key {
        crate::env::set_var(key_var, v);
    } else {
        crate::env::remove_var(key_var);
    }
    if let Some(v) = prev_file {
        crate::env::set_var(file_var, v);
    } else {
        crate::env::remove_var(file_var);
    }
}

#[test]
fn configured_api_key_source_rejects_invalid_values() {
    let _lock = crate::storage::lock_test_env();
    let key_var = "HORON_AGENT_OPENAI_COMPAT_API_KEY_NAME";
    let file_var = "HORON_AGENT_OPENAI_COMPAT_ENV_FILE";
    let prev_key = std::env::var(key_var).ok();
    let prev_file = std::env::var(file_var).ok();

    crate::env::set_var(key_var, "bad-key");
    crate::env::set_var(file_var, "../bad.env");

    let source =
        crate::provider_catalog::configured_api_key_source(key_var, file_var, "OPENAI_COMPAT_API_KEY", "compat.env");
    assert!(source.is_none());

    if let Some(v) = prev_key {
        crate::env::set_var(key_var, v);
    } else {
        crate::env::remove_var(key_var);
    }
    if let Some(v) = prev_file {
        crate::env::set_var(file_var, v);
    } else {
        crate::env::remove_var(file_var);
    }
}

/// Test binaries must never open real browser windows: login/onboarding flows
/// are exercised heavily by unit tests, and each ungated `open::that` pops an
/// OAuth page on the developer's desktop. `running_in_test_harness` detects
/// the Cargo `deps/` test-binary path, and `browser_suppressed` must honor
/// it even without --no-browser or NO_BROWSER/HORON_AGENT_NO_BROWSER.
#[test]
fn browser_suppressed_inside_test_harness_without_env_overrides() {
    assert!(
        super::running_in_test_harness(),
        "test binary should be detected even with a custom Cargo target directory"
    );
    assert!(
        super::browser_suppressed(false),
        "browser opens must be suppressed in test binaries even without --no-browser/env vars"
    );
}

#[test]
fn test_harness_paths_include_custom_cargo_target_directories() {
    for path in [
        "/work/target/debug/deps/test_binary-0123456789abcdef",
        "/work/target/debug/deps/custom_runner",
        "/build-cache/debug/deps/test_binary-0123456789abcdef",
        "/build-cache/aarch64-apple-darwin/release/deps/test_binary-0123456789abcdef",
        r"C:\build-cache\debug\deps\test_binary-0123456789abcdef.exe",
    ] {
        assert!(super::is_test_harness_path(path), "missed test binary: {path}");
    }
    for path in [
        "/usr/local/bin/horon-agent",
        "/work/target/debug/horon-agent",
        "/build-cache/debug/deps/horon-agent",
        "/build-cache/debug/deps/libhoron-agent-0123456789abcdef.rlib",
        "/build-cache/debug/deps/test_binary-not_a_cargo_hash",
    ] {
        assert!(!super::is_test_harness_path(path), "not a test binary: {path}");
    }
}
