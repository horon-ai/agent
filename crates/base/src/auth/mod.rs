pub mod account_pool;
pub mod account_store;
pub mod active_method;
pub mod azure;
pub mod codex;
mod commands;
pub mod doctor;
pub mod env_facts;
pub mod external;
pub mod integration;
pub mod lifecycle;
pub mod login_diagnostics;
pub mod login_ui;
pub mod oauth;
pub(crate) mod refresh_coordinator;
pub mod refresh_state;
mod status_types;
#[cfg(any(test, feature = "test-support"))]
pub mod test_sandbox;
pub mod transfer;
pub mod validation;

pub(crate) use commands::command_exists;
#[cfg(test)]
pub(crate) use commands::{
    command_candidates, contains_path_separator, dedup_preserve_order, has_extension, is_wsl2_windows_path,
};

pub use status_types::{
    AuthCredentialSource, AuthExpiryConfidence, AuthReadinessLevel, AuthRefreshSupport, AuthState, AuthStatus,
    AuthValidationMethod, ProviderAuth, ProviderAuthAssessment,
};

pub use active_method::{ActiveCredential, ResolvedProviderAuth, resolve_dual_credential_auth};

use crate::provider_catalog::LoginProviderAuthStateKey;
use crate::provider_catalog::LoginProviderDescriptor;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, RwLock};
use std::time::Instant;

/// Cached auth status plus the `HORON_AGENT_HOME` it was computed under.
///
/// Auth probes read credential files relative to `HORON_AGENT_HOME`. Tests swap
/// `HORON_AGENT_HOME` to per-test temp dirs, and a status computed under one home
/// must never be served for another (issue #361: parallel provider tests
/// intermittently observed another test's auth snapshot through this global
/// cache). In production the home never changes, so the key check is free.
type CachedAuthStatus = (AuthStatus, Instant, Option<std::ffi::OsString>);

static AUTH_STATUS_CACHE: std::sync::LazyLock<RwLock<Option<CachedAuthStatus>>> =
    std::sync::LazyLock::new(|| RwLock::new(None));
static AUTH_STATUS_FAST_CACHE: std::sync::LazyLock<RwLock<Option<CachedAuthStatus>>> =
    std::sync::LazyLock::new(|| RwLock::new(None));

fn auth_cache_home_key() -> Option<std::ffi::OsString> {
    std::env::var_os("HORON_AGENT_HOME")
}

const AUTH_STATUS_CACHE_TTL_SECS: u64 = 30;
const AUTH_STATUS_FAST_CACHE_TTL_SECS: u64 = 60;

