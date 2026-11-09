use crate::build;
use crate::storage;
use anyhow::{Context, Result};
pub use horon_agent_update_core::{
    DownloadProgress, GIT_PULL_DIVERGED_SUMMARY, GitHubAsset, GitHubRelease, UpdateCheckResult,
    format_download_progress_bar, summarize_update_error, summary_is_divergence,
};
use horon_agent_update_core::{
    get_asset_name, summarize_git_pull_failure, verify_asset_checksum_text, version_is_newer,
};

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

mod activation;
mod digest;
mod release_asset;

#[path = "update_dev_guard.rs"]
mod update_dev_guard;
#[path = "update_metadata.rs"]
mod update_metadata;
#[path = "update_rate_limit.rs"]
mod update_rate_limit;
use update_metadata::CachedRelease;
pub use update_metadata::UpdateMetadata;
pub use update_rate_limit::{RATE_LIMIT_ERROR_PREFIX, is_rate_limit_error};
use update_rate_limit::{clear_rate_limit_backoff, rate_limit_error};

const GITHUB_REPO: &str = "horon-ai/agent";
/// Minimum gap between *automatic* update checks.
///
/// Every automatic check costs one or two unauthenticated `api.github.com`
/// requests, which share a 60 req/hour per-IP bucket with everything else on
/// the machine (and everything behind the same NAT). A 60s gap meant a user
/// who opens horon-agent a few dozen times an hour exhausted the bucket and then saw
/// spurious 403s. Half an hour is far below any realistic release cadence and
/// keeps automatic checks to at most a couple of requests per hour.
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(5);
/// Time allowed for the initial TCP/TLS connect to the download host.
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Total wall-clock budget for a single download *attempt*.
///
/// This is intentionally a per-attempt budget, not a budget for the whole
/// asset. The old code used a single 120s total timeout for the entire
/// transfer, so on a slow link a multi-megabyte asset could never finish: it
/// was killed mid-stream, the partial bytes were discarded, and every relaunch
/// restarted from zero. We now cap each attempt and resume via HTTP Range, so
/// a slow-but-progressing download completes across several attempts while a
/// genuinely hung connection still gets bounded.
const DOWNLOAD_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(120);
/// How many *consecutive* stalled attempts (attempts that made no forward
/// progress) to tolerate before giving up. Any attempt that downloads new
/// bytes resets this counter, so a slow-but-progressing download keeps
/// resuming via HTTP Range for as long as it needs; only a genuinely stuck
/// connection eventually fails.
const DOWNLOAD_MAX_ATTEMPTS: usize = 10;
const DOWNLOAD_PROGRESS_UPDATE_STEP: u64 = 1_048_576;

pub fn is_release_build() -> bool {
    horon_agent_build_meta::is_release_build()
}

fn current_update_semver() -> &'static str {
    horon_agent_build_meta::update_semver()
}

/// Dev display versions include a commit-count offset, not release precedence.
/// Use the base version to reject older releases, then verify that installing
/// a newer release would not discard commits from the running development build.
fn release_is_update(release: &GitHubRelease) -> Result<bool> {
    release_is_update_with(&release.tag_name, current_update_semver(), is_release_build(), || {
        update_dev_guard::should_install_release(&release.tag_name)
    })
}

fn release_is_update_with(
    release: &str,
    current: &str,
    release_build: bool,
    dev_guard: impl FnOnce() -> Result<bool>,
) -> Result<bool> {
    if !version_is_newer(release, current) {
        return Ok(false);
    }
    if release_build {
        return Ok(true);
    }
    dev_guard()
}

fn source_build_root() -> Result<PathBuf> {
    Ok(storage::horon_agent_dir()?.join("builds").join("source"))
}

pub fn should_auto_update() -> bool {
    should_auto_update_with(
        std::env::var("HORON_AGENT_NO_AUTO_UPDATE").is_ok(),
        is_release_build(),
        || std::env::current_exe().ok(),
        crate::logging::info,
    )
}

fn should_auto_update_with(
    disabled: bool,
    release_build: bool,
    current_exe: impl FnOnce() -> Option<PathBuf>,
    log_skip_reason: impl FnOnce(&str),
) -> bool {
    if disabled || !release_build {
        return false;
    }

    if let Some(exe) = current_exe()
        && let Some(reason) = auto_update_git_repo_skip_reason(&exe)
    {
        log_skip_reason(reason);
        return false;
    }

    true
}

