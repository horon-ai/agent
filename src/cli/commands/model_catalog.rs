use std::collections::BTreeSet;
use std::io::{self, Write};

use anyhow::Result;
use serde::Serialize;

use crate::cli::args::ModelListFilter;
use crate::provider::external::fetch_named_openai_models;

#[derive(Serialize)]
struct ProfileReport<'a> {
    profile: &'a str,
    source: &'static str,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    models: BTreeSet<String>,
}

pub(super) async fn run(filter: &ModelListFilter, json: bool) -> Result<()> {
    let mut reports = Vec::new();
    let search = filter.search.as_deref().map(str::to_lowercase);
    for (name, profile) in &crate::config::config().providers {
        if filter
            .profile
            .as_deref()
            .is_some_and(|wanted| !wanted.eq_ignore_ascii_case(name))
        {
            continue;
        }
        let search_lower = search.as_deref();
        let configured_models = || -> BTreeSet<String> {
            profile
                .models
                .iter()
                .map(|model| model.id.trim())
                .chain(profile.default_model.iter().map(|model| model.trim()))
                .filter(|model| {
                    !model.is_empty() && search_lower.is_none_or(|needle| contains_case_insensitive(model, needle))
                })
                .map(str::to_owned)
                .collect()
        };
        let (source, status, reason, models) = if filter.refresh {
            match fetch_named_openai_models(name, profile).await {
                Ok(Some(models)) => (
                    "endpoint",
                    "refreshed",
                    None,
                    models
                        .into_iter()
                        .filter(|model| search_lower.is_none_or(|needle| contains_case_insensitive(model, needle)))
                        .collect(),
                ),
                Ok(None) => (
                    "configuration",
                    "unsupported",
                    Some("profile does not use OpenAI model discovery".to_owned()),
                    configured_models(),
                ),
                Err(error) => ("configuration", "failed", Some(error.to_string()), configured_models()),
            }
        } else {
            ("configuration", "local", None, configured_models())
        };
        reports.push(ProfileReport {
            profile: name,
            source,
            status,
            reason,
            models,
        });
    }
    if let Some(profile) = filter.profile.as_deref().filter(|_| reports.is_empty()) {
        anyhow::bail!("no configured profile matches {profile:?}");
    }
    let mut out = io::stdout().lock();
    if json {
        serde_json::to_writer_pretty(&mut out, &serde_json::json!({ "profiles": reports }))?;
        writeln!(out)?;
    } else {
        for report in reports {
            if filter.refresh {
                if let Some(reason) = report.reason {
                    writeln!(out, "{:?}: {} ({reason})", report.profile, report.status)?;
                } else {
                    writeln!(out, "{:?}: {}", report.profile, report.status)?;
                }
            }
            for model in report.models {
                writeln!(out, "{model}")?;
            }
        }
    }
    Ok(())
}

fn contains_case_insensitive(haystack: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if haystack.is_ascii() && needle_lower.is_ascii() {
        return haystack
            .as_bytes()
            .windows(needle_lower.len())
            .any(|window| window.eq_ignore_ascii_case(needle_lower.as_bytes()));
    }
    haystack.to_lowercase().contains(needle_lower)
}

#[cfg(test)]
mod tests {
    use super::contains_case_insensitive;

    #[test]
    fn contains_case_insensitive_handles_ascii_unicode_and_length_edges() {
        assert!(contains_case_insensitive("GPT-4o-Mini", ""));
        assert!(contains_case_insensitive("GPT-4o-Mini", "4o-mi"));
        assert!(!contains_case_insensitive("gpt-4", "gpt-4-turbo"));
        assert!(!contains_case_insensitive("gpt-4o", "claude"));
        assert!(contains_case_insensitive("ÄÖÜ-model", "äöü"));
    }
}
