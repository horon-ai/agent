//! Tests for the `[tools] irc` switch, the `[irc]` section, and the template.

use crate::config::{Config, IRC_TOOL, PLAN_TOOL, TOOLBOX_TOOL, ToolConfig};
use std::ffi::OsString;

fn restore_env_var(key: &str, previous: Option<OsString>) {
    if let Some(previous) = previous {
        crate::env::set_var(key, previous);
    } else {
        crate::env::remove_var(key);
    }
}

fn core_config() -> ToolConfig {
    ToolConfig {
        profile: crate::config::CORE_TOOL_PROFILE.to_string(),
        ..ToolConfig::default()
    }
}

#[test]
fn irc_switch_adds_irc_to_core_allow_list() {
    let cfg = ToolConfig {
        irc: true,
        ..core_config()
    };
    let allowed = cfg
        .selection_with_ambient(&crate::config::AmbientConfig::default())
        .allowed_tools
        .expect("core is an allow-list");
    assert!(allowed.contains(IRC_TOOL), "the irc tool must join the core list");
    assert!(allowed.contains(TOOLBOX_TOOL));
    assert_eq!(
        allowed.len(),
        18 + 16 + 3 + 1,
        "the open core surface plus irc, the checkpoint pair, and the dispatcher"
    );
    assert!(!allowed.contains(PLAN_TOOL), "plan mode stays off");
    println!(
        "--- manual QA: core allow-list with irc = {} entries including {IRC_TOOL} ---",
        allowed.len()
    );
}

/// `enabled = ["*"]` asks for every tool, so a swarm worker (which narrows only
/// the untouched core surface) must not cut it down to the core set.
#[test]
fn all_tools_sentinel_is_not_the_core_surface() {
    assert!(core_config().is_core_surface());
    let all = ToolConfig {
        enabled: vec!["*".to_string()],
        ..core_config()
    };
    assert!(!all.is_core_surface());
    let blank = ToolConfig {
        enabled: vec![" ".to_string()],
        ..core_config()
    };
    assert!(blank.is_core_surface(), "a blank entry names no tool");
}

#[test]
fn irc_switch_off_removes_only_irc_from_the_core_allow_list() {
    let on = core_config()
        .selection_with_ambient(&crate::config::AmbientConfig::default())
        .allowed_tools
        .expect("core is an allow-list");
    let off = ToolConfig {
        irc: false,
        ..core_config()
    }
    .selection_with_ambient(&crate::config::AmbientConfig::default())
    .allowed_tools
    .expect("core is an allow-list");

    assert!(on.contains(IRC_TOOL), "the default surface reaches irc");
    assert!(!off.contains(IRC_TOOL), "the switch off removes irc");
    let mut expected = on;
    expected.remove(IRC_TOOL);
    assert_eq!(off, expected, "the switch off leaves every other tool alone");

    // A list that is spelled out stays exactly as written: the switch joins
    // only the default core surface, and naming irc is how another list gets it.
    for profile in ["acp", "minimal"] {
        let allowed = ToolConfig {
            profile: profile.to_string(),
            ..ToolConfig::default()
        }
        .selection_with_ambient(&crate::config::AmbientConfig::default())
        .allowed_tools
        .expect("profile is an allow-list");
        assert!(!allowed.contains(IRC_TOOL), "{profile} stays as written");
    }
    let named = ToolConfig {
        enabled: vec!["read".to_string(), IRC_TOOL.to_string()],
        ..ToolConfig::default()
    }
    .selection_with_ambient(&crate::config::AmbientConfig::default())
    .allowed_tools
    .expect("an explicit list is an allow-list");
    assert_eq!(
        named,
        ["read".to_string(), IRC_TOOL.to_string()].into_iter().collect(),
        "an explicit list that names irc reaches it and nothing else"
    );
}

#[test]
fn irc_switch_does_not_populate_none_profile() {
    let cfg = ToolConfig {
        profile: "none".to_string(),
        irc: true,
        ..ToolConfig::default()
    };
    let allowed = cfg
        .selection_with_ambient(&crate::config::AmbientConfig::default())
        .allowed_tools
        .expect("none is an empty allow-list");
    assert!(allowed.is_empty(), "an explicitly disabled surface must stay empty");
}

#[test]
fn irc_section_defaults_match_documented_values() {
    let defaults = Config::default().irc;
    assert_eq!(defaults.mailbox_cap, 100);
    assert_eq!(defaults.max_body_bytes, 65_536);
    assert_eq!(defaults.wait_max_secs, 300);
    assert_eq!(defaults.broadcast_concurrency, 16);

    let parsed: Config =
        toml::from_str("[irc]\nmailbox_cap = 0\nmax_body_bytes = 0\nwait_max_secs = 0\nbroadcast_concurrency = 0\n")
            .expect("the irc section must parse");
    let clamped = parsed.irc.clamped();
    assert_eq!(clamped.mailbox_cap, 1);
    assert_eq!(clamped.max_body_bytes, 1);
    assert_eq!(clamped.wait_max_secs, 1);
    assert_eq!(clamped.broadcast_concurrency, 1);
    println!("--- manual QA: default [irc] limits {:?} ---", defaults);
}

#[test]
fn irc_env_override_turns_switch_on() {
    let _guard = crate::storage::lock_test_env();
    let previous = std::env::var_os("HORON_AGENT_IRC");

    let mut off = Config {
        tools: ToolConfig {
            irc: false,
            ..ToolConfig::default()
        },
        ..Config::default()
    };
    crate::env::set_var("HORON_AGENT_IRC", "1");
    off.apply_env_overrides();
    assert!(off.tools.irc, "the env override turns the switch on");

    let mut ignored = Config {
        tools: ToolConfig {
            irc: false,
            ..ToolConfig::default()
        },
        ..Config::default()
    };
    crate::env::set_var("HORON_AGENT_IRC", "maybe");
    ignored.apply_env_overrides();
    assert!(!ignored.tools.irc, "an unparseable value must be ignored");

    crate::env::set_var("HORON_AGENT_IRC", "0");
    let mut disabled = Config::default();
    disabled.apply_env_overrides();
    assert!(!disabled.tools.irc);

    restore_env_var("HORON_AGENT_IRC", previous);
}

#[test]
fn default_file_documents_irc_switch_and_section() {
    let template = Config::default_config_file_contents();
    for key in ["irc = true", "HORON_AGENT_IRC", "[irc]", "mailbox_cap = 100"] {
        assert!(template.contains(key), "the template should document `{key}`");
    }
    for key in [
        "max_body_bytes = 65536",
        "wait_max_secs = 300",
        "broadcast_concurrency = 16",
    ] {
        assert!(template.contains(key), "the template should document `{key}`");
    }

    let parsed = toml::from_str::<Config>(&template).expect("the shipped template must parse");
    assert!(parsed.tools.irc, "the shipped switch is on");
    assert_eq!(parsed.irc, Config::default().irc);
}
