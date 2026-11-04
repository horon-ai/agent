use super::{CORE_TOOL_PROFILE, CORE_TOOLS, ESSENTIAL_TOOLS, LoadMode, TOOLBOX_TOOL, ToolConfig};
use std::collections::BTreeMap;

fn core_config() -> ToolConfig {
    ToolConfig {
        profile: CORE_TOOL_PROFILE.to_string(),
        ..ToolConfig::default()
    }
}

fn full_config() -> ToolConfig {
    ToolConfig {
        profile: "full".to_string(),
        ..ToolConfig::default()
    }
}

fn overrides(entries: &[(&str, LoadMode)]) -> BTreeMap<String, LoadMode> {
    entries
        .iter()
        .map(|(name, mode)| ((*name).to_string(), *mode))
        .collect()
}

#[test]
fn core_profile_without_listing_keeps_every_allowed_tool_essential() {
    let cfg = core_config();
    let allowed = cfg.allowed_tools().expect("core is an allow-list");
    let tiers = cfg.load_tiers(Some(&allowed), false);

    assert_eq!(tiers.default_tier(), LoadMode::Essential);
    assert!(!tiers.core_listed(), "the flag is stored as the caller resolved it");
    assert_eq!(
        allowed.len(),
        CORE_TOOLS.len() + 1,
        "core is the kept set plus the dispatcher"
    );
    for name in &allowed {
        assert_eq!(tiers.tier(name), LoadMode::Essential, "{name} must stay eager");
    }
}

/// A `core`-listed session keeps only the essential tools in the cached prefix.
#[test]
fn core_profile_lists_the_core_tools_it_does_not_send_eagerly() {
    let cfg = core_config();
    let allowed = cfg.allowed_tools().expect("core is an allow-list");
    let tiers = cfg.load_tiers(Some(&allowed), true);

    assert_eq!(ESSENTIAL_TOOLS.len(), 7, "the eager core set is seven tools");
    assert!(tiers.core_listed());
    assert_eq!(tiers.default_tier(), LoadMode::Essential);
    for name in ESSENTIAL_TOOLS {
        assert_eq!(tiers.tier(name), LoadMode::Essential, "{name} stays eager");
    }
    assert_eq!(
        tiers.tier(TOOLBOX_TOOL),
        LoadMode::Essential,
        "the dispatcher is the only way to the listed tools, so it stays eager"
    );
    for name in CORE_TOOLS {
        let expected = if ESSENTIAL_TOOLS.contains(name) {
            LoadMode::Essential
        } else {
            LoadMode::Discoverable
        };
        assert_eq!(tiers.tier(name), expected, "{name} resolves to its listed tier");
    }
    for name in ["gmail", "browser", "swarm"] {
        assert_eq!(
            tiers.tier(name),
            LoadMode::Essential,
            "{name} is outside the core set, so the listing does not reach it"
        );
    }
}

#[test]
fn an_essential_override_beats_the_core_listing() {
    let cfg = ToolConfig {
        load_mode: overrides(&[("ask", LoadMode::Essential), ("monitor", LoadMode::Off)]),
        ..core_config()
    };
    let allowed = cfg.allowed_tools().expect("core is an allow-list");
    let tiers = cfg.load_tiers(Some(&allowed), true);

    assert_eq!(tiers.tier("ask"), LoadMode::Essential, "an explicit entry wins");
    assert_eq!(tiers.tier("monitor"), LoadMode::Off, "off still removes the tool");
    assert_eq!(tiers.tier("undo"), LoadMode::Discoverable, "an untouched listed tool");
    assert_eq!(tiers.tier("bash"), LoadMode::Essential, "an untouched essential tool");
}

/// The listing is only safe while the dispatcher reaches the listed tools.
#[test]
fn toolbox_reachability_follows_the_off_override_and_the_deny_list() {
    assert!(core_config().toolbox_reachable(), "the default core session keeps it");
    assert!(full_config().toolbox_reachable(), "full keeps it too");

    let named_off = ToolConfig {
        load_mode: overrides(&[(TOOLBOX_TOOL, LoadMode::Off)]),
        ..core_config()
    };
    assert!(!named_off.toolbox_reachable(), "an off entry removes the dispatcher");

    let denied = ToolConfig {
        disabled: vec![TOOLBOX_TOOL.to_string()],
        ..core_config()
    };
    assert!(!denied.toolbox_reachable(), "a deny entry removes the dispatcher");

    let other_off = ToolConfig {
        load_mode: overrides(&[("ask", LoadMode::Off)]),
        ..core_config()
    };
    assert!(other_off.toolbox_reachable(), "an unrelated override leaves it alone");
}

