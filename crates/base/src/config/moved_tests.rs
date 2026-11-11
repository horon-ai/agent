//! Config tests that live here to keep `config_tests.rs` inside its size ratchet.
//!
//! `config_tests.rs` is a tracked oversized test file and may not grow, so new
//! or relocated tests get their own file instead of extending it. These two
//! groups were written for the sections they cover and only moved, unchanged.

use super::{
    AmbientConfig, CLIENTLESS_APPROVAL_TOOL, CORE_TOOL_PROFILE, CORE_TOOLS, CustomWebSearchConfig,
    CustomWebSearchMethod, ToolConfig, WebSearchConfig, WebSearchEngine, config,
};

#[test]
fn custom_websearch_config_toml_roundtrip() {
    let input = r#"
engine = "custom"
fallback_engines = []
[custom]
endpoint = "https://search.example.test/api"
method = "POST"
query_param = "search"
count_param = "limit"
params = { format = "json" }
body = { options = { safe = true } }
headers = { Accept = "application/json" }
header_env = { Authorization = "SEARCH_AUTH" }
results_pointer = "/data/hits"
title_pointer = "/name"
url_pointer = "/link"
snippet_pointer = "/summary"
"#;
    let config: WebSearchConfig = toml::from_str(input).unwrap();
    assert_eq!(config.engine, WebSearchEngine::Custom);
    assert_eq!(WebSearchEngine::parse("custom"), Some(WebSearchEngine::Custom));
    assert!(WebSearchEngine::Custom.is_local());
    assert_eq!(WebSearchEngine::Custom.as_str(), "custom");
    let encoded = toml::to_string(&config).unwrap();
    let decoded: WebSearchConfig = toml::from_str(&encoded).unwrap();
    assert_eq!(
        serde_json::to_value(config).unwrap(),
        serde_json::to_value(decoded).unwrap()
    );
    let no_count: CustomWebSearchConfig = toml::from_str("endpoint = 'https://example.test'").unwrap();
    assert!(no_count.count_param.is_none());
    assert_eq!(no_count.method, CustomWebSearchMethod::Get);
    assert!(WebSearchConfig::default().custom.is_none());
}

fn ambient_core_config() -> ToolConfig {
    ToolConfig {
        profile: CORE_TOOL_PROFILE.to_string(),
        ..ToolConfig::default()
    }
}

fn ambient_clientless(on: bool) -> AmbientConfig {
    AmbientConfig {
        approval_for_clientless_runs: on,
        ..AmbientConfig::default()
    }
}

#[test]
fn clientless_approval_joins_the_core_allow_list() {
    let cfg = ambient_core_config();

    let off = cfg
        .selection_with_ambient(&ambient_clientless(false))
        .allowed_tools
        .expect("core is an allow-list");
    assert_eq!(
        off.len(),
        CORE_TOOLS.len() + 1,
        "the default core surface is the kept set plus the dispatcher"
    );
    assert!(!off.contains(CLIENTLESS_APPROVAL_TOOL));

    let on = cfg
        .selection_with_ambient(&ambient_clientless(true))
        .allowed_tools
        .expect("core is an allow-list");
    assert!(
        on.contains(CLIENTLESS_APPROVAL_TOOL),
        "a clientless run must be able to call request_permission, got {on:?}"
    );
    assert_eq!(
        on.len(),
        CORE_TOOLS.len() + 2,
        "exactly one tool joins the core surface"
    );
    assert_eq!(CORE_TOOLS.len(), 17, "CORE_TOOLS itself grows to seventeen");
}

#[test]
fn selection_applies_the_live_ambient_settings() {
    // The production entry point must widen exactly like the explicit
    // variant: the agent builds its allow-list from `selection()`.
    let cfg = ambient_core_config();
    let live = config();
    assert_eq!(
        cfg.selection().allowed_tools,
        cfg.selection_with_ambient(&live.ambient).allowed_tools
    );
}

#[test]
fn explicit_disabled_wins_over_the_clientless_join() {
    let cfg = ToolConfig {
        profile: CORE_TOOL_PROFILE.to_string(),
        disabled: vec![CLIENTLESS_APPROVAL_TOOL.to_string()],
        ..ToolConfig::default()
    };
    let allowed = cfg
        .selection_with_ambient(&ambient_clientless(true))
        .allowed_tools
        .expect("core is an allow-list");
    assert!(!allowed.contains(CLIENTLESS_APPROVAL_TOOL));
}

#[test]
fn empty_allow_list_is_not_widened() {
    let cfg = ToolConfig {
        profile: "none".to_string(),
        ..ToolConfig::default()
    };
    let allowed = cfg
        .selection_with_ambient(&ambient_clientless(true))
        .allowed_tools
        .expect("none is an empty allow-list");
    assert!(allowed.is_empty(), "an explicitly disabled surface must stay empty");
}

#[test]
fn explicit_enabled_list_also_gains_the_clientless_tool() {
    let cfg = ToolConfig {
        enabled: vec!["bash".to_string()],
        ..ToolConfig::default()
    };
    let allowed = cfg
        .selection_with_ambient(&ambient_clientless(true))
        .allowed_tools
        .expect("explicit enabled is an allow-list");
    assert!(allowed.contains("bash"));
    assert!(allowed.contains(CLIENTLESS_APPROVAL_TOOL));
}
