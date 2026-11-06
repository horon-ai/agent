//! Reporting for config file changes: what changed, and whether it is live.
//!
//! Editing `~/.horon-agent/config.toml` (by hand, via `/config`, or by an agent
//! writing the file) is only useful if you can tell whether the running
//! process actually picked the change up. Most of the config is re-read
//! through the reloadable [`crate::config::config`] cache and takes effect
//! within its staleness check, but a few sections are consumed once during
//! process/server startup and genuinely need a restart.
//!
//! This module turns "the file changed" into a specific, checkable answer:
//! which keys changed, from what to what, and whether each one is live now.

use super::ConfigCacheFingerprint;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};

/// Whether a changed config key takes effect in the running process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// Re-read through the config cache; the change is already in effect.
    Live,
    /// Consumed once at server/process startup; needs a restart.
    NeedsRestart,
}

impl Liveness {
    pub fn label(self) -> &'static str {
        match self {
            Self::Live => "live now",
            Self::NeedsRestart => "needs restart",
        }
    }
}

/// Config sections that are read once during startup rather than through the
/// reloadable config cache.
///
/// Keep this list small and evidence-based: add a section only when the code
/// that consumes it demonstrably snapshots the value at startup. Everything
/// else defaults to [`Liveness::Live`], which matches how `config()` works.
const RESTART_REQUIRED_SECTIONS: &[&str] = &[
    // `gateway.{enabled,port,bind_addr}` are snapshotted by
    // `Server::spawn_gateway` when the listener is created.
    "gateway", // ACP adapter settings are consumed when the adapter process starts.
    "acp",
];

/// Liveness of a dotted config key such as `keybindings.scroll_up`.
pub fn liveness_for_key(key: &str) -> Liveness {
    let section = key.split('.').next().unwrap_or(key);
    if RESTART_REQUIRED_SECTIONS.contains(&section) {
        Liveness::NeedsRestart
    } else {
        Liveness::Live
    }
}

/// A single changed config key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigChange {
    /// Dotted key path, e.g. `keybindings.scroll_up`.
    pub key: String,
    /// Rendered previous value, or `None` when the key was absent.
    pub before: Option<String>,
    /// Rendered new value, or `None` when the key was removed.
    pub after: Option<String>,
    pub liveness: Liveness,
}

impl ConfigChange {
    fn render(&self) -> String {
        let before = self.before.as_deref().unwrap_or("(unset)");
        let after = self.after.as_deref().unwrap_or("(unset)");
        format!("- `{}`: {} -> {} ({})", self.key, before, after, self.liveness.label())
    }
}

/// Diff two config files' TOML text into a per-key change list.
///
/// Both sides are parsed leniently: a side that fails to parse is treated as
/// empty, so a change report is still produced for a file that was previously
/// broken. Returns changes sorted by key.
pub fn diff_toml(before: &str, after: &str) -> Vec<ConfigChange> {
    let before = flatten_toml(before);
    let after = flatten_toml(after);

    let mut keys: Vec<&String> = before.keys().chain(after.keys()).collect();
    keys.sort();
    keys.dedup();

    keys.into_iter()
        .filter_map(|key| {
            let old = before.get(key);
            let new = after.get(key);
            if old == new {
                return None;
            }
            Some(ConfigChange {
                key: key.clone(),
                before: old.cloned(),
                after: new.cloned(),
                liveness: liveness_for_key(key),
            })
        })
        .collect()
}

/// Human/agent-readable summary of a config edit.
///
/// Returns `None` when nothing semantically changed (formatting or comment-only
/// edits), so callers can stay quiet instead of reporting a no-op.
pub fn summarize_toml_change(before: &str, after: &str) -> Option<String> {
    let changes = diff_toml(before, after);
    if changes.is_empty() {
        return None;
    }
    Some(summarize_changes(&changes))
}

/// Render an already-computed change list.
pub fn summarize_changes(changes: &[ConfigChange]) -> String {
    let mut out = String::from("Config changes:\n");
    for change in changes {
        out.push_str(&change.render());
        out.push('\n');
    }

    let restart: Vec<&ConfigChange> = changes
        .iter()
        .filter(|c| c.liveness == Liveness::NeedsRestart)
        .collect();
    if restart.is_empty() {
        out.push_str(
            "All changes are live in running horon-agent sessions; no restart needed.\
             \nKeybinding edits apply to the next keystroke.",
        );
    } else {
        let keys: Vec<&str> = restart.iter().map(|c| c.key.as_str()).collect();
        out.push_str(&format!(
            "Restart required for: {}. Other changes are already live.",
            keys.join(", ")
        ));
    }
    out
}

/// Flatten TOML text into dotted-key -> rendered-value pairs.
///
/// Arrays and inline tables render as compact JSON so an element-level edit
/// still shows up as a change on the owning key.
fn flatten_toml(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(value) = toml::from_str::<Value>(text) else {
        return out;
    };
    flatten_value(String::new(), &value, &mut out);
    out
}

fn flatten_value(prefix: String, value: &Value, out: &mut BTreeMap<String, String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten_value(path, child, out);
            }
        }
        other => {
            if prefix.is_empty() {
                return;
            }
            out.insert(prefix, render_value(other));
        }
    }
}

fn render_value(value: &Value) -> String {
    match value {
        Value::String(s) => format!("\"{s}\""),
        other => other.to_string(),
    }
}

pub(super) fn describe_config_reload(
    forced: bool,
    previous: &ConfigCacheFingerprint,
    next: &ConfigCacheFingerprint,
) -> String {
    let mut parts = Vec::new();
    if forced {
        parts.push("forced=true".to_string());
    }
    if previous.path != next.path {
        parts.push(format!(
            "path={:?}->{:?}",
            previous.path.as_ref().map(|p| p.display().to_string()),
            next.path.as_ref().map(|p| p.display().to_string())
        ));
    }
    if previous.modified != next.modified {
        parts.push("modified_changed=true".to_string());
    }
    if previous.len != next.len {
        parts.push(format!("len={:?}->{:?}", previous.len, next.len));
    }
    let env_changes = describe_env_changes(&previous.env, &next.env);
    if !env_changes.is_empty() {
        parts.push(format!("env=[{}]", env_changes.join(", ")));
    }
    if parts.is_empty() {
        "unchanged".to_string()
    } else {
        parts.join(" ")
    }
}

pub(super) fn describe_env_changes(previous: &[(String, String)], next: &[(String, String)]) -> Vec<String> {
    let previous_map: BTreeMap<&str, &str> = previous
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let next_map: BTreeMap<&str, &str> = next.iter().map(|(key, value)| (key.as_str(), value.as_str())).collect();
    let keys: BTreeSet<&str> = previous_map.keys().chain(next_map.keys()).copied().collect();

    keys.into_iter()
        .filter_map(|key| match (previous_map.get(key), next_map.get(key)) {
            (Some(previous), Some(next)) if previous != next => Some(format!(
                "{}:changed({}->{})",
                key,
                env_value_fingerprint(previous),
                env_value_fingerprint(next)
            )),
            (None, Some(next)) => Some(format!("{}:added({})", key, env_value_fingerprint(next))),
            (Some(previous), None) => Some(format!("{}:removed({})", key, env_value_fingerprint(previous))),
            _ => None,
        })
        .collect()
}

fn env_value_fingerprint(value: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    format!("len:{} hash:{:016x}", value.len(), hasher.finish())
}

#[cfg(test)]
#[path = "change_report_tests.rs"]
mod tests;
