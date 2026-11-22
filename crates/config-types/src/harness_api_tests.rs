use super::*;

#[test]
fn the_section_defaults_to_no_tcp_listener_and_no_acknowledgement() {
    let empty: HarnessApiConfig = toml::from_str("").expect("an empty section parses");
    assert_eq!(empty, HarnessApiConfig::default());
    assert_eq!(empty.bind, None);
    assert!(!empty.insecure_behind_proxy);
}

#[test]
fn the_section_reads_a_bind_address_and_the_proxy_acknowledgement() {
    let section: HarnessApiConfig =
        toml::from_str("bind = \"127.0.0.1:7946\"\ninsecure_behind_proxy = true\n").expect("section parses");
    assert_eq!(section.bind.as_deref(), Some("127.0.0.1:7946"));
    assert!(section.insecure_behind_proxy);
}

#[test]
fn a_section_without_bind_keeps_the_local_socket_only() {
    let section: HarnessApiConfig = toml::from_str("insecure_behind_proxy = true\n").expect("section parses");
    assert_eq!(section.bind, None);
    assert!(section.insecure_behind_proxy);
}

#[test]
fn the_section_round_trips_through_serialization() {
    let section = HarnessApiConfig {
        bind: Some("[::1]:7946".to_string()),
        insecure_behind_proxy: true,
    };
    let text = toml::to_string(&section).expect("a section serializes");
    let reread: HarnessApiConfig = toml::from_str(&text).expect("what we write parses back");
    assert_eq!(reread, section);
}
