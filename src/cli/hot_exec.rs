use anyhow::Result;
use std::process::Command as ProcessCommand;

pub use crate::session_rebuild::{hot_rebuild, spawn_background_session_rebuild};

use crate::{build, update};

pub fn get_repo_dir() -> Option<std::path::PathBuf> {
    build::get_repo_dir()
}

/// Minimum interval between `git fetch` update probes across all horon-agent
/// processes. Every source-build client spawn used to fetch unconditionally,
/// so spawning N clients at once ran N concurrent `git fetch` + ssh sessions
/// against the remote. One probe per interval per machine is plenty; a marker
/// file's mtime coordinates it (same pattern as the session-backup pruner).
const UPDATE_FETCH_INTERVAL_SECS: u64 = 15 * 60;

fn claim_update_fetch_slot() -> bool {
    let Ok(base) = crate::storage::horon_agent_dir() else {
        // Cannot coordinate without a home dir; fall back to probing.
        return true;
    };
    let marker = base.join("update-fetch.stamp");
    if let Ok(metadata) = std::fs::metadata(&marker)
        && let Ok(modified) = metadata.modified()
        && let Ok(age) = std::time::SystemTime::now().duration_since(modified)
        && age.as_secs() < UPDATE_FETCH_INTERVAL_SECS
    {
        return false;
    }
    // Touch before fetching so a spawn burst collapses to ~one fetch.
    horon_agent_platform::fs::write_owner_only(&marker, b"").is_ok()
}

/// `None` means this local checkout has no tracking branch, not a failed check.
pub fn check_for_updates() -> Result<Option<bool>> {
    let repo_dir = get_repo_dir().ok_or_else(|| anyhow::anyhow!("Could not find horon-agent source repository"))?;
    check_for_updates_in(&repo_dir, claim_update_fetch_slot)
}

pub(super) fn check_for_updates_in(
    repo_dir: &std::path::Path,
    claim_fetch: impl FnOnce() -> bool,
) -> Result<Option<bool>> {
    // Unlike @{u}, these commands distinguish a valid local branch without
    // tracking from an inaccessible repository or a broken upstream ref.
    let branch = source_update_git(repo_dir, &["rev-parse", "--symbolic-full-name", "HEAD"])?;
    if branch == "HEAD" {
        return Ok(None); // Detached source checkouts cannot be auto-updated either.
    }
    let upstream = source_update_git(repo_dir, &["for-each-ref", "--format=%(upstream)", &branch])?;
    if upstream.is_empty() {
        return Ok(None);
    }

    // Do not fetch or claim the shared throttle slot for an untracked branch.
    if claim_fetch() {
        source_update_git(repo_dir, &["fetch", "-q"])?;
    }
    // When another process claimed the slot, use the fresh-enough local refs.
    let behind = source_update_git(repo_dir, &["rev-list", "--count", "HEAD..@{u}"])?;
    let count: u64 = behind.parse()?;
    Ok(Some(count > 0))
}

