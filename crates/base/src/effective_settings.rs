//! The allow-listed settings a screen may display, with the layer behind each.
//!
//! The daemon's config holds far more than a client should ever see: provider
//! tables with credential variable names, hook commands, MCP headers. This
//! module therefore answers from a fixed table of rows rather than by
//! serializing anything, which is what makes "no credential can travel" a
//! property of the code instead of a promise about a serializer.
//!
//! A row carries the value in force and the layer that supplied it. The value
//! is read from the merged [`Config`] this process runs with, so an environment
//! override is reflected in the text a client prints. The layer is decided
//! from evidence, strongest first: a command-line flag the launcher recorded, a
//! variable this process actually read, the config file as this process parsed
//! it, and finally the shipped default. A layer that did not apply is never
//! named, because "I did not set that" and "the file disagrees with me" are
//! different problems and a screen acts on the difference.

use std::sync::Mutex;

use crate::config::{ApprovalMode, Config, McpToolsMode, SecretsMode};

/// Where one reported value came from.
///
/// The spellings match the harness API's `SettingSource`, which is what a
/// client receives; the mapping lives in the server that builds the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveSource {
    /// No flag, variable or file set it; the shipped default is in force.
    Default,
    /// A command-line flag supplied it.
    Cli,
    /// An environment variable overrode everything else.
    Env,
    /// The config file supplied it.
    File,
}

/// One allow-listed value, as the daemon reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveSettingRow {
    /// Dotted config path, e.g. `provider.stream_idle_timeout_secs`.
    pub key: String,
    /// Human-readable name for the row.
    pub label: String,
    /// The value in force, as the screen prints it.
    pub value: String,
    /// The layer that supplied it.
    pub source: EffectiveSource,
    /// The flag, variable name or file path `source` points at.
    ///
    /// Absent for [`EffectiveSource::Default`], which names no origin.
    pub origin: Option<String>,
}

