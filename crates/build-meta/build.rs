use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod build_inputs;

// Build metadata generator for the horon-agent workspace.
//
// This is the single source of truth for the HORON_AGENT_* compile-time values that
// previously lived in the root `horon-agent` crate's build.rs. It is hosted in the
// leaf `horon-agent-build-meta` crate so every workspace crate can read identical
// values (via `horon_agent_build_meta::*`) without duplicating this script.
//
// NOTE: because this crate's own package version is unrelated to horon-agent's, we
// parse the root `Cargo.toml` `[package].version` for the base semver instead
// of `CARGO_PKG_VERSION`.

fn main() {
    let repo_root = repo_root();

    let pkg_version = root_package_version(&repo_root).unwrap_or_else(|| "0.0.0".to_string());
    let base_version = parse_semver(&pkg_version).unwrap_or(BuildSemver {
        major: 0,
        minor: 0,
        patch: 0,
        prerelease: None,
    });
    let build_semver = resolve_build_semver(&base_version);
    let full_semver = build_semver.full();
    let base_semver = base_version.full();
    let update_semver = if parse_semver_from_env().is_some() {
        full_semver.clone()
    } else {
        base_semver.clone()
    };

    let git_hash = env_or_metadata_or_git(
        &repo_root,
        "HORON_AGENT_BUILD_GIT_HASH",
        "git_hash",
        ["rev-parse", "--short", "HEAD"],
    )
    .filter(|value| !value.is_empty())
    .unwrap_or_else(|| "unknown".to_string());

    // Get git commit date (full datetime with timezone for accurate age calculation)
    let git_date = env_or_metadata_or_git(
        &repo_root,
        "HORON_AGENT_BUILD_GIT_DATE",
        "git_date",
        ["log", "-1", "--format=%ci"],
    )
    .filter(|value| !value.is_empty())
    .unwrap_or_else(|| "unknown".to_string());

    let dirty = match std::env::var("HORON_AGENT_BUILD_GIT_DIRTY") {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "dirty"
        ),
        Err(_) => metadata_value("git_dirty")
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "dirty"
                )
            })
            .or_else(|| git_output(&repo_root, ["status", "--porcelain"]).map(|output| !output.is_empty()))
            .unwrap_or(false),
    };

    // Get git tag (e.g., "v0.1.2" if HEAD is tagged, or "v0.1.2-3-gabc1234" if ahead)
    let git_tag = env_or_metadata_or_git(
        &repo_root,
        "HORON_AGENT_BUILD_GIT_TAG",
        "git_tag",
        ["describe", "--tags", "--always"],
    )
    .unwrap_or_default();

    // Get recent commit messages with commit timestamps and version tag decorations.
    // Format: "hash|timestamp|decorations|subject" per line.
    // We embed a deeper window so /changelog can cover many more releases.
    let raw_log = std::env::var("HORON_AGENT_BUILD_CHANGELOG_RAW")
        .ok()
        .or_else(|| metadata_value("changelog_raw"))
        .or_else(|| git_output(&repo_root, ["log", "-700", "--format=%h|%ct|%D|%s"]))
        .unwrap_or_default();

    // Normalize to "hash<RS>tag<RS>timestamp<RS>subject" — extract version tag or
    // leave empty. We use ASCII record/unit separators so fields can safely
    // contain punctuation.
    let changelog = raw_log
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(4, '|');
            let hash = parts.next()?;
            let timestamp = parts.next().unwrap_or("");
            let decorations = parts.next().unwrap_or("");
            let subject = parts.next()?;
            let tag = decorations
                .split(',')
                .map(|d| d.trim())
                .find(|d| d.starts_with("tag: v"))
                .and_then(|d| d.strip_prefix("tag: "))
                .unwrap_or("");
            Some(format!("{}\x1e{}\x1e{}\x1e{}", hash, tag, timestamp, subject))
        })
        .collect::<Vec<_>>()
        .join("\x1f");

    // Build version string:
    //   Release:    v0.2.17 (abc1234)
    //   Prerelease: v0.2.0-alpha.1 (abc1234)
    //   Dev:        v0.2.17-dev (abc1234)
    //   Dirty:      v0.2.17-dev (abc1234, dirty)
    let is_release = std::env::var("HORON_AGENT_RELEASE_BUILD").is_ok();
    let version = if is_release {
        format!("v{full_semver} ({git_hash})")
    } else if build_semver.prerelease.is_some() {
        // The suffix already names the stage, so `-dev` would invent a version
        // no tag names (`v0.2.0-alpha-dev`); a dirty tree stays visible.
        if dirty {
            format!("v{full_semver} ({git_hash}, dirty)")
        } else {
            format!("v{full_semver} ({git_hash})")
        }
    } else if dirty {
        format!("v{full_semver}-dev ({git_hash}, dirty)")
    } else {
        format!("v{full_semver}-dev ({git_hash})")
    };

    // Set environment variables for compilation
    println!("cargo:rustc-env=HORON_AGENT_GIT_HASH={}", git_hash);
    println!("cargo:rustc-env=HORON_AGENT_GIT_DATE={}", git_date);
    println!("cargo:rustc-env=HORON_AGENT_VERSION={}", version);
    println!("cargo:rustc-env=HORON_AGENT_SEMVER={}", full_semver);
    println!("cargo:rustc-env=HORON_AGENT_BASE_SEMVER={}", base_semver);
    println!("cargo:rustc-env=HORON_AGENT_UPDATE_SEMVER={}", update_semver);
    println!("cargo:rustc-env=HORON_AGENT_GIT_TAG={}", git_tag);
    println!("cargo:rustc-env=HORON_AGENT_CHANGELOG={}", changelog);
    println!("cargo:rustc-env=HORON_AGENT_PKG_VERSION={}", pkg_version);

    // Profile and feature list, so an A/B run can refuse to compare a debug
    // binary against a release one (or two feature sets) instead of attributing
    // a build difference to the code under test. Only the profile is readable
    // here: this build script belongs to the leaf `build-meta` crate, whose own
    // feature set is empty, so `CARGO_FEATURE_*` here says nothing about the
    // crate actually being measured. Each crate reports its own feature list
    // from its own `cfg!` flags.
    let build_profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=HORON_AGENT_BUILD_PROFILE={}", build_profile);

    // Forward HORON_AGENT_RELEASE_BUILD env var if set (CI sets this for release binaries)
    if std::env::var("HORON_AGENT_RELEASE_BUILD").is_ok() {
        println!("cargo:rustc-env=HORON_AGENT_RELEASE_BUILD=1");
    }

    // Re-run only on inputs that should genuinely change the embedded metadata.
    //
    // IMPORTANT: we deliberately do NOT declare `.git/HEAD` or `.git/index` as
    // `rerun-if-changed` inputs. Those files' mtimes change on every `git add`,
    // `git status`, commit, and concurrent-agent git op. Cargo treats a build
    // script as dirty whenever any declared input is newer than the script's
    // output file, reruns it, and then force-recompiles every dependent crate
    // via StaleDepFingerprint -- even when the emitted output is byte-identical.
    // Since `horon-agent-build-meta` sits at the bottom of the crate graph
    // (base -> app-core -> tui -> cli all depend on it), watching the git files
    // turned routine git activity into a full-tree recompile (~18s) on every
    // incremental build. See the deterministic-semver note in
    // `resolve_build_semver` for the companion fix.
    //
    // Correctness is preserved where it matters:
    //   * Release/dist builds set HORON_AGENT_RELEASE_BUILD=1 and HORON_AGENT_BUILD_SEMVER,
    //     both of which DO force a rerun (declared below), so released binaries
    //     always embed the exact version/hash.
    //   * A `[package].version` bump touches Cargo.toml (declared below), which
    //     refreshes the embedded metadata for the next build.
    //   * `cargo clean` / editing this build script naturally re-runs it.
    // For ordinary dev builds the embedded git hash/dirty flag may lag the very
    // latest commit within a session; that is a cosmetic `--version` detail and
    // an acceptable trade for keeping incremental builds incremental.
    println!("cargo:rerun-if-changed={}", repo_root.join("Cargo.toml").display());
    println!("cargo:rerun-if-env-changed=HORON_AGENT_RELEASE_BUILD");
    println!("cargo:rerun-if-env-changed=HORON_AGENT_BUILD_SEMVER");
    // Allow callers to force a metadata refresh (e.g. install scripts) without a
    // full clean, by bumping this env var.
    println!("cargo:rerun-if-env-changed=HORON_AGENT_BUILD_GIT_HASH");
    println!("cargo:rerun-if-env-changed=HORON_AGENT_BUILD_GIT_DATE");
    println!("cargo:rerun-if-env-changed=HORON_AGENT_BUILD_GIT_DIRTY");
    println!("cargo:rerun-if-env-changed=HORON_AGENT_BUILD_GIT_TAG");
    let metadata_path = std::env::var_os("HORON_AGENT_BUILD_METADATA_FILE");
    for directive in build_inputs::metadata_rerun_inputs(metadata_path.as_deref()) {
        println!("{directive}");
    }
}

