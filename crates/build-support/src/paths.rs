use super::{
    current_binary_path, read_current_version, read_shared_server_version, read_stable_version,
    shared_server_binary_path, stable_binary_path,
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use horon_agent_storage as storage;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Get the horon-agent repository directory
pub fn get_repo_dir() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("HORON_AGENT_REPO_DIR") {
        let path = PathBuf::from(path);
        if is_horon_agent_repo(&path) {
            return Some(path);
        }
    }

    // First try: compile-time directory
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = PathBuf::from(manifest_dir);
    if let Some(repo) = find_repo_in_ancestors(&path) {
        return Some(repo);
    }

    // Fallback: check relative to executable
    if let Ok(exe) = std::env::current_exe() {
        // Assume structure: repo/target/<profile>/<binary> (platform-specific executable name)
        if let Some(repo) = exe.parent().and_then(|p| p.parent()).and_then(|p| p.parent())
            && is_horon_agent_repo(repo)
        {
            return Some(repo.to_path_buf());
        }
    }

    // Final fallback: search upward from current working directory.
    // This matters for self-dev sessions launched from the repo but running
    // from an installed canary/stable binary whose current_exe() is outside
    // the source tree.
    if let Ok(cwd) = std::env::current_dir()
        && let Some(repo) = find_repo_in_ancestors(&cwd)
    {
        return Some(repo);
    }

    None
}

pub fn find_repo_in_ancestors(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        if is_horon_agent_repo(dir) {
            return Some(dir.to_path_buf());
        }
    }
    None
}

pub fn binary_stem() -> &'static str {
    "horon-agent"
}

pub fn binary_name() -> &'static str {
    if cfg!(windows) {
        "horon-agent.exe"
    } else {
        binary_stem()
    }
}

/// Resolve a channel/launcher binary path to the file that actually runs.
///
/// Release archives install a tiny `horon-agent` wrapper script alongside the real
/// `horon-agent-<platform>.bin` payload (the wrapper sets `LD_LIBRARY_PATH` and execs
/// the payload). Channel symlinks point at the wrapper and reload/exec must
/// keep using the wrapper, but the *running process* (`current_exe()`) is the
/// payload. Any "is the candidate the same/newer binary than the running one?"
/// comparison must therefore compare payloads. Comparing the wrapper against
/// the payload compares two different files with unrelated mtimes, which made
/// `server_has_newer_binary()` report a phantom update forever and locked
/// post-`/update` sessions into an infinite reload loop.
///
/// Returns the canonicalized payload path when `path` resolves to a wrapper
/// script with a unique sibling `<stem>-*.bin` payload; otherwise returns the
/// canonicalized input path.
pub fn resolve_binary_payload(path: &Path) -> PathBuf {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    wrapper_payload_sibling(&canonical).unwrap_or(canonical)
}

fn wrapper_payload_sibling(path: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(path).ok()?;
    // Wrapper scripts are a few hundred bytes; real binaries are tens of MB.
    // The size gate keeps us from reading a large binary just to check "#!".
    if !meta.is_file() || meta.len() > 4096 {
        return None;
    }
    let mut prefix = [0u8; 2];
    {
        use std::io::Read;
        let mut file = std::fs::File::open(path).ok()?;
        file.read_exact(&mut prefix).ok()?;
    }
    if &prefix != b"#!" {
        return None;
    }
    let dir = path.parent()?;
    let stem = path.file_stem()?.to_str()?;
    let payload_prefix = format!("{stem}-");
    let mut payload: Option<PathBuf> = None;
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let name = name.to_str()?;
        if name.starts_with(&payload_prefix) && name.ends_with(".bin") {
            if payload.is_some() {
                // Ambiguous: more than one payload candidate. Refuse to guess.
                return None;
            }
            payload = Some(entry.path());
        }
    }
    payload.filter(|p| p.is_file())
}

fn profile_binary_path(repo_dir: &Path, profile: &str) -> PathBuf {
    repo_dir.join("target").join(profile).join(binary_name())
}

pub fn release_binary_path(repo_dir: &Path) -> PathBuf {
    profile_binary_path(repo_dir, "release")
}

fn binary_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|meta| meta.modified().ok())
}

fn newest_existing_binary(candidates: Vec<(PathBuf, &'static str)>) -> Option<(PathBuf, &'static str)> {
    candidates
        .into_iter()
        .filter(|(path, _)| path.exists())
        .max_by_key(|(path, _)| binary_mtime(path))
}

fn existing_binary(path: Result<PathBuf>, label: &'static str) -> Option<(PathBuf, &'static str)> {
    path.ok().filter(|path| path.exists()).map(|path| (path, label))
}

pub fn current_binary_built_at() -> Option<DateTime<Utc>> {
    let modified: SystemTime = std::env::current_exe()
        .ok()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|meta| meta.modified().ok())?;
    Some(DateTime::<Utc>::from(modified))
}

