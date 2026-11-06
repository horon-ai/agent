//! Compatibility tests for notification settings that no longer exist.

use super::Config;

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