fn source_update_git(repo_dir: &std::path::Path, args: &[&str]) -> Result<String> {
    use anyhow::Context;

    let output = ProcessCommand::new("git")
        .args(args)
        .current_dir(repo_dir)
        .output()
        .with_context(|| format!("Could not run git {}", args.join(" ")))?;
    if !output.status.success() {
        anyhow::bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

/// True when the source checkout has local commits that upstream lacks, so a
/// fast-forward pull (and therefore auto-update) can never succeed. Returns
/// `None` when the repo or upstream cannot be inspected.
pub fn local_commits_ahead_of_upstream() -> Option<bool> {
    let repo_dir = get_repo_dir()?;
    let ahead = ProcessCommand::new("git")
        .args(["rev-list", "--count", "@{u}..HEAD"])
        .current_dir(&repo_dir)
        .output()
        .ok()?;
    if !ahead.status.success() {
        return None;
    }
    let count: u32 = String::from_utf8_lossy(&ahead.stdout).trim().parse().unwrap_or(0);
    Some(count > 0)
}

pub fn run_auto_update() -> Result<()> {
    use crate::bus::{Bus, BusEvent, UpdateStatus};

    let repo_dir = get_repo_dir().ok_or_else(|| anyhow::anyhow!("Could not find horon-agent repository"))?;

    update::run_git_pull_ff_only(&repo_dir, true)?;

    crate::logging::info("Building updated source version...");
    let build_output = ProcessCommand::new("cargo")
        .args(["build", "--release"])
        .current_dir(&repo_dir)
        .output()?;

    if !build_output.status.success() {
        let stderr = String::from_utf8_lossy(&build_output.stderr);
        let stdout = String::from_utf8_lossy(&build_output.stdout);
        if !stderr.trim().is_empty() {
            crate::logging::error(&format!("auto-update cargo stderr:\n{}", stderr.trim()));
        }
        if !stdout.trim().is_empty() {
            crate::logging::info(&format!("auto-update cargo stdout:\n{}", stdout.trim()));
        }
        anyhow::bail!("cargo build failed");
    }

    // The interactive client is its own package and cargo invocation. A failed
    // client build keeps the installed one, which the install carries over.
    let tui_built = ProcessCommand::new("cargo")
        .args(["build", "--release", "-p", "horon-tui"])
        .current_dir(&repo_dir)
        .output()
        .is_ok_and(|output| output.status.success());
    if !tui_built {
        crate::logging::warn("auto-update: horon-tui did not build; keeping the installed client");
    }

    if let Err(e) = build::install_local_release(&repo_dir) {
        crate::logging::warn(&format!("auto-update install failed: {}", e));
    }

    let hash = ProcessCommand::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(&repo_dir)
        .output()?;
    let hash = String::from_utf8_lossy(&hash.stdout);
    let version = format!("main-{}", hash.trim());
    Bus::global().publish(BusEvent::UpdateStatus(UpdateStatus::Installed {
        version: version.clone(),
    }));

    // With a live TUI session, let the app reload gracefully (input line
    // saved, current turn finished, session resumed) instead of exec-ing over
    // the running interface, which visibly resets the screen.
    if let Some(session_id) = crate::get_current_session() {
        use crate::bus::{ClientMaintenanceAction, SessionUpdateStatus};
        crate::logging::info(&format!(
            "Updated to {}. Requesting graceful session reload...",
            version
        ));
        Bus::global().publish(BusEvent::SessionUpdateStatus(SessionUpdateStatus::ReadyToReload {
            session_id,
            action: ClientMaintenanceAction::Update,
            version,
        }));
        return Ok(());
    }

    crate::logging::info(&format!("Updated to {}. Restarting...", version));
    std::thread::sleep(std::time::Duration::from_millis(250));

    let exe = build::client_update_candidate()
        .map(|(p, _)| p)
        .or_else(|| std::env::current_exe().ok())
        .ok_or_else(|| anyhow::anyhow!("No executable path found after update"))?;
    let args: Vec<String> = std::env::args().skip(1).collect();

    let err = horon_agent_platform::replace_process(ProcessCommand::new(&exe).args(&args).arg("--no-update"));

    Err(anyhow::anyhow!("Failed to exec new binary {:?}: {}", exe, err))
}

/// Explicit updates follow the configured channel for release and dev builds alike.
/// Source rebuilds belong to /rebuild, not the stable update path.
pub fn run_update() -> Result<()> {
    let current_exe = match std::env::current_exe() {
        Ok(path) => Some(path),
        Err(error) => {
            crate::logging::warn(&format!(
                "update: cannot resolve the running binary ({error}); skipping npm-install and PATH checks"
            ));
            None
        }
    };
    if let Some(hint) = npm_managed_install_hint(current_exe.as_deref()) {
        anyhow::bail!("{hint}");
    }

    super::output::print_centered("Checking GitHub for latest release...");
    match update::check_for_explicit_update_blocking() {
        Ok(update::ExplicitUpdateCheck::Available(candidate)) => {
            let label = candidate.label();
            super::output::print_centered(&match &candidate {
                update::UpdateCandidate::Release(_) => {
                    format!(
                        "Downloading {} \u{2192} {}...",
                        horon_agent_build_meta::version(),
                        label
                    )
                }
                update::UpdateCandidate::Source { .. } => {
                    format!("Building {label} from source (this can take several minutes)...")
                }
            });
            let _path = update::install_update_blocking(&candidate, |progress| {
                super::output::print_centered(&format!("{} {}", label, update::format_download_progress_bar(progress)));
            })
            .map_err(|error| anyhow::anyhow!("{}", format_update_failure("Update install failed", &error)))?;
            super::output::print_centered(&format!("✅ Updated to {}", label));
            reload_server_after_update("installed update");
            super::output::print_centered("Restart horon-agent to use the new version.");
            if let Some(notice) = current_post_update_path_notice(&label, current_exe.as_deref()) {
                super::output::print_centered(&notice);
            }
        }
        Ok(update::ExplicitUpdateCheck::UpToDate {
            latest_tag,
            kept_dev_build,
        }) => {
            if repair_stale_shared_server_after_update_check() {
                reload_server_after_update("repaired stale server target");
            }
            if kept_dev_build && let Some(tag) = latest_tag {
                super::output::print_centered(&format!(
                    "Already up to date ({}; latest published release is {})",
                    horon_agent_build_meta::version(),
                    tag
                ));
            } else {
                super::output::print_centered(&format!("Already up to date ({})", horon_agent_build_meta::version()));
            }
        }
        Err(error) => {
            anyhow::bail!("{}", format_update_failure("Update check failed", &error));
        }
    }
    Ok(())
}

fn path_is_inside_npm_package(path: &std::path::Path) -> bool {
    let mut prev_scope = false;
    for component in path.components() {
        let name = component.as_os_str().to_string_lossy();
        if prev_scope && name.starts_with("horon-") {
            return true;
        }
        prev_scope = name == "@horon-ai";
    }
    false
}

/// Refuse in-place self-updates when invoked from `@horon-ai/horon` npm
/// platform packages, where npm owns both `horon-agent` and `horon-tui`.
fn npm_managed_install_hint(current_exe: Option<&std::path::Path>) -> Option<String> {
    let exe = current_exe?;
    let canonical = match std::fs::canonicalize(exe) {
        Ok(path) => path,
        Err(_) => exe.to_path_buf(),
    };
    if path_is_inside_npm_package(exe) || path_is_inside_npm_package(&canonical) {
        return Some(format!(
            "This horon installation is managed by npm ({}), so `horon update` cannot replace it in place. Run `npm install -g @horon-ai/horon@latest` instead.",
            exe.display()
        ));
    }
    None
}

fn canonical_or_lexical(path: &std::path::Path) -> std::path::PathBuf {
    match std::fs::canonicalize(path) {
        Ok(resolved) => resolved,
        Err(_) => path.to_path_buf(),
    }
}

fn same_binary_target(a: &std::path::Path, b: &std::path::Path) -> bool {
    build::resolve_binary_payload(a) == build::resolve_binary_payload(b)
}

fn first_binary_on_path(path_env: &std::ffi::OsStr, names: &[String]) -> Option<(String, std::path::PathBuf)> {
    for dir in std::env::split_paths(path_env) {
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some((name.clone(), candidate));
            }
        }
    }
    None
}

