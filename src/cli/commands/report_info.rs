use anyhow::Result;
use serde::Serialize;

use crate::cli::provider_init::{self, ProviderChoice};

#[derive(Debug, Serialize)]
pub(super) struct ProviderListEntry {
    pub(super) id: String,
    pub(super) display_name: String,
    pub(super) auth_kind: Option<String>,
    pub(super) recommended: bool,
    pub(super) aliases: Vec<String>,
    pub(super) detail: Option<String>,
}

#[derive(Debug, Serialize)]
struct ProviderListReport {
    providers: Vec<ProviderListEntry>,
}

#[derive(Debug, Serialize)]
struct ProviderCurrentReport {
    requested_provider: String,
    requested_model: Option<String>,
    resolved_provider: String,
    selected_model: String,
    ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    unavailable_reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct VersionReport {
    pub(super) version: String,
    pub(super) semver: String,
    pub(super) base_semver: String,
    pub(super) update_semver: String,
    pub(super) git_hash: String,
    pub(super) git_tag: String,
    pub(super) build_time: String,
    pub(super) git_date: String,
    pub(super) release_build: bool,
    pub(super) build_profile: String,
    pub(super) build_features: Vec<String>,
}

#[derive(Debug, Serialize)]
struct UsageLimitReport {
    name: String,
    usage_percent: f32,
    resets_at: Option<String>,
    reset_in: Option<String>,
}

#[derive(Debug, Serialize)]
struct UsageProviderReport {
    provider_name: String,
    limits: Vec<UsageLimitReport>,
    extra_info: Vec<(String, String)>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct UsageReport {
    providers: Vec<UsageProviderReport>,
}

pub(super) fn run_provider_list_command(emit_json: bool) -> Result<()> {
    let providers = list_cli_providers();

    if emit_json {
        let report = ProviderListReport { providers };
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for provider in providers {
            if let Some(detail) = provider.detail.as_deref() {
                println!("{}\t{}\t{}", provider.id, provider.display_name, detail);
            } else {
                println!("{}\t{}", provider.id, provider.display_name);
            }
        }
    }

    Ok(())
}

pub(super) async fn run_provider_current_command(
    choice: &ProviderChoice,
    model: Option<&str>,
    emit_json: bool,
) -> Result<()> {
    let provider = provider_init::init_provider_for_diagnostics(choice, model).await?;
    let unavailable_reason = provider.unavailable_reason();
    let report = ProviderCurrentReport {
        requested_provider: choice.as_arg_value().to_string(),
        requested_model: model.map(str::to_string),
        resolved_provider: crate::provider_catalog::runtime_provider_display_name(provider.name()),
        selected_model: provider.model(),
        ready: unavailable_reason.is_none(),
        unavailable_reason,
    };

    if emit_json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("requested_provider\t{}", report.requested_provider);
        if let Some(requested_model) = report.requested_model.as_deref() {
            println!("requested_model\t{}", requested_model);
        }
        println!("resolved_provider\t{}", report.resolved_provider);
        println!("selected_model\t{}", report.selected_model);
        println!("ready\t{}", report.ready);
        if let Some(reason) = report.unavailable_reason.as_deref() {
            println!("unavailable_reason\t{reason}");
        }
    }

    Ok(())
}

/// Features this binary was compiled with, in declaration order.
///
/// Enumerated from the crate's own `cfg!` flags because a build script in a leaf
/// crate cannot see the features of the crate being measured. An A/B run
/// compares this list before timing anything, so a feature difference is refused
/// instead of being attributed to the change under test.
fn enabled_features() -> Vec<String> {
    let flags = [
        ("embeddings", cfg!(feature = "embeddings")),
        ("jemalloc", cfg!(feature = "jemalloc")),
        ("jemalloc-prof", cfg!(feature = "jemalloc-prof")),
        (
            "legacy-session-index-import",
            cfg!(feature = "legacy-session-index-import"),
        ),
        ("pdf", cfg!(feature = "pdf")),
    ];
    flags
        .into_iter()
        .filter(|&(_, enabled)| enabled)
        .map(|(name, _)| name.to_string())
        .collect()
}

pub(super) fn run_version_command(emit_json: bool) -> Result<()> {
    let report = VersionReport {
        version: horon_agent_build_meta::version().to_string(),
        semver: horon_agent_build_meta::semver().to_string(),
        base_semver: horon_agent_build_meta::base_semver().to_string(),
        update_semver: horon_agent_build_meta::update_semver().to_string(),
        git_hash: horon_agent_build_meta::git_hash().to_string(),
        git_tag: horon_agent_build_meta::git_tag().to_string(),
        build_time: crate::build::current_binary_build_time_string().unwrap_or_else(|| "unknown".to_string()),
        git_date: horon_agent_build_meta::git_date().to_string(),
        release_build: horon_agent_build_meta::is_release_build(),
        build_profile: horon_agent_build_meta::build_profile().to_string(),
        build_features: enabled_features(),
    };

    if emit_json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("version\t{}", report.version);
        println!("semver\t{}", report.semver);
        println!("base_semver\t{}", report.base_semver);
        println!("update_semver\t{}", report.update_semver);
        println!("git_hash\t{}", report.git_hash);
        println!("git_tag\t{}", report.git_tag);
        println!("build_time\t{}", report.build_time);
        println!("git_date\t{}", report.git_date);
        println!("release_build\t{}", report.release_build);
        println!("build_profile\t{}", report.build_profile);
        println!("build_features\t{}", report.build_features.join(","));
    }