pub fn run_git_pull_ff_only(repo_dir: &Path, quiet: bool) -> Result<()> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("pull").arg("--ff-only");
    if quiet {
        cmd.arg("-q");
    }
    let output = cmd.current_dir(repo_dir).output().context("Failed to run git pull")?;

    if output.status.success() {
        Ok(())
    } else {
        anyhow::bail!("{}", summarize_git_pull_failure(&output.stderr));
    }
}

fn is_inside_git_repo(path: &std::path::Path) -> bool {
    let mut dir = if path.is_dir() { Some(path) } else { path.parent() };

    while let Some(d) = dir {
        if d.join(".git").exists() {
            return true;
        }
        dir = d.parent();
    }
    false
}

fn auto_update_git_repo_skip_reason(path: &std::path::Path) -> Option<&'static str> {
    is_inside_git_repo(path).then_some(
        "Automatic update check skipped because the running executable is inside a Git repository. Rebuild this checkout executable, or run `horon-agent update` and relaunch with the installed `horon-agent` launcher.",
    )
}

/// Env var naming a fake "latest release" endpoint used by the self-dev update
/// rehearsal (`scripts/release/rehearse_update.py`).
pub const UPDATE_RELEASE_URL_OVERRIDE_ENV: &str = "HORON_AGENT_UPDATE_RELEASE_URL";

/// Release-endpoint override for update rehearsals.
///
/// Only honored when `HORON_HOME` points at a sandbox, so a stray environment
/// variable can never redirect a real install to an arbitrary download.
fn release_url_override() -> Option<String> {
    release_url_override_with(
        std::env::var(UPDATE_RELEASE_URL_OVERRIDE_ENV).ok(),
        storage::running_with_sandboxed_home(),
    )
}

fn release_url_override_with(value: Option<String>, sandboxed_home: bool) -> Option<String> {
    let value = value?.trim().to_string();
    if value.is_empty() {
        return None;
    }
    if !sandboxed_home {
        crate::logging::warn(&format!(
            "Ignoring {UPDATE_RELEASE_URL_OVERRIDE_ENV}: only honored with a sandboxed HORON_HOME"
        ));
        return None;
    }
    Some(value)
}

pub fn fetch_latest_release_blocking() -> Result<GitHubRelease> {
    let override_url = release_url_override();
    // Never send a GitHub token or GitHub headers to a non-GitHub rehearsal endpoint.
    let github = override_url.is_none();
    let url = override_url.unwrap_or_else(|| format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest"));

    let cache = UpdateMetadata::load()
        .unwrap_or_default()
        .latest_release_cache
        .filter(|cache| cache.url == url);
    let (release, etag) = request_latest_release(updater_client()?, &url, github, cache.as_ref())?;
    clear_rate_limit_backoff();
    if let Some(etag) = etag {
        let mut metadata = UpdateMetadata::load().unwrap_or_default();
        metadata.latest_release_cache = Some(CachedRelease {
            url,
            etag,
            release: release.clone(),
        });
        if let Err(error) = metadata.save() {
            crate::logging::warn(&format!("update: could not cache release info: {error:#}"));
        }
    }
    Ok(release)
}

/// Fetch the latest release, revalidating `cache` with `If-None-Match` so an
/// unchanged release costs a 304. Returns the release and the new ETag from a 200.
fn request_latest_release(
    client: &reqwest::blocking::Client,
    url: &str,
    github: bool,
    cache: Option<&CachedRelease>,
) -> Result<(GitHubRelease, Option<String>)> {
    let mut request = if github {
        github_api_request(client, url)
    } else {
        client.get(url).timeout(UPDATE_CHECK_TIMEOUT)
    };
    if let Some(cache) = cache {
        request = request.header(reqwest::header::IF_NONE_MATCH, &cache.etag);
    }
    let response = request.send().context("Failed to fetch release info")?;
    let status = response.status();

    if let Some(cache) = cache.filter(|_| status == reqwest::StatusCode::NOT_MODIFIED) {
        return Ok((cache.release.clone(), None));
    }
    if let Some(error) = rate_limit_error(&response) {
        return Err(error);
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        if github && horon_agent_base::github::github_public_api_token().is_none() {
            anyhow::bail!(
                "No releases found for {GITHUB_REPO}; a private repository needs GH_TOKEN, GITHUB_TOKEN, or `gh auth login`"
            );
        }
        anyhow::bail!("No releases found for {GITHUB_REPO}");
    }
    if !status.is_success() {
        anyhow::bail!("GitHub API error: {}", status);
    }

    let etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let release: GitHubRelease = response.json().context("Failed to parse release info")?;
    Ok((release, etag))
}

fn github_api_request(client: &reqwest::blocking::Client, url: &str) -> reqwest::blocking::RequestBuilder {
    let request = client
        .get(url)
        .timeout(UPDATE_CHECK_TIMEOUT)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");

    // Authenticated requests use the user's 5000 req/h quota instead of the
    // shared unauthenticated 60 req/h per-IP bucket, which other tools on the
    // same machine or NAT can exhaust and cause spurious 403s on update
    // checks. Falls back to unauthenticated when no token is available.
    if let Some(token) = horon_agent_base::github::github_public_api_token() {
        request.bearer_auth(token)
    } else {
        request
    }
}

/// Branch the source (main) channel tracks.
const SOURCE_UPDATE_BRANCH: &str = "dev";

fn latest_source_sha_blocking() -> Result<String> {
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/commits/{SOURCE_UPDATE_BRANCH}");
    let response = github_api_request(updater_client()?, &url)
        .send()
        .context("Failed to check the source branch")?;
    if let Some(error) = rate_limit_error(&response) {
        return Err(error);
    }
    if !response.status().is_success() {
        anyhow::bail!(
            "GitHub API error checking {SOURCE_UPDATE_BRANCH}: {}",
            response.status()
        );
    }

    let commit: serde_json::Value = response.json().context("Failed to parse commit info")?;
    Ok(commit["sha"].as_str().unwrap_or("").get(..7).unwrap_or("").to_string())
}

/// One process-wide HTTP client for every updater request. There is no total
/// timeout here: API calls use `UPDATE_CHECK_TIMEOUT` and downloads use
/// `DOWNLOAD_ATTEMPT_TIMEOUT` per request.
fn updater_client() -> Result<&'static reqwest::blocking::Client> {
    static CLIENT: std::sync::OnceLock<reqwest::blocking::Client> = std::sync::OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    let built = reqwest::blocking::Client::builder()
        .use_preconfigured_tls(horon_agent_platform::tls_client_config().as_ref().clone())
        .connect_timeout(DOWNLOAD_CONNECT_TIMEOUT)
        .user_agent("horon-agent-updater")
        .build()?;
    Ok(CLIENT.get_or_init(|| built))
}

