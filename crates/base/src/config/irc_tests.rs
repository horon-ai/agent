//! Tests for the `[tools] irc` switch, the `[irc]` section, and the template.

use crate::config::{CORE_TOOLS, Config, IRC_TOOL, PLAN_TOOL, TOOLBOX_TOOL, ToolConfig};
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
    assert_eq!(allowed.len(), CORE_TOOLS.len() + 2);
    assert!(!allowed.contains(PLAN_TOOL), "plan mode stays off");
    println!(
        "--- manual QA: core allow-list with irc = {} entries including {IRC_TOOL} ---",
        allowed.len()
    );
}

#[test]
fn irc_switch_off_keeps_core_allow_list_unchanged() {
    let cfg = core_config();
    let allowed = cfg
        .selection_with_ambient(&crate::config::AmbientConfig::default())
        .allowed_tools
        .expect("core is an allow-list");
    let mut expected: std::collections::HashSet<String> = CORE_TOOLS.iter().map(|name| (*name).to_string()).collect();
    expected.insert(TOOLBOX_TOOL.to_string());
    assert_eq!(allowed, expected, "the default core surface must not change");
    assert!(!allowed.contains(IRC_TOOL));

    for profile in ["acp", "minimal"] {
        let off = ToolConfig {
            profile: profile.to_string(),
            ..ToolConfig::default()
        }
        .selection_with_ambient(&crate::config::AmbientConfig::default())
        .allowed_tools
        .expect("profile is an allow-list");
        assert!(!off.contains(IRC_TOOL), "{profile} must not gain irc by default");
    }
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
    crate::env::set_var("HORON_AGENT_IRC", "1");

    let mut cfg = Config::default();
    cfg.apply_env_overrides();
    assert!(cfg.tools.irc);

    crate::env::set_var("HORON_AGENT_IRC", "maybe");
    let mut ignored = Config::default();
    ignored.apply_env_overrides();
    assert!(!ignored.tools.irc, "an unparseable value must be ignored");

    crate::env::set_var("HORON_AGENT_IRC", "0");
    let mut off = Config {
        tools: ToolConfig {
            irc: true,
            ..ToolConfig::default()
        },
        ..Config::default()
    };
    off.apply_env_overrides();
    assert!(!off.tools.irc);

    restore_env_var("HORON_AGENT_IRC", previous);
}

#[test]
fn default_file_documents_irc_switch_and_section() {
    let template = Config::default_config_file_contents();
    for key in ["irc = false", "HORON_AGENT_IRC", "[irc]", "mailbox_cap = 100"] {
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
    assert!(!parsed.tools.irc, "the shipped switch is off");
    assert_eq!(parsed.irc, Config::default().irc);
}