/// Workspace root, derived from this crate's manifest dir (`crates/build-meta`).
fn repo_root() -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    // crates/build-meta -> crates -> <repo root>
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .unwrap_or(manifest_dir)
}

/// Parse `[package].version` from the root `Cargo.toml` without a toml dep.
fn root_package_version(repo_root: &Path) -> Option<String> {
    let data = fs::read_to_string(repo_root.join("Cargo.toml")).ok()?;
    let mut in_package = false;
    for line in data.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if in_package && let Some(rest) = trimmed.strip_prefix("version") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let value = rest.trim().trim_matches('"').to_string();
                if !value.is_empty() {
                    return Some(value);
                }
            }
        }
    }
    None
}

/// A release tag as `MAJOR.MINOR.PATCH` plus an optional prerelease suffix.
///
/// The suffix is kept because a release build of `v0.2.0-alpha.1` must report
/// exactly that: the release workflow, the archive installer and the Windows
/// installer all compare `--version` against the tag.
#[derive(Clone)]
struct BuildSemver {
    major: u32,
    minor: u32,
    patch: u32,
    prerelease: Option<String>,
}

impl BuildSemver {
    /// `MAJOR.MINOR.PATCH`, with `-PRERELEASE` when the tag carries one.
    fn full(&self) -> String {
        match &self.prerelease {
            Some(prerelease) => format!("{}.{}.{}-{}", self.major, self.minor, self.patch, prerelease),
            None => format!("{}.{}.{}", self.major, self.minor, self.patch),
        }
    }