pub fn current_binary_build_time_string() -> Option<String> {
    current_binary_built_at().map(|dt| dt.format("%Y-%m-%d %H:%M:%S %z").to_string())
}

fn home_dir() -> Result<PathBuf> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("USERPROFILE").map(PathBuf::from))
        .map_err(|_| anyhow::anyhow!("HOME/USERPROFILE not set"))
}

fn non_empty_env_path(name: &str) -> Option<PathBuf> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Directory for the single launcher path users execute from PATH.
///
/// Defaults to `~/.local/bin` on Unix, `%LOCALAPPDATA%\horon-agent\bin` on Windows.
/// Overridable with `HORON_AGENT_INSTALL_DIR`.
pub fn launcher_dir() -> Result<PathBuf> {
    if let Some(custom) = non_empty_env_path("HORON_AGENT_INSTALL_DIR") {
        return Ok(custom);
    }

    if let Some(sandbox_home) = non_empty_env_path("HORON_HOME") {
        return Ok(sandbox_home.join("bin"));
    }

    #[cfg(windows)]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            return Ok(PathBuf::from(local).join("horon-agent").join("bin"));
        }
        Ok(home_dir()?
            .join("AppData")
            .join("Local")
            .join("horon-agent")
            .join("bin"))
    }
    #[cfg(not(windows))]
    {
        Ok(home_dir()?.join(".local").join("bin"))
    }
}

/// Path to the launcher binary (`~/.local/bin/horon-agent` by default).
pub fn launcher_binary_path() -> Result<PathBuf> {
    Ok(launcher_dir()?.join(binary_name()))
}

/// Extra launcher names installed next to `horon-agent`. They run the same binary.
pub const LAUNCHER_ALIASES: [&str; 2] = ["horon", "hrn"];

fn alias_file_name(alias: &str) -> String {
    if cfg!(windows) {
        format!("{alias}.exe")
    } else {
        alias.to_string()
    }
}

/// Paths of the alias launchers (`horon`, `hrn`) in the launcher directory.
pub fn launcher_alias_paths() -> Result<Vec<PathBuf>> {
    let dir = launcher_dir()?;
    Ok(LAUNCHER_ALIASES
        .iter()
        .map(|alias| dir.join(alias_file_name(alias)))
        .collect())
}

/// An alias path is ours to replace when it is absent, a symlink to a
/// `horon-agent` binary (Unix), or a byte-identical copy of the previous
/// launcher (Windows, where launchers are copies). Anything else is another
/// program that happens to use the same name and must not be overwritten.
fn alias_is_replaceable(alias: &Path, previous_launcher: Option<&[u8]>) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(alias) else {
        return !alias.exists();
    };
    if meta.file_type().is_symlink() {
        return std::fs::read_link(alias)
            .ok()
            .and_then(|target| target.file_stem().map(|stem| stem == binary_stem()))
            .unwrap_or(false);
    }
    meta.is_file()
        && previous_launcher.is_some_and(|previous| std::fs::read(alias).is_ok_and(|bytes| bytes == previous))
}

fn update_launcher_symlink(target: &Path) -> Result<PathBuf> {
    let launcher = launcher_binary_path()?;

    if let Some(parent) = launcher.parent() {
        storage::ensure_dir(parent)?;
    }

    // Windows launchers are copies, so ownership of an alias is judged against
    // the launcher's content from before this swap.
    let previous_launcher = if cfg!(windows) {
        std::fs::read(&launcher).ok()
    } else {
        None
    };

    let temp = launcher.parent().unwrap_or_else(|| Path::new(".")).join(format!(
        ".{}-launcher-{}",
        binary_stem(),
        std::process::id()
    ));

    crate::platform_support::atomic_symlink_swap(target, &launcher, &temp)?;

    // Aliases are best effort: a missing alias never fails an install or update.
    for alias in launcher_alias_paths()? {
        if alias_is_replaceable(&alias, previous_launcher.as_deref()) {
            let _ = crate::platform_support::atomic_symlink_swap(target, &alias, &temp);
        }
    }
    let _ = std::fs::remove_file(&temp);
    Ok(launcher)
}

/// Update launcher path to point at the current channel binary.
pub fn update_launcher_symlink_to_current() -> Result<PathBuf> {
    let current = current_binary_path()?;
    update_launcher_symlink(&current)
}

