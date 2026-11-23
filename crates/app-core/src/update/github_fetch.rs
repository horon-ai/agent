//! Resilient GitHub release and commit lookup for `horon update`.
//!
//! `horon-ai/agent` is a public repository. Authenticated `api.github.com`
//! calls get a 5,000 req/hr quota, but an expired, revoked, or narrowly scoped
//! `GH_TOKEN`/`GITHUB_TOKEN` (or `gh auth token`) can fail with 401/403/404,
//! and anonymous `api.github.com` calls share a 60 req/hr per-IP bucket.
//!
//! This module keeps updates working across both failure modes:
//! 1. If an authenticated GitHub API call returns 401/403/404, retry once
//!    without credentials.
//! 2. If `api.github.com` is rate-limited or still fails with 401/403/5xx,
//!    fall back to the public release redirect
//!    (`https://github.com/horon-ai/agent/releases/latest` -> `/releases/tag/<tag>`)
//!    and read `SHA256SUMS` from `/releases/download/<tag>/SHA256SUMS` — the
//!    same public route `scripts/install/install.sh` and `install.ps1` use —
//!    so every asset download still verifies against `SHA256SUMS`.

use anyhow::{Context, Result, bail};
use horon_agent_update_core::{GitHubAsset, GitHubRelease, parse_sha256sums};
use std::collections::BTreeSet;
use std::process::Command;

use super::update_metadata::CachedRelease;
use super::update_rate_limit::{RATE_LIMIT_ERROR_PREFIX, detect_rate_limit_backoff, record_rate_limit_backoff};
use super::{DOWNLOAD_ATTEMPT_TIMEOUT, GITHUB_REPO, UPDATE_CHECK_TIMEOUT};

pub(super) const GITHUB_WEB_BASE: &str = "https://github.com/horon-ai/agent";

/// Build a GitHub API GET request with optional ETag revalidation and bearer token.
fn build_github_api_get(
    client: &reqwest::blocking::Client,
    url: &str,
    token: Option<&str>,
    if_none_match: Option<&str>,
) -> reqwest::blocking::RequestBuilder {
    let mut request = client
        .get(url)
        .timeout(UPDATE_CHECK_TIMEOUT)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(etag) = if_none_match {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    request
}

/// Send a GitHub API GET request, retrying once without credentials if a token
/// was rejected (401/403/404) so a stale or narrowly scoped local token never
/// breaks access to the public repository.
pub(super) fn send_github_api_get(
    client: &reqwest::blocking::Client,
    url: &str,
    token: Option<&str>,
    if_none_match: Option<&str>,
) -> std::result::Result<reqwest::blocking::Response, reqwest::Error> {
    let response = build_github_api_get(client, url, token, if_none_match).send()?;
    if token.is_some()
        && matches!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN | reqwest::StatusCode::NOT_FOUND
        )
    {
        crate::logging::warn(&format!(
            "update: GitHub API returned {} with local credentials; retrying without authentication",
            response.status()
        ));
        return build_github_api_get(client, url, None, if_none_match).send();
    }
    Ok(response)
}

/// Validate a release tag segment before placing it into a download URL.
pub(super) fn is_valid_release_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.as_bytes()[0].is_ascii_alphanumeric()
        && tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-+".contains(&byte))
        && !tag.contains("..")
        && !tag.ends_with('.')
        && !tag.ends_with(".lock")
}

/// Extract a validated release tag from a `/releases/tag/<tag>` URL path.
fn extract_release_tag_from_path(path: &str) -> Option<&str> {
    let (_, tail) = path.split_once("/releases/tag/")?;
    let tag = tail.trim_end_matches('/');
    if tag.contains('/') || !is_valid_release_tag(tag) {
        return None;
    }
    Some(tag)
}

