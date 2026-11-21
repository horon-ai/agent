use anyhow::{Context, Result};
use serde::Serialize;
use std::io::Read;
use std::path::PathBuf;

use crate::cli::args::ProviderAuthArg;
use crate::config::{Config, NamedProviderAuth, NamedProviderConfig, NamedProviderModelConfig, NamedProviderType};
use crate::provider_catalog::{
    api_base_uses_localhost, is_safe_env_file_name, is_safe_env_key_name, normalize_api_base, save_named_api_key,
};

#[derive(Debug)]
pub(crate) struct ProviderAddOptions {
    pub name: String,
    pub base_url: String,
    pub model: String,
    pub context_window: Option<usize>,
    pub api_key_env: Option<String>,
    pub api_key: Option<String>,
    pub api_key_stdin: bool,
    pub no_api_key: bool,
    pub provider_type: NamedProviderType,
    pub auth: Option<ProviderAuthArg>,
    pub auth_header: Option<String>,
    pub env_file: Option<String>,
    pub set_default: bool,
    pub overwrite: bool,
    pub provider_routing: bool,
    pub model_catalog: bool,
    pub json: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct ProviderSetupReport {
    status: &'static str,
    profile: String,
    config_path: String,
    #[serde(rename = "type")]
    provider_type: &'static str,
    api_base: String,
    model: String,
    api_key_env: Option<String>,
    env_file: Option<String>,
    env_file_path: Option<String>,
    api_key_stored: bool,
    auth: String,
    default_set: bool,
    run_command: String,
}

pub(crate) fn run_provider_add_command(options: ProviderAddOptions) -> Result<()> {
    let emit_json = options.json;
    let report = configure_provider_profile(options)?;

    if emit_json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Added provider profile '{}'", report.profile);
        println!("  config: {}", report.config_path);
        println!("  base:   {}", report.api_base);
        println!("  model:  {}", report.model);
        println!("  type:   {}", report.provider_type);
        println!("  auth:   {}", report.auth);
        if let Some(env_file_path) = &report.env_file_path {
            if report.api_key_stored {
                println!(
                    "  key:    {} in {}",
                    report.api_key_env.as_deref().unwrap_or("API key"),
                    env_file_path
                );
            } else {
                println!(
                    "  key:    {} (also reads {})",
                    report.api_key_env.as_deref().unwrap_or("API key"),
                    env_file_path
                );
            }
        } else if let Some(api_key_env) = &report.api_key_env {
            println!("  key:    environment variable {}", api_key_env);
        }
        if report.default_set {
            println!("  default: yes");
        }
        println!();
        println!("Run:       {}", report.run_command);
    }

    Ok(())
}