#[test]
fn full_profile_defers_every_tool_outside_core() {
    let cfg = full_config();
    assert!(cfg.allowed_tools().is_none(), "full is unrestricted");
    let tiers = cfg.load_tiers(None, false);

    assert_eq!(tiers.default_tier(), LoadMode::Discoverable);
    assert!(!tiers.core_listed(), "full does not list core tools by name");
    for name in CORE_TOOLS {
        assert_eq!(tiers.tier(name), LoadMode::Essential, "{name} is a core tool");
    }
    for name in ["browser", "swarm", "gmail", "hashline_edit"] {
        assert_eq!(
            tiers.tier(name),
            LoadMode::Discoverable,
            "{name} is not core, so full defers it until something references it"
        );
    }
    for name in ["mcp_search", "mcp_call"] {
        assert_eq!(
            tiers.tier(name),
            LoadMode::Essential,
            "{name} is the MCP discovery surface and must stay callable"
        );
    }
    assert_eq!(
        tiers.tier(TOOLBOX_TOOL),
        LoadMode::Essential,
        "the dispatcher is the discovery path for the deferred surface, so full must never defer it"
    );
}

#[test]
fn toolbox_is_explicitly_essential_and_only_core_and_full_reach_it() {
    let full = full_config();
    assert_eq!(full.load_tiers(None, false).tier(TOOLBOX_TOOL), LoadMode::Essential);
    assert_eq!(
        full.load_tiers(None, false).default_tier(),
        LoadMode::Discoverable,
        "the exception must not change the full default"
    );

    // An explicit `discoverable` entry cannot defer the dispatcher either:
    // deferring it would leave nothing able to reference the deferred set.
    let explicit = ToolConfig {
        profile: "full".to_string(),
        load_mode: overrides(&[(TOOLBOX_TOOL, LoadMode::Discoverable)]),
        ..ToolConfig::default()
    };
    assert_eq!(explicit.load_tiers(None, false).tier(TOOLBOX_TOOL), LoadMode::Essential);

    // `off` is still honored: the coercion only prevents deferral, it does not
    // keep a tool the operator removed.
    let removed = ToolConfig {
        profile: "full".to_string(),
        load_mode: overrides(&[(TOOLBOX_TOOL, LoadMode::Off)]),
        ..ToolConfig::default()
    };
    assert_eq!(removed.load_tiers(None, false).tier(TOOLBOX_TOOL), LoadMode::Off);

    let core_allowed = core_config().allowed_tools().expect("core is an allow-list");
    assert!(core_allowed.contains(TOOLBOX_TOOL), "core must reach the dispatcher");

    let acp = ToolConfig {
        profile: "acp".to_string(),
        ..ToolConfig::default()
    };
    let acp_allowed = acp.allowed_tools().expect("acp is an allow-list");
    assert!(!acp_allowed.contains(TOOLBOX_TOOL), "acp must not reach the dispatcher");
}

#[test]
fn an_unset_profile_keeps_every_tool_eager() {
    let cfg = ToolConfig::default();
    assert!(cfg.allowed_tools().is_none(), "an unset profile is unrestricted");
    let tiers = cfg.load_tiers(None, false);

    assert_eq!(
        tiers.default_tier(),
        LoadMode::Essential,
        "only an explicit full profile opts into the discoverable tier"
    );
    assert_eq!(tiers.tier("gmail"), LoadMode::Essential);
}

#[test]
fn acp_profile_never_widens_and_stays_eager() {
    let cfg = ToolConfig {
        profile: "acp".to_string(),
        ..ToolConfig::default()
    };
    let allowed = cfg.allowed_tools().expect("acp is an allow-list");
    let tiers = cfg.load_tiers(Some(&allowed), false);

    assert_eq!(allowed.len(), 12, "acp keeps its own twelve-tool allow-list");
    assert_eq!(tiers.default_tier(), LoadMode::Essential);
    for name in &allowed {
        assert_eq!(tiers.tier(name), LoadMode::Essential);
    }
    for name in ["webfetch", "websearch", "skill_manage", "toolbox", "swarm"] {
        assert!(!allowed.contains(name), "the resolver must never add {name} to acp");
    }
}