/// Pick the download route for a release asset. Private repositories only
/// serve assets through the API URL with a token; everything else (public
/// GitHub, loopback test servers) uses the browser URL without credentials.
fn asset_download_route<'a>(asset: &'a GitHubAsset, token: Option<&'a str>) -> (&'a str, Option<&'a str>) {
    match token {
        Some(token) if asset.api_url.starts_with("https://api.github.com/") => (&asset.api_url, Some(token)),
        _ => (&asset.browser_download_url, None),
    }
}

/// GitHub redirects the API asset URL to signed storage on another host;
/// reqwest drops `Authorization` on that cross-host redirect, while `Range`
/// survives so resume keeps working.
fn asset_request(client: &reqwest::blocking::Client, asset: &GitHubAsset) -> reqwest::blocking::RequestBuilder {
    let token = horon_agent_base::github::github_public_api_token();
    let (url, token) = asset_download_route(asset, token.as_deref());
    let request = client.get(url).timeout(DOWNLOAD_ATTEMPT_TIMEOUT);
    match token {
        Some(token) => request
            .header(reqwest::header::ACCEPT, "application/octet-stream")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .bearer_auth(token),
        None => request,
    }
}

/// What an update check found; installing it is a separate step.
pub enum UpdateCandidate {
    Release(GitHubRelease),
    Source { latest_sha: String },
}

impl UpdateCandidate {
    pub fn label(&self) -> String {
        match self {
            Self::Release(release) => release.tag_name.clone(),
            Self::Source { latest_sha } => format!("main-{latest_sha}"),
        }
    }
}

pub fn install_update_blocking(
    candidate: &UpdateCandidate,
    on_progress: impl FnMut(DownloadProgress),
) -> Result<PathBuf> {
    match candidate {
        UpdateCandidate::Release(release) => download_and_install_blocking_with_progress(release, on_progress),
        UpdateCandidate::Source { latest_sha } => install_main_source_update_blocking(latest_sha),
    }
}

/// Authenticate git against the private updater repository without putting the token in argv.
fn github_git_auth(command: &mut std::process::Command) {
    command.env("GIT_TERMINAL_PROMPT", "0");
    if std::env::var_os("GIT_CONFIG_COUNT").is_some() {
        crate::logging::info("update: GIT_CONFIG_COUNT already set; not injecting GitHub credentials");
        return;
    }
    let Some(token) = horon_agent_base::github::github_public_api_token() else {
        return;
    };
    use base64::Engine as _;
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
    command
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraheader")
        .env("GIT_CONFIG_VALUE_0", format!("AUTHORIZATION: basic {basic}"));
}

