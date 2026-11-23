use super::*;

#[test]
fn the_section_reads_a_bind_address_and_the_proxy_acknowledgement() {
    let section: HarnessApiConfig =
        toml::from_str("bind = \"127.0.0.1:7946\"\ninsecure_behind_proxy = true\n").expect("section parses");
    assert_eq!(section.bind.as_deref(), Some("127.0.0.1:7946"));
    assert!(section.insecure_behind_proxy);
}