/// One allow-listed key, with the row a settings screen prints for it.
///
/// `env` names the environment variable that overrides the key and how a value
/// of that variable is validated: a variable the daemon ignores must not be
/// reported as the source of the merged value, or a screen would blame a
/// variable that never applied. `file_value` is the matching check for the
/// config file, and it is what keeps a wrongly shaped value out of the file's
/// rows.
struct Spec {
    key: &'static str,
    label: &'static str,
    env: Option<(&'static str, fn(&str) -> bool)>,
    file_value: fn(&toml::Value) -> bool,
    value: fn(&Config) -> String,
}

/// The whole surface a client can read. Anything not listed here cannot be
/// reported, whatever the config file holds.
const SPECS: &[Spec] = &[
    Spec {
        key: "provider.openai_reasoning_effort",
        label: "OpenAI reasoning effort",
        env: Some(("HORON_AGENT_OPENAI_REASONING_EFFORT", non_empty)),
        file_value: text,
        value: |config| optional_text(config.provider.openai_reasoning_effort.as_deref()),
    },
    Spec {
        key: "provider.anthropic_reasoning_effort",
        label: "Anthropic reasoning effort",
        env: Some(("HORON_AGENT_ANTHROPIC_REASONING_EFFORT", non_empty)),
        file_value: text,
        value: |config| optional_text(config.provider.anthropic_reasoning_effort.as_deref()),
    },
    Spec {
        key: "provider.stream_idle_timeout_secs",
        label: "Stream idle timeout (seconds)",
        env: Some(("HORON_AGENT_STREAM_IDLE_TIMEOUT_SECS", positive_u64)),
        file_value: uint64,
        value: |config| config.provider.stream_idle_timeout_secs.to_string(),
    },
    Spec {
        key: "provider.max_retries",
        label: "Max request attempts",
        env: Some(("HORON_AGENT_MAX_RETRIES", positive_u32)),
        file_value: uint32,
        value: |config| config.provider.max_retries.to_string(),
    },
    Spec {
        key: "tools.profile",
        label: "Tool profile",
        env: Some(("HORON_AGENT_TOOL_PROFILE", any_text)),
        file_value: text,
        value: |config| optional_text(Some(config.tools.profile.as_str())),
    },
    Spec {
        key: "tools.mcp_tools",
        label: "MCP tool exposure",
        env: Some(("HORON_AGENT_MCP_TOOLS", valid_mcp_mode)),
        file_value: mcp_mode,
        value: |config| config.tools.mcp_tools.as_str().to_string(),
    },
    Spec {
        key: "tools.approval_mode",
        label: "Approval mode",
        env: None,
        file_value: approval_mode,
        value: |config| config.tools.approval_mode.as_str().to_string(),
    },
    Spec {
        key: "tools.ask_policy",
        label: "Ask policy",
        env: None,
        file_value: text,
        value: |config| config.tools.ask_policy.as_str().to_string(),
    },
    Spec {
        key: "tools.secrets_mode",
        label: "Secrets mode",
        env: None,
        file_value: secrets_mode,
        value: |config| config.tools.secrets_mode.as_str().to_string(),
    },
    Spec {
        key: "compaction.mode",
        label: "Compaction mode",
        env: None,
        file_value: text,
        value: |config| config.compaction.mode.as_str().to_string(),
    },
    Spec {
        key: "messages.follow_up_mode",
        label: "Follow-up delivery",
        env: None,
        file_value: text,
        value: |config| config.messages.follow_up_mode.as_str().to_string(),
    },
    Spec {
        key: "messages.steering_mode",
        label: "Steering delivery",
        env: None,
        file_value: text,
        value: |config| config.messages.steering_mode.as_str().to_string(),
    },
    Spec {
        key: "messages.interrupt_mode",
        label: "Interrupt timing",
        env: None,
        file_value: text,
        value: |config| config.messages.interrupt_mode.as_str().to_string(),
    },
];

/// How the command line supplied one environment override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchOrigin {
    /// A flag, named as it was typed, supplied the value.
    Flag(&'static str),
    /// The launch chose the shipped default because nothing selected one. The
    /// variable exists in this process, but a user's environment never set it,
    /// so it must not be reported as the environment layer.
    Default,
}

/// Origins the launcher recorded for the variables it set itself.
///
/// The client turns flags such as `--tool-profile` into environment variables
/// before the config is first read, so the environment alone cannot tell a flag
/// from a variable the user exported. This record is that missing evidence.
static LAUNCH_ORIGINS: Mutex<Vec<(&'static str, LaunchOrigin)>> = Mutex::new(Vec::new());

/// Record how the command line supplied the variable `env`.
///
/// Called by the launcher for the few flags that become config overrides. A
/// later call for the same variable replaces the earlier one, so a default
/// chosen first is corrected by a flag applied after it.
pub fn record_launch_origin(env: &'static str, origin: LaunchOrigin) {
    let mut origins = LAUNCH_ORIGINS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match origins.iter_mut().find(|(name, _)| *name == env) {
        Some(entry) => entry.1 = origin,
        None => origins.push((env, origin)),
    }
}

/// Record a command-line flag that supplied the variable `env`.
///
/// One line at the call site, so the launcher can name each flag beside the
/// `set_var` it performs without growing that dispatch function.
pub fn record_launch_flag(env: &'static str, flag: &'static str) {
    record_launch_origin(env, LaunchOrigin::Flag(flag));
}

/// Record that the launch chose the shipped default for `env`.
///
/// The variable exists in this process afterwards, but the user's environment
/// never set it, so a report must not name the environment as its source.
pub fn record_launch_default(env: &'static str) {
    record_launch_origin(env, LaunchOrigin::Default);
}

fn launch_origin(env: &str) -> Option<LaunchOrigin> {
    let origins = LAUNCH_ORIGINS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    origins.iter().find(|(name, _)| *name == env).map(|(_, origin)| *origin)
}

/// The allow-listed values of this process, each with its layer.
///
/// Read from the same merged config the daemon runs with, and answered by the
/// daemon itself: a bridge process would report its own environment and its own
/// read of the file, which need not match the session it is serving.
pub fn effective_settings() -> Vec<EffectiveSettingRow> {
    let config = crate::config::config();
    let file = FileLayer::read();
    // `var_os` keeps a non-UTF-8 value from being read as "unset", and
    // `to_str` returns `Option`, so no fallible conversion is swallowed here.
    report(
        config,
        file.as_ref(),
        |name| std::env::var_os(name).and_then(|value| value.to_str().map(str::to_owned)),
        launch_origin,
    )
}

/// The rows for `config`, with each one's source decided from the layers.
///
/// Split from [`effective_settings`] so a test can supply an exact config,
/// file, environment and launch record instead of the process's own.
fn report(
    config: &Config,
    file: Option<&FileLayer>,
    env: impl Fn(&str) -> Option<String>,
    launch: impl Fn(&str) -> Option<LaunchOrigin>,
) -> Vec<EffectiveSettingRow> {
    SPECS
        .iter()
        .map(|spec| {
            let (source, origin) = provenance(spec, config, file, &env, &launch);
            EffectiveSettingRow {
                key: spec.key.to_string(),
                label: spec.label.to_string(),
                value: (spec.value)(config),
                source,
                origin,
            }
        })
        .collect()
}

/// Which layer supplied `spec`'s value, and the flag, variable or path it is.
fn provenance(
    spec: &Spec,
    config: &Config,
    file: Option<&FileLayer>,
    env: &impl Fn(&str) -> Option<String>,
    launch: &impl Fn(&str) -> Option<LaunchOrigin>,
) -> (EffectiveSource, Option<String>) {
    let supplied = spec.env.and_then(|(name, _)| launch(name));
    match supplied {
        Some(LaunchOrigin::Flag(flag)) => return (EffectiveSource::Cli, Some(flag.to_string())),
        // The launcher's own default is not the user's environment: look past
        // the variable it left behind rather than blaming it.
        Some(LaunchOrigin::Default) => {}
        None => {
            if let Some((name, accepts)) = spec.env
                && let Some(raw) = env(name)
                && accepts(&raw)
            {
                return (EffectiveSource::Env, Some(name.to_string()));
            }
        }
    }
    if let Some(file) = file
        && file.supplied(spec, config)
    {
        return (EffectiveSource::File, Some(file.path.clone()));
    }
    (EffectiveSource::Default, None)
}

/// The config file as this process parsed it, kept for the checks a source
/// needs.
struct FileLayer {
    path: String,
    /// The file-only config, so a declared value can be compared with the one
    /// in force.
    config: Config,
    document: toml::Value,
}

impl FileLayer {
    /// Read the file, or give up when it cannot be read or does not parse.
    ///
    /// A file that does not parse had no effect on the daemon either, so every
    /// row falls through to its other layers rather than naming a file that was
    /// ignored. That covers both a TOML syntax error and a value the daemon's
    /// own types reject: either one makes the whole file inert.
    fn read() -> Option<Self> {
        let (config, document) = match Config::load_file_layer() {
            Ok(Some(layer)) => layer,
            // Absent or unparseable: either way the daemon applied nothing, and
            // the error is dropped on purpose because its text quotes the
            // offending line, which is where a credential would sit.
            Ok(None) | Err(_) => return None,
        };
        let path = Config::path()?;
        Some(Self {
            path: path.display().to_string(),
            config,
            document,
        })
    }

    /// Whether this file supplied `spec`'s value.
    ///
    /// Three things must hold: the document declares the key, the declared
    /// value is one the daemon applies, and the file-only config renders the
    /// same value as the one in force. The last check keeps a file edited after
    /// the config was cached from being named for a value it no longer holds.
    fn supplied(&self, spec: &Spec, config: &Config) -> bool {
        let Some(raw) = self.value(spec.key) else {
            return false;
        };
        if !(spec.file_value)(raw) {
            return false;
        }
        (spec.value)(&self.config) == (spec.value)(config)
    }

    /// The value `key` has in this document, walking dotted table names.
    fn value(&self, key: &str) -> Option<&toml::Value> {
        let mut cursor = &self.document;
        for part in key.split('.') {
            cursor = cursor.get(part)?;
        }
        Some(cursor)
    }
}

/// A value's text, with an unset key said out loud rather than left blank.
fn optional_text(value: Option<&str>) -> String {
    match value.map(str::trim) {
        Some(text) if !text.is_empty() => text.to_string(),
        _ => "(unset)".to_string(),
    }
}

/// A declared value the daemon reads as text.
fn text(raw: &toml::Value) -> bool {
    raw.as_str().is_some()
}

/// A declared value the daemon reads as a whole number in `u64`.
fn uint64(raw: &toml::Value) -> bool {
    raw.as_integer().is_some_and(|value| u64::try_from(value).is_ok())
}

/// A declared value the daemon reads as a whole number in `u32`.
fn uint32(raw: &toml::Value) -> bool {
    raw.as_integer().is_some_and(|value| u32::try_from(value).is_ok())
}

/// A declared value the daemon reads as an approval mode.
///
/// `ApprovalMode` deliberately degrades an unrecognized string to its default,
/// so a file that holds `"nonsense"` still loads and its value is not the one
/// in force. Naming the file for that row would be a lie, so the same parser
/// the daemon applies decides whether the file supplied anything.
fn approval_mode(raw: &toml::Value) -> bool {
    raw.as_str().and_then(ApprovalMode::parse).is_some()
}

/// A declared value the daemon reads as a secrets mode, with the same lenient
/// fallback as [`approval_mode`].
fn secrets_mode(raw: &toml::Value) -> bool {
    raw.as_str().and_then(SecretsMode::parse).is_some()
}

/// A declared value the daemon reads as an MCP exposure mode.
fn mcp_mode(raw: &toml::Value) -> bool {
    raw.as_str().and_then(McpToolsMode::parse).is_some()
}

/// An environment value the daemon applies as a string when it is not blank.
fn non_empty(raw: &str) -> bool {
    !raw.trim().is_empty()
}

/// An environment value the daemon assigns without a blank check.
///
/// `HORON_AGENT_TOOL_PROFILE` overwrites `tools.profile` verbatim, so an empty
/// value clears the profile instead of leaving it alone. Naming the variable is
/// then the only truthful source for the value in force, and treating it as
/// unset would blame a file or the default for a value it never supplied.
fn any_text(_: &str) -> bool {
    true
}

/// An environment value the daemon would accept as a whole number above zero.
fn positive_u64(raw: &str) -> bool {
    raw.trim().parse::<u64>().is_ok_and(|value| value > 0)
}

/// An environment value the daemon would accept as a retry count above zero.
fn positive_u32(raw: &str) -> bool {
    raw.trim().parse::<u32>().is_ok_and(|value| value > 0)
}

/// An environment value the daemon would accept as an MCP exposure mode.
fn valid_mcp_mode(raw: &str) -> bool {
    McpToolsMode::parse(raw).is_some()
}

#[cfg(test)]
#[path = "effective_settings_tests.rs"]
mod tests;