    /// `MAJOR.MINOR.PATCH`, the release line without a prerelease suffix.
    fn core(&self) -> (u32, u32, u32) {
        (self.major, self.minor, self.patch)
    }
}

/// Parse `[v]MAJOR.MINOR.PATCH[-PRERELEASE]`; build metadata is refused.
fn parse_semver(value: &str) -> Option<BuildSemver> {
    let trimmed = value.trim().trim_start_matches('v');
    let (core, prerelease) = match trimmed.split_once('-') {
        Some((core, prerelease)) => (core, Some(prerelease)),
        None => (trimmed, None),
    };
    // `+build` has no release identity of its own, so it is not a version here.
    if core.contains('+') || prerelease.is_some_and(|pre| pre.is_empty() || pre.contains('+')) {
        return None;
    }
    if let Some(prerelease) = prerelease {
        let valid = prerelease
            .split('.')
            .all(|identifier| !identifier.is_empty() && identifier.bytes().all(|byte| byte.is_ascii_alphanumeric()));
        if !valid {
            return None;
        }
    }
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(BuildSemver {
        major,
        minor,
        patch,
        prerelease: prerelease.map(str::to_string),
    })
}

/// The `HORON_AGENT_BUILD_SEMVER` override CI passes as the release tag.
///
/// Parsed rather than copied so a suffix is kept and a malformed value is not
/// embedded in the binary; the pruning is deliberate, because a bad override
/// must not silently rename a release.
fn parse_semver_from_env() -> Option<BuildSemver> {
    std::env::var("HORON_AGENT_BUILD_SEMVER")
        .ok()
        .and_then(|value| parse_semver(&value))
}