/// Bumped whenever the cached auth status is invalidated.
///
/// Lets downstream caches (notably the TUI's prepared-header cache) key on
/// "have credentials changed" without re-running the expensive probes
/// themselves, so a credential change repaints immediately instead of waiting
/// out an unrelated TTL.
static AUTH_STATUS_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Current auth-status generation; see [`AUTH_STATUS_GENERATION`].
pub fn auth_status_generation() -> u64 {
    AUTH_STATUS_GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

/// Bump the auth generation without clearing the cached status.
///
/// Tests that only need to observe generation-driven invalidation use this so
/// they do not evict the process-global auth cache that sibling tests in the
/// same binary rely on.
pub fn bump_auth_status_generation_for_tests() {
    AUTH_STATUS_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Guards the background refresh spawned by
/// [`AuthStatus::check_fast_nonblocking`] so an expired snapshot triggers one
/// probe thread, not one per frame.
static AUTH_REFRESH_IN_FLIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Per-process cache for command existence lookups.
/// CLI tools don't get installed/uninstalled while horon-agent is running, so caching
/// indefinitely per process is correct and avoids repeated PATH scans.
static COMMAND_EXISTS_CACHE: std::sync::LazyLock<Mutex<HashMap<String, bool>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthProbeMode {
    Full,
    Fast,
}

pub fn browser_suppressed(cli_no_browser: bool) -> bool {
    cli_no_browser
        || env_truthy("NO_BROWSER")
        || env_truthy("HORON_AGENT_NO_BROWSER")
        || running_in_test_harness()
        || browser_unusable_here()
}

/// True when the probed environment says a browser launch cannot work.
///
/// Without this, horon-agent would open a browser that does not exist (or that opens
/// on the wrong machine, over SSH) and then wait out a callback timeout before
/// telling the user anything. Probing first turns a 60-second dead end into an
/// immediate fallback to a paste/device flow.
///
/// Deliberately conservative: only a *positive* determination suppresses the
/// browser, so an inconclusive probe never downgrades a working setup.
fn browser_unusable_here() -> bool {
    use crate::auth::env_facts::{AuthMethodChoice, EnvFacts};
    static CHOICE: std::sync::OnceLock<AuthMethodChoice> = std::sync::OnceLock::new();
    matches!(
        CHOICE.get_or_init(|| EnvFacts::probe().preferred_auth_method()),
        AuthMethodChoice::DeviceCode | AuthMethodChoice::ApiKeyNonInteractive
    )
}

/// True when the current process is a Rust test binary (`cargo test` /
/// `cargo nextest`). Cargo places hashed test executables in `deps/` beneath
/// its build directory, which can be relocated with `CARGO_TARGET_DIR`.
///
/// Used to keep tests from opening real browser windows (OAuth login pages,
/// files) on the developer's desktop: many login/onboarding flows are
/// exercised by TUI tests, and without this guard each test run could pop
/// multiple browser tabs. Set `HORON_AGENT_ALLOW_BROWSER_IN_TESTS=1` to opt out
/// (e.g. for an intentionally interactive live test).
pub fn running_in_test_harness() -> bool {
    static IN_TEST_HARNESS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IN_TEST_HARNESS.get_or_init(|| {
        if env_truthy("HORON_AGENT_ALLOW_BROWSER_IN_TESTS") {
            return false;
        }
        std::env::current_exe()
            .ok()
            .map(|exe| is_test_harness_path(&exe.to_string_lossy()))
            .unwrap_or(false)
    })
}

fn is_test_harness_path(path: &str) -> bool {
    let path = path.replace('\\', "/");
    // Preserve detection for the default target tree, including custom runners.
    if path.contains("/target/") && path.contains("/deps/") {
        return true;
    }
    let mut components = path.rsplit('/');
    let Some(filename) = components.next() else {
        return false;
    };
    if components.next() != Some("deps") {
        return false;
    }
    let stem = filename.strip_suffix(".exe").unwrap_or(filename);
    let Some((name, hash)) = stem.rsplit_once('-') else {
        return false;
    };
    !name.is_empty() && hash.len() == 16 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn env_truthy(key: &str) -> bool {
    std::env::var(key)
        .ok()
        .map(|value| {
            let trimmed = value.trim();
            !trimmed.is_empty() && trimmed != "0" && !trimmed.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

fn auth_timing_logging_enabled() -> bool {
    env_truthy("HORON_AGENT_AUTH_TIMING")
}

fn openai_api_key_configured() -> bool {
    crate::provider_catalog::load_api_key_from_env_or_config("OPENAI_API_KEY", "openai.env")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

fn auth_state_label(state: AuthState) -> &'static str {
    match state {
        AuthState::Available => "available",
        AuthState::Expired => "expired",
        AuthState::NotConfigured => "not_configured",
    }
}

fn bool_label(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn log_auth_status_snapshot(event: &str, status: &AuthStatus) {
    crate::logging::auth_event(
        event,
        "all",
        &[
            ("horon-agent", auth_state_label(status.horon_agent)),
            ("openai", auth_state_label(status.openai)),
            ("azure", auth_state_label(status.azure)),
            ("azure_api_auth", bool_label(status.azure_has_api_key)),
            ("azure_entra", bool_label(status.azure_uses_entra)),
            ("openai_compatible", auth_state_label(status.openai_compatible_any)),
        ],
    );
}

fn auth_readiness_for_provider(
    provider: LoginProviderDescriptor,
    state: AuthState,
    last_validation: Option<&crate::auth::validation::ProviderValidationRecord>,
) -> AuthReadinessLevel {
    match state {
        AuthState::NotConfigured => AuthReadinessLevel::None,
        AuthState::Expired => AuthReadinessLevel::CredentialPresent,
        AuthState::Available => {
            if last_validation.and_then(|record| record.provider_smoke_ok) == Some(true) {
                return model_smoke_readiness_for_provider(provider);
            }

            available_provider_base_readiness(provider)
        }
    }
}

fn available_provider_base_readiness(_provider: LoginProviderDescriptor) -> AuthReadinessLevel {
    AuthReadinessLevel::CredentialPresent
}

fn model_smoke_readiness_for_provider(provider: LoginProviderDescriptor) -> AuthReadinessLevel {
    match provider.target {
        // Azure model names are deployment IDs. A successful smoke call proves the
        // resource, auth, and selected deployment all work together.
        crate::provider_catalog::LoginProviderTarget::Azure => AuthReadinessLevel::DeploymentValid,
        _ => AuthReadinessLevel::RequestValid,
    }
}

impl AuthStatus {
    /// Check all authentication sources and return their status.
    /// Results are cached for 30 seconds to avoid expensive PATH scanning on every frame.
    pub fn check() -> Self {
        let home_key = auth_cache_home_key();
        if let Ok(cache) = AUTH_STATUS_CACHE.read()
            && let Some((ref status, ref when, ref cached_home)) = *cache
            && when.elapsed().as_secs() < AUTH_STATUS_CACHE_TTL_SECS
            && *cached_home == home_key
        {
            return status.clone();
        }

        let status = Self::check_uncached();

        if let Ok(mut cache) = AUTH_STATUS_CACHE.write() {
            *cache = Some((status.clone(), Instant::now(), home_key.clone()));
        }
        if let Ok(mut cache) = AUTH_STATUS_FAST_CACHE.write() {
            *cache = Some((status.clone(), Instant::now(), home_key));
        }

        status
    }

    /// Fast auth snapshot for interactive UI surfaces like `/account`.
    ///
    /// Prefers a recent full probe, and otherwise falls back to a cheap
    /// local-files/env-only probe that avoids subprocesses such as
    /// `cursor-agent status` or `sqlite3` lookups. Do not reuse the full cache
    /// forever: external credential files may be deleted or replaced while the
    /// process is running.
    pub fn check_fast() -> Self {
        let home_key = auth_cache_home_key();
        if let Ok(cache) = AUTH_STATUS_CACHE.read()
            && let Some((ref status, ref when, ref cached_home)) = *cache
            && when.elapsed().as_secs() < AUTH_STATUS_CACHE_TTL_SECS
            && *cached_home == home_key
        {
            return status.clone();
        }

        if let Ok(cache) = AUTH_STATUS_FAST_CACHE.read()
            && let Some((ref status, ref when, ref cached_home)) = *cache
            && when.elapsed().as_secs() < AUTH_STATUS_FAST_CACHE_TTL_SECS
            && *cached_home == home_key
        {
            return status.clone();
        }

        // Single-flight: a startup prewarm (see `prewarm_fast_in_background`)
        // may already be probing. Wait for it and reuse its snapshot instead of
        // running a second identical probe in parallel on the frame thread.
        static PROBE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _probe_guard = PROBE_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Ok(cache) = AUTH_STATUS_FAST_CACHE.read()
            && let Some((ref status, ref when, ref cached_home)) = *cache
            && when.elapsed().as_secs() < AUTH_STATUS_FAST_CACHE_TTL_SECS
            && *cached_home == home_key
        {
            return status.clone();
        }

        let status = Self::check_uncached_fast();
        if let Ok(mut cache) = AUTH_STATUS_FAST_CACHE.write() {
            *cache = Some((status.clone(), Instant::now(), home_key));
        }

        status
    }

    /// Start the fast auth probe on a background thread so the first TUI
    /// frame finds a warm snapshot instead of paying ~5-10ms of credential
    /// file reads inline. Safe to call any time: [`Self::check_fast`] is
    /// single-flight, so a caller that races the prewarm waits for it rather
    /// than probing twice.
    pub fn prewarm_fast_in_background() {
        if running_in_test_harness() {
            return;
        }
        let _ = std::thread::Builder::new().name("auth-prewarm".into()).spawn(|| {
            let _ = Self::check_fast();
        });
    }

    /// Non-blocking auth snapshot for per-frame render paths (the TUI header).
    ///
    /// [`Self::check_fast`] blocks on a cold probe (~20-30ms of credential-file
    /// reads on this hardware), and the TUI's prepared-header cache re-probes
    /// every TTL lapse - directly on the render thread, where it showed up as a
    /// periodic 40-55ms frame while typing. This variant never runs a probe on
    /// the caller's thread once a snapshot exists: it serves the freshest
    /// cached snapshot (even if expired) and refreshes the cache on a detached
    /// background thread, bumping the auth generation when the refreshed
    /// snapshot actually differs so signature-keyed caches repaint.
    ///
    /// The only blocking case is the very first call in a process with no
    /// cached snapshot at all, which matches the old behavior for frame one.
    /// Tests always take the blocking path: background refreshes racing
    /// per-test `HORON_AGENT_HOME` swaps would poison the shared cache.
    pub fn check_fast_nonblocking() -> Self {
        if running_in_test_harness() {
            return Self::check_fast();
        }

        let home_key = auth_cache_home_key();
        if let Ok(cache) = AUTH_STATUS_CACHE.read()
            && let Some((ref status, ref when, ref cached_home)) = *cache
            && when.elapsed().as_secs() < AUTH_STATUS_CACHE_TTL_SECS
            && *cached_home == home_key
        {
            return status.clone();
        }

        let stale = AUTH_STATUS_FAST_CACHE
            .read()
            .ok()
            .and_then(|cache| cache.clone())
            .filter(|(_, _, cached_home)| *cached_home == home_key);

        let Some((status, when, _)) = stale else {
            // First probe of the process: nothing to serve yet.
            return Self::check_fast();
        };

        if when.elapsed().as_secs() >= AUTH_STATUS_FAST_CACHE_TTL_SECS
            && !AUTH_REFRESH_IN_FLIGHT.swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            let previous = status.clone();
            std::thread::Builder::new()
                .name("auth-refresh".into())
                .spawn(move || {
                    let refreshed = Self::check_uncached_fast();
                    let changed = format!("{previous:?}") != format!("{refreshed:?}");
                    if let Ok(mut cache) = AUTH_STATUS_FAST_CACHE.write() {
                        *cache = Some((refreshed, Instant::now(), auth_cache_home_key()));
                    }
                    AUTH_REFRESH_IN_FLIGHT.store(false, std::sync::atomic::Ordering::Release);
                    if changed {
                        AUTH_STATUS_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                })
                .map(drop)
                .unwrap_or_else(|_| AUTH_REFRESH_IN_FLIGHT.store(false, std::sync::atomic::Ordering::Release));
        }

        status
    }

    /// Returns true if at least one provider has usable credentials.
    pub fn has_any_available(&self) -> bool {
        self.horon_agent == AuthState::Available
            || self.openai == AuthState::Available
            || self.azure == AuthState::Available
            || self.openai_compatible_any == AuthState::Available
    }

    /// Emit a structured, non-secret snapshot of which providers currently have
    /// credentials configured. This is the single best line to ask a user to
    /// share when debugging "my model picker is empty / only OpenAI+Anthropic
    /// show / login silently failed" reports: it records, per provider, whether
    /// horon-agent believes credentials are available/expired/missing without leaking
    /// any token or key material.
    ///
    /// `surface` describes where the snapshot was taken from (for example
    /// `model_picker`, `auth_changed`, `catalog_refresh`) so logs can be
    /// correlated with the user action that triggered them.
    pub fn log_snapshot(&self, surface: &str) {
        crate::logging::event_info(
            "auth_status_snapshot",
            vec![
                ("surface", surface.to_string()),
                ("any_available", self.has_any_available().to_string()),
                ("horon-agent", self.horon_agent.label().to_string()),
                ("openai", self.openai.label().to_string()),
                ("openai_oauth", self.openai_has_oauth.to_string()),
                ("openai_api", self.openai_has_api_key.to_string()),
                ("azure", self.azure.label().to_string()),
                ("azure_api", self.azure_has_api_key.to_string()),
                ("azure_entra", self.azure_uses_entra.to_string()),
                ("openai_compatible", self.openai_compatible_any.label().to_string()),
            ],
        );
    }

    pub fn has_any_untrusted_external_auth() -> bool {
        crate::auth::codex::has_unconsented_legacy_credentials()
            || crate::auth::external::has_any_unconsented_external_auth()
    }

    pub fn state_for_key(&self, key: LoginProviderAuthStateKey) -> AuthState {
        match key {
            LoginProviderAuthStateKey::ExternalImport => {
                if Self::has_any_untrusted_external_auth() {
                    AuthState::Available
                } else {
                    AuthState::NotConfigured
                }
            }
            LoginProviderAuthStateKey::HoronAgent => self.horon_agent,

            LoginProviderAuthStateKey::OpenAi => self.openai,
            LoginProviderAuthStateKey::Azure => self.azure,
            LoginProviderAuthStateKey::OpenRouterLike => self.openai_compatible_any,
            // Backends for these login providers were removed from horon-agent,
            // so no credential can make them available.
            LoginProviderAuthStateKey::Anthropic
            | LoginProviderAuthStateKey::Bedrock
            | LoginProviderAuthStateKey::Copilot
            | LoginProviderAuthStateKey::Gemini
            | LoginProviderAuthStateKey::Antigravity
            | LoginProviderAuthStateKey::Cursor
            | LoginProviderAuthStateKey::GrokBuild => AuthState::NotConfigured,
        }
    }

    pub fn state_for_provider(&self, provider: LoginProviderDescriptor) -> AuthState {
        match provider.target {
            crate::provider_catalog::LoginProviderTarget::AutoImport => {
                if Self::has_any_untrusted_external_auth() {
                    AuthState::Available
                } else {
                    AuthState::NotConfigured
                }
            }
            crate::provider_catalog::LoginProviderTarget::HoronAgent => {
                if crate::subscription_catalog::has_credentials() {
                    AuthState::Available
                } else {
                    AuthState::NotConfigured
                }
            }
            crate::provider_catalog::LoginProviderTarget::ClaudeApiKey => {
                if api_key_available("ANTHROPIC_API_KEY", "anthropic.env") {
                    AuthState::Available
                } else {
                    AuthState::NotConfigured
                }
            }
            // The `claude` login provider is the *OAuth/subscription* path: the
            // mirror image of the `anthropic-api` rule above. It must report on
            // the OAuth credential alone, never borrow the API key's
            // availability, so the two rows never blur into one ambiguous
            // "OAuth + API key" answer.
            // Same split for OpenAI: `openai` is the ChatGPT/Codex OAuth login,
            // `openai-api` (handled above) is the API-key login.
            crate::provider_catalog::LoginProviderTarget::OpenAi => self.openai_oauth_state,
            crate::provider_catalog::LoginProviderTarget::OpenAiCompatible(profile) => {
                if crate::provider_catalog::openai_compatible_profile_is_configured(profile) {
                    AuthState::Available
                } else {
                    AuthState::NotConfigured
                }
            }
            _ => self.state_for_key(provider.auth_state_key),
        }
    }

    pub fn method_detail_for_provider(&self, provider: LoginProviderDescriptor) -> String {
        match provider.target {
            crate::provider_catalog::LoginProviderTarget::AutoImport => {
                if Self::has_any_untrusted_external_auth() {
                    "Existing external logins detected".to_string()
                } else {
                    "No importable external logins found".to_string()
                }
            }
            crate::provider_catalog::LoginProviderTarget::HoronAgent => {
                if self.state_for_provider(provider) == AuthState::Available {
                    if crate::subscription_catalog::has_router_base() {
                        format!(
                            "API key (`{}`) + router base",
                            crate::subscription_catalog::HORON_AGENT_API_KEY_ENV
                        )
                    } else {
                        format!(
                            "API key (`{}`), router base pending",
                            crate::subscription_catalog::HORON_AGENT_API_KEY_ENV
                        )
                    }
                } else {
                    "not configured".to_string()
                }
            }
            crate::provider_catalog::LoginProviderTarget::OpenAiCompatible(profile) => {
                let resolved = crate::provider_catalog::resolve_openai_compatible_profile(profile);
                if self.state_for_provider(provider) == AuthState::Available {
                    if resolved.requires_api_key {
                        format!("API key (`{}`)", resolved.api_key_env)
                    } else if crate::provider_catalog::load_api_key_from_env_or_config(
                        &resolved.api_key_env,
                        &resolved.env_file,
                    )
                    .is_some()
                    {
                        format!(
                            "local endpoint (`{}`) + optional API key (`{}`)",
                            resolved.api_base, resolved.api_key_env
                        )
                    } else {
                        format!("local endpoint (`{}`)", resolved.api_base)
                    }
                } else {
                    "not configured".to_string()
                }
            }
            _ => provider.auth_status_method.to_string(),
        }
    }

    pub fn assessment_for_provider(&self, provider: LoginProviderDescriptor) -> ProviderAuthAssessment {
        let state = self.state_for_provider(provider);
        let method_detail = self.method_detail_for_provider(provider);
        let last_validation = crate::auth::validation::get(provider.id);
        let last_refresh = crate::auth::refresh_state::get(provider.id);

        let (credential_source, credential_source_detail, expiry_confidence, refresh_support, validation_method) =
            match provider.target {
                crate::provider_catalog::LoginProviderTarget::AutoImport => (
                    if Self::has_any_untrusted_external_auth() {
                        AuthCredentialSource::TrustedExternalFile
                    } else {
                        AuthCredentialSource::None
                    },
                    if Self::has_any_untrusted_external_auth() {
                        "untrusted external auth sources detected".to_string()
                    } else {
                        "none detected".to_string()
                    },
                    AuthExpiryConfidence::Unknown,
                    AuthRefreshSupport::ExternalManaged,
                    AuthValidationMethod::TrustedImportScan,
                ),
                crate::provider_catalog::LoginProviderTarget::HoronAgent => {
                    let (source, detail) = summarize_sources(vec![
                        env_source(crate::subscription_catalog::HORON_AGENT_API_KEY_ENV),
                        config_source(
                            crate::subscription_catalog::HORON_AGENT_API_KEY_ENV,
                            crate::subscription_catalog::HORON_AGENT_ENV_FILE,
                            "~/.config/horon-agent/horon-agent-subscription.env",
                        ),
                    ]);
                    (
                        source,
                        detail,
                        AuthExpiryConfidence::NotApplicable,
                        AuthRefreshSupport::NotApplicable,
                        AuthValidationMethod::PresenceCheck,
                    )
                }
                crate::provider_catalog::LoginProviderTarget::OpenAiCompatible(profile) => {
                    // Prefer the active named config profile's credential location
                    // (set via `--provider-profile`) over the built-in profile env
                    // so the reported source matches what runtime actually uses (#402).
                    let (source, detail) = if let Some((key_env, env_file)) =
                        crate::provider_catalog::active_named_provider_profile_credential_source()
                    {
                        summarize_sources(vec![
                            env_source(&key_env),
                            config_source(&key_env, &env_file, format!("~/.config/horon-agent/{}", env_file)),
                            external_api_key_source(&key_env),
                        ])
                    } else {
                        let resolved = crate::provider_catalog::resolve_openai_compatible_profile(profile);
                        summarize_sources(vec![
                            env_source(&resolved.api_key_env),
                            config_source(
                                &resolved.api_key_env,
                                &resolved.env_file,
                                format!("~/.config/horon-agent/{}", resolved.env_file),
                            ),
                            external_api_key_source(&resolved.api_key_env),
                        ])
                    };
                    (
                        source,
                        detail,
                        AuthExpiryConfidence::NotApplicable,
                        AuthRefreshSupport::NotApplicable,
                        AuthValidationMethod::PresenceCheck,
                    )
                }
                _ => assessment_for_key(self, provider.auth_state_key, state),
            };

        ProviderAuthAssessment {
            state,
            readiness: auth_readiness_for_provider(provider, state, last_validation.as_ref()),
            method_detail,
            credential_source,
            credential_source_detail,
            expiry_confidence,
            refresh_support,
            validation_method,
            last_validation,
            last_refresh,
        }
    }

    /// Clear only the cached auth snapshots.
    ///
    /// Use this when changing an in-memory credential selection on an already
    /// configured provider. It deliberately does not bump the process-wide auth
    /// generation, because provider forks restore that selection frequently and
    /// must not invalidate every session's model-route memo.
    pub fn invalidate_cached_status() {
        if let Ok(mut cache) = AUTH_STATUS_CACHE.write() {
            *cache = None;
        }
        if let Ok(mut cache) = AUTH_STATUS_FAST_CACHE.write() {
            *cache = None;
        }
        AUTH_STATUS_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Invalidate all auth-derived state after credentials actually change.
    pub fn invalidate_cache() {
        Self::invalidate_cached_status();
        crate::provider::pricing::invalidate_auth_pricing_memos();
        crate::memory_rerank::clear_failure_backoff();
        crate::logging::auth_event("auth_status_cache_invalidated", "all", &[]);
    }

    fn check_uncached() -> Self {
        let (status, _) = build_auth_status_uncached(AuthProbeMode::Full);
        log_auth_status_snapshot("auth_status_check", &status);
        status
    }

    fn check_uncached_fast() -> Self {
        let total_start = Instant::now();
        let (status, timings) = build_auth_status_uncached(AuthProbeMode::Fast);

        let nonzero: Vec<String> = timings
            .iter()
            .filter(|(_, ms)| *ms > 0)
            .map(|(name, ms)| format!("{name}={ms}ms"))
            .collect();
        if auth_timing_logging_enabled() {
            crate::logging::info(&format!(
                "[TIMING] auth_check_fast: total={}ms, nonzero=[{}]",
                total_start.elapsed().as_millis(),
                nonzero.join(", ")
            ));
        }

        log_auth_status_snapshot("auth_status_check_fast", &status);
        status
    }
}

fn build_auth_status_uncached(_mode: AuthProbeMode) -> (AuthStatus, Vec<(&'static str, u128)>) {
    let mut status = AuthStatus::default();
    let mut timings = Vec::new();

    record_auth_probe_step(&mut timings, "horon-agent", || probe_horon_agent_status(&mut status));
    record_auth_probe_step(&mut timings, "azure", || probe_azure_status(&mut status));
    record_auth_probe_step(&mut timings, "openai", || probe_openai_status(&mut status));
    record_auth_probe_step(&mut timings, "openai_compatible", || {
        let configured = crate::provider_catalog::openai_compatible_profiles()
            .iter()
            .copied()
            .any(crate::provider_catalog::openai_compatible_profile_is_configured);
        status.openai_compatible_any = if configured {
            AuthState::Available
        } else {
            AuthState::NotConfigured
        };
    });

    (status, timings)
}

fn record_auth_probe_step(timings: &mut Vec<(&'static str, u128)>, name: &'static str, probe: impl FnOnce()) {
    let step_start = Instant::now();
    probe();
    timings.push((name, step_start.elapsed().as_millis()));
}

fn probe_horon_agent_status(status: &mut AuthStatus) {
    if crate::subscription_catalog::has_credentials() {
        status.horon_agent = AuthState::Available;
    }
}

fn probe_azure_status(status: &mut AuthStatus) {
    status.azure_has_api_key = crate::auth::azure::has_api_key();
    status.azure_uses_entra = crate::auth::azure::uses_entra_id();
    if crate::auth::azure::has_configuration() {
        status.azure = AuthState::Available;
    }
}

fn probe_openai_status(status: &mut AuthStatus) {
    if let Ok(creds) = codex::load_credentials() {
        if !creds.refresh_token.is_empty() {
            status.openai_has_oauth = true;
            if let Some(expires_at) = creds.expires_at {
                let now_ms = chrono::Utc::now().timestamp_millis();
                if expires_at > now_ms {
                    status.openai = AuthState::Available;
                } else {
                    status.openai = AuthState::Expired;
                }
            } else {
                // No expiry info, assume available.
                status.openai = AuthState::Available;
            }
            // Record the OAuth credential's own state before the API key below
            // can mask an expired (or absent) OAuth login in the combined state.
            status.openai_oauth_state = status.openai;
        } else if !creds.access_token.is_empty() {
            status.openai_has_api_key = true;
            status.openai = AuthState::Available;
        }
    }

    // Fall back to env/config API key, or combine with OAuth.
    if openai_api_key_configured() {
        status.openai_has_api_key = true;
        status.openai = AuthState::Available;
    }
}

fn assessment_for_key(
    status: &AuthStatus,
    key: LoginProviderAuthStateKey,
    state: AuthState,
) -> (
    AuthCredentialSource,
    String,
    AuthExpiryConfidence,
    AuthRefreshSupport,
    AuthValidationMethod,
) {
    match key {
        // The Claude/OpenAI rows that reach here are the OAuth/subscription
        // login providers; their API-key counterparts (`anthropic-api`,
        // `openai-api`) attribute sources separately via their
        // `LoginProviderTarget` arms. Describe only the OAuth credential here so
        // an API key never makes the OAuth row look configured (or vice versa).
        LoginProviderAuthStateKey::OpenAi => {
            let (source, detail) = summarize_sources(vec![openai_oauth_source(status)]);
            (
                source,
                detail,
                if state == AuthState::NotConfigured {
                    AuthExpiryConfidence::Unknown
                } else {
                    AuthExpiryConfidence::Exact
                },
                AuthRefreshSupport::Automatic,
                AuthValidationMethod::TimestampCheck,
            )
        }
        LoginProviderAuthStateKey::HoronAgent
        | LoginProviderAuthStateKey::Azure
        | LoginProviderAuthStateKey::Bedrock
        | LoginProviderAuthStateKey::OpenRouterLike
        | LoginProviderAuthStateKey::Anthropic
        | LoginProviderAuthStateKey::Copilot
        | LoginProviderAuthStateKey::Gemini
        | LoginProviderAuthStateKey::Antigravity
        | LoginProviderAuthStateKey::Cursor
        | LoginProviderAuthStateKey::GrokBuild
        | LoginProviderAuthStateKey::ExternalImport => (
            AuthCredentialSource::None,
            "not configured".to_string(),
            AuthExpiryConfidence::Unknown,
            AuthRefreshSupport::Unknown,
            AuthValidationMethod::Unknown,
        ),
    }
}

fn summarize_sources(sources: Vec<Option<(AuthCredentialSource, String)>>) -> (AuthCredentialSource, String) {
    let mut collected = Vec::new();
    for source in sources.into_iter().flatten() {
        if !collected.iter().any(|(_, detail)| detail == &source.1) {
            collected.push(source);
        }
    }
    match collected.len() {
        0 => (AuthCredentialSource::None, "not configured".to_string()),
        1 => {
            let mut iter = collected.into_iter();
            if let Some(only) = iter.next() {
                only
            } else {
                unreachable!("collected.len() == 1 but no source was present")
            }
        }
        _ => (
            AuthCredentialSource::Mixed,
            collected
                .into_iter()
                .map(|(_, detail)| detail)
                .collect::<Vec<_>>()
                .join(" + "),
        ),
    }
}

fn env_source(env_key: &str) -> Option<(AuthCredentialSource, String)> {
    env_var_nonempty(env_key).then(|| {
        (
            AuthCredentialSource::EnvironmentVariable,
            format!("{env_key} environment variable"),
        )
    })
}

fn config_source(
    env_key: &str,
    file_name: &str,
    path_label: impl Into<String>,
) -> Option<(AuthCredentialSource, String)> {
    config_file_has_key(file_name, env_key).then(|| {
        (
            AuthCredentialSource::AppConfigFile,
            format!("{} ({env_key})", path_label.into()),
        )
    })
}

fn external_api_key_source(env_key: &str) -> Option<(AuthCredentialSource, String)> {
    crate::auth::external::load_api_key_for_env(env_key).map(|_| {
        (
            AuthCredentialSource::TrustedExternalFile,
            format!("trusted external auth import ({env_key})"),
        )
    })
}

fn openai_oauth_source(status: &AuthStatus) -> Option<(AuthCredentialSource, String)> {
    if !status.openai_has_oauth {
        return None;
    }
    if !crate::auth::codex::list_accounts().unwrap_or_default().is_empty() {
        return Some((
            AuthCredentialSource::HoronAgentManagedFile,
            "~/.horon-agent/openai-auth.json".to_string(),
        ));
    }
    if crate::auth::codex::legacy_auth_allowed() && crate::auth::codex::legacy_auth_source_exists() {
        return Some((
            AuthCredentialSource::TrustedExternalFile,
            "trusted legacy Codex auth file".to_string(),
        ));
    }
    if crate::auth::external::load_openai_oauth_tokens().is_some() {
        return Some((
            AuthCredentialSource::TrustedExternalFile,
            "trusted external auth import".to_string(),
        ));
    }
    None
}

fn env_var_nonempty(key: &str) -> bool {
    std::env::var(key)
        .ok()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

fn config_file_has_key(file_name: &str, env_key: &str) -> bool {
    let Ok(config_dir) = crate::storage::app_config_dir() else {
        return false;
    };
    let path = config_dir.join(file_name);
    config_file_contains_assignment(&path, env_key)
}

fn config_file_contains_assignment(path: &Path, env_key: &str) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    let prefix = format!("{env_key}=");
    content.lines().any(|line| {
        line.strip_prefix(&prefix)
            .map(|value| !value.trim().trim_matches('"').trim_matches('\'').is_empty())
            .unwrap_or(false)
    })
}

fn api_key_available(env_key: &str, file_name: &str) -> bool {
    crate::provider_catalog::load_api_key_from_env_or_config(env_key, file_name).is_some()
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