/// Resolve a release for `tag` directly from the public GitHub release download
/// endpoint by reading and parsing `SHA256SUMS`.
pub(super) fn fetch_release_by_tag_via_web(
    client: &reqwest::blocking::Client,
    web_base_url: &str,
    tag: &str,
) -> Result<GitHubRelease> {
    if !is_valid_release_tag(tag) {
        bail!("invalid release tag {tag:?}");
    }
    let base = web_base_url.trim_end_matches('/');
    let download_base = format!("{base}/releases/download/{tag}");
    let sums_url = format!("{download_base}/SHA256SUMS");
    let response = client
        .get(&sums_url)
        .timeout(UPDATE_CHECK_TIMEOUT)
        .send()
        .with_context(|| format!("failed to fetch SHA256SUMS for {tag}"))?;
    if !response.status().is_success() {
        bail!("public release checksums unavailable for {tag} ({})", response.status());
    }
    let sums_text = response
        .text()
        .with_context(|| format!("failed to read SHA256SUMS for {tag}"))?;
    let checksums = parse_sha256sums(&sums_text)?;
    let mut names: BTreeSet<String> = checksums.into_keys().collect();
    names.insert("SHA256SUMS".to_owned());

    let assets = names
        .into_iter()
        .map(|name| GitHubAsset {
            browser_download_url: format!("{download_base}/{name}"),
            name,
            api_url: String::new(),
            _size: 0,
        })
        .collect();

    Ok(GitHubRelease {
        tag_name: tag.to_owned(),
        _name: Some(tag.to_owned()),
        _html_url: format!("{base}/releases/tag/{tag}"),
        _published_at: None,
        assets,
        _target_commitish: String::new(),
    })
}

/// Resolve the latest public release through `github.com/<repo>/releases/latest`
/// redirect + `SHA256SUMS`, avoiding the 60 req/hr unauthenticated API limit.
pub(super) fn fetch_latest_release_via_web(
    client: &reqwest::blocking::Client,
    web_base_url: &str,
) -> Result<GitHubRelease> {
    let base = web_base_url.trim_end_matches('/');
    let latest_url = format!("{base}/releases/latest");
    let response = client
        .get(&latest_url)
        .timeout(UPDATE_CHECK_TIMEOUT)
        .send()
        .context("failed to resolve public latest release redirect")?;
    if !response.status().is_success() {
        bail!("public latest release lookup returned {}", response.status());
    }
    let final_path = response.url().path().to_owned();
    let Some(tag) = extract_release_tag_from_path(&final_path) else {
        bail!("public latest release URL did not redirect to a release tag");
    };
    fetch_release_by_tag_via_web(client, base, tag)
}