/// Update launcher path to point at the stable channel binary.
pub fn update_launcher_symlink_to_stable() -> Result<PathBuf> {
    let stable = stable_binary_path()?;
    update_launcher_symlink(&stable)
}

/// Resolve which client binary should be considered for launches, updates, and reloads.
///
/// Order matters:
/// - Prefer the published `current` channel first (active local build)
/// - Then launcher path
/// - Then stable channel path
/// - Finally currently running executable
pub fn client_update_candidate() -> Option<(PathBuf, &'static str)> {
    if let Some(current) = existing_binary(current_binary_path(), "current") {
        return Some(current);
    }

    if let Some(launcher) = existing_binary(launcher_binary_path(), "launcher") {
        return Some(launcher);
    }

    if let Some(stable) = existing_binary(stable_binary_path(), "stable") {
        return Some(stable);
    }

    std::env::current_exe().ok().map(|exe| (exe, "current"))
}

/// Resolve the binary that the shared daemon should spawn or reload into.
///
/// This intentionally does not follow the fast-moving `current` channel. The
/// shared server should only run binaries that were explicitly promoted onto the
/// shared-server channel (or stable as fallback), so local dirty self-dev builds
/// stop taking out every client by accident.
pub fn shared_server_update_candidate() -> Option<(PathBuf, &'static str)> {
    if let Some(shared_server) = existing_binary(shared_server_binary_path(), "shared-server")
        && shared_server_channel_is_current_enough()
    {
        return Some(shared_server);
    }

    if let Some(stable) = existing_binary(stable_binary_path(), "stable") {
        return Some(stable);
    }

    std::env::current_exe().ok().map(|exe| (exe, "current"))
}

fn shared_server_channel_is_current_enough() -> bool {
    let shared = read_shared_server_version().ok().flatten();
    let Some(shared) = shared.as_deref().map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };

    let stable = read_stable_version().ok().flatten();
    if stable
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some_and(|stable| stable == shared)
    {
        return true;
    }

    let current = read_current_version().ok().flatten();
    current
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some_and(|current| current == shared)
}

fn normalize_version_marker(value: &str) -> String {
    let value = value.trim();
    let value = value.strip_prefix('v').unwrap_or(value);
    value.split([' ', '(', ')']).next().unwrap_or(value).trim().to_string()
}

pub fn version_matches_installed_channel(version: &str, git_hash: &str) -> bool {
    let version = normalize_version_marker(version);
    let git_hash = git_hash.trim();
    let mut saw_marker = false;
    for marker in [read_stable_version(), read_current_version()] {
        let Some(marker) = marker.ok().flatten() else {
            continue;
        };
        let marker_trimmed = marker.trim();
        if marker_trimmed.is_empty() {
            continue;
        }
        saw_marker = true;
        if normalize_version_marker(marker_trimmed) == version {
            return true;
        }
        if !git_hash.is_empty()
            && git_hash != "unknown"
            && (marker_trimmed == git_hash || marker_trimmed.starts_with(git_hash))
        {
            return true;
        }
    }
    !saw_marker
}

/// Resolve the best binary to use for `/reload`.
///
/// This mostly follows `client_update_candidate`, but if a freshly built repo
/// release binary exists and is newer than the selected channel binary, prefer
/// that so local rebuilds can reload correctly even if publishing the build
/// failed.
pub fn preferred_reload_candidate() -> Option<(PathBuf, &'static str)> {
    let candidate = client_update_candidate();

    let repo_binary = get_repo_dir()
        .and_then(|repo_dir| newest_existing_binary(vec![(release_binary_path(&repo_dir), "repo-release")]));

    let repo_is_newer = |repo: &Path, current: &Path| {
        // `current` may be a channel symlink to a release wrapper script;
        // compare the payload that actually runs, not the wrapper.
        match (binary_mtime(repo), binary_mtime(&resolve_binary_payload(current))) {
            (Some(repo), Some(current)) => repo > current,
            (Some(_), None) => true,
            _ => false,
        }
    };

    match (repo_binary, candidate) {
        (Some((repo, label)), Some((current, _))) if repo_is_newer(&repo, &current) => Some((repo, label)),
        (Some((repo, label)), None) => Some((repo, label)),
        (_, Some(candidate)) => Some(candidate),
        (None, None) => None,
    }
}