pub(crate) fn configure_provider_profile(options: ProviderAddOptions) -> Result<ProviderSetupReport> {
    let name = validate_profile_name(&options.name)?;
    ensure_profile_name_not_reserved(&name)?;

    let api_base = normalize_api_base(&options.base_url).ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid --base-url '{}'. Use https://... or http://localhost/127.0.0.1/private-LAN for local servers.",
            options.base_url
        )
    })?;
    let model = options.model.trim().to_string();
    if model.is_empty() {
        anyhow::bail!("--model cannot be empty");
    }
    if matches!(options.context_window, Some(0)) {
        anyhow::bail!("--context-window must be greater than 0");
    }

    let api_key = read_api_key(&options)?;
    let stored_auth = resolve_auth_mode(&options, api_key.as_deref(), &api_base)?;
    let effective_auth = stored_auth.unwrap_or_else(|| options.provider_type.default_auth());
    let uses_auth = !matches!(effective_auth, NamedProviderAuth::None);

    if !uses_auth && options.auth_header.is_some() {
        anyhow::bail!("--auth-header can only be used with --auth api-key");
    }
    if !matches!(options.auth, Some(ProviderAuthArg::ApiKey)) && options.auth_header.is_some() {
        anyhow::bail!("--auth-header requires --auth api-key");
    }

    let api_key_env = if uses_auth {
        Some(resolve_api_key_env(&name, options.api_key_env.as_deref())?)
    } else {
        None
    };

    let env_file = if uses_auth && (api_key.is_some() || options.env_file.is_some()) {
        Some(resolve_env_file(&name, options.env_file.as_deref())?)
    } else {
        None
    };

    if uses_auth
        && api_key.is_none()
        && options.api_key_env.is_none()
        && options.env_file.is_none()
        && !api_base_uses_localhost(&api_base)
    {
        anyhow::bail!(
            "Remote provider '{}' needs an API key source. Use --api-key-env NAME, --api-key-stdin, --api-key VALUE, or --auth none if this endpoint truly needs no auth.",
            name
        );
    }

    if let (Some(key), Some(env_key), Some(file_name)) =
        (api_key.as_deref(), api_key_env.as_deref(), env_file.as_deref())
    {
        save_named_api_key(file_name, env_key, key)?;
    }
    let api_key_stored = api_key.is_some() && env_file.is_some();

    let profile = NamedProviderConfig {
        provider_type: options.provider_type,
        base_url: api_base.clone(),
        api: None,
        auth: stored_auth,
        auth_header: match effective_auth {
            NamedProviderAuth::Header => options
                .auth_header
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string),
            _ => None,
        },
        headers: std::collections::BTreeMap::new(),
        api_key_env: api_key_env.clone(),
        api_key: None,
        env_file: env_file.clone(),
        default_model: Some(model.clone()),
        requires_api_key: Some(uses_auth),
        provider_routing: options.provider_routing,
        model_catalog: options.model_catalog,
        allow_provider_pinning: options.provider_routing,
        models: vec![NamedProviderModelConfig {
            id: model.clone(),
            reasoning: None,
            reasoning_effort: None,
            context_window: options.context_window,
            input: Vec::new(),
        }],
        extra_body: None,
        prompt_cache_key: None,
        stream_first_event_timeout_secs: None,
        stream_idle_timeout_secs: None,
        max_in_flight_requests: None,
        supports_reasoning_effort: None,
        disable_reasoning_heuristics: false,
        model_roles: Default::default(),
    };

    let config_path = Config::path().ok_or_else(|| anyhow::anyhow!("No config path"))?;
    let content = std::fs::read_to_string(&config_path).unwrap_or_default();
    let existing = parse_config_or_default(&content)
        .with_context(|| format!("failed to parse existing config at {}", config_path.display()))?;
    if existing.providers.contains_key(&name) && !options.overwrite {
        anyhow::bail!(
            "Provider profile '{}' already exists. Re-run with --overwrite to replace it.",
            name
        );
    }

    let mut updated = if options.overwrite {
        remove_named_provider_sections(&content, &name)
    } else {
        content
    };
    if options.set_default {
        updated = upsert_provider_defaults(updated, &name, &model);
    }
    updated = append_profile_section(updated, &name, &profile);

    toml::from_str::<Config>(&updated)
        .with_context(|| format!("generated provider config for '{}' was not valid TOML", name))?;

    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&config_path, updated)?;

    let env_file_path = env_file
        .as_deref()
        .map(|file| crate::storage::app_config_dir().map(|dir| dir.join(file)))
        .transpose()?
        .map(path_to_string);

    Ok(ProviderSetupReport {
        status: "ok",
        profile: name.clone(),
        config_path: path_to_string(config_path),
        provider_type: provider_type_label(&profile.provider_type),
        api_base,
        model: model.clone(),
        api_key_env,
        env_file,
        env_file_path,
        api_key_stored,
        auth: auth_label(&profile.resolved_auth()).to_string(),
        default_set: options.set_default,
        run_command: format!(
            "horon-agent --provider-profile {} --model {} run 'hello'",
            shell_quote(&name),
            shell_quote(&model)
        ),
    })
}

fn parse_config_or_default(content: &str) -> Result<Config> {
    if content.trim().is_empty() {
        Ok(Config::default())
    } else {
        Ok(toml::from_str::<Config>(content)?)
    }
}