/// The source build is confirmed against the published release of the same
/// version, whose `SHA256SUMS` identifies the release before the multi-minute build.
fn install_main_source_update_blocking(latest_sha: &str) -> Result<PathBuf> {
    let repo_dir = checkout_source(latest_sha)?;
    let version = source_package_version(&repo_dir)?;
    let tag = format!("v{version}");
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/tags/{tag}");
    let response = github_api_request(updater_client()?, &url)
        .send()
        .context("Failed to look up the source release")?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!("main channel needs the published release {tag} on {GITHUB_REPO} for its checksums; none found");
    }
    let release: GitHubRelease = response.error_for_status()?.json()?;
    if release.tag_name != tag {
        anyhow::bail!("source build and release metadata name different versions");
    }
    let binary = compile_source(&repo_dir, latest_sha)?;
    install_source_package(&release, &binary, &format!("main-{latest_sha}"))
}

fn install_source_package(release: &GitHubRelease, binary: &Path, label: &str) -> Result<PathBuf> {
    release_asset::validate_version(label)?;
    let builds = build::builds_dir()?;
    let _lock = lock_release_update(&builds)?;
    let version = release.tag_name.strip_prefix('v').unwrap_or(&release.tag_name);
    let installed = release_asset::stage_source(
        updater_client()?,
        release,
        binary,
        version,
        label,
        &builds.join("versions"),
    )?;
    activate_prepared_version(label, label, "source")?;
    Ok(installed)
}

#[cfg(feature = "test-support")]
pub fn install_source_package_for_test(release: &GitHubRelease, binary: &Path, label: &str) -> Result<PathBuf> {
    install_source_package(release, binary, label)
}

/// Log the full error and return a single short line for the UI.
///
/// Update failures come from many layers and are often multi-line, so the
/// verbose text belongs in the log while the card/notice stay one line.
fn short_update_error(context: &str, error: &anyhow::Error) -> String {
    crate::logging::warn(&format!("update: {}: {:#}", context, error));
    summarize_update_error(&format!("{:#}", error))
}

pub fn check_for_update_blocking() -> Result<Option<UpdateCandidate>> {
    let channel = crate::config::config().features.update_channel;
    match channel {
        crate::config::UpdateChannel::Main => check_for_main_update_blocking(),
        crate::config::UpdateChannel::Stable => check_for_stable_update_blocking(),
    }
}

fn check_for_stable_update_blocking() -> Result<Option<UpdateCandidate>> {
    let release = fetch_latest_release_blocking()?;

    // Same exact-name rule the installer uses, so an offered update is installable.
    if release_is_update(&release)? && release_asset::binary_asset(&release).is_ok() {
        return Ok(Some(UpdateCandidate::Release(release)));
    }

    Ok(None)
}

/// Check for updates on the main (cutting edge) channel.
/// Compares the current binary's git hash against the latest commit on the dev branch.
/// A new commit becomes a source candidate when cargo is available; otherwise
/// the latest GitHub Release is checked instead. Nothing is built here.
fn check_for_main_update_blocking() -> Result<Option<UpdateCandidate>> {
    let current_hash = horon_agent_build_meta::git_hash();
    if current_hash.is_empty() || current_hash == "unknown" {
        crate::logging::info("Main channel: no git hash in binary, skipping update check");
        return Ok(None);
    }

    let latest_sha = latest_source_sha_blocking()?;

    if latest_sha.is_empty() {
        return Ok(None);
    }

    // Compare short hashes
    let current_short = if current_hash.len() >= 7 {
        &current_hash[..7]
    } else {
        current_hash
    };

    if current_short == latest_sha {
        crate::logging::info(&format!("Main channel: up to date ({})", current_short));
        return Ok(None);
    }

    crate::logging::info(&format!("Main channel: new commit {} -> {}", current_short, latest_sha));

    if has_cargo() {
        return Ok(Some(UpdateCandidate::Source { latest_sha }));
    }

    crate::logging::info("Main channel: cargo not found, falling back to latest release");
    check_for_stable_update_blocking()
}

