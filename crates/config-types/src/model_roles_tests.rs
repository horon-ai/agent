use super::*;

fn roles(default: Option<&str>, plan: Option<&str>, aux: Option<&str>) -> ModelRoles {
    ModelRoles {
        default: default.map(str::to_string),
        plan: plan.map(str::to_string),
        aux: aux.map(str::to_string),
        judge: None,
        image: None,
    }
}

fn roles_with(judge: Option<&str>, aux: Option<&str>, default: Option<&str>) -> ModelRoles {
    ModelRoles {
        default: default.map(str::to_string),
        plan: None,
        aux: aux.map(str::to_string),
        judge: judge.map(str::to_string),
        image: None,
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
fn model_role_resolution_reads_the_judge_entry_before_its_fallbacks() {
    let roles = roles_with(Some("judge-model"), Some("aux-model"), Some("role-default"));

    assert_eq!(
        resolve_role_model(ModelRole::Judge, None, &roles, Some("profile"), Some("provider"))
            .expect("judge entry wins"),
        "judge-model"
    );
}

#[test]
fn model_role_resolution_falls_back_from_judge_to_aux_before_default() {
    let roles = roles_with(None, Some("aux-model"), Some("role-default"));

    assert_eq!(
        resolve_role_model(ModelRole::Judge, None, &roles, Some("profile"), Some("provider")).expect("aux role entry"),
        "aux-model",
        "a judgment is a cheap side task, so it inherits the aux model before the default one"
    );

    let roles = roles_with(None, None, Some("role-default"));
    assert_eq!(
        resolve_role_model(ModelRole::Judge, None, &roles, Some("profile"), Some("provider"))
            .expect("default role entry"),
        "role-default"
    );
}

#[test]
fn model_role_resolution_never_inherits_a_prefixed_value_silently() {
    // A fallback step is still a role entry, so it is refused with the role
    // that asked for it rather than the role that wrote the value.
    let roles = roles_with(None, Some("openai:gpt-5"), None);

    let error = resolve_role_model(ModelRole::Judge, None, &roles, Some("profile"), Some("provider"))
        .expect_err("a prefixed inherited value must be refused");
    assert_eq!(
        error,
        RoleResolutionError::RoutePrefix {
            role: ModelRole::Judge,
            value: "openai:gpt-5".to_string(),
            prefix: "openai".to_string(),
        }
    );
}

#[test]
fn model_role_fallbacks_are_ordered_and_never_contain_the_role_itself() {
    assert_eq!(ModelRole::Default.fallback_roles(), &[]);
    assert_eq!(ModelRole::Plan.fallback_roles(), &[ModelRole::Default]);
    assert_eq!(ModelRole::Aux.fallback_roles(), &[ModelRole::Default]);
    assert_eq!(ModelRole::Judge.fallback_roles(), &[ModelRole::Aux, ModelRole::Default]);
    for role in [ModelRole::Default, ModelRole::Plan, ModelRole::Aux, ModelRole::Judge] {
        assert!(
            !role.fallback_roles().contains(&role),
            "{role} must not inherit from itself"
        );
    }
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
judge = "my-judge"
image = "my-image"
"#;
    let parsed: ProfileFile = toml::from_str(raw).expect("profile table with roles and fallbacks");
    let config = parsed.providers.get("my-gw").expect("my-gw profile");
    assert_eq!(config.model_roles.fallback_models(), vec!["m2", "m3"]);
    assert_eq!(config.model_roles.roles.default.as_deref(), Some("my-model"));
    assert_eq!(config.model_roles.roles.plan.as_deref(), Some("my-planner"));
    assert_eq!(config.model_roles.roles.aux.as_deref(), Some("my-small-model"));
    assert_eq!(config.model_roles.roles.judge.as_deref(), Some("my-judge"));
    assert_eq!(config.model_roles.roles.image.as_deref(), Some("my-image"));

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

fn roles_with_image(image: Option<&str>, default: Option<&str>) -> ModelRoles {
    ModelRoles {
        default: default.map(str::to_string),
        plan: None,
        aux: None,
        judge: None,
        image: image.map(str::to_string),
    }
}

#[test]
fn the_image_role_resolves_its_own_entry() {
    let roles = roles_with_image(Some("image-model"), Some("text-default"));
    assert_eq!(
        resolve_role_model(ModelRole::Image, None, &roles, Some("profile"), None).expect("the image entry resolves"),
        "image-model"
    );
}

#[test]
fn the_image_role_has_no_inherited_fallback() {
    // A text model cannot answer an image request, so falling back would send a
    // call that must fail. An unset `image` entry is "not configured", not
    // "use the default".
    let roles = roles_with_image(None, Some("text-default"));
    let error = resolve_role_model(ModelRole::Image, None, &roles, Some("profile"), None)
        .expect_err("no image model is configured");
    let message = error.to_string();
    assert!(message.contains("image"), "{message}");
    assert!(
        !message.contains("roles.default"),
        "the error names the role that failed: {message}"
    );
}

#[test]
fn the_image_role_refuses_a_route_prefix_like_every_other_role() {
    let roles = roles_with_image(Some("gateway:an-image"), None);
    let error =
        resolve_role_model(ModelRole::Image, None, &roles, Some("profile"), None).expect_err("a route is not a model");
    assert!(error.to_string().contains("image"), "{error}");
}

#[test]
fn a_run_override_beats_the_image_entry() {
    let roles = roles_with_image(Some("image-model"), None);
    assert_eq!(
        resolve_role_model(ModelRole::Image, Some("override-image"), &roles, Some("profile"), None,)
            .expect("the override resolves"),
        "override-image"
    );
}

#[test]
fn an_image_entry_alone_makes_the_roles_non_empty() {
    let roles = roles_with_image(Some("image-model"), None);
    assert!(!roles.is_empty());
}
