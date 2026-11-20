//! Read the daemon's allow-listed settings for the settings screen.
//!
//! The read is optional: a daemon or bridge older than the request answers
//! with `unknown_request`, and the SDK refuses it before sending when the
//! capability was never advertised. Both mean the same thing to the screen —
//! this daemon does not report its settings — so they become
//! [`DaemonSettings::Unavailable`] and the rest of the panel keeps working.
//! Any other failure is an error the caller reports, because a broken
//! connection must not be presented as "this daemon has no settings".

use horon_agent_sdk::api::ErrorCode;
use horon_agent_sdk::{Error, ErrorKind, HoronAgentClient};

use super::Incoming;
use crate::settings::DaemonSettings;

/// The rows `session_id`'s daemon reports, as the settings screen shows them.
pub(super) fn read(client: &HoronAgentClient, session_id: &str) -> Result<Option<Incoming>, Error> {
    if !client.supports(horon_agent_sdk::api::CAPABILITY_EFFECTIVE_SETTINGS_V1) {
        return Ok(Some(Incoming::DaemonSettings(DaemonSettings::Unavailable(
            "the bridge does not report daemon settings".to_string(),
        ))));
    }
    match client.get_effective_settings(session_id) {
        Ok(settings) => Ok(Some(Incoming::DaemonSettings(settings.into()))),
        Err(error) if unsupported(&error) => Ok(Some(Incoming::DaemonSettings(DaemonSettings::Unavailable(
            "the daemon does not report daemon settings".to_string(),
        )))),
        Err(error) => Err(error),
    }
}

/// Whether this failure means "this server has no such request" rather than
/// "the request broke".
fn unsupported(error: &Error) -> bool {
    matches!(
        &error.kind,
        ErrorKind::Unsupported
            | ErrorKind::Harness(ErrorCode::UnknownRequest)
            | ErrorKind::Harness(ErrorCode::Unsupported)
    )
}