/// Check if cargo is available on the system
fn has_cargo() -> bool {
    std::process::Command::new("cargo")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_source_command(mut command: std::process::Command, failure: &str) -> Result<String> {
    let output = command.output()?;
    if !output.status.success() {
        anyhow::bail!("{failure}: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn source_git(repo_dir: &Path, args: &[&str]) -> Result<String> {
    let mut command = std::process::Command::new("git");
    command.args(args).current_dir(repo_dir);
    run_source_command(command, "source checkout command failed")
}

/// Clone or fast-forward the source checkout and confirm it sits at `expected_sha`.
fn checkout_source(expected_sha: &str) -> Result<PathBuf> {
    if expected_sha.len() != 7 || !expected_sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("invalid source update commit");
    }
    let build_dir = source_build_root()?;
    fs::create_dir_all(&build_dir)?;
    let repo_dir = build_dir.join("horon-agent");
    if repo_dir.join(".git").exists() {
        if !source_git(&repo_dir, &["status", "--porcelain"])?.is_empty() {
            anyhow::bail!("source checkout has local changes; update did not overwrite them");
        }
        let mut pull = std::process::Command::new("git");
        pull.args(["pull", "--ff-only", "origin", SOURCE_UPDATE_BRANCH])
            .current_dir(&repo_dir);
        github_git_auth(&mut pull);
        run_source_command(pull, "source checkout command failed")?;
    } else {
        let clone_url = format!("https://github.com/{GITHUB_REPO}.git");
        let mut clone = std::process::Command::new("git");
        clone
            .args([
                "clone",
                "--depth",
                "1",
                "--branch",
                SOURCE_UPDATE_BRANCH,
                &clone_url,
                "horon-agent",
            ])
            .current_dir(&build_dir);
        github_git_auth(&mut clone);
        run_source_command(clone, "git clone failed")?;
    }
    if source_git(&repo_dir, &["rev-parse", "--short=7", "HEAD"])? != expected_sha {
        anyhow::bail!("source head changed since the update check; no version was installed");
    }
    Ok(repo_dir)
}

fn source_package_version(repo_dir: &Path) -> Result<String> {
    let metadata = std::process::Command::new("cargo")
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
            "--locked",
        ])
        .current_dir(repo_dir)
        .output()?;
    if !metadata.status.success() {
        anyhow::bail!(
            "could not identify the source package: {}",
            String::from_utf8_lossy(&metadata.stderr)
        );
    }
    let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout)?;
    let version = metadata["packages"]
        .as_array()
        .and_then(|packages| packages.iter().find(|entry| entry["name"] == "horon-agent"))
        .and_then(|package| package["version"].as_str())
        .context("source metadata has no agent version")?
        .to_owned();
    release_asset::validate_version(&version)?;
    Ok(version)
}

