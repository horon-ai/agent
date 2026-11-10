use super::*;

fn roles(default: Option<&str>, plan: Option<&str>, aux: Option<&str>) -> ModelRoles {
    ModelRoles {
        default: default.map(str::to_string),
        plan: plan.map(str::to_string),
        aux: aux.map(str::to_string),
    }
}

#[test]
fn model_role_resolution_prefers_the_run_override() {
    let roles = roles(Some("role-default"), Some("role-plan"), None);

    assert_eq!(
        resolve_role_model(
            ModelRole::Plan,
            Some("override"),
            &roles,
            Some("profile"),
            Some("provider")
        )
        .expect("override wins"),
        "override"
    );
}

#[test]
fn model_role_resolution_reads_the_role_entry_before_the_default_role() {
    let roles = roles(Some("role-default"), Some("role-plan"), None);

    assert_eq!(
        resolve_role_model(ModelRole::Plan, None, &roles, Some("profile"), Some("provider")).expect("role entry wins"),
        "role-plan"
    );
}

#[test]
fn model_role_resolution_falls_back_to_the_default_role_entry() {
    let roles = roles(Some("role-default"), None, None);

    assert_eq!(
        resolve_role_model(ModelRole::Plan, None, &roles, Some("profile"), Some("provider"))
            .expect("default role entry"),
        "role-default"
    );
    assert_eq!(
        resolve_role_model(ModelRole::Default, None, &roles, Some("profile"), Some("provider"))
            .expect("default role entry"),
        "role-default"
    );
}

#[test]
fn model_role_resolution_falls_back_to_the_profile_then_provider_default() {
    let unset = ModelRoles::default();

    assert_eq!(
        resolve_role_model(ModelRole::Aux, None, &unset, Some("profile"), Some("provider")).expect("profile default"),
        "profile"
    );
    assert_eq!(
        resolve_role_model(ModelRole::Aux, None, &unset, None, Some("provider")).expect("provider default"),
        "provider"
    );
}

#[test]
fn model_role_resolution_treats_an_empty_string_as_unset_at_every_step() {
    let roles = roles(Some(""), Some("   "), None);

    assert_eq!(
        resolve_role_model(ModelRole::Aux, Some(""), &roles, Some("  "), Some("provider"))
            .expect("blank steps are skipped"),
        "provider"
    );
    assert_eq!(
        resolve_role_model(ModelRole::Aux, Some(" run "), &roles, Some("profile"), Some("provider"))
            .expect("override is trimmed"),
        "run"
    );
}

#[test]
fn model_role_resolution_rejects_a_route_prefixed_value_and_names_the_role() {
    for (value, prefix) in [
        ("openai:gpt-5", "openai"),
        ("openai-api:gpt-5", "openai-api"),
        ("openai-oauth:gpt-5", "openai-oauth"),
        ("my-gw:a-model", "my-gw"),
    ] {
        let roles = roles(None, Some(value), None);
        let error = resolve_role_model(ModelRole::Plan, None, &roles, Some("profile"), Some("provider"))
            .expect_err("a route prefix must be refused");
        assert_eq!(
            error,
            RoleResolutionError::RoutePrefix {
                role: ModelRole::Plan,
                value: value.to_string(),
                prefix: prefix.to_string(),
            }
        );
        let message = error.to_string();
        assert!(message.contains("plan"), "error must name the role: {message}");
        assert!(
            message.contains(&format!("{prefix}:")),
            "error must name the refused prefix: {message}"
        );
    }
}

#[test]
fn model_role_resolution_rejects_a_route_prefixed_default_role_entry() {
    let roles = roles(Some("openai:gpt-5"), None, None);

    let error = resolve_role_model(ModelRole::Aux, None, &roles, Some("profile"), Some("provider"))
        .expect_err("a prefixed default role must be refused");
    assert_eq!(
        error,
        RoleResolutionError::RoutePrefix {
            role: ModelRole::Aux,
            value: "openai:gpt-5".to_string(),
            prefix: "openai".to_string(),
        }
    );
    assert!(error.to_string().contains("aux"), "error must name the role");
}