fn should_try_web_fallback(status: reqwest::StatusCode, is_rate_limited: bool) -> bool {
    is_rate_limited
        || status == reqwest::StatusCode::UNAUTHORIZED
        || status == reqwest::StatusCode::FORBIDDEN
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

/// Fetch the latest release from `api_url`, revalidating `cache` with
/// `If-None-Match`. When `github` is true, rejected tokens retry anonymously
/// and API rate limits / auth / server errors fall back to the public web
/// redirect + `SHA256SUMS`.
pub(super) fn fetch_latest_release_with_fallback(
    client: &reqwest::blocking::Client,
    api_url: &str,
    web_base_url: &str,
    github: bool,
    token: Option<&str>,
    cache: Option<&CachedRelease>,
) -> Result<(GitHubRelease, Option<String>)> {
    let if_none_match = cache.map(|entry| entry.etag.as_str());
    let response = if github {
        send_github_api_get(client, api_url, token, if_none_match)
    } else {
        let mut request = client.get(api_url).timeout(UPDATE_CHECK_TIMEOUT);
        if let Some(etag) = if_none_match {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        request.send()
    }
    .context("Failed to fetch release info")?;

    let status = response.status();
    if let Some(cached) = cache.filter(|_| status == reqwest::StatusCode::NOT_MODIFIED) {
        return Ok((cached.release.clone(), None));
    }

    let rate_limit_backoff = detect_rate_limit_backoff(&response);
    if github && should_try_web_fallback(status, rate_limit_backoff.is_some()) {
        match fetch_latest_release_via_web(client, web_base_url) {
            Ok(release) => {
                crate::logging::info(&format!(
                    "update: resolved latest release {} via public redirect fallback after GitHub API returned {}",
                    release.tag_name, status
                ));
                return Ok((release, None));
            }
            Err(fallback_error) => {
                crate::logging::warn(&format!(
                    "update: public release redirect fallback failed after GitHub API {}: {:#}",
                    status, fallback_error
                ));
            }
        }
    }

    if let Some(backoff) = rate_limit_backoff {
        record_rate_limit_backoff(backoff);
        bail!("{}", RATE_LIMIT_ERROR_PREFIX);
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        if github && token.is_none() {
            bail!(
                "No releases found for {GITHUB_REPO}; a private repository needs GH_TOKEN, GITHUB_TOKEN, or `gh auth login`"
            );
        }
        bail!("No releases found for {GITHUB_REPO}");
    }
    if !status.is_success() {
        bail!("GitHub API error: {}", status);
    }

    let etag = match response.headers().get(reqwest::header::ETAG) {
        Some(value) => match value.to_str() {
            Ok(text) => Some(text.to_owned()),
            Err(_) => None,
        },
        None => None,
    };
    let release: GitHubRelease = response.json().context("Failed to parse release info")?;
    Ok((release, etag))
}

/// Fetch one tagged release from GitHub API with public `SHA256SUMS` fallback.
pub(super) fn fetch_release_by_tag_with_fallback(
    client: &reqwest::blocking::Client,
    api_url: &str,
    web_base_url: &str,
    tag: &str,
    token: Option<&str>,
) -> Result<GitHubRelease> {
    let response = send_github_api_get(client, api_url, token, None).context("Failed to look up the source release")?;
    let status = response.status();
    let rate_limit_backoff = detect_rate_limit_backoff(&response);

    if should_try_web_fallback(status, rate_limit_backoff.is_some()) {
        match fetch_release_by_tag_via_web(client, web_base_url, tag) {
            Ok(release) => {
                crate::logging::info(&format!(
                    "update: resolved release {} via public download fallback after GitHub API returned {}",
                    tag, status
                ));
                return Ok(release);
            }
            Err(fallback_error) => {
                crate::logging::warn(&format!(
                    "update: public release fallback for {} failed after GitHub API {}: {:#}",
                    tag, status, fallback_error
                ));
            }
        }
    }

    if let Some(backoff) = rate_limit_backoff {
        record_rate_limit_backoff(backoff);
        bail!("{}", RATE_LIMIT_ERROR_PREFIX);
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        bail!("main channel needs the published release {tag} on {GITHUB_REPO} for its checksums; none found");
    }
    let release: GitHubRelease = response.error_for_status()?.json()?;
    Ok(release)
}

/// Resolve the 7-character tip SHA of `branch` via `git ls-remote`, avoiding
/// GitHub API quota limits when checking the `main` source channel.
pub(super) fn latest_source_sha_via_git(repo_url: &str, branch: &str) -> Result<String> {
    let ref_name = format!("refs/heads/{branch}");
    let output = Command::new("git")
        .args(["ls-remote", repo_url, &ref_name])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("Failed to run git ls-remote")?;
    if !output.status.success() {
        bail!(
            "git ls-remote failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8(output.stdout).context("git ls-remote returned invalid UTF-8")?;
    parse_ls_remote_short_sha(&stdout, &ref_name)
}

fn parse_ls_remote_short_sha(stdout: &str, ref_name: &str) -> Result<String> {
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        if let (Some(sha), Some(found_ref)) = (parts.next(), parts.next())
            && found_ref == ref_name
            && sha.len() >= 7
            && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Ok(sha[..7].to_ascii_lowercase());
        }
    }
    bail!("git ls-remote did not report {ref_name}");
}

/// Resolve the 7-character tip SHA of `branch` via GitHub API with anonymous
/// retry and `git ls-remote` fallback on API rate-limit or auth/server errors.
pub(super) fn fetch_branch_short_sha(
    client: &reqwest::blocking::Client,
    branch: &str,
    token: Option<&str>,
) -> Result<String> {
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/commits/{branch}");
    let response = send_github_api_get(client, &url, token, None).context("Failed to check the source branch")?;
    let status = response.status();
    let rate_limit_backoff = detect_rate_limit_backoff(&response);
    if should_try_web_fallback(status, rate_limit_backoff.is_some()) {
        let repo_url = format!("https://github.com/{GITHUB_REPO}.git");
        match latest_source_sha_via_git(&repo_url, branch) {
            Ok(sha) => {
                crate::logging::info(&format!(
                    "update: resolved {branch} tip {sha} via git ls-remote after GitHub API returned {status}"
                ));
                return Ok(sha);
            }
            Err(fallback_error) => {
                crate::logging::warn(&format!(
                    "update: git ls-remote fallback failed after GitHub API {status}: {fallback_error:#}"
                ));
            }
        }
    }
    if let Some(backoff) = rate_limit_backoff {
        record_rate_limit_backoff(backoff);
        bail!("{}", RATE_LIMIT_ERROR_PREFIX);
    }
    if !status.is_success() {
        bail!("GitHub API error checking {branch}: {}", status);
    }

    let commit: serde_json::Value = response.json().context("Failed to parse commit info")?;
    Ok(commit["sha"].as_str().unwrap_or("").get(..7).unwrap_or("").to_string())
}

pub(super) fn build_asset_request(
    client: &reqwest::blocking::Client,
    url: &str,
    token: Option<&str>,
) -> reqwest::blocking::RequestBuilder {
    let request = client.get(url).timeout(DOWNLOAD_ATTEMPT_TIMEOUT);
    match token {
        Some(token) => request
            .header(reqwest::header::ACCEPT, "application/octet-stream")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .bearer_auth(token),
        None => request,
    }
}

pub(super) fn should_fallback_asset_status(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN | reqwest::StatusCode::NOT_FOUND
    )
}

pub(super) fn download_small_asset_with_routes(
    client: &reqwest::blocking::Client,
    primary_url: &str,
    primary_token: Option<&str>,
    fallback_url: Option<&str>,
) -> Result<Vec<u8>> {
    let response = build_asset_request(client, primary_url, primary_token).send()?;
    if primary_token.is_some()
        && should_fallback_asset_status(response.status())
        && let Some(fallback) = fallback_url
    {
        crate::logging::warn(&format!(
            "update: GitHub API asset download returned {}; falling back to direct release URL",
            response.status()
        ));
        return Ok(build_asset_request(client, fallback, None)
            .send()?
            .error_for_status()?
            .bytes()?
            .to_vec());
    }
    Ok(response.error_for_status()?.bytes()?.to_vec())
}

/// Authenticate git against the updater repository without putting the token in argv.
fn inject_github_git_auth(command: &mut Command) -> bool {
    command.env("GIT_TERMINAL_PROMPT", "0");
    if std::env::var_os("GIT_CONFIG_COUNT").is_some() {
        crate::logging::info("update: GIT_CONFIG_COUNT already set; not injecting GitHub credentials");
        return false;
    }
    let Some(token) = horon_agent_base::github::github_public_api_token() else {
        return false;
    };
    use base64::Engine as _;
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
    command
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraheader")
        .env("GIT_CONFIG_VALUE_0", format!("AUTHORIZATION: basic {basic}"));
    true
}

fn git_stderr_looks_like_auth_failure(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("authentication failed")
        || lower.contains("could not read username")
        || lower.contains("401")
        || lower.contains("403")
}

pub(super) fn run_github_git_command(args: &[&str], current_dir: &std::path::Path, failure: &str) -> Result<String> {
    let mut command = Command::new("git");
    command.args(args).current_dir(current_dir);
    let used_injected_auth = inject_github_git_auth(&mut command);
    let output = command.output()?;
    if output.status.success() {
        return Ok(String::from_utf8(output.stdout)?.trim().to_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if used_injected_auth && git_stderr_looks_like_auth_failure(&stderr) {
        crate::logging::warn("update: git command failed with injected GitHub credentials; retrying anonymously");
        let mut anonymous = Command::new("git");
        anonymous
            .args(args)
            .current_dir(current_dir)
            .env("GIT_TERMINAL_PROMPT", "0");
        let retry = anonymous.output()?;
        if retry.status.success() {
            return Ok(String::from_utf8(retry.stdout)?.trim().to_owned());
        }
        bail!("{failure}: {}", String::from_utf8_lossy(&retry.stderr));
    }
    bail!("{failure}: {stderr}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn read_http_request(stream: &mut std::net::TcpStream) -> (String, bool) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let path = request_line.split_whitespace().nth(1).unwrap_or("").to_owned();
        let mut has_auth = false;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                break;
            }
            if line.to_ascii_lowercase().starts_with("authorization:") {
                has_auth = true;
            }
        }
        (path, has_auth)
    }

    #[test]
    fn rate_limited_api_falls_back_to_public_web_redirect_and_sha256sums() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let agent_archive = format!("{}.tar.gz", super::super::get_asset_name().trim_end_matches(".exe"));
        let sums_body = format!(
            "{}  {agent_archive}\n{}  horon-tui-macos-aarch64.tar.gz\n",
            "a".repeat(64),
            "b".repeat(64)
        );
        let sums_for_server = sums_body.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..4 {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let (path, has_auth) = read_http_request(&mut stream);
                assert!(!has_auth, "fallback requests must not carry credentials: {path}");
                let response = match path.as_str() {
                    "/api/releases/latest" => "HTTP/1.1 403 Forbidden\r\nX-RateLimit-Remaining: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
                    "/web/releases/latest" => format!(
                        "HTTP/1.1 302 Found\r\nLocation: /web/releases/tag/v9.9.9\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    ),
                    "/web/releases/tag/v9.9.9" => {
                        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_owned()
                    }
                    "/web/releases/download/v9.9.9/SHA256SUMS" => format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sums_for_server}",
                        sums_for_server.len()
                    ),
                    other => panic!("unexpected path: {other}"),
                };
                stream.write_all(response.as_bytes()).unwrap();
            }
        });

        let client = reqwest::blocking::Client::builder().no_proxy().build().unwrap();
        let (release, etag) = fetch_latest_release_with_fallback(
            &client,
            &format!("{base}/api/releases/latest"),
            &format!("{base}/web"),
            true,
            None,
            None,
        )
        .expect("rate-limited API must fall back to public release redirect");
        server.join().unwrap();

        assert_eq!(release.tag_name, "v9.9.9");
        assert_eq!(etag, None);
        let agent = release
            .assets
            .iter()
            .find(|asset| asset.name == agent_archive)
            .expect("agent archive listed from SHA256SUMS");
        assert_eq!(
            agent.browser_download_url,
            format!("{base}/web/releases/download/v9.9.9/{agent_archive}")
        );
        assert!(agent.api_url.is_empty());
        assert!(
            release.assets.iter().any(|asset| asset.name == "SHA256SUMS"),
            "SHA256SUMS asset must be included for downstream verification"
        );
    }

    #[test]
    fn web_fallback_rejects_redirect_without_release_tag() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let (path, _) = read_http_request(&mut stream);
                let response = match path.as_str() {
                    "/web/releases/latest" => {
                        "HTTP/1.1 302 Found\r\nLocation: /login\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_owned()
                    }
                    "/login" => "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nlogin".to_owned(),
                    other => panic!("unexpected path: {other}"),
                };
                stream.write_all(response.as_bytes()).unwrap();
            }
        });

        let client = reqwest::blocking::Client::builder().no_proxy().build().unwrap();
        let error = fetch_latest_release_via_web(&client, &format!("{base}/web"))
            .unwrap_err()
            .to_string();
        server.join().unwrap();
        assert!(error.contains("did not redirect to a release tag"), "{error}");
    }

    #[test]
    fn non_github_rehearsal_url_never_sends_token_or_uses_web_fallback() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_srv = Arc::clone(&calls);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (path, has_auth) = read_http_request(&mut stream);
            calls_srv.fetch_add(1, Ordering::SeqCst);
            assert_eq!(path, "/release.json");
            assert!(!has_auth, "non-GitHub rehearsal URL must never receive a token");
            stream
                .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
        });

        let client = reqwest::blocking::Client::builder().no_proxy().build().unwrap();
        let err = fetch_latest_release_with_fallback(
            &client,
            &format!("{base}/release.json"),
            &format!("{base}/web"),
            false,
            Some("ghp_should_never_be_sent"),
            None,
        )
        .unwrap_err()
        .to_string();
        server.join().unwrap();

        assert!(err.contains("500"), "{err}");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn parse_ls_remote_short_sha_extracts_exact_branch_tip() {
        let output = "deadbeef1234567890abcdef1234567890abcdef\trefs/heads/main\n\
                      0123456789abcdef0123456789abcdef01234567\trefs/heads/dev\n";
        assert_eq!(parse_ls_remote_short_sha(output, "refs/heads/dev").unwrap(), "0123456");
        assert!(parse_ls_remote_short_sha(output, "refs/heads/missing").is_err());
    }
}
