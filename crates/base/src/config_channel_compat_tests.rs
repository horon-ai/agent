//! Compatibility tests for settings that no longer exist.

use super::Config;
use crate::config::SwarmSpawnMode;

/// The messenger channel and email keys were removed from `[safety]`. A config
/// file written by an older version still sets them, so it must load and keep
/// the settings that remain.
#[test]
fn removed_channel_and_email_keys_still_load_config() {
    let config: Config = toml::from_str(
        "[safety]\ntelegram_enabled = true\nemail_smtp_host = \"smtp.gmail.com\"\ndiscord_enabled = true\njade_relay_enabled = true\nntfy_topic = \"topic\"\ndesktop_notifications = false\n",
    )
    .expect("legacy channel keys must not break config");
    assert_eq!(config.safety.ntfy_topic.as_deref(), Some("topic"));
    assert!(!config.safety.desktop_notifications);
}

/// `[notifications]`, the native provider-search switches, the inline swarm
/// gallery layout, and the Copilot premium / model-picker provider keys are
/// gone. Unknown keys are ignored, so an older file must still load and keep
/// every surviving setting.
#[test]
fn removed_sections_and_keys_still_load_config() {
    let config: Config = toml::from_str(
        r#"
[notifications]
turn_complete = true
turn_complete_min_secs = 60
turn_complete_sound = "Glass"

[websearch]
engine = "bing"
prefer_native = true
native_max_uses = 5
native_allowed_domains = ["docs.rs"]
native_blocked_domains = ["example.com"]
native_anthropic_tool_version = "web_search_20250305"

[agents]
swarm_spawn_mode = "visible"
swarm_gallery_max_pct = 40
swarm_strip_layout = "horizontal"

[provider]
copilot_premium = "zero"
model_picker_providers = ["openrouter"]
default_model = "kept-model"
"#,
    )
    .expect("removed sections and keys must not break config");

    assert_eq!(config.websearch.engine, super::WebSearchEngine::Bing);
    assert_eq!(config.agents.swarm_spawn_mode, SwarmSpawnMode::Inline);
    assert_eq!(config.provider.default_model.as_deref(), Some("kept-model"));
}

/// The removed `visible`/`headed` swarm spawn values selected a terminal
/// window. They now mean the in-process `inline` mode, both in a config file
/// and through the environment override parser.
#[test]
fn legacy_visible_spawn_mode_means_inline() {
    for value in ["visible", "headed"] {
        assert_eq!(SwarmSpawnMode::parse(value), Some(SwarmSpawnMode::Inline));
        let config: Config = toml::from_str(&format!("[agents]\nswarm_spawn_mode = \"{value}\"\n"))
            .expect("a legacy visible/headed swarm_spawn_mode must keep parsing");
        assert_eq!(config.agents.swarm_spawn_mode, SwarmSpawnMode::Inline);
    }
}