fn resolve_build_semver(base_version: &BuildSemver) -> BuildSemver {
    if let Some(explicit) = parse_semver_from_env() {
        return explicit;
    }

    // Dev builds derive the patch number deterministically from committed git
    // state: `base.patch + <commits since the base-version tag>`. This is a pure
    // function of HEAD, so the emitted HORON_AGENT_SEMVER/HORON_AGENT_VERSION only change when
    // an actual commit lands, NOT on every build-script rerun.
    //
    // The previous implementation incremented a persistent counter on every
    // rerun. Because the build script reruns whenever `.git/index`/`.git/HEAD`
    // change (any `git add`, commit, or concurrent agent git op), that side
    // effect churned the version string on essentially every build, which in
    // turn invalidated `horon-agent-build-meta` and force-recompiled the entire crate
    // graph (base -> app-core -> tui -> cli). Deriving the value deterministically
    // keeps incremental rebuilds incremental.
    let offset = commits_since_base_tag(base_version.core()).unwrap_or(0);
    BuildSemver {
        major: base_version.major,
        minor: base_version.minor,
        patch: base_version.patch.saturating_add(offset),
        prerelease: base_version.prerelease.clone(),
    }
}

/// Count commits between the base-version tag (`vMAJOR.MINOR.PATCH`) and HEAD.
/// Returns `None` when git is unavailable or the tag does not exist yet, in which
/// case the caller falls back to the base patch (still deterministic).
fn commits_since_base_tag(base_version: (u32, u32, u32)) -> Option<u32> {
    let repo_root = repo_root();
    let tag = format!("v{}.{}.{}", base_version.0, base_version.1, base_version.2);
    let range = format!("{tag}..HEAD");
    let out = git_output(&repo_root, ["rev-list", "--count", range.as_str()])?;
    out.trim().parse::<u32>().ok()
}

fn env_or_metadata_or_git<const N: usize>(
    repo_root: &Path,
    env_name: &str,
    metadata_key: &str,
    git_args: [&str; N],
) -> Option<String> {
    std::env::var(env_name)
        .ok()
        .or_else(|| metadata_value(metadata_key))
        .or_else(|| git_output(repo_root, git_args))
        .map(|value| value.trim().to_string())
}

fn git_output<const N: usize>(repo_root: &Path, args: [&str; N]) -> Option<String> {
    let output = Command::new("git").current_dir(repo_root).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn metadata_value(key: &str) -> Option<String> {
    let path = std::env::var("HORON_AGENT_BUILD_METADATA_FILE").ok()?;
    let data = fs::read_to_string(path).ok()?;
    let mut lines = data.lines();
    while let Some(line) = lines.next() {
        if let Some((entry_key, marker)) = line.split_once("<<") {
            if entry_key == key {
                let mut value = String::new();
                for value_line in lines.by_ref() {
                    if value_line == marker {
                        return Some(value);
                    }
                    if !value.is_empty() {
                        value.push('\n');
                    }
                    value.push_str(value_line);
                }
                return Some(value);
            }
            continue;
        }

        if let Some((entry_key, entry_value)) = line.split_once('=')
            && entry_key == key
        {
            return Some(entry_value.to_string());
        }
    }
    None
}
