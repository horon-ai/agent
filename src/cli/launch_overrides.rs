//! Per-invocation model role and prewalk overrides.
//!
//! `--role <name>=<model>`, `--prewalk <into>` and `--no-prewalk` change the
//! effective configuration for this process only. They are applied as
//! documented environment overrides before the first role or prewalk read, so
//! nothing is written to `config.toml`: a settings save reads the file layer,
//! which never sees them.
//!
//! `--role default=<model>` names the session model, the resolution slot
//! `--model` already fills, so it sets the launch model. Every other role is
//! rewritten inside the active `[providers.<name>]` profile, which is where
//! roles live.

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

use super::args::Args;

/// Launch flags that override roles and prewalk for this process only.
#[derive(clap::Args, Debug)]
pub(crate) struct LaunchOverrideArgs {
    /// Override a model role for this launch only: NAME=MODEL. Repeatable.
    ///
    /// Roles are default, plan, aux, judge and image. `default` names the
    /// session model, like --model. The value is never written to config.toml.
    #[arg(long = "role", global = true, value_name = "NAME=MODEL", action = clap::ArgAction::Append)]
    pub(crate) role: Vec<String>,

    /// Disable the `[prewalk]` handoff for this launch only.
    #[arg(long = "no-prewalk", global = true)]
    pub(crate) no_prewalk: bool,

    /// Enable the `[prewalk]` handoff into this model or `role:<name>` for this launch only.
    #[arg(long = "prewalk", global = true, value_name = "INTO")]
    pub(crate) prewalk: Option<String>,

    /// Skip AGENTS.md discovery (project and global) for this launch only.
    #[arg(long = "no-context-files", global = true)]
    pub(crate) no_context_files: bool,

    /// Skip imported rules, prompt overlays and preferred-tools guidance for this launch only.
    #[arg(long = "no-rules", global = true)]
    pub(crate) no_rules: bool,

    /// Hide every skill from this launch only; wins over --skills.
    #[arg(long = "no-skills", global = true)]
    pub(crate) no_skills: bool,

    /// Keep only skills matching these comma-separated names or `*` patterns for this launch only. Repeatable.
    #[arg(long = "skills", global = true, value_name = "PATTERNS", action = clap::ArgAction::Append)]
    pub(crate) skills: Vec<String>,
}
use super::output;
use super::provider_init::ProviderChoice;
use crate::config::{
    Config, ModelReference, ModelRole, ModelRoles, NamedProviderConfig, PrewalkConfig, parse_model_reference,
    resolve_role_model,
};

/// Profile the role overrides belong to, so the config layer never has to guess
/// the active profile from provider markers set at a different time.
pub(crate) const ROLE_PROFILE_ENV: &str = "HORON_AGENT_LAUNCH_ROLE_PROFILE";

const SUPPORTED_ROLES: [ModelRole; 5] = [
    ModelRole::Default,
    ModelRole::Plan,
    ModelRole::Aux,
    ModelRole::Judge,
    ModelRole::Image,
];

/// What the prewalk flags decided for this launch.
#[derive(Debug, Default, PartialEq, Eq)]
struct PrewalkOverride {
    /// `None` when neither prewalk flag was given.
    enabled: Option<bool>,
    /// The `into` value `--prewalk` selected, when it takes effect.
    into: Option<String>,
    /// The `--prewalk` value `--no-prewalk` superseded.
    ignored: Option<String>,
    /// The model the effective `into` resolves to, for the report.
    target: Option<String>,
}

/// Apply the launch overrides, or fail before anything reads a model.
pub(crate) fn apply(args: &mut Args) -> Result<()> {
    apply_discovery(&args.launch)?;
    if args.launch.role.is_empty() && args.launch.prewalk.is_none() && !args.launch.no_prewalk {
        return Ok(());
    }
    let config = crate::config::config();
    let profile_name = active_profile_name(args, config);
    let profile = profile_name.as_deref().and_then(|name| config.providers.get(name));
    let roles = parse_role_overrides(&args.launch.role)?;
    if roles.iter().any(|(role, _)| *role != ModelRole::Default) && profile.is_none() {
        bail!(
            "--role changes a model role, and roles live inside one provider profile; \
             add [providers.<name>] with --provider-profile <name> or [provider] default_provider"
        );
    }
    for (_, model) in &roles {
        validate_model(profile_name.as_deref(), profile, model)?;
    }
    let prewalk = effective_prewalk(config, args, profile_name.as_deref(), profile, &roles)?;
    apply_default_role(args, &roles)?;

    for (role, model) in &roles {
        crate::env::set_var(role.launch_env_key(), model);
    }
    if let Some(name) = profile_name.as_deref().filter(|_| !roles.is_empty()) {
        crate::env::set_var(ROLE_PROFILE_ENV, name);
    }
    if let Some(enabled) = prewalk.enabled {
        crate::env::set_var(PrewalkConfig::ENABLED_ENV, if enabled { "1" } else { "0" });
    }
    if let Some(into) = &prewalk.into {
        crate::env::set_var(PrewalkConfig::INTO_ENV, into);
    }
    // The env keys are documented overrides, so a reload re-applies them. The
    // invalidation makes the already-parsed cache observe them for this launch.
    crate::config::invalidate_config_cache();
    report(&roles, &prewalk);
    Ok(())
}