fn current_post_update_path_notice(label: &str, current_exe: Option<&std::path::Path>) -> Option<String> {
    let exe = current_exe?;
    let builds_dir = match build::builds_dir() {
        Ok(dir) => dir,
        Err(_) => return None,
    };
    let launcher_path = match build::launcher_binary_path() {
        Ok(path) => path,
        Err(_) => return None,
    };
    let path_env = std::env::var_os("PATH");
    post_update_path_notice(label, exe, &builds_dir, &launcher_path, path_env.as_deref())
}

/// Warn when the newly installed launcher will not be what the user runs next:
/// either `horon update` ran from a binary outside the managed install layout
/// (e.g., `target/debug/horon-agent`, `cargo install`, or a copied binary), or
/// another `horon`/`horon-agent` earlier on `PATH` shadows the updated launcher.
fn post_update_path_notice(
    label: &str,
    current_exe: &std::path::Path,
    builds_dir: &std::path::Path,
    launcher_path: &std::path::Path,
    path_env: Option<&std::ffi::OsStr>,
) -> Option<String> {
    let exe_canon = canonical_or_lexical(current_exe);
    let builds_canon = canonical_or_lexical(builds_dir);
    let launcher_dir = launcher_path.parent()?;
    let launcher_dir_canon = canonical_or_lexical(launcher_dir);

    let inside_managed_layout = exe_canon.starts_with(&builds_canon)
        || exe_canon.starts_with(&launcher_dir_canon)
        || current_exe.starts_with(launcher_dir);
    if !inside_managed_layout {
        return Some(format!(
            "Note: {} was installed to {}, which does not replace the binary you ran ({}). Run {} to use {}.",
            label,
            launcher_path.display(),
            current_exe.display(),
            launcher_path.display(),
            label
        ));
    }

    let path_env = path_env?;
    let suffix = std::env::consts::EXE_SUFFIX;
    let names = vec![format!("horon{suffix}"), format!("horon-agent{suffix}")];
    let (found_name, found_path) = first_binary_on_path(path_env, &names)?;
    if found_path.parent() == Some(launcher_dir) || same_binary_target(&found_path, launcher_path) {
        return None;
    }
    Some(format!(
        "Note: {} was installed to {}, but `{}` on PATH currently resolves to {}. Put {} earlier in PATH or remove the older copy.",
        label,
        launcher_path.display(),
        found_name,
        found_path.display(),
        launcher_dir.display()
    ))
}