/// Check if a directory is the horon-agent repository
pub fn is_horon_agent_repo(dir: &Path) -> bool {
    // Check for Cargo.toml with name = "horon-agent"
    let cargo_toml = dir.join("Cargo.toml");
    if !cargo_toml.exists() {
        return false;
    }

    // Check for a .git directory or gitdir file (worktrees use a file).
    if !dir.join(".git").exists() {
        return false;
    }

    // Read Cargo.toml and check package name
    if let Ok(content) = std::fs::read_to_string(&cargo_toml)
        && content.contains("name = \"horon-agent\"")
    {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_fixture(git_file: bool) -> tempfile::TempDir {
        let temp = tempfile::TempDir::new().expect("temp repo");
        if git_file {
            std::fs::write(temp.path().join(".git"), "gitdir: /tmp/horon-agent-test-git\n").expect("git file");
        } else {
            std::fs::create_dir_all(temp.path().join(".git")).expect("git dir");
        }
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"horon-agent\"\nversion = \"0.1.0\"\n",
        )
        .expect("Cargo.toml");
        temp
    }

    #[test]
    fn find_repo_in_ancestors_finds_workspace_from_crate_dir() {
        let repo = repo_fixture(false);
        let crate_dir = repo.path().join("crates").join("build-support");
        std::fs::create_dir_all(&crate_dir).expect("crate dir");

        assert_eq!(find_repo_in_ancestors(&crate_dir).as_deref(), Some(repo.path()));
    }

    #[test]
    fn is_horon_agent_repo_accepts_git_file_for_worktree() {
        let repo = repo_fixture(true);
        assert!(is_horon_agent_repo(repo.path()));
    }

    /// Build a release-style install dir: `horon-agent` wrapper script + payload.
    fn release_install_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::TempDir::new().expect("temp install");
        let wrapper = temp.path().join("horon-agent");
        let payload = temp.path().join("horon-agent-linux-x86_64.bin");
        std::fs::write(
            &wrapper,
            "#!/usr/bin/env sh\nexec ./horon-agent-linux-x86_64.bin \"$@\"\n",
        )
        .expect("wrapper");
        std::fs::write(&payload, vec![0x7fu8; 64]).expect("payload");
        (temp, wrapper, payload)
    }

    #[test]
    fn resolve_binary_payload_maps_wrapper_to_payload() {
        let (_temp, wrapper, payload) = release_install_fixture();
        assert_eq!(
            resolve_binary_payload(&wrapper),
            std::fs::canonicalize(&payload).expect("canonical payload")
        );
    }

    #[test]
    fn resolve_binary_payload_follows_channel_symlink_to_wrapper() {
        // The real layout: builds/<channel>/horon-agent -> versions/<v>/horon-agent (wrapper).
        let (temp, wrapper, payload) = release_install_fixture();
        let channel_dir = temp.path().join("channel");
        std::fs::create_dir_all(&channel_dir).expect("channel dir");
        let link = channel_dir.join("horon-agent");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&wrapper, &link).expect("symlink");
        #[cfg(not(unix))]
        std::fs::copy(&wrapper, &link).map(|_| ()).expect("copy");
        assert_eq!(
            resolve_binary_payload(&link),
            std::fs::canonicalize(&payload).expect("canonical payload")
        );
    }

    #[test]
    fn resolve_binary_payload_keeps_real_binary() {
        // A normal (non-wrapper) binary resolves to itself even with a sibling
        // `.bin` file present: it is too large / not a script.
        let temp = tempfile::TempDir::new().expect("temp install");
        let binary = temp.path().join("horon-agent");
        std::fs::write(&binary, vec![0x7fu8; 8192]).expect("binary");
        std::fs::write(temp.path().join("horon-agent-linux-x86_64.bin"), [0u8; 8]).expect("bin");
        assert_eq!(
            resolve_binary_payload(&binary),
            std::fs::canonicalize(&binary).expect("canonical binary")
        );
    }

    #[test]
    fn resolve_binary_payload_keeps_small_script_without_payload() {
        let temp = tempfile::TempDir::new().expect("temp install");
        let script = temp.path().join("horon-agent");
        std::fs::write(&script, "#!/usr/bin/env sh\nexec true\n").expect("script");
        assert_eq!(
            resolve_binary_payload(&script),
            std::fs::canonicalize(&script).expect("canonical script")
        );
    }

    #[test]
    fn resolve_binary_payload_refuses_ambiguous_payloads() {
        let (temp, wrapper, _payload) = release_install_fixture();
        std::fs::write(temp.path().join("horon-agent-macos-aarch64.bin"), [0u8; 8]).expect("second bin");
        assert_eq!(
            resolve_binary_payload(&wrapper),
            std::fs::canonicalize(&wrapper).expect("canonical wrapper")
        );
    }
}