#[test]
fn model_role_resolution_keeps_a_run_override_route_spelling() {
    // A run override is not a role value: `--model <profile>:<model>` is an
    // existing spelling that the prefix handling in front of this function
    // already understands, so it must pass through unchanged.
    assert_eq!(
        resolve_role_model(
            ModelRole::Default,
            Some("my-gw:a-model"),
            &ModelRoles::default(),
            None,
            None
        )
        .expect("override is returned as given"),
        "my-gw:a-model"
    );
}

#[test]
fn model_role_resolution_reports_every_empty_step_and_names_the_role() {
    let error = resolve_role_model(ModelRole::Aux, Some(""), &ModelRoles::default(), None, Some(" "))
        .expect_err("nothing resolved");

    assert_eq!(error, RoleResolutionError::Unresolved { role: ModelRole::Aux });
    let message = error.to_string();
    assert!(message.contains("aux"), "error must name the role: {message}");
    assert!(message.contains("roles.aux"), "error must name the role key: {message}");
    assert!(
        message.contains("[provider] default_model"),
        "error must name the provider default key: {message}"
    );
}

#[test]
fn model_role_config_trims_the_fallback_list_and_drops_blank_entries() {
    let config = ModelRoleConfig {
        fallback_models: vec![" m2 ".to_string(), String::new(), "   ".to_string(), "m3".to_string()],
        roles: ModelRoles::default(),
    };

    assert_eq!(config.fallback_models(), vec!["m2", "m3"]);
}

#[test]
fn model_role_config_round_trips_the_profile_table_from_toml() {
    #[derive(Debug, serde::Deserialize, serde::Serialize)]
    struct ProfileFile {
        providers: std::collections::BTreeMap<String, crate::NamedProviderConfig>,
    }

    let raw = r#"
[providers.my-gw]
default_model = "my-model"
fallback_models = ["m2", "m3"]

[providers.my-gw.roles]
default = "my-model"
plan = "my-planner"
aux = "my-small-model"
"#;
    let parsed: ProfileFile = toml::from_str(raw).expect("profile table with roles and fallbacks");
    let config = parsed.providers.get("my-gw").expect("my-gw profile");
    assert_eq!(config.model_roles.fallback_models(), vec!["m2", "m3"]);
    assert_eq!(config.model_roles.roles.default.as_deref(), Some("my-model"));
    assert_eq!(config.model_roles.roles.plan.as_deref(), Some("my-planner"));
    assert_eq!(config.model_roles.roles.aux.as_deref(), Some("my-small-model"));

    let encoded = toml::to_string(&parsed).expect("serialize profile table");
    let reparsed: ProfileFile = toml::from_str(&encoded).expect("reparse serialized profile table");
    assert_eq!(reparsed.providers["my-gw"].model_roles, config.model_roles);
}

#[test]
fn model_role_config_deserializes_an_unset_profile_to_empty() {
    #[derive(Debug, serde::Deserialize, serde::Serialize)]
    struct ProfileFile {
        providers: std::collections::BTreeMap<String, crate::NamedProviderConfig>,
    }

    let parsed: ProfileFile = toml::from_str(
        r#"
[providers.my-gw]
default_model = "my-model"
"#,
    )
    .expect("profile without roles");

    let config = &parsed.providers["my-gw"];
    assert!(config.model_roles.is_empty());
    assert!(config.model_roles.fallback_models().is_empty());
    assert!(config.model_roles.roles.is_empty());

    // A profile that mentions neither key must stay byte-identical through a
    // save-and-reload cycle, so an existing config file gains no new keys.
    let encoded = toml::to_string(&parsed).expect("serialize profile table");
    assert!(
        !encoded.contains("fallback_models"),
        "no fallback key may appear: {encoded}"
    );
    assert!(!encoded.contains("roles"), "no roles key may appear: {encoded}");
    let reparsed: ProfileFile = toml::from_str(&encoded).expect("reparse serialized profile table");
    assert_eq!(reparsed.providers["my-gw"], *config);
}