/// Log the full error chain and return a clear CLI error containing both cause
/// and actionable next steps without 72-character TUI truncation.
fn format_update_failure(stage: &str, error: &anyhow::Error) -> String {
    let full = format!("{error:#}");
    crate::logging::warn(&format!("update: {stage}: {full}"));

    let first_line = full
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("unknown error");
    let lower = full.to_ascii_lowercase();

    if lower.contains("managed by npm")
        || lower.contains("no prebuilt binary for this platform")
        || lower.contains("run 'pkg install")
    {
        return format!("{stage}: {first_line}");
    }
    if update::is_rate_limit_error(&full) {
        return format!(
            "{stage}: GitHub API rate limit reached and public release fallback failed. Wait and retry, or set GH_TOKEN / run `gh auth login` for a higher quota."
        );
    }
    if lower.contains("401 unauthorized")
        || lower.contains("403 forbidden")
        || lower.contains("private repository needs")
    {
        return format!(
            "{stage}: {first_line}. Check or unset GH_TOKEN/GITHUB_TOKEN, or refresh credentials with `gh auth login`."
        );
    }
    if lower.contains("dns")
        || lower.contains("failed to lookup address")
        || lower.contains("name or service not known")
        || lower.contains("connection refused")
        || lower.contains("connection reset")
        || lower.contains("network is unreachable")
        || lower.contains("tcp connect error")
    {
        return format!(
            "{stage}: could not reach GitHub ({first_line}). Check your internet connection or proxy settings and retry `horon update`."
        );
    }
    if lower.contains("timed out") || lower.contains("timeout") {
        return format!("{stage}: GitHub did not respond in time. Retry `horon update` in a moment.");
    }
    if lower.contains("certificate") || lower.contains("tls ") || lower.contains("ssl") {
        return format!(
            "{stage}: TLS error reaching GitHub ({first_line}). Check your system clock, proxy, or CA certificates and retry."
        );
    }
    if lower.contains("checksum") || lower.contains("sha256sums") {
        return format!(
            "{stage}: {first_line}. No installed files were changed; retry `horon update` or reinstall with the install script."
        );
    }
    if lower.contains("permission denied") || lower.contains("read-only file system") || lower.contains("os error 13") {
        return format!(
            "{stage}: {first_line}. Check write permissions on the install directory (or set HORON_AGENT_INSTALL_DIR) and retry."
        );
    }
    if lower.contains("another release update is already running") {
        return format!("{stage}: another horon update is already running. Wait for it to finish and retry.");
    }
    if lower.contains("no space left") {
        return format!(
            "{stage}: not enough disk space to stage the update. Free disk space and retry `horon update`."
        );
    }
    format!("{stage}: {first_line}. See ~/.horon/logs for full details.")
}

fn repair_stale_shared_server_after_update_check() -> bool {
    match build::repair_stale_shared_server_channel() {
        Ok(build::SharedServerRepair::Repaired { previous, repaired_to }) => {
            crate::logging::info(&format!(
                "update: repaired stale shared-server channel {:?} -> {}",
                previous, repaired_to
            ));
            super::output::print_centered(&format!("Repaired stale server reload target: {}", repaired_to));
            true
        }
        Ok(build::SharedServerRepair::AlreadyCurrent) => false,
        Err(error) => {
            crate::logging::warn(&format!(
                "update: failed to repair stale shared-server channel: {}",
                error
            ));
            false
        }
    }
}