#[test]
fn explicit_tier_reports_only_what_the_operator_wrote() {
    let cfg = ToolConfig {
        load_mode: overrides(&[
            ("ask", LoadMode::Discoverable),
            ("shell", LoadMode::Off),
            (TOOLBOX_TOOL, LoadMode::Discoverable),
        ]),
        ..core_config()
    };
    let allowed = cfg.allowed_tools().expect("core is an allow-list");
    let tiers = cfg.load_tiers(Some(&allowed), true);

    assert_eq!(tiers.explicit_tier("ask"), Some(LoadMode::Discoverable));
    assert_eq!(tiers.explicit_tier("shell"), Some(LoadMode::Off), "aliases resolve");
    assert_eq!(tiers.explicit_tier("bash"), Some(LoadMode::Off));
    assert_eq!(tiers.explicit_tier("undo"), None, "the profile decides the rest");
    assert_eq!(
        tiers.explicit_tier(TOOLBOX_TOOL),
        Some(LoadMode::Essential),
        "the dispatcher coercion applies here too, so a caller can never defer it"
    );
}

#[test]
fn explicit_entries_override_the_profile_and_core() {
    let cfg = ToolConfig {
        profile: "full".to_string(),
        load_mode: overrides(&[
            ("browser", LoadMode::Essential),
            ("bash", LoadMode::Off),
            ("swarm", LoadMode::Off),
        ]),
        ..ToolConfig::default()
    };
    let tiers = cfg.load_tiers(None, false);

    assert_eq!(tiers.tier("browser"), LoadMode::Essential);
    assert_eq!(tiers.tier("bash"), LoadMode::Off, "an override beats the core set");
    assert_eq!(tiers.tier("swarm"), LoadMode::Off);
    assert_eq!(tiers.tier("gmail"), LoadMode::Discoverable);
}

#[test]
fn an_override_key_resolves_through_the_tool_alias() {
    // `shell` and `shell_exec` both resolve to the registry key `bash`, so an
    // operator writing either spelling must land on the same tool.
    let cfg = ToolConfig {
        profile: "full".to_string(),
        load_mode: overrides(&[("shell", LoadMode::Off), ("file_grep", LoadMode::Essential)]),
        ..ToolConfig::default()
    };
    let tiers = cfg.load_tiers(None, false);

    assert_eq!(tiers.tier("bash"), LoadMode::Off);
    assert_eq!(tiers.tier("agentgrep"), LoadMode::Essential);
}

#[test]
fn an_unknown_tool_name_is_inert() {
    let cfg = ToolConfig {
        profile: "full".to_string(),
        load_mode: overrides(&[("no_such_tool_xyz", LoadMode::Off)]),
        ..ToolConfig::default()
    };
    let tiers = cfg.load_tiers(None, false);

    assert_eq!(tiers.tier("no_such_tool_xyz"), LoadMode::Off);
    assert_eq!(tiers.tier("gmail"), LoadMode::Discoverable);
    assert_eq!(tiers.tier("bash"), LoadMode::Essential);
}

#[test]
fn load_mode_parses_from_toml_and_defaults_to_empty() {
    assert!(ToolConfig::default().load_mode.is_empty());

    let parsed: ToolConfig = toml::from_str(
        r#"
[load_mode]
browser = "off"
ask = "essential"
swarm = "discoverable"
"#,
    )
    .expect("the load_mode table must parse");

    assert_eq!(parsed.load_mode.get("browser"), Some(&LoadMode::Off));
    assert_eq!(parsed.load_mode.get("ask"), Some(&LoadMode::Essential));
    assert_eq!(parsed.load_mode.get("swarm"), Some(&LoadMode::Discoverable));
    assert_eq!(LoadMode::parse("Off"), Some(LoadMode::Off));
    assert_eq!(LoadMode::parse("discoverable"), Some(LoadMode::Discoverable));
    assert_eq!(LoadMode::parse("always"), None);
}
