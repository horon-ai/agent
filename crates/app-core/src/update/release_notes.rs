//! Read-only lookup of one published release's notes.
//!
//! This is an explicit, user-requested read. It never downloads or installs a
//! build, so a failure cannot change or restart the running release.

use anyhow::{Context, Result};
use serde::Deserialize;

use super::{GITHUB_REPO, github_api_request, updater_client};

/// The notes of one published release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseNotes {
    pub tag: String,
    pub name: Option<String>,
    pub url: String,
    pub published_at: Option<String>,
    pub body: String,
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    name: Option<String>,
    published_at: Option<String>,
    body: Option<String>,
}

/// The release page for `tag`, usable as a pointer when notes cannot be read.
pub fn release_url(tag: &str) -> String {
    format!("https://github.com/{GITHUB_REPO}/releases/tag/{tag}")
}

/// Accept `0.1.5` or `v0.1.5` and return the `v`-prefixed tag. Only characters
/// a version tag can carry are allowed, so the value is safe in a URL path.
pub fn normalize_tag(version: &str) -> Result<String> {
    let version = version.trim();
    let bare = version.strip_prefix('v').unwrap_or(version);
    let valid = !bare.is_empty()
        && bare.chars().next().is_some_and(|c| c.is_ascii_digit())
        && bare
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
    anyhow::ensure!(valid, "{version:?} is not a release version such as 0.1.5");
    Ok(format!("v{bare}"))
}

/// Look up the notes for `tag`. Errors are one short line with no URL or token.
pub fn fetch_release_notes_blocking(tag: &str) -> Result<ReleaseNotes> {
    let tag = normalize_tag(tag)?;
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/tags/{tag}");
    let response = github_api_request(updater_client()?, &url).send().map_err(|error| {
        anyhow::anyhow!(if error.is_timeout() {
            "GitHub did not answer in time"
        } else {
            "GitHub could not be reached"
        })
    })?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!("no published release is named {tag}");
    }
    anyhow::ensure!(status.is_success(), "GitHub returned HTTP {}", status.as_u16());
    let raw: RawRelease = response.json().context("GitHub returned an unreadable release")?;
    anyhow::ensure!(raw.tag_name == tag, "GitHub returned a different release than {tag}");
    Ok(ReleaseNotes {
        tag: raw.tag_name,
        name: raw.name.map(|name| plain_text(&name)),
        url: release_url(&tag),
        published_at: raw.published_at.map(|date| plain_text(&date)),
        body: plain_text(raw.body.as_deref().unwrap_or("")),
    })
}

/// Drop terminal control characters and bidirectional overrides, keeping line
/// breaks and tabs, so release text cannot move the cursor or recolor output.
pub fn plain_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .filter(|c| {
            matches!(c, '\n' | '\t')
                || !(c.is_control() || matches!(*c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_become_tags_and_unsafe_values_are_refused() {
        assert_eq!(normalize_tag("0.1.5").unwrap(), "v0.1.5");
        assert_eq!(normalize_tag(" v0.1.5-rc.1+build ").unwrap(), "v0.1.5-rc.1+build");
        for bad in ["", "v", "../x", "0.1/5", "latest", "0.1.5?x=1"] {
            assert!(normalize_tag(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn release_text_loses_escapes_but_keeps_lines() {
        let text = plain_text("a\u{1b}[31mred\u{1b}[0m\r\nb\tc\u{202e}d\u{7}");
        assert_eq!(text, "a[31mred[0m\nb\tcd");
    }
}
