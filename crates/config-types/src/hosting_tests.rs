use super::*;

fn tenant(raw: &str) -> TenantId {
    TenantId::parse(raw).expect("valid tenant id")
}

#[test]
fn unset_environment_selects_no_hosting() {
    let policy = HostingPolicy::new(HostingMode::None, None, &HostingSection::default()).expect("policy");
    assert_eq!(policy.mode(), HostingMode::None);
    assert!(!policy.is_hosted());
    assert!(!policy.is_per_user_container());
    assert!(!policy.is_shared_worker());
    assert!(policy.inherits_operator_logins());
    assert_eq!(policy.tenant_id(), None);
}

#[test]
fn each_selectable_mode_parses_and_serves_its_tenant() {
    let container = HostingPolicy::new(
        HostingMode::parse("per-user-container").expect("mode"),
        Some(tenant("alice")),
        &HostingSection::default(),
    )
    .expect("policy");
    assert!(container.is_per_user_container());
    assert_eq!(container.tenant_id().map(TenantId::as_str), Some("alice"));
    assert!(container.allows_shared_children());
    assert!(!container.inherits_operator_logins());

    let worker = HostingPolicy::new(
        HostingMode::parse("shared-worker").expect("mode"),
        Some(tenant("bob")),
        &HostingSection::default(),
    )
    .expect("policy");
    assert!(worker.is_shared_worker());
    assert!(!worker.allows_shared_children());
}

#[test]
fn an_unknown_mode_names_the_allowed_values() {
    for raw in ["", "none", "container", "per_user_container", "Per-User-Container"] {
        let error = HostingMode::parse(raw).expect_err(raw);
        let message = error.to_string();
        assert!(message.contains("allowed values"), "{raw:?}: {message}");
        assert!(
            message.contains("per-user-container, shared-worker"),
            "{raw:?}: {message}"
        );
    }
}

#[test]
fn hosted_modes_require_a_tenant_id() {
    for mode in HostingMode::selectable() {
        let error = HostingPolicy::new(mode, None, &HostingSection::default()).expect_err("missing tenant");
        assert_eq!(error, HostingConfigError::MissingTenantId(mode));
        assert!(error.to_string().contains("HORON_AGENT_TENANT_ID"));
    }
}

#[test]
fn tenant_ids_that_could_escape_or_confuse_are_refused() {
    for raw in [
        "",
        ".",
        "..",
        "../alice",
        "alice/bob",
        "alice\\bob",
        "/alice",
        "-alice",
        "alice-",
        "Alice",
        "al ice",
        "alice.bob",
        "alicé",
        &"a".repeat(MAX_TENANT_ID_LEN + 1),
    ] {
        let error = TenantId::parse(raw).expect_err(raw);
        assert!(
            matches!(error, HostingConfigError::InvalidTenantId { .. }),
            "{raw:?} gave {error:?}"
        );
    }
    assert_eq!(
        TenantId::parse(&"a".repeat(MAX_TENANT_ID_LEN))
            .expect("max length is allowed")
            .as_str()
            .len(),
        MAX_TENANT_ID_LEN
    );
    assert_eq!(
        TenantId::parse("alice-01").expect("hyphen and digits").as_str(),
        "alice-01"
    );
}

#[test]
fn config_narrows_shared_children_but_never_widens_a_shared_worker() {
    let narrowed = HostingSection {
        allow_shared_children: Some(false),
        allowed_mcp_servers: None,
        mode: None,
    };
    let container =
        HostingPolicy::new(HostingMode::PerUserContainer, Some(tenant("alice")), &narrowed).expect("policy");
    assert!(!container.allows_shared_children());

    let widened = HostingSection {
        allow_shared_children: Some(true),
        allowed_mcp_servers: None,
        mode: Some("shared-worker".to_string()),
    };
    let worker = HostingPolicy::new(HostingMode::SharedWorker, Some(tenant("bob")), &widened).expect("policy");
    assert!(
        !worker.allows_shared_children(),
        "a file must not widen a shared worker"
    );
    assert_eq!(worker.mode(), HostingMode::SharedWorker);
}

#[test]
fn a_tenant_id_outside_hosting_is_not_part_of_the_identity() {
    let policy =
        HostingPolicy::new(HostingMode::None, Some(tenant("alice")), &HostingSection::default()).expect("policy");
    assert_eq!(policy.tenant_id(), None);
}

#[test]
fn an_absent_server_list_permits_every_server() {
    let policy = HostingPolicy::new(
        HostingMode::PerUserContainer,
        Some(tenant("alice")),
        &HostingSection::default(),
    )
    .expect("policy");
    assert_eq!(policy.allowed_mcp_servers(), None);
    assert!(policy.permits_mcp_server("anything"));
}

#[test]
fn an_empty_server_list_refuses_every_server() {
    // `[]` in the file must keep its meaning: refuse all, not "no narrowing".
    let section: HostingSection = toml::from_str("allowed_mcp_servers = []\n").expect("an empty array parses");
    assert_eq!(section.allowed_mcp_servers, Some(Vec::new()));
    let policy = HostingPolicy::new(HostingMode::PerUserContainer, Some(tenant("alice")), &section).expect("policy");
    assert_eq!(policy.allowed_mcp_servers(), Some(&[][..]));
    assert!(!policy.permits_mcp_server("anything"));
}

#[test]
fn a_server_list_is_trimmed_deduplicated_and_sorted() {
    let section: HostingSection =
        toml::from_str("allowed_mcp_servers = [\" zeta \", \"alpha\", \"alpha\", \"  \"]\n").expect("a list parses");
    let policy = HostingPolicy::new(HostingMode::PerUserContainer, Some(tenant("alice")), &section).expect("policy");
    assert_eq!(
        policy.allowed_mcp_servers(),
        Some(&["alpha".to_string(), "zeta".to_string()][..])
    );
    assert!(policy.permits_mcp_server("alpha"));
    assert!(policy.permits_mcp_server("zeta"));
    assert!(!policy.permits_mcp_server("Alpha"), "the match is exact");
    assert!(
        !policy.permits_mcp_server("beta"),
        "a server outside the list is refused"
    );
}

#[test]
fn the_server_list_is_read_in_every_mode() {
    // Narrowing is safe in any mode: the ordinary process, a container and a
    // shared worker all keep the names the operator listed and nothing else.
    for mode in [
        HostingMode::None,
        HostingMode::PerUserContainer,
        HostingMode::SharedWorker,
    ] {
        let tenant_id = mode.is_hosted().then(|| tenant("alice"));
        let policy = HostingPolicy::new(
            mode,
            tenant_id,
            &HostingSection {
                allow_shared_children: None,
                allowed_mcp_servers: Some(vec!["only".to_string()]),
                mode: None,
            },
        )
        .expect("policy");
        assert!(policy.permits_mcp_server("only"), "{mode}");
        assert!(!policy.permits_mcp_server("other"), "{mode}");
    }
}
