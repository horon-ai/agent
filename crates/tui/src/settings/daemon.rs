//! The daemon values a settings screen may display, and what a client knows
//! about them.
//!
//! The list is the daemon's, not this client's: the server decides which keys
//! it reports and sends each one as labelled text with the layer behind it.
//! So the client holds the rows verbatim and never derives a value itself.
//! Four states matter to the screen, and they are separate on purpose:
//!
//! * nothing has been asked yet (`Unknown`), so the tab says so instead of
//!   pretending the defaults are the daemon's;
//! * the daemon or bridge cannot answer (`Unavailable`), which is a working
//!   connection that simply predates the request: the rest of the panel keeps
//!   working and the tab explains the gap;
//! * the read itself failed (`Failed`), which is a different problem from a
//!   daemon that has nothing to say and must not leave the tab waiting;
//! * the rows arrived (`Loaded`), which may legitimately be empty.
//!
//! Nothing here writes, and no row carries raw file text: the only text a row
//! holds is the value the server chose to report.

use horon_agent_sdk::api::{EffectiveSetting, SettingSource};

/// What the client knows about the daemon's allow-listed settings.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum DaemonSettings {
    /// Not asked yet, or the answer has not arrived.
    #[default]
    Unknown,
    /// The daemon or bridge could not answer, and why.
    Unavailable(String),
    /// The read failed, and why. Separate from [`Self::Unavailable`] because a
    /// broken read must not be shown as a daemon that has nothing to report,
    /// and it must not leave the tab on "reading" with no request in flight.
    Failed(String),
    /// The rows the server reported, in its order.
    Loaded(Vec<DaemonRow>),
}

/// One reported value, kept as the server wrote it.
#[derive(Debug, Clone, PartialEq)]
pub struct DaemonRow {
    /// Dotted config path, e.g. `provider.stream_idle_timeout_secs`.
    pub key: String,
    /// Name the screen prints for the row.
    pub label: String,
    /// The value in force, as text.
    pub value: String,
    /// The layer that supplied it.
    pub source: SettingSource,
    /// File path, variable name or flag `source` points at.
    pub origin: Option<String>,
}

impl DaemonRow {
    /// Where this row's value came from, as one line.
    ///
    /// A file is named by its path, an environment override by its variable
    /// name and a command-line flag by the flag itself, so the row answers
    /// "why is this not what I set?" without the screen guessing at layers.
    pub fn source_label(&self) -> String {
        match (&self.source, &self.origin) {
            (SettingSource::Default, _) | (_, None) => "daemon default".to_string(),
            (SettingSource::Cli, Some(flag)) => format!("cli {flag}"),
            (SettingSource::File, Some(path)) => path.clone(),
            (SettingSource::Env, Some(name)) => format!("env {name}"),
        }
    }
}

impl DaemonSettings {
    /// The rows, or an empty slice in each state that reports none.
    pub fn rows(&self) -> &[DaemonRow] {
        match self {
            Self::Loaded(rows) => rows,
            Self::Unknown | Self::Unavailable(_) | Self::Failed(_) => &[],
        }
    }

    /// The sentence the screen shows instead of rows, when it shows none.
    pub fn notice(&self) -> &str {
        match self {
            Self::Unknown => "reading the daemon settings",
            Self::Unavailable(_) => "this daemon does not report its settings",
            Self::Failed(_) => "the daemon settings could not be read",
            Self::Loaded(rows) if rows.is_empty() => "the daemon reported no settings",
            Self::Loaded(_) => "",
        }
    }
}

impl From<Vec<EffectiveSetting>> for DaemonSettings {
    fn from(settings: Vec<EffectiveSetting>) -> Self {
        Self::Loaded(
            settings
                .into_iter()
                .map(|setting| DaemonRow {
                    key: setting.key,
                    label: setting.label,
                    value: setting.value,
                    source: setting.source,
                    origin: setting.origin,
                })
                .collect(),
        )
    }
}
