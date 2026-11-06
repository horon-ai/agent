//! Credential discovery and environment capability probing.
//!
//! horon-agent talks to OpenAI-compatible `/chat/completions` endpoints, so the
//! only credentials it manages are API keys named by a `[providers.<name>]`
//! profile's `api_key_env` (or its private env file, or a trusted external CLI
//! auth file). This module answers one question — is any profile configured —
//! and caches the answer for the process.

pub mod env_facts;
pub mod external;
#[cfg(any(test, feature = "test-support"))]
pub mod test_sandbox;

pub use horon_agent_auth_types::AuthState;

use serde::{Deserialize, Serialize};
use std::time::Instant;

/// Cached, non-secret snapshot of whether any OpenAI-compatible profile has
/// usable credentials.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthStatus {
    /// Any OpenAI-compatible profile (Cerebras, Groq, a local server, ...) has
    /// usable credentials.
    #[serde(default)]
    pub openai_compatible_any: AuthState,
}

/// Cached auth status plus the `HORON_AGENT_HOME` it was computed under.
///
/// Probes read credential files relative to `HORON_AGENT_HOME`. Tests swap that
/// home to per-test temp dirs, and a status computed under one home must never
/// be served for another.
type CachedAuthStatus = (AuthStatus, Instant, Option<std::ffi::OsString>);

static AUTH_STATUS_CACHE: std::sync::LazyLock<std::sync::RwLock<Option<CachedAuthStatus>>> =
    std::sync::LazyLock::new(|| std::sync::RwLock::new(None));
static AUTH_STATUS_FAST_CACHE: std::sync::LazyLock<std::sync::RwLock<Option<CachedAuthStatus>>> =
    std::sync::LazyLock::new(|| std::sync::RwLock::new(None));

fn auth_cache_home_key() -> Option<std::ffi::OsString> {
    std::env::var_os("HORON_AGENT_HOME")
}

const AUTH_STATUS_CACHE_TTL_SECS: u64 = 30;
const AUTH_STATUS_FAST_CACHE_TTL_SECS: u64 = 60;

