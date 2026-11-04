//! Build and runtime version metadata for horon-agent.
//!
//! The build script (`build.rs`) computes git- and version-derived values and
//! emits them via `cargo:rustc-env`. Most binaries use those compile-time values.
//! A fast local release may wrap an already-built selfdev binary and set
//! `HORON_AGENT_RUNTIME_RELEASE_SEMVER`; the accessor functions below then present the
//! tagged release identity without recompiling the dependency graph solely to
//! change version strings.

use std::sync::OnceLock;

#[cfg(test)]
#[path = "../build_inputs.rs"]
mod build_inputs;

/// Compile-time human-readable version string, e.g. `v0.14.6-dev (abc1234)`.
pub const VERSION: &str = env!("HORON_AGENT_VERSION");
/// Short git hash of the build commit, e.g. `abc1234` (or `unknown`).
pub const GIT_HASH: &str = env!("HORON_AGENT_GIT_HASH");
/// Commit date/time of the build commit (or `unknown`).
pub const GIT_DATE: &str = env!("HORON_AGENT_GIT_DATE");
/// `git describe --tags --always` output (may be empty).
pub const GIT_TAG: &str = env!("HORON_AGENT_GIT_TAG");
/// Compile-time auto-incrementing build semver (dev) or explicit release semver.
pub const SEMVER: &str = env!("HORON_AGENT_SEMVER");
/// Compile-time base semver taken from the root `Cargo.toml` package version.
pub const BASE_SEMVER: &str = env!("HORON_AGENT_BASE_SEMVER");
/// Compile-time semver used for update comparisons.
pub const UPDATE_SEMVER: &str = env!("HORON_AGENT_UPDATE_SEMVER");
/// Encoded changelog (record/unit separated). See build.rs for the format.
pub const CHANGELOG: &str = env!("HORON_AGENT_CHANGELOG");
/// Compile-time root crate package version.
pub const PKG_VERSION: &str = env!("HORON_AGENT_PKG_VERSION");
/// Cargo build profile this binary was compiled with: `debug` or `release`.
pub const BUILD_PROFILE: &str = env!("HORON_AGENT_BUILD_PROFILE");

static RUNTIME_RELEASE_SEMVER: OnceLock<Option<String>> = OnceLock::new();
static RUNTIME_VERSION: OnceLock<Option<String>> = OnceLock::new();
static RUNTIME_GIT_HASH: OnceLock<Option<String>> = OnceLock::new();
static RUNTIME_GIT_DATE: OnceLock<Option<String>> = OnceLock::new();
static RUNTIME_GIT_TAG: OnceLock<Option<String>> = OnceLock::new();

fn parse_release_semver(value: &str) -> Option<String> {
    let value = value.trim().trim_start_matches('v');
    let mut parts = value.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next()?.parse::<u32>().ok()?;
    let patch = parts.next()?.parse::<u32>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(format!("{major}.{minor}.{patch}"))
}

/// Optional release semver supplied by the fast-release wrapper at process start.
pub fn runtime_release_semver() -> Option<&'static str> {
    RUNTIME_RELEASE_SEMVER
        .get_or_init(|| {
            std::env::var("HORON_AGENT_RUNTIME_RELEASE_SEMVER")
                .ok()
                .and_then(|value| parse_release_semver(&value))
        })
        .as_deref()
}

/// Human-readable runtime version, honoring a fast-release wrapper override.
pub fn version() -> &'static str {
    RUNTIME_VERSION
        .get_or_init(|| runtime_release_semver().map(|semver| format!("v{semver} ({})", git_hash())))
        .as_deref()
        .unwrap_or(VERSION)
}

fn runtime_identity_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Runtime git hash, honoring the fast-release wrapper identity.
pub fn git_hash() -> &'static str {
    RUNTIME_GIT_HASH
        .get_or_init(|| runtime_identity_value("HORON_AGENT_RUNTIME_RELEASE_GIT_HASH"))
        .as_deref()
        .unwrap_or(GIT_HASH)
}

/// Runtime git date, honoring the fast-release wrapper identity.
pub fn git_date() -> &'static str {
    RUNTIME_GIT_DATE
        .get_or_init(|| runtime_identity_value("HORON_AGENT_RUNTIME_RELEASE_GIT_DATE"))
        .as_deref()
        .unwrap_or(GIT_DATE)
}

/// Runtime git tag, honoring the fast-release wrapper identity.
pub fn git_tag() -> &'static str {
    RUNTIME_GIT_TAG
        .get_or_init(|| runtime_identity_value("HORON_AGENT_RUNTIME_RELEASE_GIT_TAG"))
        .as_deref()
        .unwrap_or(GIT_TAG)
}

/// Runtime build semver, honoring a fast-release wrapper override.
pub fn semver() -> &'static str {
    runtime_release_semver().unwrap_or(SEMVER)
}

/// Runtime base semver, honoring a fast-release wrapper override.
pub fn base_semver() -> &'static str {
    runtime_release_semver().unwrap_or(BASE_SEMVER)
}

/// Runtime update-comparison semver, honoring a fast-release wrapper override.
pub fn update_semver() -> &'static str {
    runtime_release_semver().unwrap_or(UPDATE_SEMVER)
}

/// Runtime package version, honoring a fast-release wrapper override.
pub fn pkg_version() -> &'static str {
    runtime_release_semver().unwrap_or(PKG_VERSION)
}

/// Whether this process should behave as a release build.
pub fn is_release_build() -> bool {
    option_env!("HORON_AGENT_RELEASE_BUILD").is_some() || runtime_release_semver().is_some()
}

/// Cargo profile the binary was compiled with, as `debug` or `release`.
///
/// An A/B run compares this before timing anything: a debug-vs-release pair
/// measures the profile, not the change under test.
pub fn build_profile() -> &'static str {
    BUILD_PROFILE
}

#[cfg(test)]
mod tests {
    use super::parse_release_semver;

    #[test]
    fn runtime_release_semver_accepts_only_three_numeric_components() {
        assert_eq!(parse_release_semver("v1.2.3").as_deref(), Some("1.2.3"));
        assert_eq!(parse_release_semver(" 1.2.3 ").as_deref(), Some("1.2.3"));
        assert_eq!(parse_release_semver("1.2"), None);
        assert_eq!(parse_release_semver("1.2.3.4"), None);
        assert_eq!(parse_release_semver("1.2.beta"), None);
    }
}