fn validate_profile_name(raw: &str) -> Result<String> {
    let name = raw.trim();
    if name.is_empty() {
        anyhow::bail!("provider profile name cannot be empty");
    }
    if name.len() > 64 {
        anyhow::bail!("provider profile name must be at most 64 characters");
    }
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        anyhow::bail!("provider profile name cannot be empty");
    };
    if !first.is_ascii_alphanumeric() {
        anyhow::bail!("provider profile name must start with a letter or number");
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        anyhow::bail!("provider profile name may only contain ASCII letters, numbers, '-' and '_'");
    }
    Ok(name.to_string())
}

fn ensure_profile_name_not_reserved(name: &str) -> Result<()> {
    const RESERVED_PROVIDER_NAMES: &[&str] = &["auto", "claude-subprocess", "compat", "custom", "azure-openai", "aoai"];
    if crate::provider_catalog::openai_compatible_profile_by_id(name).is_some()
        || RESERVED_PROVIDER_NAMES
            .iter()
            .any(|reserved| name.eq_ignore_ascii_case(reserved))
    {
        anyhow::bail!(
            "'{}' is a built-in provider id or alias. Choose a non-reserved profile name such as '{}-api'.",
            name,
            name
        );
    }
    Ok(())
}

fn read_api_key(options: &ProviderAddOptions) -> Result<Option<String>> {
    if options.api_key_stdin {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        let key = input.trim().to_string();
        if key.is_empty() {
            anyhow::bail!("--api-key-stdin was set, but stdin was empty");
        }
        Ok(Some(key))
    } else {
        Ok(options
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(ToString::to_string))
    }
}

/// Auth mode to store in the written profile.
///
/// `None` means the user did not name a mode, so no `auth` line is written and
/// the profile's `type` default applies. Two cases still resolve to an explicit
/// [`NamedProviderAuth::None`]: `--no-api-key`/`--auth none`, and a keyless
/// localhost profile, because neither may fall through to a type default that
/// would send an empty credential.
fn resolve_auth_mode(
    options: &ProviderAddOptions,
    api_key: Option<&str>,
    api_base: &str,
) -> Result<Option<NamedProviderAuth>> {
    if options.no_api_key {
        return Ok(Some(NamedProviderAuth::None));
    }
    if matches!(options.auth, Some(ProviderAuthArg::None)) {
        if api_key.is_some() || options.api_key_env.is_some() || options.env_file.is_some() {
            anyhow::bail!("--auth none cannot be combined with API key options");
        }
        return Ok(Some(NamedProviderAuth::None));
    }
    if options.auth.is_none()
        && api_key.is_none()
        && options.api_key_env.is_none()
        && options.env_file.is_none()
        && api_base_uses_localhost(api_base)
    {
        return Ok(Some(NamedProviderAuth::None));
    }

    Ok(options.auth.map(|auth| match auth {
        ProviderAuthArg::Bearer => NamedProviderAuth::Bearer,
        ProviderAuthArg::ApiKey => NamedProviderAuth::Header,
        ProviderAuthArg::None => NamedProviderAuth::None,
    }))
}

fn resolve_api_key_env(name: &str, configured: Option<&str>) -> Result<String> {
    let env_name = configured
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| derived_api_key_env(name));
    if !is_safe_env_key_name(&env_name) {
        anyhow::bail!(
            "Invalid --api-key-env '{}'. Use uppercase letters, numbers, and underscores only.",
            env_name
        );
    }
    Ok(env_name)
}

fn resolve_env_file(name: &str, configured: Option<&str>) -> Result<String> {
    let file = configured
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("provider-{}.env", name));
    if !is_safe_env_file_name(&file) {
        anyhow::bail!(
            "Invalid --env-file '{}'. Use a file name only, with letters, numbers, '.', '_' or '-'.",
            file
        );
    }
    Ok(file)
}

fn derived_api_key_env(name: &str) -> String {
    let suffix = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("HORON_AGENT_PROVIDER_{}_API_KEY", suffix)
}

