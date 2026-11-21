//! The allow-listed settings this daemon loaded, as the harness API carries
//! them.
//!
//! The rows themselves are decided by `horon_agent_base::effective_settings`,
//! which owns the configuration, its environment overrides and the launch
//! record. This module only maps them onto the protocol's reply, so the same
//! report answers the internal `get_effective_settings` request a bridge
//! forwards.
//!
//! Nothing here widens the allow-list: the keys, their labels and their values
//! all come from that table, and a row outside it cannot be produced.

use horon_agent_harness_api::{EffectiveSetting, SettingSource};

use crate::effective_settings::{EffectiveSettingRow, EffectiveSource, effective_settings};
use crate::protocol::ServerEvent;

/// The reply to `get_effective_settings`.
pub(super) fn event(id: u64) -> ServerEvent {
    let settings: Vec<EffectiveSetting> = effective_settings().into_iter().map(api_row).collect();
    ServerEvent::EffectiveSettings {
        id,
        // The row type is plain data, so this cannot fail; a failure would still
        // be reported as an unreadable reply rather than a wrong list.
        settings: serde_json::to_value(settings).unwrap_or(serde_json::Value::Null),
    }
}

fn api_row(row: EffectiveSettingRow) -> EffectiveSetting {
    EffectiveSetting {
        key: row.key,
        label: row.label,
        value: row.value,
        source: api_source(row.source),
        origin: row.origin,
    }
}

fn api_source(source: EffectiveSource) -> SettingSource {
    match source {
        EffectiveSource::Default => SettingSource::Default,
        EffectiveSource::Cli => SettingSource::Cli,
        EffectiveSource::Env => SettingSource::Env,
        EffectiveSource::File => SettingSource::File,
    }
}
