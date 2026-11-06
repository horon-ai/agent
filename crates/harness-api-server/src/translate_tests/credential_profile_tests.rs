//! `SetApiKey` provider names must map to profiles the daemon can activate.

use super::tests::{ScopedHoronAgentHome, only_reply_event};
use super::*;

#[test]
fn credential_provisioning_maps_every_accepted_name_to_a_live_profile() {
    let home = ScopedHoronAgentHome::new("credential-profile-ids");
    let config = home.path.join("config/horon-agent");
    std::fs::create_dir_all(&config).unwrap();

    for (index, provider) in [
        "anthropic-api",
        "claude-api",
        "anthropic-key",
        "claude-key",
        "openai-api",
        "openai-key",
        "openai-apikey",
        "openrouter",
        "gemini-api",
        "gemini",
        "google-gemini",
    ]
    .iter()
    .enumerate()
    {
        let outbound = BridgeState::default().api_request_to_legacy(&json!({
            "req": "set_api_key",
            "id": index as u64,
            "provider": provider,
            "api_key": "secret"
        }));
        let [Outbound::Legacy(notify)] = outbound.as_slice() else {
            panic!("{provider} should be accepted: {outbound:?}");
        };
        let canonical = notify["provider"].as_str().expect("canonical provider id");
        let profile = horon_agent_base::provider_catalog::openai_compatible_profile_by_id(canonical)
            .unwrap_or_else(|| panic!("{provider} maps to unknown profile {canonical}"));
        let saved = std::fs::read_to_string(config.join(profile.env_file))
            .unwrap_or_else(|error| panic!("{provider} did not write {}: {error}", profile.env_file));
        assert!(
            saved.contains(&format!("{}=", profile.api_key_env)),
            "{provider} wrote the wrong env key into {}: {saved}",
            profile.env_file
        );
    }

    for provider in ["cursor", "horon-agent"] {
        let event = only_reply_event(BridgeState::default().api_request_to_legacy(&json!({
            "req": "set_api_key",
            "id": 99,
            "provider": provider,
            "api_key": "secret"
        })));
        assert!(
            matches!(
                event,
                ApiEvent::Error {
                    code: ErrorCode::InvalidRequest,
                    ..
                }
            ),
            "{provider} must be rejected: {event:?}"
        );
    }
    assert!(!config.join("cursor.env").exists());
}
