//! Tests for the MCP tool-schema disk cache.

use super::*;
use crate::mcp::protocol::McpServerConfig;
use serde_json::json;
use std::collections::HashMap;

fn cfg(command: &str, args: &[&str]) -> McpServerConfig {
    McpServerConfig {
        command: command.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        env: HashMap::new(),
        shared: true,
        transport: None,
        url: None,
        headers: HashMap::new(),
        enabled: None,
        disabled: None,
        timeout_secs: None,
    }
}

fn tool(name: &str) -> McpToolDef {
    McpToolDef {
        name: name.to_string(),
        description: Some(format!("{name} desc")),
        input_schema: json!({"type": "object"}),
    }
}

#[test]
fn fingerprint_is_stable_and_sensitive() {
    let a = cfg("node", &["server.js"]);
    let b = cfg("node", &["server.js"]);
    assert_eq!(
        fingerprint_config(&a),
        fingerprint_config(&b),
        "identical configs must fingerprint equally"
    );

    // Different args -> different fingerprint.
    let c = cfg("node", &["other.js"]);
    assert_ne!(fingerprint_config(&a), fingerprint_config(&c));

    // Different command -> different fingerprint.
    let d = cfg("python", &["server.js"]);
    assert_ne!(fingerprint_config(&a), fingerprint_config(&d));

    // Different shared flag -> different fingerprint.
    let mut e = cfg("node", &["server.js"]);
    e.shared = false;
    assert_ne!(fingerprint_config(&a), fingerprint_config(&e));
}

#[test]
fn fingerprint_env_order_independent() {
    let mut a = cfg("node", &["s.js"]);
    a.env.insert("A".into(), "1".into());
    a.env.insert("B".into(), "2".into());
    let mut b = cfg("node", &["s.js"]);
    b.env.insert("B".into(), "2".into());
    b.env.insert("A".into(), "1".into());
    assert_eq!(
        fingerprint_config(&a),
        fingerprint_config(&b),
        "env map ordering must not affect fingerprint"
    );

    let mut c = cfg("node", &["s.js"]);
    c.env.insert("A".into(), "different".into());
    assert_ne!(fingerprint_config(&a), fingerprint_config(&c));
}

#[test]
fn fingerprint_is_sensitive_to_http_url_and_headers() {
    let mut a = cfg("", &[]);
    a.transport = Some("http".to_string());
    a.url = Some("https://one.example/mcp".to_string());
    a.headers.insert("Authorization".to_string(), "Bearer one".to_string());

    let mut changed_url = a.clone();
    changed_url.url = Some("https://two.example/mcp".to_string());
    assert_ne!(fingerprint_config(&a), fingerprint_config(&changed_url));

    let mut changed_header = a.clone();
    changed_header
        .headers
        .insert("Authorization".to_string(), "Bearer two".to_string());
    assert_ne!(fingerprint_config(&a), fingerprint_config(&changed_header));
}

#[test]
fn tools_for_respects_fingerprint() {
    let mut cache = McpSchemaCache::default();
    let config = cfg("node", &["s.js"]);
    cache.update("srv", &config, vec![tool("alpha"), tool("beta")]);

    // Same config -> cached tools returned.
    let got = cache.tools_for("srv", &config).expect("cached tools");
    assert_eq!(got.len(), 2);

    // Reconfigured server -> stale schemas must NOT be returned.
    let reconfigured = cfg("node", &["s2.js"]);
    assert!(
        cache.tools_for("srv", &reconfigured).is_none(),
        "config change must invalidate cached schemas"
    );

    // Unknown server -> None.
    assert!(cache.tools_for("nope", &config).is_none());
}

#[test]
fn update_reports_change_only_on_diff() {
    let mut cache = McpSchemaCache::default();
    let config = cfg("node", &["s.js"]);

    assert!(cache.update("srv", &config, vec![tool("a")]), "new entry changes");
    assert!(
        !cache.update("srv", &config, vec![tool("a")]),
        "identical re-update must not be marked changed"
    );
    assert!(
        cache.update("srv", &config, vec![tool("a"), tool("b")]),
        "added tool must be marked changed"
    );
    // Tool reordering should NOT count as a change (set comparison).
    assert!(
        !cache.update("srv", &config, vec![tool("b"), tool("a")]),
        "reordered identical tools must not churn the cache"
    );
}

