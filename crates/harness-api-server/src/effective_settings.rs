//! `get_effective_settings`: the allow-listed daemon values a settings screen
//! may display, answered by the bridge itself.
//!
//! The daemon's config file holds far more than a settings screen should ever
//! see: provider tables with credential variable names, hook commands, MCP
//! headers. This module therefore answers from a fixed table of rows rather
//! than by serializing anything, which is what makes "no credential can
//! travel" a property of the code instead of a promise about a serializer.
//!
//! A row carries the value in force and the layer that supplied it. The value
//! comes from the same merged [`Config`] the daemon runs with, so an
//! environment override is reflected in the text a client prints; the layer is
//! then decided by checking the environment variable first and the file
//! second, which is the order the daemon itself applies them.

use std::path::Path;

use horon_agent_base::config::{Config, McpToolsMode};
use horon_agent_harness_api::{ApiEvent, EffectiveSetting, ServerFrame, SettingSource};
use serde_json::Value;

/// One allow-listed key, with the row a settings screen prints for it.
///
/// `env` names the environment variable that overrides the key and how a value
/// of that variable is validated: a variable the daemon ignores must not be
/// reported as the source of the merged value, or a screen would blame a
/// variable that never applied.
struct Spec {
    key: &'static str,
    label: &'static str,
    env: Option<(&'static str, fn(&str) -> bool)>,
    value: fn(&Config) -> String,
}

/// The whole surface a client can read. Anything not listed here cannot be
/// reported, whatever the config file holds.
const SPECS: &[Spec] = &[
    Spec {
        key: "provider.openai_reasoning_effort",
        label: "OpenAI reasoning effort",
        env: Some(("HORON_AGENT_OPENAI_REASONING_EFFORT", non_empty)),
        value: |config| optional_text(config.provider.openai_reasoning_effort.as_deref()),
    },
    Spec {
        key: "provider.anthropic_reasoning_effort",
        label: "Anthropic reasoning effort",
        env: Some(("HORON_AGENT_ANTHROPIC_REASONING_EFFORT", non_empty)),
        value: |config| optional_text(config.provider.anthropic_reasoning_effort.as_deref()),
    },
    Spec {
        key: "provider.stream_idle_timeout_secs",
        label: "Stream idle timeout (seconds)",
        env: Some(("HORON_AGENT_STREAM_IDLE_TIMEOUT_SECS", positive_u64)),
        value: |config| config.provider.stream_idle_timeout_secs.to_string(),
    },
    Spec {
        key: "provider.max_retries",
        label: "Max request attempts",
        env: Some(("HORON_AGENT_MAX_RETRIES", positive_u32)),
        value: |config| config.provider.max_retries.to_string(),
    },
    Spec {
        key: "tools.profile",
        label: "Tool profile",
        env: Some(("HORON_AGENT_TOOL_PROFILE", any_text)),
        value: |config| optional_text(Some(config.tools.profile.as_str())),
    },
    Spec {
        key: "tools.mcp_tools",
        label: "MCP tool exposure",
        env: Some(("HORON_AGENT_MCP_TOOLS", valid_mcp_mode)),
        value: |config| config.tools.mcp_tools.as_str().to_string(),
    },
    Spec {
        key: "tools.approval_mode",
        label: "Approval mode",
        env: None,
        value: |config| config.tools.approval_mode.as_str().to_string(),
    },
    Spec {
        key: "tools.ask_policy",
        label: "Ask policy",
        env: None,
        value: |config| config.tools.ask_policy.as_str().to_string(),
    },
    Spec {
        key: "tools.secrets_mode",
        label: "Secrets mode",
        env: None,
        value: |config| config.tools.secrets_mode.as_str().to_string(),
    },
    Spec {
        key: "compaction.mode",
        label: "Compaction mode",
        env: None,
        value: |config| config.compaction.mode.as_str().to_string(),
    },
    Spec {
        key: "messages.follow_up_mode",
        label: "Follow-up delivery",
        env: None,
        value: |config| config.messages.follow_up_mode.as_str().to_string(),
    },
    Spec {
        key: "messages.steering_mode",
        label: "Steering delivery",
        env: None,
        value: |config| config.messages.steering_mode.as_str().to_string(),
    },
    Spec {
        key: "messages.interrupt_mode",
        label: "Interrupt timing",
        env: None,
        value: |config| config.messages.interrupt_mode.as_str().to_string(),
    },
];

/// Answer `get_effective_settings`, or `None` for every other request.
///
/// The values are the daemon's config, so the bridge answers without a daemon
/// round trip: the file it reads is the same one the daemon loaded, and the
/// request has to work on a daemon too old to know it.
pub(crate) fn reply(request: &Value) -> Option<ServerFrame> {
    if request["req"] != "get_effective_settings" {
        return None;
    }
    let reply_to = request["id"].as_u64()?;
    let session_id = match request["session_id"].as_str() {
        Some(session_id) => session_id.to_string(),
        None => String::new(),
    };
    let config = horon_agent_base::config::config();
    let file = Config::path().as_deref().map(FileLayer::read);
    // `var_os` keeps a non-UTF-8 value from being read as "unset", and
    // `to_str` returns `Option`, so no fallible conversion is swallowed here.
    let settings = report(&config, file.as_ref(), |name| {
        std::env::var_os(name).and_then(|value| value.to_str().map(str::to_owned))
    });
    Some(ServerFrame::reply(
        reply_to,
        ApiEvent::EffectiveSettings { session_id, settings },
    ))
}

/// The rows for `config`, with each one's source decided from `file` and `env`.
///
/// Split from [`reply`] so a test can supply an exact config, file and
/// environment instead of the process's own.
fn report(config: &Config, file: Option<&FileLayer>, env: impl Fn(&str) -> Option<String>) -> Vec<EffectiveSetting> {
    SPECS
        .iter()
        .map(|spec| {
            let (source, origin) = provenance(spec, file, &env);
            EffectiveSetting {
                key: spec.key.to_string(),
                label: spec.label.to_string(),
                value: (spec.value)(config),
                source,
                origin,
            }
        })
        .collect()
}

/// Which layer supplied `spec`'s value, and the path or name it points at.
fn provenance(
    spec: &Spec,
    file: Option<&FileLayer>,
    env: &impl Fn(&str) -> Option<String>,
) -> (SettingSource, Option<String>) {
    if let Some((name, accepts)) = spec.env
        && let Some(raw) = env(name)
        && accepts(&raw)
    {
        return (SettingSource::Env, Some(name.to_string()));
    }
    if let Some(file) = file
        && file.holds(spec.key)
    {
        return (SettingSource::File, Some(file.path.clone()));
    }
    (SettingSource::Default, None)
}

/// The config file as text, kept only for the presence checks a source needs.
struct FileLayer {
    path: String,
    document: Option<toml::Value>,
}

impl FileLayer {
    /// Read the file, or keep it as absent when it cannot be read or parsed.
    ///
    /// A file that does not parse had no effect on the daemon either, so every
    /// row falls through to the default rather than naming a file that was
    /// ignored. A file that is not there reads the same way: no document, so
    /// nothing it never held can be named as a source.
    fn read(path: &Path) -> Self {
        Self {
            path: path.display().to_string(),
            document: read_document(path),
        }
    }