/// Parse every `--role` occurrence into a role and the model that wins.
///
/// A repeated role keeps its last value, so a wrapper can append its own value
/// after a default one without rewriting earlier arguments.
fn parse_role_overrides(values: &[String]) -> Result<Vec<(ModelRole, String)>> {
    let mut roles: Vec<(ModelRole, String)> = Vec::new();
    for value in values {
        let Some((name, model)) = value.split_once('=') else {
            bail!("--role expects NAME=MODEL, got {value:?}; supported roles are default, plan, aux, judge and image");
        };
        let role = role_from_name(name.trim())?;
        let model = model.trim();
        if model.is_empty() {
            bail!("--role {name} needs a model id after `=`");
        }
        match roles.iter_mut().find(|(existing, _)| *existing == role) {
            Some((_, current)) => *current = model.to_string(),
            None => roles.push((role, model.to_string())),
        }
    }
    Ok(roles)
}

fn role_from_name(name: &str) -> Result<ModelRole> {
    SUPPORTED_ROLES
        .into_iter()
        .find(|role| role.as_str() == name)
        .ok_or_else(|| {
            anyhow::anyhow!("unknown model role {name:?}; supported roles are default, plan, aux, judge and image")
        })
}

/// A prewalk `into` role, limited to the roles the handoff already understands.
fn prewalk_role(name: &str) -> Result<ModelRole> {
    match name {
        "default" => Ok(ModelRole::Default),
        "plan" => Ok(ModelRole::Plan),
        "aux" => Ok(ModelRole::Aux),
        other => bail!("prewalk role {other:?} is not supported; use default, plan or aux, or a model id"),
    }
}

/// Reject a model the active profile's own catalog does not offer.
///
/// An empty catalog cannot prove a model wrong, exactly as `--model` treats it:
/// the endpoint stays the authority until `[[providers.<name>.models]]` lists
/// ids. This is the check that turns a typo into a launch failure instead of a
/// request the endpoint rejects later.
fn validate_model(profile_name: Option<&str>, profile: Option<&NamedProviderConfig>, model: &str) -> Result<()> {
    let Some(profile) = profile else {
        return Ok(());
    };
    let offered = profile
        .models
        .iter()
        .map(|entry| entry.id.trim())
        .filter(|id| !id.is_empty())
        .collect::<Vec<_>>();
    if !offered.is_empty() && !offered.contains(&model) {
        let name = profile_name.unwrap_or("<profile>");
        bail!(
            "model {model:?} is not offered by provider profile {name:?} (models: {}); \
             fix the override or add the model to [[providers.{name}.models]]",
            offered.join(", ")
        );
    }
    Ok(())
}

/// `--role default=<model>` fills the slot `--model` fills.
fn apply_default_role(args: &mut Args, roles: &[(ModelRole, String)]) -> Result<()> {
    let Some((_, model)) = roles.iter().find(|(role, _)| *role == ModelRole::Default) else {
        return Ok(());
    };
    match args.model.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        Some(existing) if existing != model => {
            bail!("--role default={model} conflicts with --model {existing}; both name the session model, so pass one")
        }
        _ => {
            args.model = Some(model.clone());
            Ok(())
        }
    }
}

/// Resolve the prewalk flags, with `--no-prewalk` taking precedence.
fn effective_prewalk(
    config: &Config,
    args: &Args,
    profile_name: Option<&str>,
    profile: Option<&NamedProviderConfig>,
    roles: &[(ModelRole, String)],
) -> Result<PrewalkOverride> {
    if args.launch.no_prewalk {
        return Ok(PrewalkOverride {
            enabled: Some(false),
            into: None,
            ignored: args.launch.prewalk.clone(),
            target: None,
        });
    }
    let Some(raw) = args.launch.prewalk.as_deref() else {
        return Ok(PrewalkOverride::default());
    };
    let into = raw.trim();
    if into.is_empty() {
        bail!("--prewalk needs a model id or role:<name>");
    }
    let target = match parse_model_reference(into) {
        ModelReference::Model(model) => {
            validate_model(profile_name, profile, model)?;
            Some(model.to_string())
        }
        ModelReference::Role(name) => {
            let role = prewalk_role(name)?;
            let effective = effective_roles(profile, roles);
            let resolved = resolve_role_model(
                role,
                None,
                &effective,
                profile.and_then(|profile| profile.default_model.as_deref()),
                config.provider.default_model.as_deref(),
            )
            .unwrap_or_else(|_| String::new());
            (!resolved.is_empty()).then_some(resolved)
        }
    };
    Ok(PrewalkOverride {
        enabled: Some(true),
        into: Some(into.to_string()),
        ignored: None,
        target,
    })
}

fn effective_roles(profile: Option<&NamedProviderConfig>, roles: &[(ModelRole, String)]) -> ModelRoles {
    let mut effective = match profile {
        Some(profile) => profile.model_roles.roles.clone(),
        None => ModelRoles::default(),
    };
    for (role, model) in roles {
        effective.set(*role, model.clone());
    }
    effective
}