    Ok(())
}

pub(super) async fn run_usage_command(emit_json: bool) -> Result<()> {
    let providers = crate::usage::fetch_all_provider_usage().await;

    let report = UsageReport {
        providers: providers.iter().map(usage_provider_report).collect(),
    };

    if emit_json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    if report.providers.is_empty() {
        println!("No connected providers");
        println!();
        println!("Next steps:");
        println!("- Run `horon-agent provider add` to store an OpenAI-compatible profile and its API key.");
        return Ok(());
    }

    // One width for every provider section so bars align across them.
    let name_width = report
        .providers
        .iter()
        .flat_map(|provider| provider.limits.iter())
        .map(|limit| limit.name.chars().count())
        .max()
        .unwrap_or(0);

    for (idx, provider) in report.providers.iter().enumerate() {
        if idx > 0 {
            println!();
        }

        println!("{}", provider.provider_name);
        println!("{}", "-".repeat(provider.provider_name.chars().count()));

        if let Some(error) = &provider.error {
            println!("error: {}", error);
            continue;
        }

        if provider.limits.is_empty() && provider.extra_info.is_empty() {
            println!("No usage data available.");
            continue;
        }

        for limit in &provider.limits {
            match limit.reset_in.as_deref() {
                Some(reset_in) => println!(
                    "{:<width$}: {} (resets in {})",
                    limit.name,
                    crate::usage::format_usage_bar(limit.usage_percent, 15),
                    reset_in,
                    width = name_width
                ),
                None => println!(
                    "{:<width$}: {}",
                    limit.name,
                    crate::usage::format_usage_bar(limit.usage_percent, 15),
                    width = name_width
                ),
            }
        }

        if !provider.extra_info.is_empty() {
            if !provider.limits.is_empty() {
                println!();
            }
            for (key, value) in &provider.extra_info {
                println!("{}: {}", key, value);
            }
        }
    }

    Ok(())
}

fn usage_provider_report(provider: &crate::usage::ProviderUsage) -> UsageProviderReport {
    UsageProviderReport {
        provider_name: provider.provider_name.clone(),
        limits: provider
            .limits
            .iter()
            .map(|limit| UsageLimitReport {
                name: limit.name.clone(),
                usage_percent: limit.usage_percent,
                resets_at: limit.resets_at.clone(),
                reset_in: limit.resets_at.as_deref().map(crate::usage::format_reset_time),
            })
            .collect(),
        extra_info: provider.extra_info.clone(),
        error: provider.error.clone(),
    }
}

pub(super) fn list_cli_providers() -> Vec<ProviderListEntry> {
    let mut entries = vec![ProviderListEntry {
        id: "auto".to_string(),
        display_name: "Auto-detect".to_string(),
        auth_kind: None,
        recommended: true,
        aliases: Vec::new(),
        detail: Some("Follow [provider] default_provider or --provider-profile".to_string()),
    }];
    for profile in crate::provider_catalog::openai_compatible_profiles() {
        entries.push(ProviderListEntry {
            id: profile.id.to_string(),
            display_name: profile.display_name.to_string(),
            auth_kind: Some(if profile.requires_api_key { "api key" } else { "none" }.to_string()),
            recommended: false,
            aliases: Vec::new(),
            detail: Some(profile.api_base.to_string()),
        });
    }
    for (name, profile) in &crate::config::config().providers {
        entries.push(ProviderListEntry {
            id: name.clone(),
            display_name: format!("{name} (config.toml)"),
            auth_kind: Some(
                match profile.auth {
                    crate::config::NamedProviderAuth::Bearer => "bearer",
                    crate::config::NamedProviderAuth::Header => "header",
                    crate::config::NamedProviderAuth::None => "none",
                }
                .to_string(),
            ),
            recommended: false,
            aliases: Vec::new(),
            detail: Some(profile.base_url.clone()),
        });
    }
    entries
}
