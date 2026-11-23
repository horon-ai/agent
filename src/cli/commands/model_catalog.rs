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
        let mut report = ProfileReport {
            profile: name,
            source: "configuration",
            status: "local",
            reason: None,
            models: profile
                .models
                .iter()
                .map(|model| model.id.trim())
                .chain(profile.default_model.iter().map(|model| model.trim()))
                .filter(|model| !model.is_empty())
                .map(str::to_owned)
                .collect(),
        };
        if filter.refresh {
            match fetch_named_openai_models(name, profile).await {
                Ok(Some(models)) => {
                    report.source = "endpoint";
                    report.status = "refreshed";
                    report.models = models.into_iter().collect();
                }
                Ok(None) => {
                    report.status = "unsupported";
                    report.reason = Some("profile does not use OpenAI model discovery".to_owned());
                }
                Err(error) => {
                    report.status = "failed";
                    report.reason = Some(error.to_string());
                }
            }
        }
        if let Some(search) = search.as_deref() {
            report.models.retain(|model| model.to_lowercase().contains(search));
        }
        reports.push(report);
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