/// The named profile this launch resolves to, if any.
fn active_profile_name(args: &Args, config: &Config) -> Option<String> {
    env_nonempty(&[
        "HORON_AGENT_NAMED_PROVIDER_PROFILE",
        "HORON_AGENT_PROVIDER_PROFILE_NAME",
    ])
    .or_else(|| nonempty(args.provider_profile.as_deref()))
    .or_else(|| match &args.provider {
        ProviderChoice::Profile(name) => nonempty(Some(name)),
        ProviderChoice::Auto | ProviderChoice::OpenaiCompatible => None,
    })
    .or_else(|| nonempty(config.provider.default_provider.as_deref()))
}

/// The first key that names a value, ignoring unset and blank variables.
fn env_nonempty(keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn nonempty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Report the effective target on stderr so a scripted run can tell what it got.
fn report(roles: &[(ModelRole, String)], prewalk: &PrewalkOverride) {
    for (role, model) in roles {
        output::stderr_info(format!(
            "launch override: role {role} -> {model} (this invocation only)"
        ));
    }
    match (prewalk.enabled, prewalk.into.as_deref(), prewalk.ignored.as_deref()) {
        (Some(false), _, Some(ignored)) => output::stderr_info(format!(
            "launch override: prewalk off (this invocation only); --prewalk {ignored} ignored"
        )),
        (Some(false), _, None) => output::stderr_info("launch override: prewalk off (this invocation only)"),
        (Some(true), Some(into), _) => match prewalk.target.as_deref() {
            Some(target) => output::stderr_info(format!(
                "launch override: prewalk into {into} -> {target} (this invocation only)"
            )),
            None => output::stderr_info(format!("launch override: prewalk into {into} (this invocation only)")),
        },
        _ => {}
    }
}

/// Turn off optional instruction discovery for this process and name what it skips.
///
/// The base system prompt is not optional and is never skipped. Only sources
/// that exist are reported, so the line says what the default run would have read.
fn apply_discovery(launch: &LaunchOverrideArgs) -> Result<()> {
    use crate::prompt::{NO_CONTEXT_FILES_ENV, NO_RULES_ENV};
    if launch.no_context_files {
        crate::env::set_var(NO_CONTEXT_FILES_ENV, "1");
        let mut sources = vec![("project AGENTS.md", PathBuf::from("AGENTS.md"))];
        if let Ok(global) = crate::storage::user_home_path("AGENTS.md") {
            sources.push(("global AGENTS.md", global));
        }
        let skipped = existing(&sources);
        report_skipped("--no-context-files", &skipped);
    }
    if launch.no_rules {
        crate::env::set_var(NO_RULES_ENV, "1");
        let mut sources = vec![(
            ".horon/prompt-overlay.md",
            Path::new(".horon/prompt-overlay.md").to_path_buf(),
        )];
        sources.push((
            ".horon/preferred-tools.md",
            Path::new(".horon/preferred-tools.md").to_path_buf(),
        ));
        if let Ok(dir) = crate::storage::horon_agent_dir() {
            sources.push(("~/.horon/prompt-overlay.md", dir.join("prompt-overlay.md")));
            sources.push(("~/.horon/preferred-tools.md", dir.join("preferred-tools.md")));
        }
        let mut skipped = existing(&sources);
        if crate::config::config().features.foreign_rules {
            skipped.push("imported foreign rules");
        }
        report_skipped("--no-rules", &skipped);
    }
    apply_skill_filter(launch)
}

/// `--no-skills` hides everything and wins over `--skills`; otherwise the
/// patterns are checked here so a bad one fails before any request.
fn apply_skill_filter(launch: &LaunchOverrideArgs) -> Result<()> {
    use crate::skill::{NO_SKILLS_ENV, SKILLS_ALLOW_ENV, parse_patterns};
    let patterns = launch
        .skills
        .iter()
        .map(|raw| parse_patterns(raw))
        .collect::<Result<Vec<_>>>()?
        .concat();
    if launch.no_skills {
        crate::env::set_var(NO_SKILLS_ENV, "1");
        let ignored = if patterns.is_empty() {
            String::new()
        } else {
            format!("; --skills {} ignored", patterns.join(","))
        };
        output::stderr_info(format!("launch override: skills off (this invocation only){ignored}"));
    } else if !patterns.is_empty() {
        let joined = patterns.join(",");
        crate::env::set_var(SKILLS_ALLOW_ENV, &joined);
        output::stderr_info(format!(
            "launch override: skills limited to {joined} (this invocation only)"
        ));
    }
    Ok(())
}

fn existing(sources: &[(&'static str, PathBuf)]) -> Vec<&'static str> {
    sources
        .iter()
        .filter(|(_, path)| path.is_file())
        .map(|(label, _)| *label)
        .collect()
}

fn report_skipped(flag: &str, skipped: &[&str]) {
    let list = if skipped.is_empty() {
        "no source was present".to_string()
    } else {
        skipped.join(", ")
    };
    output::stderr_info(format!("launch override: {flag} skips {list} (this invocation only)"));
}