fn append_profile_section(mut content: String, name: &str, profile: &NamedProviderConfig) -> String {
    if !content.trim().is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    if !content.is_empty() && !content.ends_with("\n\n") {
        content.push('\n');
    }

    content.push_str(&format!("[providers.{name}]\n"));
    content.push_str(&format!(
        "type = {}\n",
        toml_quote(provider_type_label(&profile.provider_type))
    ));
    content.push_str(&format!("base_url = {}\n", toml_quote(&profile.base_url)));
    if let Some(auth) = profile.auth.as_ref() {
        content.push_str(&format!("auth = {}\n", toml_quote(auth_label(auth))));
    }
    if let Some(header) = profile.auth_header.as_deref() {
        content.push_str(&format!("auth_header = {}\n", toml_quote(header)));
    }
    if let Some(api_key_env) = profile.api_key_env.as_deref() {
        content.push_str(&format!("api_key_env = {}\n", toml_quote(api_key_env)));
    }
    if let Some(env_file) = profile.env_file.as_deref() {
        content.push_str(&format!("env_file = {}\n", toml_quote(env_file)));
    }
    if let Some(default_model) = profile.default_model.as_deref() {
        content.push_str(&format!("default_model = {}\n", toml_quote(default_model)));
    }
    if let Some(requires_api_key) = profile.requires_api_key {
        content.push_str(&format!("requires_api_key = {requires_api_key}\n"));
    }
    if let Some(prompt_cache_key) = profile.prompt_cache_key {
        content.push_str(&format!("prompt_cache_key = {prompt_cache_key}\n"));
    }
    if profile.provider_routing {
        content.push_str("provider_routing = true\nallow_provider_pinning = true\n");
    }
    if profile.model_catalog {
        content.push_str("model_catalog = true\n");
    }

    for model in &profile.models {
        content.push_str(&format!("\n[[providers.{name}.models]]\n"));
        content.push_str(&format!("id = {}\n", toml_quote(&model.id)));
        if let Some(limit) = model.context_window {
            content.push_str(&format!("context_window = {limit}\n"));
        }
    }

    content
}

fn upsert_provider_defaults(content: String, profile: &str, model: &str) -> String {
    let mut lines = split_lines_lossy(&content);
    let provider_idx = lines.iter().position(|line| line.trim() == "[provider]");

    let Some(idx) = provider_idx else {
        let mut prefix = String::from("[provider]\n");
        prefix.push_str(&format!("default_provider = {}\n", toml_quote(profile)));
        prefix.push_str(&format!("default_model = {}\n\n", toml_quote(model)));
        if content.trim().is_empty() {
            return prefix;
        }
        return format!("{prefix}{}", content.trim_start_matches('\n'));
    };

    let end = lines
        .iter()
        .enumerate()
        .skip(idx + 1)
        .find(|(_, line)| is_toml_header(line))
        .map(|(line_idx, _)| line_idx)
        .unwrap_or(lines.len());

    upsert_key_in_range(&mut lines, idx + 1, end, "default_provider", &toml_quote(profile));
    let end = lines
        .iter()
        .enumerate()
        .skip(idx + 1)
        .find(|(_, line)| is_toml_header(line))
        .map(|(line_idx, _)| line_idx)
        .unwrap_or(lines.len());
    upsert_key_in_range(&mut lines, idx + 1, end, "default_model", &toml_quote(model));

    join_lines(lines)
}

fn upsert_key_in_range(lines: &mut Vec<String>, start: usize, end: usize, key: &str, value: &str) {
    for line in lines.iter_mut().take(end).skip(start) {
        if line_has_toml_key(line, key) {
            *line = format!("{key} = {value}");
            return;
        }
    }
    lines.insert(end, format!("{key} = {value}"));
}

fn remove_named_provider_sections(content: &str, name: &str) -> String {
    let lines = split_lines_lossy(content);
    let mut kept = Vec::with_capacity(lines.len());
    let mut skip = false;

    for line in lines {
        if is_toml_header(&line) {
            skip = is_named_provider_header(&line, name);
        }
        if !skip {
            kept.push(line);
        }
    }

    join_lines(kept)
}