fn compile_source(repo_dir: &Path, expected_sha: &str) -> Result<PathBuf> {
    let output = std::process::Command::new("cargo")
        .args([
            "build",
            "--release",
            "--locked",
            "-p",
            "horon-agent",
            "--bin",
            "horon-agent",
        ])
        .current_dir(repo_dir)
        .env("HORON_AGENT_RELEASE_BUILD", "1")
        .output()?;
    if !output.status.success() {
        anyhow::bail!("cargo build failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    if source_git(repo_dir, &["rev-parse", "--short=7", "HEAD"])? != expected_sha
        || !source_git(repo_dir, &["status", "--porcelain"])?.is_empty()
    {
        anyhow::bail!("source checkout changed while building; no version was installed");
    }
    let binary = build::release_binary_path(repo_dir);
    if !binary.is_file() {
        anyhow::bail!("built binary is missing at {}", binary.display());
    }
    Ok(binary)
}

/// Download an asset into memory, retrying with HTTP Range resume so a slow or
/// flaky connection recovers instead of restarting from zero.
///
/// Returns the full asset bytes plus the best-known total size (for callers
/// that want a final size). Progress callbacks are invoked across retries using
/// the cumulative bytes already on disk, so the UI never appears to go
/// backwards when a stalled connection reconnects.
fn download_asset_with_resume(
    client: &reqwest::blocking::Client,
    asset: &GitHubAsset,
    total_hint: Option<u64>,
    on_progress: &mut impl FnMut(DownloadProgress),
) -> Result<(Vec<u8>, Option<u64>)> {
    let mut bytes: Vec<u8> = Vec::with_capacity(total_hint.unwrap_or_default().min(usize::MAX as u64) as usize);
    let mut total = total_hint;
    let mut next_progress_at = 0_u64;
    let mut last_error: Option<anyhow::Error> = None;
    // Count only *consecutive stalls* (attempts that made no forward progress).
    // A slow-but-advancing download resets this and keeps resuming, so it can
    // take as long as it needs; only a genuinely stuck connection gives up.
    let mut stalls = 0_usize;
    let mut attempt = 0_usize;

    on_progress(DownloadProgress { downloaded: 0, total });

    while stalls < DOWNLOAD_MAX_ATTEMPTS {
        attempt += 1;
        let resume_from = bytes.len() as u64;
        let mut request = asset_request(client, asset);
        if resume_from > 0 {
            // Ask the server to continue where we left off.
            request = request.header(reqwest::header::RANGE, format!("bytes={}-", resume_from));
        }

        let response = match request.send() {
            Ok(response) => response,
            Err(err) => {
                last_error = Some(anyhow::anyhow!("Failed to download update: {}", err));
                log_download_retry(attempt, resume_from, &err);
                stalls += 1;
                continue;
            }
        };

        let status = response.status();
        if resume_from > 0 && status == reqwest::StatusCode::OK {
            // Server ignored the Range header and is resending from the start;
            // discard the partial buffer so we don't corrupt the result.
            bytes.clear();
            next_progress_at = 0;
        } else if resume_from > 0 && status != reqwest::StatusCode::PARTIAL_CONTENT {
            last_error = Some(anyhow::anyhow!("Resume request returned unexpected status {}", status));
            log_download_retry_status(attempt, resume_from, status);
            stalls += 1;
            continue;
        } else if !status.is_success() {
            last_error = Some(anyhow::anyhow!("Download failed: {}", status));
            log_download_retry_status(attempt, resume_from, status);
            stalls += 1;
            continue;
        }

        // Establish the total size. For a 206 response, content-length is the
        // remaining bytes, so prefer the original hint / Content-Range total.
        if total.is_none() {
            total = content_range_total(&response).or_else(|| {
                if status == reqwest::StatusCode::PARTIAL_CONTENT {
                    response
                        .content_length()
                        .map(|len| len.saturating_add(bytes.len() as u64))
                } else {
                    response.content_length()
                }
            });
        }

        let before = bytes.len() as u64;
        let read_result = read_response_into(response, &mut bytes, &mut next_progress_at, total, on_progress);
        let made_progress = bytes.len() as u64 > before;

        match read_result {
            Ok(()) => {
                let downloaded = bytes.len() as u64;
                // If we know the total and fell short, treat as a stall and retry.
                if let Some(total) = total
                    && downloaded < total
                {
                    last_error = Some(anyhow::anyhow!(
                        "Download ended early ({} of {} bytes)",
                        downloaded,
                        total
                    ));
                    log_download_retry_short(attempt, downloaded, total);
                    stalls = if made_progress { 0 } else { stalls + 1 };
                    continue;
                }
                on_progress(DownloadProgress { downloaded, total });
                return Ok((bytes, total));
            }
            Err(err) => {
                let downloaded = bytes.len() as u64;
                crate::logging::warn(&format!(
                    "Update download attempt {} stream error at {} bytes: {}; retrying with resume",
                    attempt, downloaded, err
                ));
                last_error = Some(err);
                stalls = if made_progress { 0 } else { stalls + 1 };
                continue;
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        anyhow::anyhow!(
            "Download stalled with no progress after {} attempts",
            DOWNLOAD_MAX_ATTEMPTS
        )
    }))
}

fn read_response_into(
    mut response: reqwest::blocking::Response,
    bytes: &mut Vec<u8>,
    next_progress_at: &mut u64,
    total: Option<u64>,
    on_progress: &mut impl FnMut(DownloadProgress),
) -> Result<()> {
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = response.read(&mut buffer).context("Failed to read download")?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        let downloaded = bytes.len() as u64;
        if downloaded >= *next_progress_at || total.is_some_and(|total| downloaded >= total) {
            on_progress(DownloadProgress { downloaded, total });
            *next_progress_at = downloaded.saturating_add(DOWNLOAD_PROGRESS_UPDATE_STEP);
        }
    }
    Ok(())
}

fn content_range_total(response: &reqwest::blocking::Response) -> Option<u64> {
    // Content-Range: bytes 200-1023/1024
    response
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.rsplit('/').next())
        .and_then(|total| total.trim().parse::<u64>().ok())
}

fn log_download_retry(attempt: usize, resume_from: u64, err: &impl std::fmt::Display) {
    crate::logging::warn(&format!(
        "Update download attempt {}/{} failed at {} bytes: {}; retrying with resume",
        attempt, DOWNLOAD_MAX_ATTEMPTS, resume_from, err
    ));
}

