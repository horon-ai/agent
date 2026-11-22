use std::io::{self, Write};

use anyhow::Result;
use serde::Serialize;

use crate::update::{ReleaseNotes, fetch_release_notes_blocking, normalize_tag, release_url};

#[derive(Serialize)]
struct Report<'a> {
    version: &'a str,
    running_version: &'a str,
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    published_at: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<&'a str>,
}

/// Print the notes of `version` (the running release by default). This only
/// reads from GitHub: nothing is downloaded or installed, and a failed lookup
/// prints the version and release page instead of changing anything.
pub(super) fn run(version: Option<&str>, json: bool) -> Result<()> {
    let running = horon_agent_build_meta::version();
    let tag = match version {
        Some(version) => normalize_tag(version)?,
        None => normalize_tag(horon_agent_build_meta::base_semver())?,
    };
    // The lookup uses a blocking client, which must not run on a runtime thread.
    let lookup = {
        let tag = tag.clone();
        std::thread::spawn(move || fetch_release_notes_blocking(&tag))
            .join()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("the release lookup stopped unexpectedly")))
    };
    report(&tag, running, lookup, json, io::stdout().lock())
}

fn report(tag: &str, running: &str, lookup: Result<ReleaseNotes>, json: bool, mut out: impl Write) -> Result<()> {
    let failure = lookup.as_ref().err().map(|error| error.to_string());
    let notes = match &lookup {
        Ok(notes) => Some(notes),
        Err(_) => None,
    };
    let reason = failure.as_deref();
    if json {
        let report = Report {
            version: tag,
            running_version: running,
            available: notes.is_some(),
            reason,
            url: release_url(tag),
            name: notes.and_then(|notes| notes.name.as_deref()),
            published_at: notes.and_then(|notes| notes.published_at.as_deref()),
            notes: notes.map(|notes| notes.body.as_str()),
        };
        serde_json::to_writer_pretty(&mut out, &report)?;
        writeln!(out)?;
        return Ok(());
    }
    match notes {
        Some(notes) => {
            writeln!(out, "{}", notes.name.as_deref().unwrap_or(tag))?;
            if let Some(date) = notes.published_at.as_deref() {
                writeln!(out, "Published {date}")?;
            }
            writeln!(out, "\n{}", notes.body.trim_end())?;
        }
        None => {
            writeln!(
                out,
                "Release notes for {tag} are unavailable: {}.",
                reason.unwrap_or("unknown reason")
            )?;
            writeln!(out, "Running version: {running}\nRelease page: {}", release_url(tag))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notes() -> ReleaseNotes {
        ReleaseNotes {
            tag: "v0.1.5".to_owned(),
            name: Some("v0.1.5".to_owned()),
            url: release_url("v0.1.5"),
            published_at: Some("2026-11-20T00:00:00Z".to_owned()),
            body: "- fixed a thing\n".to_owned(),
        }
    }

    fn render(lookup: Result<ReleaseNotes>, json: bool) -> String {
        let mut out = Vec::new();
        report("v0.1.5", "0.1.4", lookup, json, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn notes_print_with_their_date() {
        assert_eq!(
            render(Ok(notes()), false),
            "v0.1.5\nPublished 2026-11-20T00:00:00Z\n\n- fixed a thing\n"
        );
    }

    #[test]
    fn a_failed_lookup_names_the_version_and_the_release_page() {
        let text = render(Err(anyhow::anyhow!("GitHub could not be reached")), false);
        assert_eq!(
            text,
            "Release notes for v0.1.5 are unavailable: GitHub could not be reached.\n\
             Running version: 0.1.4\n\
             Release page: https://github.com/horon-ai/agent/releases/tag/v0.1.5\n"
        );
        let json: serde_json::Value = serde_json::from_str(&render(Err(anyhow::anyhow!("down")), true)).unwrap();
        assert_eq!(json["available"], false);
        assert_eq!(json["reason"], "down");
        assert_eq!(json["version"], "v0.1.5");
        assert!(json.get("notes").is_none());
    }
}