fn reload_server_after_update(reason: &str) {
    let exe = build::client_update_candidate()
        .map(|(path, _)| path)
        .or_else(|| std::env::current_exe().ok());
    let Some(exe) = exe else {
        crate::logging::warn("update: could not find horon-agent binary to reload stale server");
        return;
    };

    let output = ProcessCommand::new(&exe)
        .args(["--no-update", "server", "reload", "--force"])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            crate::logging::info(&format!(
                "update: requested server reload after {} via {:?}",
                reason, exe
            ));
        }
        Ok(output) => {
            crate::logging::warn(&format!(
                "update: server reload after {} failed with status {:?}: {}",
                reason,
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Err(error) => {
            crate::logging::warn(&format!(
                "update: failed to request server reload after {} via {:?}: {}",
                reason, exe, error
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn npm_managed_install_is_rejected_with_npm_upgrade_command() {
        let unix_npm = Path::new("/opt/homebrew/lib/node_modules/@horon-ai/horon-darwin-arm64/bin/horon-agent");
        let hint = npm_managed_install_hint(Some(unix_npm)).expect("npm binary must be detected");
        assert!(hint.contains("managed by npm"), "{hint}");
        assert!(hint.contains("npm install -g @horon-ai/horon@latest"), "{hint}");

        let managed = Path::new("/Users/example/.horon/builds/versions/0.1.22/horon-agent");
        assert_eq!(npm_managed_install_hint(Some(managed)), None);
    }

    #[test]
    fn post_update_notice_flags_non_managed_binary_and_shadowed_path() {
        let root = tempfile::tempdir().unwrap();
        let builds = root.path().join("builds");
        let version_dir = builds.join("versions").join("0.1.22");
        let launcher_dir = root.path().join("managed-bin");
        let other_dir = root.path().join("other-bin");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::create_dir_all(&launcher_dir).unwrap();
        std::fs::create_dir_all(&other_dir).unwrap();

        let suffix = std::env::consts::EXE_SUFFIX;
        let version_bin = version_dir.join(format!("horon-agent{suffix}"));
        let launcher_bin = launcher_dir.join(format!("horon-agent{suffix}"));
        let shadow_bin = other_dir.join(format!("horon{suffix}"));
        let dev_bin = root
            .path()
            .join("target")
            .join("debug")
            .join(format!("horon-agent{suffix}"));
        std::fs::create_dir_all(dev_bin.parent().unwrap()).unwrap();
        std::fs::write(&version_bin, b"new").unwrap();
        std::fs::write(&launcher_bin, b"new").unwrap();
        std::fs::write(&shadow_bin, b"old").unwrap();
        std::fs::write(&dev_bin, b"dev").unwrap();

        // 1. Invoked from outside managed layout (e.g., cargo target/debug).
        let dev_notice = post_update_path_notice("v0.1.22", &dev_bin, &builds, &launcher_bin, None)
            .expect("non-managed binary must produce a notice");
        assert!(dev_notice.contains(&launcher_bin.display().to_string()), "{dev_notice}");
        assert!(dev_notice.contains(&dev_bin.display().to_string()), "{dev_notice}");

        // 2. Invoked from managed binary, but PATH puts an older horon first.
        let shadowed_path = std::env::join_paths([&other_dir, &launcher_dir]).unwrap();
        let shadow_notice =
            post_update_path_notice("v0.1.22", &version_bin, &builds, &launcher_bin, Some(&shadowed_path))
                .expect("shadowed PATH binary must produce a notice");
        assert!(
            shadow_notice.contains(&shadow_bin.display().to_string()),
            "{shadow_notice}"
        );
        assert!(
            shadow_notice.contains(&launcher_dir.display().to_string()),
            "{shadow_notice}"
        );

        // 3. Invoked from managed binary with managed launcher first on PATH -> silent.
        let clean_path = std::env::join_paths([&launcher_dir, &other_dir]).unwrap();
        assert_eq!(
            post_update_path_notice("v0.1.22", &version_bin, &builds, &launcher_bin, Some(&clean_path)),
            None
        );
    }

    #[test]
    fn format_update_failure_preserves_cause_and_next_step_without_truncating() {
        let cases = [
            (
                anyhow::anyhow!("GitHub API rate limit reached"),
                "rate limit",
                "gh auth login",
            ),
            (
                anyhow::anyhow!("GitHub API error: 401 Unauthorized"),
                "401 Unauthorized",
                "GH_TOKEN",
            ),
            (
                anyhow::anyhow!("Failed to fetch release info: dns error: failed to lookup address"),
                "could not reach GitHub",
                "internet connection",
            ),
            (
                anyhow::anyhow!("Checksum mismatch for horon-agent-macos-aarch64.tar.gz: expected a, got b"),
                "Checksum mismatch",
                "No installed files were changed",
            ),
            (
                anyhow::anyhow!("Permission denied (os error 13)"),
                "Permission denied",
                "HORON_AGENT_INSTALL_DIR",
            ),
            (
                anyhow::anyhow!("another release update is already running"),
                "already running",
                "Wait for it to finish",
            ),
            (
                anyhow::anyhow!(
                    "Release v9.9.9 is published, but has no prebuilt binary for this platform (missing asset). Check https://github.com/horon-ai/agent/releases/tag/v9.9.9 or retry once release assets finish uploading."
                ),
                "no prebuilt binary for this platform",
                "https://github.com/horon-ai/agent/releases/tag/v9.9.9",
            ),
        ];

        for (error, expected_cause, expected_action) in cases {
            let formatted = format_update_failure("Update check failed", &error);
            assert!(
                formatted.contains(expected_cause),
                "missing cause {expected_cause:?} in: {formatted}"
            );
            assert!(
                formatted.contains(expected_action),
                "missing next action {expected_action:?} in: {formatted}"
            );
            assert!(
                !formatted.contains('…'),
                "CLI update errors must not truncate: {formatted}"
            );
        }
    }
}
