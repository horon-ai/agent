//! `get_effective_settings`: the daemon values a client may display, read-only.
//!
//! The daemon holds far more configuration than a client should ever see. A
//! settings screen needs a small, named subset — how much the model
//! deliberates, how long a silent request may run, which tools a session
//! exposes — and it needs to say *where each value came from*, because
//! "I did not set that" and "the file disagrees with me" are different
//! problems.
//!
//! So this is an allow-list, not a dump. Every row names its key, a label a
//! screen can print, the value in force as text, and the layer that supplied
//! it. Nothing outside the list can travel: there is no "other keys" field and
//! no field carrying raw file text, so a credential cannot reach a client even
//! when the config file holds one.
//!
//! The daemon answers from its own loaded configuration: it, not the bridge, is
//! the process whose environment, launch flags and config file the session
//! actually runs with. The bridge translates the reply and advertises
//! [`CAPABILITY_EFFECTIVE_SETTINGS_V1`](crate::CAPABILITY_EFFECTIVE_SETTINGS_V1);
//! against a daemon that predates the request it answers `unsupported` naming
//! the capability, so a client sees the gap instead of waiting out a timeout.

use serde::{Deserialize, Serialize};

/// Where one reported value came from.
///
/// The spellings are stable identifiers rather than prose: a client maps them
/// to its own words, and `origin` carries the file path, the variable name or
/// the flag so a screen can be specific without the server writing a sentence.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SettingSource {
    /// No file, variable or flag set it; the shipped default is in force.
    Default,
    /// A command-line flag supplied it. `origin` names the flag.
    Cli,
    /// A settings file supplied it.
    File,
    /// An environment variable overrode everything else.
    Env,
}

impl SettingSource {
    /// The spelling this source travels as.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Cli => "cli",
            Self::File => "file",
            Self::Env => "env",
        }
    }
}

/// One daemon value a client may display.
///
/// `value` is text because the screen prints it: a client that rendered a
/// number itself would have to know which keys are numeric, and that knowledge
/// belongs beside the allow-list rather than in every client.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EffectiveSetting {
    /// Dotted config path, e.g. `provider.stream_idle_timeout_secs`.
    pub key: String,
    /// Human-readable name for the row.
    pub label: String,
    /// The value in force, as the screen prints it.
    pub value: String,
    /// The layer that supplied it.
    pub source: SettingSource,
    /// The file path, environment variable name or flag `source` points at.
    ///
    /// Absent for [`SettingSource::Default`], which names no origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}
