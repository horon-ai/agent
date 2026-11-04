use super::*;

#[test]
fn settings_without_the_failover_fields_keep_the_safe_defaults() {
    // A bare `#[serde(default)]` on the u32 would yield 0 attempts and silently
    // disable failover, which is why the field names a default function.
    let raw = r#"{ "stream_idle_timeout_secs": 180 }"#;
    let provider: ProviderConfig = serde_json::from_str(raw).expect("parse provider settings");
    assert!(!provider.autonomous_failover);
    assert_eq!(provider.autonomous_failover_max_attempts, 3);

    let defaults = ProviderConfig::default();
    assert!(!defaults.autonomous_failover);
    assert_eq!(defaults.autonomous_failover_max_attempts, 3);
}

#[test]
fn explicit_failover_settings_round_trip_by_value() {
    let raw = r#"{ "autonomous_failover": true, "autonomous_failover_max_attempts": 5 }"#;
    let provider: ProviderConfig = serde_json::from_str(raw).expect("parse provider settings");
    assert!(provider.autonomous_failover);
    assert_eq!(provider.autonomous_failover_max_attempts, 5);

    let encoded = serde_json::to_string(&provider).expect("serialize provider settings");
    let decoded: ProviderConfig = serde_json::from_str(&encoded).expect("parse provider settings");
    assert!(decoded.autonomous_failover);
    assert_eq!(decoded.autonomous_failover_max_attempts, 5);
}