fn is_named_provider_header(line: &str, name: &str) -> bool {
    let trimmed = line.trim();
    let inner = if trimmed.starts_with("[[") && trimmed.ends_with("]]") {
        &trimmed[2..trimmed.len() - 2]
    } else if trimmed.starts_with('[') && trimmed.ends_with(']') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        return false;
    };
    let inner = inner.trim();
    let plain = format!("providers.{name}");
    let double_quoted = format!("providers.{}", toml_quote(name));
    let single_quoted = format!("providers.'{name}'");
    inner == plain
        || inner == format!("{plain}.models")
        || inner == double_quoted
        || inner == format!("{double_quoted}.models")
        || inner == single_quoted
        || inner == format!("{single_quoted}.models")
}

fn is_toml_header(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('[') && trimmed.ends_with(']')
}

fn line_has_toml_key(line: &str, key: &str) -> bool {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix(key) else {
        return false;
    };
    rest.trim_start().starts_with('=')
}

fn split_lines_lossy(content: &str) -> Vec<String> {
    content.lines().map(ToString::to_string).collect()
}

fn join_lines(lines: Vec<String>) -> String {
    let mut joined = lines.join("\n");
    if !joined.is_empty() {
        joined.push('\n');
    }
    joined
}

fn auth_label(auth: &NamedProviderAuth) -> &'static str {
    match auth {
        NamedProviderAuth::Bearer => "bearer",
        NamedProviderAuth::Header => "header",
        NamedProviderAuth::None => "none",
    }
}

/// The `type = "…"` value written to config.toml, matching the serde names of
/// [`NamedProviderType`].
fn provider_type_label(provider_type: &NamedProviderType) -> &'static str {
    match provider_type {
        NamedProviderType::OpenAiCompatible => "openai-compatible",
        NamedProviderType::OpenAiResponses => "openai-responses",
        NamedProviderType::AnthropicCompatible => "anthropic-compatible",
        NamedProviderType::GeminiCompatible => "gemini-compatible",
        NamedProviderType::OpenRouter => "open-router",
    }
}