    /// Whether this file declares `key`, so the row can name it.
    fn holds(&self, key: &str) -> bool {
        let Some(document) = &self.document else {
            return false;
        };
        let mut cursor = document;
        for part in key.split('.') {
            match cursor.get(part) {
                Some(next) => cursor = next,
                None => return false,
            }
        }
        true
    }
}

/// The TOML `path` holds, or `None` when it is missing or malformed.
///
/// Both failures are dropped instead of logged on purpose. A TOML error quotes
/// the offending line, and that line is exactly where a credential would sit,
/// so the text must not reach a log or a client; the daemon ignored a file it
/// could not parse, and this answers the same way.
fn read_document(path: &Path) -> Option<toml::Value> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return None;
    };
    let Ok(document) = toml::from_str::<toml::Value>(&text) else {
        return None;
    };
    Some(document)
}

/// A value's text, with an unset key said out loud rather than left blank.
fn optional_text(value: Option<&str>) -> String {
    match value.map(str::trim) {
        Some(text) if !text.is_empty() => text.to_string(),
        _ => "(unset)".to_string(),
    }
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
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A config with distinctive non-default values, so a row that fell back
    /// to a default would be visible rather than plausible.
    fn distinctive_config() -> Config {
        let mut config = Config::default();
        config.provider.openai_reasoning_effort = Some("high".into());
        config.provider.stream_idle_timeout_secs = 240;
        config.provider.max_retries = 3;
        config.tools.profile = "full".into();
        config
    }

    fn file(text: &str) -> FileLayer {
        FileLayer {
            path: "/home/u/config.toml".into(),
            document: Some(toml::from_str(text).expect("the fixture parses")),
        }
    }

    fn empty_env(_: &str) -> Option<String> {
        None
    }

    fn pick<'a>(rows: &'a [EffectiveSetting], key: &str) -> &'a EffectiveSetting {
        rows.iter()
            .find(|row| row.key == key)
            .expect("the allow-list names this key")
    }

    #[test]
    fn an_unset_key_reports_the_default_with_no_origin() {
        // The shipped default names an effort ("low"), so the fixture clears it
        // to reach the unset case: the row must say the value is absent instead
        // of printing a default the daemon never had to invent.
        let mut config = Config::default();
        config.provider.openai_reasoning_effort = None;
        let rows = report(&config, None, empty_env);
        let row = pick(&rows, "provider.openai_reasoning_effort");
        assert_eq!(row.source, SettingSource::Default);
        assert_eq!(row.origin, None);
        assert_eq!(row.value, "(unset)", "an absent value is said out loud");
    }

    #[test]
    fn a_key_the_file_declares_names_that_file() {
        let rows = report(
            &distinctive_config(),
            Some(&file("[provider]\nstream_idle_timeout_secs = 240\n")),
            empty_env,
        );
        let row = pick(&rows, "provider.stream_idle_timeout_secs");
        assert_eq!(row.value, "240");
        assert_eq!(row.source, SettingSource::File);
        assert_eq!(row.origin.as_deref(), Some("/home/u/config.toml"));
    }

    #[test]
    fn a_key_the_file_does_not_declare_stays_a_default_even_when_it_differs() {
        // The config's value could come from anywhere; only the file it was
        // actually written in may be named as the source.
        let rows = report(
            &distinctive_config(),
            Some(&file("[tools]\nprofile = \"full\"\n")),
            empty_env,
        );
        let row = pick(&rows, "provider.stream_idle_timeout_secs");
        assert_eq!(row.value, "240");
        assert_eq!(
            row.source,
            SettingSource::Default,
            "a file that never mentions the key is not its source"
        );
    }

    #[test]
    fn an_environment_variable_that_the_daemon_would_apply_wins_over_the_file() {
        let env: BTreeMap<&str, &str> = BTreeMap::from([("HORON_AGENT_STREAM_IDLE_TIMEOUT_SECS", "300")]);
        let rows = report(
            &distinctive_config(),
            Some(&file("[provider]\nstream_idle_timeout_secs = 240\n")),
            |name| env.get(name).map(|value| (*value).to_string()),
        );
        let row = pick(&rows, "provider.stream_idle_timeout_secs");
        assert_eq!(row.source, SettingSource::Env);
        assert_eq!(row.origin.as_deref(), Some("HORON_AGENT_STREAM_IDLE_TIMEOUT_SECS"));
    }

    #[test]
    fn an_environment_value_the_daemon_ignores_is_not_named_as_the_source() {
        // A zero timeout is filtered out by the daemon, so the file's value is
        // still the one in force and the variable must not be blamed for it.
        let env: BTreeMap<&str, &str> = BTreeMap::from([("HORON_AGENT_STREAM_IDLE_TIMEOUT_SECS", "0")]);
        let rows = report(
            &distinctive_config(),
            Some(&file("[provider]\nstream_idle_timeout_secs = 240\n")),
            |name| env.get(name).map(|value| (*value).to_string()),
        );
        let row = pick(&rows, "provider.stream_idle_timeout_secs");
        assert_eq!(row.source, SettingSource::File);
    }

    #[test]
    fn a_file_that_does_not_parse_supplies_no_source() {
        let broken = FileLayer {
            path: "/home/u/config.toml".into(),
            document: None,
        };
        let rows = report(&distinctive_config(), Some(&broken), empty_env);
        assert!(
            rows.iter().all(|row| row.source == SettingSource::Default),
            "a file the daemon ignored cannot be the source of anything"
        );
    }

    #[test]
    fn a_missing_file_records_no_document_so_no_row_names_it() {
        // The path is kept for a file that exists, but a file that is not there
        // supplied nothing: every row reads as the default instead of naming a
        // path the daemon never read.
        let missing = FileLayer::read(Path::new("/nonexistent/horon-agent/effective-settings/config.toml"));
        assert!(!missing.holds("provider.stream_idle_timeout_secs"));
        let rows = report(&distinctive_config(), Some(&missing), empty_env);
        assert!(
            rows.iter().all(|row| row.source == SettingSource::Default),
            "a file that was never read cannot be the source of anything"
        );
    }

    #[test]
    fn a_blank_tool_profile_variable_is_the_source_the_daemon_applied() {
        // `HORON_AGENT_TOOL_PROFILE` is assigned without a blank check, so an
        // empty value clears the profile rather than leaving the file's value
        // in force. Naming the file, or the default, would be wrong.
        let env: BTreeMap<&str, &str> = BTreeMap::from([("HORON_AGENT_TOOL_PROFILE", "")]);
        let mut config = distinctive_config();
        config.tools.profile = String::new();
        let rows = report(&config, Some(&file("[tools]\nprofile = \"full\"\n")), |name| {
            env.get(name).map(|value| (*value).to_string())
        });
        let row = pick(&rows, "tools.profile");
        assert_eq!(row.source, SettingSource::Env);
        assert_eq!(row.origin.as_deref(), Some("HORON_AGENT_TOOL_PROFILE"));
        assert_eq!(row.value, "(unset)", "a cleared profile is said out loud");
    }

    #[test]
    fn the_report_holds_only_allow_listed_keys() {
        let rows = report(&distinctive_config(), None, empty_env);
        let keys: Vec<&str> = rows.iter().map(|row| row.key.as_str()).collect();
        let allowed: Vec<&str> = SPECS.iter().map(|spec| spec.key).collect();
        assert_eq!(keys, allowed, "the report is the allow-list, in its order");
        for forbidden in [
            "provider.api_key_env",
            "providers",
            "stream_rules",
            "hooks",
            "mcp",
            "desktop",
        ] {
            assert!(
                !keys.iter().any(|key| key.starts_with(forbidden)),
                "`{forbidden}` must never be reported"
            );
        }
    }

    #[test]
    fn a_get_effective_settings_request_is_answered_for_the_named_session() {
        let frame = reply(&serde_json::json!({"v": 1, "id": 7, "req": "get_effective_settings", "session_id": "s1"}))
            .expect("the request is answered");

        assert_eq!(frame.reply_to, Some(7));
        match frame.event {
            ApiEvent::EffectiveSettings { session_id, settings } => {
                assert_eq!(session_id, "s1");
                assert!(!settings.is_empty(), "the allow-list is never empty");
            }
            other => panic!("expected effective_settings, got {other:?}"),
        }
    }

    #[test]
    fn any_other_request_is_left_to_the_translator() {
        assert!(reply(&serde_json::json!({"v": 1, "id": 7, "req": "get_message_modes"})).is_none());
        assert!(reply(&serde_json::json!({"v": 1, "id": 7, "req": "list_sessions"})).is_none());
    }

    #[test]
    fn a_request_without_an_id_is_not_answered() {
        assert!(
            reply(&serde_json::json!({"v": 1, "req": "get_effective_settings", "session_id": "s1"})).is_none(),
            "without an id there is nothing to reply to"
        );
    }
}