#[test]
fn retain_prunes_removed_servers() {
    let mut cache = McpSchemaCache::default();
    let config = cfg("node", &["s.js"]);
    cache.update("keep", &config, vec![tool("a")]);
    cache.update("drop", &config, vec![tool("b")]);
    assert_eq!(cache.server_count(), 2);

    let pruned = cache.retain_servers(&["keep".to_string()]);
    assert!(pruned, "retain must report pruning");
    assert_eq!(cache.server_count(), 1);
    assert!(cache.tools_for("keep", &config).is_some());
    assert!(cache.tools_for("drop", &config).is_none());

    // No-op retain reports false.
    assert!(!cache.retain_servers(&["keep".to_string()]));
}

#[test]
fn load_save_roundtrip_via_temp_home() {
    let _guard = crate::storage::lock_test_env();
    let tmp = tempfile::tempdir().unwrap();
    // Point the horon-agent dir at a temp location.
    unsafe {
        std::env::set_var("HORON_HOME", tmp.path());
    }

    let mut cache = McpSchemaCache::default();
    let config = cfg("node", &["s.js"]);
    cache.update("srv", &config, vec![tool("alpha")]);
    cache.save();

    let reloaded = McpSchemaCache::load();
    let tools = reloaded.tools_for("srv", &config).expect("reloaded tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "alpha");

    unsafe {
        std::env::remove_var("HORON_HOME");
    }
}

/// The deferred-discovery tools call `load_cached` on every `mcp_search` and
/// `mcp_call`. Re-parsing the whole file each time was megabytes of work per
/// tool call, so an unchanged file must reuse the parsed copy.
#[test]
fn load_cached_reuses_the_parsed_copy_until_the_file_changes() {
    let _guard = crate::storage::lock_test_env();
    let tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("HORON_HOME", tmp.path());
    }
    McpSchemaCache::invalidate_cached();

    let config = cfg("node", &["s.js"]);
    let mut cache = McpSchemaCache::default();
    cache.update("srv", &config, vec![tool("alpha")]);
    cache.save();

    let first = McpSchemaCache::load_cached();
    let second = McpSchemaCache::load_cached();
    assert!(
        Arc::ptr_eq(&first, &second),
        "an unchanged cache file must not be re-parsed"
    );
    assert_eq!(first.tools_for("srv", &config).map(<[McpToolDef]>::len), Some(1));

    // A rewrite must be observed, even inside one filesystem timestamp
    // granularity, because `save` invalidates the in-process copy.
    let mut changed = McpSchemaCache::default();
    changed.update("srv", &config, vec![tool("alpha"), tool("beta")]);
    changed.save();

    let third = McpSchemaCache::load_cached();
    assert!(!Arc::ptr_eq(&first, &third), "a rewritten cache file must be re-read");
    assert_eq!(third.tools_for("srv", &config).map(<[McpToolDef]>::len), Some(2));

    McpSchemaCache::invalidate_cached();
    unsafe {
        std::env::remove_var("HORON_HOME");
    }
}

/// A missing cache file is a cache state like any other, not a permanent miss.
/// The doc comment promises the empty cache is "also cached, keyed on the
/// absence"; without that, every `mcp_search` and `mcp_call` on a machine that
/// has never saved a cache re-walked the filesystem and re-parsed nothing.
#[test]
fn load_cached_reuses_the_empty_cache_while_the_file_is_absent() {
    let _guard = crate::storage::lock_test_env();
    let tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("HORON_HOME", tmp.path());
    }
    McpSchemaCache::invalidate_cached();

    let cache_file = tmp.path().join(CACHE_FILE);
    assert!(!cache_file.exists(), "this test needs a cold start with no cache file");

    let first = McpSchemaCache::load_cached();
    let second = McpSchemaCache::load_cached();
    assert!(
        Arc::ptr_eq(&first, &second),
        "a missing cache file must not be re-read on every call"
    );
    assert_eq!(first.server_count(), 0, "a missing file yields an empty cache");

    // The file appearing is a state change, so the cached empty copy must not
    // outlive it: `save` invalidates, and the next load carries the new entry.
    let config = cfg("node", &["s.js"]);
    let mut cache = McpSchemaCache::default();
    cache.update("srv", &config, vec![tool("alpha")]);
    cache.save();
    assert!(cache_file.exists(), "save must create the cache file");

    let third = McpSchemaCache::load_cached();
    assert!(
        !Arc::ptr_eq(&first, &third),
        "a cache file that appeared must be read, not shadowed by the empty copy"
    );
    assert_eq!(third.tools_for("srv", &config).map(<[McpToolDef]>::len), Some(1));

    McpSchemaCache::invalidate_cached();
    unsafe {
        std::env::remove_var("HORON_HOME");
    }
}