fn toml_quote(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|error| {
        crate::logging::warn(&format!(
            "failed to quote provider config string with serde_json: {error}"
        ));
        format!("\"{}\"", value.escape_default())
    })
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn path_to_string(path: PathBuf) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var_os(key);
            crate::env::set_var(key, value);
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var_os(key);
            crate::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                crate::env::set_var(self.key, previous);
            } else {
                crate::env::remove_var(self.key);
            }
        }
    }

    fn base_options() -> ProviderAddOptions {
        ProviderAddOptions {
            name: "my-api".to_string(),
            base_url: "https://llm.example.com/v1".to_string(),
            model: "model-a".to_string(),
            context_window: Some(128_000),
            api_key_env: None,
            api_key: Some("secret-test-key".to_string()),
            api_key_stdin: false,
            no_api_key: false,
            provider_type: NamedProviderType::OpenAiCompatible,
            auth: None,
            auth_header: None,
            env_file: None,
            set_default: true,
            overwrite: false,
            provider_routing: false,
            model_catalog: false,
            json: false,
        }
    }

    #[test]
    fn provider_add_writes_named_profile_env_file_and_default() {
        let _lock = crate::storage::lock_test_env();
        let temp = tempfile::TempDir::new().expect("temp dir");
        let _home = EnvVarGuard::set("HORON_HOME", temp.path());
        let _key = EnvVarGuard::remove("HORON_AGENT_PROVIDER_MY_API_API_KEY");
        let config_path = temp.path().join("config.toml");
        std::fs::write(
            &config_path,
            "# keep this comment\n[provider]\nopenai_reasoning_effort = \"low\"\n",
        )
        .expect("write config");

        let report = configure_provider_profile(base_options()).expect("configure provider");

        assert_eq!(report.profile, "my-api");
        let config = std::fs::read_to_string(&config_path).expect("read config");
        assert!(config.contains("# keep this comment"));
        assert!(config.contains("default_provider = \"my-api\""));
        assert!(config.contains("default_model = \"model-a\""));
        assert!(config.contains("[providers.my-api]"));
        assert!(!config.contains("secret-test-key"));

        let parsed: Config = toml::from_str(&config).expect("valid config");
        assert_eq!(parsed.provider.default_provider.as_deref(), Some("my-api"));
        assert_eq!(parsed.provider.default_model.as_deref(), Some("model-a"));
        let profile = parsed.providers.get("my-api").expect("profile");
        assert_eq!(profile.base_url, "https://llm.example.com/v1");
        assert_eq!(profile.default_model.as_deref(), Some("model-a"));
        assert_eq!(
            profile.api_key_env.as_deref(),
            Some("HORON_AGENT_PROVIDER_MY_API_API_KEY")
        );
        assert_eq!(profile.env_file.as_deref(), Some("provider-my-api.env"));
        assert_eq!(profile.models[0].context_window, Some(128_000));

        let env_file = temp
            .path()
            .join("config")
            .join("horon-agent")
            .join("provider-my-api.env");
        let env_content = std::fs::read_to_string(env_file).expect("env file");
        assert!(env_content.contains("HORON_AGENT_PROVIDER_MY_API_API_KEY=secret-test-key"));
    }

    #[test]
    fn provider_add_rejects_remote_without_api_key_source() {
        let _lock = crate::storage::lock_test_env();
        let temp = tempfile::TempDir::new().expect("temp dir");
        let _home = EnvVarGuard::set("HORON_HOME", temp.path());
        let mut options = base_options();
        options.api_key = None;
        options.set_default = false;

        let err = configure_provider_profile(options).expect_err("should require key source");
        assert!(err.to_string().contains("needs an API key source"));
    }

    #[test]
    fn provider_add_allows_localhost_without_api_key() {
        let _lock = crate::storage::lock_test_env();
        let temp = tempfile::TempDir::new().expect("temp dir");
        let _home = EnvVarGuard::set("HORON_HOME", temp.path());
        let mut options = base_options();
        options.base_url = "http://localhost:8000/v1".to_string();
        options.api_key = None;
        options.set_default = false;

        configure_provider_profile(options).expect("localhost no-auth should work");
        let config = std::fs::read_to_string(temp.path().join("config.toml")).expect("config");
        assert!(config.contains("auth = \"none\""));
        assert!(config.contains("requires_api_key = false"));
    }

    /// Config text of a fresh `provider add` run for `options`.
    fn written_config(options: ProviderAddOptions) -> (String, ProviderSetupReport) {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let _home = EnvVarGuard::set("HORON_HOME", temp.path());
        let report = configure_provider_profile(options).expect("configure provider");
        let config = std::fs::read_to_string(temp.path().join("config.toml")).expect("config");
        (config, report)
    }

    fn options_with_type(provider_type: NamedProviderType) -> ProviderAddOptions {
        let mut options = base_options();
        options.provider_type = provider_type;
        options.set_default = false;
        options
    }

    #[test]
    fn provider_add_default_type_writes_openai_compatible_and_bearer() {
        let _lock = crate::storage::lock_test_env();
        let (config, report) = written_config(options_with_type(NamedProviderType::OpenAiCompatible));

        assert!(config.contains("type = \"openai-compatible\""));
        assert!(
            !config.contains("\nauth = "),
            "an unset --auth must not write an auth line, got:\n{config}"
        );
        assert_eq!(report.provider_type, "openai-compatible");
        assert_eq!(report.auth, "bearer");

        let parsed: Config = toml::from_str(&config).expect("valid config");
        let profile = parsed.providers.get("my-api").expect("profile");
        assert_eq!(profile.provider_type, NamedProviderType::OpenAiCompatible);
        assert_eq!(profile.auth, None, "the type default must stay implicit");
        assert_eq!(profile.resolved_auth(), NamedProviderAuth::Bearer);
    }

    #[test]
    fn provider_add_anthropic_type_keeps_the_header_default() {
        let _lock = crate::storage::lock_test_env();
        let (config, report) = written_config(options_with_type(NamedProviderType::AnthropicCompatible));

        assert!(config.contains("type = \"anthropic-compatible\""));
        assert!(
            !config.contains("\nauth = "),
            "an unset --auth must not write an auth line, got:\n{config}"
        );
        assert!(!config.contains("auth_header = "), "no custom header was asked for");
        assert_eq!(report.provider_type, "anthropic-compatible");
        assert_eq!(report.auth, "header");

        let parsed: Config = toml::from_str(&config).expect("valid config");
        let profile = parsed.providers.get("my-api").expect("profile");
        assert_eq!(profile.provider_type, NamedProviderType::AnthropicCompatible);
        assert_eq!(profile.auth, None);
        assert_eq!(profile.resolved_auth(), NamedProviderAuth::Header);
        assert_eq!(profile.resolved_auth_header().as_deref(), Some("x-api-key"));
    }

    #[test]
    fn provider_add_gemini_type_keeps_the_header_default() {
        let _lock = crate::storage::lock_test_env();
        let (config, report) = written_config(options_with_type(NamedProviderType::GeminiCompatible));

        assert!(config.contains("type = \"gemini-compatible\""));
        assert!(
            !config.contains("\nauth = "),
            "the Gemini default auth must stay implicit"
        );
        assert_eq!(report.provider_type, "gemini-compatible");
        assert_eq!(report.auth, "header");

        let parsed: Config = toml::from_str(&config).expect("valid config");
        let profile = parsed.providers.get("my-api").expect("profile");
        assert_eq!(profile.provider_type, NamedProviderType::GeminiCompatible);
        assert_eq!(profile.resolved_auth(), NamedProviderAuth::Header);
        assert_eq!(profile.resolved_auth_header().as_deref(), Some("x-goog-api-key"));
    }

    #[test]
    fn provider_add_openai_responses_type_writes_its_own_kebab_value() {
        let _lock = crate::storage::lock_test_env();
        let (config, report) = written_config(options_with_type(NamedProviderType::OpenAiResponses));

        assert!(config.contains("type = \"openai-responses\""));
        assert_eq!(report.provider_type, "openai-responses");
        assert_eq!(report.auth, "bearer");
    }

    #[test]
    fn provider_add_explicit_auth_wins_over_the_type_default() {
        let _lock = crate::storage::lock_test_env();
        let mut options = options_with_type(NamedProviderType::AnthropicCompatible);
        options.auth = Some(ProviderAuthArg::Bearer);
        let (config, report) = written_config(options);

        assert!(config.contains("type = \"anthropic-compatible\""));
        assert!(config.contains("auth = \"bearer\""));
        assert_eq!(report.auth, "bearer");

        let parsed: Config = toml::from_str(&config).expect("valid config");
        let profile = parsed.providers.get("my-api").expect("profile");
        assert_eq!(profile.auth, Some(NamedProviderAuth::Bearer));
        assert_eq!(profile.resolved_auth(), NamedProviderAuth::Bearer);
    }

    #[test]
    fn provider_add_auth_header_without_api_key_auth_is_refused() {
        let _lock = crate::storage::lock_test_env();
        let temp = tempfile::TempDir::new().expect("temp dir");
        let _home = EnvVarGuard::set("HORON_HOME", temp.path());
        let mut options = base_options();
        options.provider_type = NamedProviderType::AnthropicCompatible;
        options.auth_header = Some("x-custom-key".to_string());
        options.set_default = false;

        let err = configure_provider_profile(options).expect_err("--auth-header alone must fail");
        assert_eq!(err.to_string(), "--auth-header requires --auth api-key");
        assert!(
            !temp.path().join("config.toml").exists(),
            "a refused run must not write a config file"
        );

        let mut options = base_options();
        options.no_api_key = true;
        options.api_key = None;
        options.auth_header = Some("x-custom-key".to_string());
        options.set_default = false;
        let err = configure_provider_profile(options).expect_err("--no-api-key plus --auth-header must fail");
        assert_eq!(err.to_string(), "--auth-header can only be used with --auth api-key");
    }
}
