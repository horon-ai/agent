use super::*;

fn inputs(bind: Option<&str>, section: HarnessApiConfig, credentials: bool) -> ExposureInputs {
    ExposureInputs {
        bind: bind.map(str::to_string),
        insecure_behind_proxy: None,
        section,
        credentials_available: credentials,
    }
}

#[test]
fn an_absent_bind_keeps_the_local_socket_only() {
    let resolved = resolve(&inputs(None, HarnessApiConfig::default(), false)).expect("local only resolves");
    assert_eq!(resolved, None);
}

#[test]
fn an_empty_bind_is_the_local_socket_only() {
    let resolved = resolve(&inputs(Some("   "), HarnessApiConfig::default(), false)).expect("blank resolves");
    assert_eq!(resolved, None);
    let section = HarnessApiConfig {
        bind: Some(String::new()),
        insecure_behind_proxy: false,
    };
    assert_eq!(
        resolve(&inputs(None, section, false)).expect("blank section resolves"),
        None
    );
}

#[test]
fn a_loopback_bind_with_a_credential_is_served() {
    let resolved = resolve(&inputs(Some("127.0.0.1:7946"), HarnessApiConfig::default(), true))
        .expect("loopback exposure resolves")
        .expect("an exposure is requested");
    assert_eq!(resolved.bind().to_string(), "127.0.0.1:7946");
    assert!(resolved.is_loopback());
    assert!(!resolved.insecure_behind_proxy());
}

#[test]
fn a_loopback_bind_without_a_credential_is_refused() {
    let error = resolve(&inputs(Some("[::1]:7946"), HarnessApiConfig::default(), false))
        .expect_err("loopback without a credential is refused");
    let text = error.to_string();
    assert!(text.contains("api-credentials issue"), "{text}");
    assert!(text.contains("every TCP connection authenticates"), "{text}");
}

#[test]
fn a_non_loopback_bind_without_a_credential_or_the_acknowledgement_names_both() {
    let error = resolve(&inputs(Some("0.0.0.0:7946"), HarnessApiConfig::default(), false)).expect_err("refused");
    let text = error.to_string();
    assert!(text.contains("api-credentials issue"), "{text}");
    assert!(text.contains("--api-insecure-behind-proxy"), "{text}");
    assert!(text.contains("insecure_behind_proxy = true"), "{text}");
}

#[test]
fn a_non_loopback_bind_with_a_credential_but_no_acknowledgement_is_refused() {
    let error = resolve(&inputs(Some("0.0.0.0:7946"), HarnessApiConfig::default(), true)).expect_err("refused");
    let text = error.to_string();
    assert!(text.contains("--api-insecure-behind-proxy"), "{text}");
    assert!(!text.contains("api-credentials issue"), "{text}");
}

#[test]
fn a_non_loopback_bind_with_the_flag_and_a_credential_is_served() {
    let resolved = ExposureInputs {
        bind: Some("0.0.0.0:7946".to_string()),
        insecure_behind_proxy: Some(true),
        section: HarnessApiConfig::default(),
        credentials_available: true,
    };
    let exposure = resolve(&resolved)
        .expect("acknowledged exposure resolves")
        .expect("exposure");
    assert_eq!(exposure.bind().to_string(), "0.0.0.0:7946");
    assert!(!exposure.is_loopback());
    assert!(exposure.insecure_behind_proxy());
}

#[test]
fn the_config_file_can_supply_the_bind_and_the_acknowledgement() {
    let section = HarnessApiConfig {
        bind: Some("0.0.0.0:7946".to_string()),
        insecure_behind_proxy: true,
    };
    let exposure = resolve(&inputs(None, section, true))
        .expect("the file enables the listener")
        .expect("exposure");
    assert_eq!(exposure.bind().to_string(), "0.0.0.0:7946");
    assert!(exposure.insecure_behind_proxy());
}

#[test]
fn an_explicit_bind_overrides_the_config_file() {
    let section = HarnessApiConfig {
        bind: Some("0.0.0.0:7946".to_string()),
        insecure_behind_proxy: true,
    };
    let exposure = resolve(&inputs(Some("127.0.0.1:8100"), section, true))
        .expect("the flag wins")
        .expect("exposure");
    assert_eq!(exposure.bind().to_string(), "127.0.0.1:8100");
}

#[test]
fn an_explicit_false_acknowledgement_overrides_the_config_file() {
    let section = HarnessApiConfig {
        bind: Some("0.0.0.0:7946".to_string()),
        insecure_behind_proxy: true,
    };
    let refused = ExposureInputs {
        bind: None,
        insecure_behind_proxy: Some(false),
        section,
        credentials_available: true,
    };
    let error = resolve(&refused).expect_err("an explicit false must not inherit the file's acknowledgement");
    assert!(error.to_string().contains("--api-insecure-behind-proxy"));
}

#[test]
fn a_host_name_bind_is_refused_with_the_expected_form() {
    let error = resolve(&inputs(Some("localhost:7946"), HarnessApiConfig::default(), true))
        .expect_err("a host name is not a socket address");
    let text = error.to_string();
    assert!(text.contains("IP:PORT"), "{text}");
    assert!(text.contains("literal IPv4 or IPv6 address"), "{text}");
}

#[test]
fn a_wildcard_ipv6_bind_is_non_loopback_and_needs_the_acknowledgement() {
    // Fly Machines and every dual-stack host bind `[::]` to accept both
    // families, so the wildcard must be classified as an exposed address
    // rather than slipping through as loopback.
    let error =
        resolve(&inputs(Some("[::]:7946"), HarnessApiConfig::default(), true)).expect_err("[::] is not loopback");
    assert!(error.to_string().contains("--api-insecure-behind-proxy"), "{error}");
    let acknowledged = ExposureInputs {
        bind: Some("[::]:7946".to_string()),
        insecure_behind_proxy: Some(true),
        section: HarnessApiConfig::default(),
        credentials_available: true,
    };
    let exposure = resolve(&acknowledged)
        .expect("an acknowledged wildcard resolves")
        .expect("exposure");
    assert!(!exposure.is_loopback());
    assert_eq!(exposure.bind().to_string(), "[::]:7946");
}

#[test]
fn a_private_ipv6_literal_bind_is_non_loopback() {
    // A Fly 6PN address is an IPv6 ULA in `fdaa::/16`; the control plane may
    // name one directly instead of the wildcard.
    let error = resolve(&inputs(
        Some("[fdaa:0:1:a7b:1:2:3:4]:7946"),
        HarnessApiConfig::default(),
        true,
    ))
    .expect_err("a 6PN address is not loopback");
    assert!(error.to_string().contains("--api-insecure-behind-proxy"), "{error}");
    let acknowledged = ExposureInputs {
        bind: Some("[fdaa:0:1:a7b:1:2:3:4]:7946".to_string()),
        insecure_behind_proxy: Some(true),
        section: HarnessApiConfig::default(),
        credentials_available: true,
    };
    let exposure = resolve(&acknowledged)
        .expect("a 6PN literal resolves")
        .expect("exposure");
    assert!(!exposure.is_loopback());
    assert_eq!(exposure.bind().to_string(), "[fdaa:0:1:a7b:1:2:3:4]:7946");
}

#[test]
fn ipv6_loopback_is_loopback_and_needs_no_proxy_acknowledgement() {
    let exposure = resolve(&inputs(Some("[::1]:7946"), HarnessApiConfig::default(), true))
        .expect("IPv6 loopback with a credential is served")
        .expect("exposure");
    assert!(exposure.is_loopback());
    assert!(!exposure.insecure_behind_proxy());
}
