use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use crate::config::NamedProviderConfig;
use crate::provider::external::fetch_named_openai_models;
use crate::provider_catalog::{
    api_base_uses_localhost, is_safe_env_key_name, load_api_key_from_env_or_config, normalize_api_base,
};

/// Distinct outcomes of one profile check. None carries a secret or an upstream body.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Outcome {
    /// Configuration is complete; connectivity was not requested.
    Configured,
    /// Configuration is complete and the endpoint answered.
    Reachable,
    InvalidBaseUrl,
    /// The profile needs a key but names no variable, or the variable is unset.
    MissingCredential,
    /// The endpoint did not answer successfully.
    Unreachable,
    /// The wire has no model-list check; configuration was still validated.
    ConnectivityUnsupported,
}

#[derive(Serialize)]
struct ProfileDiagnosis {
    profile: String,
    outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key_env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::Reachable => "reachable",
            Self::InvalidBaseUrl => "invalid_base_url",
            Self::MissingCredential => "missing_credential",
            Self::Unreachable => "unreachable",
            Self::ConnectivityUnsupported => "connectivity_unsupported",
        }
    }
}

fn credential_state(profile: &NamedProviderConfig) -> Result<(), Option<String>> {
    let local = normalize_api_base(&profile.base_url).is_some_and(|base| api_base_uses_localhost(&base));
    let needs_key = profile.requires_api_key.unwrap_or(!local);
    if !needs_key || profile.api_key.as_deref().is_some_and(|key| !key.trim().is_empty()) {
        return Ok(());
    }
    let Some(name) = profile.api_key_env.as_deref().map(str::trim).filter(|n| !n.is_empty()) else {
        return Err(None);
    };
    let file = profile.env_file.as_deref().map(str::trim).filter(|f| !f.is_empty());
    let found =
        is_safe_env_key_name(name) && load_api_key_from_env_or_config(name, file.unwrap_or("providers.env")).is_some();
    if found { Ok(()) } else { Err(Some(name.to_owned())) }
}

async fn diagnose(name: &str, profile: &NamedProviderConfig, check: bool) -> ProfileDiagnosis {
    let mut report = ProfileDiagnosis {
        profile: name.to_owned(),
        outcome: Outcome::Configured,
        api_key_env: profile.api_key_env.clone(),
        detail: None,
    };
    if normalize_api_base(&profile.base_url).is_none() {
        report.outcome = Outcome::InvalidBaseUrl;
        report.detail = Some("base_url must be an absolute https:// URL (or http on a private host)".into());
        return report;
    }
    if let Err(variable) = credential_state(profile) {
        report.outcome = Outcome::MissingCredential;
        report.detail = Some(match variable {
            Some(variable) => format!("{variable} is not set and is not in the profile's env file"),
            None => "the profile requires a key but sets no api_key_env".into(),
        });
        return report;
    }
    if !check {
        return report;
    }
    match fetch_named_openai_models(name, profile).await {
        Ok(Some(_)) => report.outcome = Outcome::Reachable,
        Ok(None) => report.outcome = Outcome::ConnectivityUnsupported,
        Err(error) => {
            report.outcome = Outcome::Unreachable;
            // Messages from the model-list client are fixed phrases or an HTTP status.
            report.detail = Some(error.to_string());
        }
    }
    report
}

/// Exit status is nonzero when any profile, or the absence of profiles, needs attention.
pub(crate) async fn run_provider_diagnose_command(check: bool, json: bool) -> Result<()> {
    let providers = &crate::config::config().providers;
    let mut reports = Vec::with_capacity(providers.len());
    for (name, profile) in providers {
        reports.push(diagnose(name, profile, check).await);
    }
    let healthy = !reports.is_empty()
        && reports.iter().all(|r| {
            matches!(
                r.outcome,
                Outcome::Configured | Outcome::Reachable | Outcome::ConnectivityUnsupported
            )
        });
    let mut out = std::io::stdout().lock();
    if json {
        let status = if providers.is_empty() {
            "no_providers"
        } else if healthy {
            "ok"
        } else {
            "attention"
        };
        serde_json::to_writer_pretty(&mut out, &serde_json::json!({ "status": status, "profiles": reports }))?;
        writeln!(out)?;
    } else if providers.is_empty() {
        writeln!(
            out,
            "no_providers\tadd a [providers.<name>] table or run `horon provider add <name> --base-url <url> --model <id>`"
        )?;
    } else {
        for r in &reports {
            match &r.detail {
                Some(detail) => writeln!(out, "{}\t{}\t{detail}", r.profile, r.outcome.label())?,
                None => writeln!(out, "{}\t{}", r.profile, r.outcome.label())?,
            }
        }
    }
    drop(out);
    if !healthy {
        std::process::exit(1);
    }
    Ok(())
}