fn log_download_retry_status(attempt: usize, resume_from: u64, status: reqwest::StatusCode) {
    crate::logging::warn(&format!(
        "Update download attempt {}/{} got status {} at {} bytes; retrying with resume",
        attempt, DOWNLOAD_MAX_ATTEMPTS, status, resume_from
    ));
}

fn log_download_retry_short(attempt: usize, downloaded: u64, total: u64) {
    crate::logging::warn(&format!(
        "Update download attempt {}/{} ended early ({} of {} bytes); retrying with resume",
        attempt, DOWNLOAD_MAX_ATTEMPTS, downloaded, total
    ));
}

fn lock_release_update(builds: &Path) -> Result<fs::File> {
    fs::create_dir_all(builds)?;
    let lock_path = builds.join(".release-update.lock");
    if fs::symlink_metadata(&lock_path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        anyhow::bail!("the release update lock is a symlink");
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    lock.try_lock().context("another release update is already running")?;
    Ok(lock)
}

fn activate_prepared_version(version: &str, installed_version: &str, installed_from: &str) -> Result<()> {
    let paths = [
        build::stable_binary_path()?,
        build::current_binary_path()?,
        build::shared_server_binary_path()?,
        build::launcher_binary_path()?,
        build::stable_version_file()?,
        build::current_version_file()?,
        build::shared_server_version_file()?,
        update_metadata::metadata_path()?,
    ];
    activation::with_rollback(&paths, || {
        let mut metadata = UpdateMetadata::load()?;
        build::advance_shared_server_if_tracking_stable(version)?;
        build::update_stable_symlink(version)?;
        build::update_current_symlink(version)?;
        build::update_launcher_symlink_to_current()?;
        metadata.installed_version = Some(installed_version.to_owned());
        metadata.installed_from = Some(installed_from.to_owned());
        metadata.last_check = SystemTime::now();
        metadata.save()?;
        Ok(())
    })
}

/// Install already downloaded assets without a second download or a shell extractor.
pub fn install_release_from_directory(
    directory: &Path,
    version: &str,
    source_binary: Option<&Path>,
) -> Result<PathBuf> {
    release_asset::validate_version(version)?;
    let builds = build::builds_dir()?;
    let _lock = lock_release_update(&builds)?;
    let binary = release_asset::stage_local(directory, version, source_binary, &builds.join("versions"))?;
    activate_prepared_version(version, &format!("v{version}"), "local-release-assets")?;
    Ok(binary)
}

pub fn download_and_install_blocking_with_progress(
    release: &GitHubRelease,
    mut on_progress: impl FnMut(DownloadProgress),
) -> Result<PathBuf> {
    let version = release.tag_name.strip_prefix('v').unwrap_or(&release.tag_name);
    release_asset::validate_version(version)?;
    let asset = release_asset::binary_asset(release)?;
    let builds = build::builds_dir()?;
    let _lock = lock_release_update(&builds)?;
    let client = updater_client()?;
    let total_hint = if asset._size > 0 { Some(asset._size) } else { None };
    let (bytes, _) = download_asset_with_resume(client, asset, total_hint, &mut on_progress)?;
    let versioned_path =
        release_asset::stage_release(client, release, asset, &bytes, version, &builds.join("versions"))?;
    activate_prepared_version(version, &release.tag_name, &asset.browser_download_url)?;
    Ok(versioned_path)
}

const TERMUX_PREFIX: &str = "/data/data/com.termux/files/usr";

/// Termux detection matching scripts/install/install.sh.
fn is_termux_env(termux_version: Option<&str>, prefix: Option<&str>, prefix_dir_exists: bool) -> bool {
    termux_version.is_some_and(|v| !v.is_empty()) || prefix == Some(TERMUX_PREFIX) || prefix_dir_exists
}

/// Termux glibc loader path for the given arch, if supported.
fn termux_glibc_interpreter(os: &str, arch: &str) -> Option<String> {
    if os != "linux" {
        return None;
    }
    let loader = match arch {
        "aarch64" | "arm64" => "ld-linux-aarch64.so.1",
        "x86_64" => "ld-linux-x86-64.so.2",
        _ => return None,
    };
    Some(format!("{TERMUX_PREFIX}/glibc/lib/{loader}"))
}

fn patch_termux_interpreter_if_needed(binary: &Path) -> Result<()> {
    let termux_version = std::env::var("TERMUX_VERSION").ok();
    let prefix = std::env::var("PREFIX").ok();
    if !is_termux_env(
        termux_version.as_deref(),
        prefix.as_deref(),
        Path::new(TERMUX_PREFIX).is_dir(),
    ) {
        return Ok(());
    }
    let Some(interpreter) = termux_glibc_interpreter(std::env::consts::OS, std::env::consts::ARCH) else {
        return Ok(());
    };
    if !Path::new(&interpreter).exists() {
        anyhow::bail!(
            "Termux detected but glibc loader {interpreter} is missing; run 'pkg install glibc' and retry the update"
        );
    }
    let status = std::process::Command::new("patchelf")
        .arg("--set-interpreter")
        .arg(&interpreter)
        .arg(binary)
        .status();
    match status {
        Ok(s) if s.success() => {
            crate::logging::info(&format!("update: patched Termux glibc ELF interpreter: {interpreter}"));
            Ok(())
        }
        Ok(s) => anyhow::bail!(
            "Failed to patch horon-agent ELF interpreter for Termux glibc (patchelf exited with {s}); update not applied"
        ),
        Err(e) => anyhow::bail!(
            "Termux detected but patchelf could not be run ({e}); run 'pkg install patchelf' and retry the update"
        ),
    }
}

#[cfg(test)]
#[path = "update_termux_tests.rs"]
mod termux_tests;

pub fn check_and_maybe_update(auto_install: bool) -> UpdateCheckResult {
    use crate::bus::{Bus, BusEvent, UpdateStatus};

    if !should_auto_update() {
        return UpdateCheckResult::NoUpdate;
    }

    if !UpdateMetadata::claim_check() {
        return UpdateCheckResult::NoUpdate;
    }

    Bus::global().publish(BusEvent::UpdateStatus(UpdateStatus::Checking));

    match check_for_update_blocking() {
        Ok(Some(candidate)) => {
            let current = horon_agent_build_meta::version().to_string();
            let latest = candidate.label();

            Bus::global().publish(BusEvent::UpdateStatus(UpdateStatus::Available {
                current: current.clone(),
                latest: latest.clone(),
            }));

            if auto_install {
                let progress_version = latest.clone();
                match install_update_blocking(&candidate, |progress| {
                    Bus::global().publish(BusEvent::UpdateStatus(UpdateStatus::Downloading {
                        version: progress_version.clone(),
                        downloaded: progress.downloaded,
                        total: progress.total,
                    }));
                }) {
                    Ok(path) => {
                        Bus::global().publish(BusEvent::UpdateStatus(UpdateStatus::Installed {
                            version: latest.clone(),
                        }));
                        UpdateCheckResult::UpdateInstalled { version: latest, path }
                    }
                    Err(e) => {
                        let msg = format!("Failed to install: {}", e);
                        Bus::global().publish(BusEvent::UpdateStatus(UpdateStatus::Error(msg.clone())));
                        UpdateCheckResult::Error(msg)
                    }
                }
            } else {
                UpdateCheckResult::UpdateAvailable { current, latest }
            }
        }
        Ok(None) => {
            repair_stale_shared_server_after_no_update();
            Bus::global().publish(BusEvent::UpdateStatus(UpdateStatus::UpToDate));
            UpdateCheckResult::NoUpdate
        }
        Err(e) => {
            let msg = short_update_error("update check failed", &e);
            if is_rate_limit_error(&msg) {
                // Throttling is not an update failure and there is nothing the
                // user needs to do, so keep it out of the UI. The backoff was
                // already persisted, so we stop retrying too.
                crate::logging::info(&msg);
                Bus::global().publish(BusEvent::UpdateStatus(UpdateStatus::UpToDate));
                return UpdateCheckResult::NoUpdate;
            }
            Bus::global().publish(BusEvent::UpdateStatus(UpdateStatus::Error(msg.clone())));
            UpdateCheckResult::Error(msg)
        }
    }
}

fn repair_stale_shared_server_after_no_update() {
    match build::repair_stale_shared_server_channel() {
        Ok(build::SharedServerRepair::Repaired { previous, repaired_to }) => {
            crate::logging::info(&format!(
                "update: repaired stale shared-server channel {:?} -> {} after no-op update check",
                previous, repaired_to
            ));
        }
        Ok(build::SharedServerRepair::AlreadyCurrent) => {}
        Err(error) => {
            crate::logging::warn(&format!(
                "update: failed to repair stale shared-server channel after no-op update check: {}",
                error
            ));
        }
    }
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "update_github_auth_tests.rs"]
mod github_auth_tests;
