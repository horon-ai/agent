//! When `[sponsors]` is left out of a written config file.

use horon_agent_config_types::SponsorsConfig;

/// Whether integration discovery settings carry no information beyond the shipped
/// default, so `[sponsors]` can be left out of written config files.
///
/// Discovery originally shipped opt-in with `enabled = false`, and because
/// config saves serialize the whole struct, any save during that window froze
/// the old default into the user's file and permanently disabled discovery even
/// after the default flipped. Omitting default sections prevents a repeat.
pub(super) fn sponsors_is_default(sponsors: &SponsorsConfig) -> bool {
    sponsors.enabled && is_default_discovery_endpoint(&sponsors.endpoint)
}

/// Endpoints used by shipped defaults. These may also be explicit user choices.
fn is_default_discovery_endpoint(endpoint: &str) -> bool {
    matches!(
        endpoint.trim_end_matches('/'),
        "https://api.horon-agent.sh/v1/discovery" | "https://api.solosystems.dev/v1/discovery"
    )
}