/// Bumped whenever the cached auth status is invalidated.
static AUTH_STATUS_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Current auth-status generation.
pub fn auth_status_generation() -> u64 {
    AUTH_STATUS_GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

/// Bump the auth generation without clearing the cached status.
pub fn bump_auth_status_generation_for_tests() {
    AUTH_STATUS_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Guards the background refresh spawned by [`AuthStatus::check_fast_nonblocking`].
static AUTH_REFRESH_IN_FLIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn browser_suppressed(cli_no_browser: bool) -> bool {
    cli_no_browser
        || env_truthy("NO_BROWSER")
        || env_truthy("HORON_AGENT_NO_BROWSER")
        || running_in_test_harness()
        || browser_unusable_here()
}

/// True when the probed environment says a browser launch cannot work.
fn browser_unusable_here() -> bool {
    use crate::auth::env_facts::{AuthMethodChoice, EnvFacts};
    static CHOICE: std::sync::LazyLock<AuthMethodChoice> =
        std::sync::LazyLock::new(|| EnvFacts::probe().preferred_auth_method());
    matches!(
        *CHOICE,
        AuthMethodChoice::DeviceCode | AuthMethodChoice::ApiKeyNonInteractive
    )
}

/// True when the current process is a Rust test binary (`cargo test` /
/// `cargo nextest`). Cargo places hashed test executables in `deps/` beneath
/// its build directory, which can be relocated with `CARGO_TARGET_DIR`.
///
/// Used to keep tests from opening real browser windows on the developer's
/// desktop. Set `HORON_AGENT_ALLOW_BROWSER_IN_TESTS=1` to opt out.
pub fn running_in_test_harness() -> bool {
    static IN_TEST_HARNESS: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        if env_truthy("HORON_AGENT_ALLOW_BROWSER_IN_TESTS") {
            return false;
        }
        std::env::current_exe()
            .ok()
            .map(|exe| is_test_harness_path(&exe.to_string_lossy()))
            .unwrap_or(false)
    });
    *IN_TEST_HARNESS
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

impl AuthStatus {
    /// Check every OpenAI-compatible profile and cache the result.
    pub fn check() -> Self {
        let home_key = auth_cache_home_key();
        if let Ok(cache) = AUTH_STATUS_CACHE.read()
            && let Some((status, when, cached_home)) = cache.as_ref()
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

    /// Fast auth snapshot for interactive surfaces.
    ///
    /// Credential checks only read config and env files, so the fast path is the
    /// same probe as the full one; the separate cache exists so callers can ask
    /// for a snapshot without forcing a refresh of the longer-lived cache.
    pub fn check_fast() -> Self {
        let home_key = auth_cache_home_key();
        if let Ok(cache) = AUTH_STATUS_CACHE.read()
            && let Some((status, when, cached_home)) = cache.as_ref()
            && when.elapsed().as_secs() < AUTH_STATUS_CACHE_TTL_SECS
            && *cached_home == home_key
        {
            return status.clone();
        }

        if let Ok(cache) = AUTH_STATUS_FAST_CACHE.read()
            && let Some((status, when, cached_home)) = cache.as_ref()
            && when.elapsed().as_secs() < AUTH_STATUS_FAST_CACHE_TTL_SECS
            && *cached_home == home_key
        {
            return status.clone();
        }

        let status = Self::check_uncached();
        if let Ok(mut cache) = AUTH_STATUS_FAST_CACHE.write() {
            *cache = Some((status.clone(), Instant::now(), home_key));
        }

        status
    }

    /// Start the auth probe on a background thread so the first frame finds a
    /// warm snapshot. Safe to call any time: [`Self::check_fast`] reuses it.
    pub fn prewarm_fast_in_background() {
        if running_in_test_harness() {
            return;
        }
        let _ = std::thread::Builder::new().name("auth-prewarm".into()).spawn(|| {
            let _ = Self::check_fast();
        });
    }

    /// Non-blocking auth snapshot for per-frame render paths.
    ///
    /// Serves the freshest cached snapshot (even if expired) and refreshes the
    /// cache on a detached background thread, bumping the auth generation when
    /// the refreshed snapshot actually differs. Tests always take the blocking
    /// path: background refreshes racing per-test `HORON_AGENT_HOME` swaps would
    /// poison the shared cache.
    pub fn check_fast_nonblocking() -> Self {
        if running_in_test_harness() {
            return Self::check_fast();
        }

        let home_key = auth_cache_home_key();
        if let Ok(cache) = AUTH_STATUS_CACHE.read()
            && let Some((status, when, cached_home)) = cache.as_ref()
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
            return Self::check_fast();
        };

        if when.elapsed().as_secs() >= AUTH_STATUS_FAST_CACHE_TTL_SECS
            && !AUTH_REFRESH_IN_FLIGHT.swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            let previous = status.clone();
            std::thread::Builder::new()
                .name("auth-refresh".into())
                .spawn(move || {
                    let refreshed = Self::check_uncached();
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

    /// Drop every cached snapshot and bump the generation.
    pub fn invalidate_cached_status() {
        if let Ok(mut cache) = AUTH_STATUS_CACHE.write() {
            *cache = None;
        }
        if let Ok(mut cache) = AUTH_STATUS_FAST_CACHE.write() {
            *cache = None;
        }
        AUTH_STATUS_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        crate::provider::pricing::invalidate_auth_pricing_memos();
    }

    pub fn invalidate_cache() {
        Self::invalidate_cached_status();
    }

    /// True if at least one OpenAI-compatible profile has usable credentials.
    pub fn has_any_available(&self) -> bool {
        self.openai_compatible_any == AuthState::Available
    }

    /// Emit a structured, non-secret snapshot of provider credential state.
    pub fn log_snapshot(&self, surface: &str) {
        crate::logging::event_info(
            "auth_status_snapshot",
            vec![
                ("surface", surface.to_string()),
                ("any_available", self.has_any_available().to_string()),
                ("openai_compatible", self.openai_compatible_any.label().to_string()),
            ],
        );
    }

    /// True when a shared credential file exists that horon-agent has not been
    /// allowed to read yet.
    pub fn has_any_untrusted_external_auth() -> bool {
        crate::auth::external::has_any_unconsented_external_auth()
    }

    fn check_uncached() -> Self {
        let configured = crate::provider_catalog::openai_compatible_profiles()
            .iter()
            .copied()
            .any(crate::provider_catalog::openai_compatible_profile_is_configured);
        let status = Self {
            openai_compatible_any: if configured {
                AuthState::Available
            } else {
                AuthState::NotConfigured
            },
        };
        status.log_snapshot("auth_status_check");
        status
    }
}
