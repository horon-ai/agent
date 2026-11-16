//! A dispatch reads the session's tool policy as a shared snapshot. These
//! tests pin what that must not change: the allow and deny answers, the
//! rejection text, what a writer does while a snapshot is held, and that an
//! SDK overlay is merged into a copy and never into the stored policy.

use super::*;
use crate::protocol::{SessionToolConfig, SessionToolDefinition};
use crate::provider::Provider;
use async_trait::async_trait;
use serde_json::json;
use tokio::sync::mpsc;

struct MockProvider;

#[async_trait]
impl Provider for MockProvider {
    async fn complete(
        &self,
        _messages: &[crate::message::Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<crate::provider::EventStream> {
        Err(anyhow::anyhow!("mock provider does not stream"))
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(MockProvider)
    }
}

/// Removes the session's overlay and stored policy when the test ends.
struct Cleanup(&'static str);

impl Drop for Cleanup {
    fn drop(&mut self) {
        sdk::remove_session(self.0);
        clear_session_tool_policy(self.0);
    }
}

/// The allow-list and the deny-list of one policy.
type NameSets = (Option<HashSet<String>>, HashSet<String>);

fn names(list: &[&str]) -> HashSet<String> {
    list.iter().map(|name| (*name).to_string()).collect()
}

fn custom(name: &str) -> SessionToolDefinition {
    SessionToolDefinition {
        name: name.into(),
        description: "SDK callback".into(),
        parameters: json!({"type": "object"}),
    }
}

fn context(session: &str) -> ToolContext {
    ToolContext {
        session_id: session.into(),
        message_id: "policy-route".into(),
        tool_call_id: "policy-route-call".into(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

/// The policy the registry stores for the session, without any overlay.
fn stored(session: &str) -> Option<Arc<SessionToolPolicy>> {
    SESSION_TOOL_POLICIES
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(session)
        .cloned()
}

fn shared_sets(session: &str) -> Option<NameSets> {
    session_tool_policy_shared(session).map(|policy| (policy.allowed_tools.clone(), policy.disabled_tools.clone()))
}

/// The merge rule, written out independently of the production code: the
/// stored sets, then the overlay's `enabled` replacing both, its `disabled`
/// added, and its custom names added to an existing allow-list.
fn reference_sets(session: &str) -> Option<NameSets> {
    let mut sets: Option<NameSets> =
        stored(session).map(|policy| (policy.allowed_tools.clone(), policy.disabled_tools.clone()));
    if let Some(config) = sdk::config(session) {
        let (allowed, disabled) = sets.get_or_insert_with(NameSets::default);
        if let Some(enabled) = config.enabled {
            *allowed = Some(enabled.into_iter().collect());
            disabled.clear();
        }
        disabled.extend(config.disabled);
        if let Some(allowed) = allowed.as_mut() {
            allowed.extend(config.custom.into_iter().map(|tool| tool.name));
        }
    }
    sets
}

/// Every name the registry has known the tool behind `name` by, plus `name`.
fn known_names(registry: &Registry, name: &str) -> HashSet<String> {
    let index = registry
        .mcp_policy
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut known = index
        .current
        .get(name)
        .and_then(|identity| index.history.get(identity))
        .cloned()
        .unwrap_or_default();
    known.insert(name.to_string());
    known
}

#[test]
fn a_session_without_an_overlay_reads_the_stored_policy_itself() {
    let session = "policy-route-shared";
    let _cleanup = Cleanup(session);
    assert!(session_tool_policy_shared(session).is_none());
    assert!(session_tool_policy(session).is_none());

    set_session_tool_policy(session, Some(names(&["read", "mcp__docs__search"])), names(&["bash"]));

    let first = session_tool_policy_shared(session).expect("the session has a policy");
    let second = session_tool_policy_shared(session).expect("the session has a policy");
    let kept = stored(session).expect("the policy is stored");
    assert!(Arc::ptr_eq(&first, &second), "two dispatches copied the policy");
    assert!(Arc::ptr_eq(&first, &kept), "a dispatch copied the stored policy");

    let owned = session_tool_policy(session).expect("the session has a policy");
    assert_eq!(owned.allowed_tools, first.allowed_tools);
    assert_eq!(owned.disabled_tools, first.disabled_tools);
    assert_eq!(shared_sets(session), reference_sets(session));
}

#[test]
fn allow_and_deny_answers_follow_the_name_rules() {
    let session = "policy-route-answers";
    let _cleanup = Cleanup(session);
    let answer = |tool: &str| session_tool_policy_allows_tool_for_test(session, tool);
    assert_eq!(answer("read"), None, "a session without a policy has no answer");

    set_session_tool_policy(session, Some(names(&["read", "mcp__docs__search"])), names(&["bash"]));
    for (tool, allowed) in [
        ("read", true),
        ("ls", false),
        ("bash", false),
        ("mcp__docs__search", true),
        ("mcp__docs__other", false),
        ("mcp_call", true),
        ("mcp_search", true),
    ] {
        assert_eq!(answer(tool), Some(allowed), "allow-list answer for {tool}");
    }

    set_session_tool_policy(session, None, names(&["mcp"]));
    for (tool, allowed) in [
        ("read", true),
        ("mcpish", true),
        ("mcp", false),
        ("mcp__docs__search", false),
        ("mcp_call", false),
    ] {
        assert_eq!(answer(tool), Some(allowed), "umbrella deny answer for {tool}");
    }

    set_session_tool_policy(session, Some(names(&["mcp"])), names(&["mcp__docs__search"]));
    for (tool, allowed) in [
        ("mcp__docs__other", true),
        ("mcp__docs__search", false),
        ("read", false),
    ] {
        assert_eq!(answer(tool), Some(allowed), "umbrella allow answer for {tool}");
    }
}

#[tokio::test]
async fn execute_rejects_with_the_allow_check_before_the_deny_check() {
    let session = "policy-route-execute";
    let _cleanup = Cleanup(session);
    let registry = Registry::new(Arc::new(MockProvider)).await;
    set_session_tool_policy(session, Some(names(&["read", "bash"])), names(&["bash", "ls"]));

    let not_allowed = registry
        .execute("ls", json!({}), context(session))
        .await
        .expect_err("a tool outside the allow-list must not run");
    assert!(
        not_allowed.to_string().contains("Tool 'ls' is not allowed"),
        "{not_allowed}"
    );

    let disabled = registry
        .execute("bash", json!({ "command": "true" }), context(session))
        .await
        .expect_err("a disabled tool must not run");
    assert!(disabled.to_string().contains("Tool 'bash' is disabled"), "{disabled}");

    // The dispatch dropped its snapshot: the stored policy is the only holder.
    let kept = stored(session).expect("the policy is stored");
    assert_eq!(Arc::strong_count(&kept), 2);
}

#[test]
fn writers_change_the_stored_policy_while_a_snapshot_is_held() {
    let session = "policy-route-writers";
    let absent = "policy-route-writers-absent";
    let _cleanup = Cleanup(session);
    let _absent_cleanup = Cleanup(absent);
    let tiers = crate::config::ToolConfig::default().load_tiers(None, false);
    set_session_tool_policy(session, Some(names(&["read"])), names(&["ls"]));

    let snapshot = session_tool_policy_shared(session).expect("the session has a policy");
    allow_session_tool(session, "ls");
    register_session_listed_mcp(session, true);
    register_session_load_tiers(session, tiers.clone());

    assert_eq!(snapshot.allowed_tools, Some(names(&["read"])));
    assert_eq!(snapshot.disabled_tools, names(&["ls"]));
    assert!(!snapshot.listed_mcp_surface);
    assert!(snapshot.load_tiers.is_none());

    let current = session_tool_policy_shared(session).expect("the session has a policy");
    assert!(!Arc::ptr_eq(&snapshot, &current));
    assert_eq!(current.allowed_tools, Some(names(&["read", "ls"])));
    assert!(current.disabled_tools.is_empty());
    assert!(current.listed_mcp_surface);
    assert_eq!(current.load_tiers.as_ref(), Some(&tiers));
    assert!(session_lists_mcp_surface(session));
    assert_eq!(session_tool_policy_allows_tool_for_test(session, "ls"), Some(true));

    // A writer never creates the entry of a session that has none.
    allow_session_tool(absent, "ls");
    register_session_listed_mcp(absent, true);
    register_session_load_tiers(absent, tiers);
    assert!(stored(absent).is_none());
    assert!(!session_lists_mcp_surface(absent));
}

#[test]
fn a_registration_still_owns_its_policy_after_a_copy_on_write() {
    let session = "policy-route-owner";
    let _cleanup = Cleanup(session);
    let registration = register_session_tool_policy(session, Some(names(&["read"])), HashSet::new());

    let snapshot = session_tool_policy_shared(session).expect("the session has a policy");
    allow_session_tool(session, "ls");
    assert_eq!(session_tool_policy_allows_tool_for_test(session, "ls"), Some(true));

    drop(registration);
    assert!(
        stored(session).is_none(),
        "the registration no longer removes its policy"
    );
    assert_eq!(snapshot.allowed_tools, Some(names(&["read"])));
}

#[test]
fn an_overlay_is_merged_into_a_copy_and_never_stored() {
    let session = "policy-route-overlay";
    let bare = "policy-route-overlay-bare";
    let _cleanup = Cleanup(session);
    let _bare_cleanup = Cleanup(bare);
    let (tx, _rx) = mpsc::unbounded_channel();
    set_session_tool_policy(session, Some(names(&["read"])), names(&["bash"]));

    let inheriting = SessionToolConfig {
        enabled: None,
        disabled: vec!["write".into()],
        custom: vec![custom("local")],
    };
    sdk::configure(session, "policy-route-owner", inheriting.clone(), tx.clone()).expect("configure the overlay");
    let (allowed, disabled) = shared_sets(session).expect("the session has a policy");
    assert_eq!(allowed, Some(names(&["read", "local"])));
    assert!(disabled.contains("bash") && disabled.contains("write"), "{disabled:?}");
    assert_eq!(shared_sets(session), reference_sets(session));

    let replacing = SessionToolConfig {
        enabled: Some(vec!["ls".into()]),
        disabled: vec!["write".into()],
        custom: vec![custom("local")],
    };
    sdk::configure(session, "policy-route-owner", replacing, tx.clone()).expect("configure the overlay");
    let (allowed, disabled) = shared_sets(session).expect("the session has a policy");
    assert_eq!(allowed, Some(names(&["ls", "local"])));
    assert!(!disabled.contains("bash") && disabled.contains("write"), "{disabled:?}");
    assert_eq!(shared_sets(session), reference_sets(session));
    assert_eq!(session_tool_policy_allows_tool_for_test(session, "read"), Some(false));
    assert_eq!(session_tool_policy_allows_tool_for_test(session, "ls"), Some(true));

    // The stored policy carries none of it, and comes back once the overlay goes.
    let kept = stored(session).expect("the policy is stored");
    assert_eq!(kept.allowed_tools, Some(names(&["read"])));
    assert_eq!(kept.disabled_tools, names(&["bash"]));
    sdk::remove_session(session);
    let plain = session_tool_policy_shared(session).expect("the session has a policy");
    assert!(Arc::ptr_eq(&plain, &kept));
    assert_eq!(session_tool_policy_allows_tool_for_test(session, "read"), Some(true));

    // An overlay alone is a policy too, and it stores nothing.
    sdk::configure(bare, "policy-route-owner", inheriting, tx).expect("configure the overlay");
    let (allowed, disabled) = shared_sets(bare).expect("an overlay is a policy");
    assert_eq!(allowed, None);
    assert!(disabled.contains("write"), "{disabled:?}");
    assert_eq!(shared_sets(bare), reference_sets(bare));
    assert!(stored(bare).is_none());
}

#[test]
fn custom_answers_for_the_sessions_own_overlay_only() {
    let session = "policy-route-custom";
    let other = "policy-route-custom-other";
    let _cleanup = Cleanup(session);
    let _other_cleanup = Cleanup(other);
    let (tx, _rx) = mpsc::unbounded_channel();
    let asked = ["local", "read", "ls", ""];
    let by_config = |session: &str, name: &str| {
        sdk::config(session).is_some_and(|config| config.custom.iter().any(|tool| tool.name == name))
    };
    assert!(!sdk::custom(session, "local"));

    let config = SessionToolConfig {
        enabled: Some(vec![]),
        disabled: vec![],
        custom: vec![custom("local"), custom("read")],
    };
    sdk::configure(session, "policy-route-owner", config, tx.clone()).expect("configure the overlay");
    sdk::configure(other, "policy-route-owner", SessionToolConfig::default(), tx).expect("configure the overlay");
    for name in asked {
        assert_eq!(sdk::custom(session, name), by_config(session, name), "{name}");
        assert_eq!(sdk::custom(other, name), by_config(other, name), "{name}");
    }
    assert!(sdk::custom(session, "local"));
    assert!(sdk::custom(session, "read"));
    assert!(!sdk::custom(session, "ls"));
    assert!(!sdk::custom(other, "local"));

    sdk::remove_session(session);
    assert!(!sdk::custom(session, "local"));
}

#[tokio::test]
async fn every_registered_name_of_an_mcp_tool_decides_allow_and_deny() {
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let manager = Arc::new(RwLock::new(crate::mcp::McpManager::with_config(
        crate::mcp::McpConfig::default(),
    )));
    let catalog: Vec<(String, crate::mcp::McpToolDef)> = [("server-a", "query-docs"), ("server_a", "query_docs")]
        .into_iter()
        .map(|(server, name)| {
            (
                server.to_string(),
                crate::mcp::McpToolDef {
                    name: name.to_string(),
                    description: Some(server.to_string()),
                    input_schema: json!({"type": "object"}),
                },
            )
        })
        .collect();
    // Registered alone the first tool takes its plain name; the colliding
    // second server then renames both, so the first has two names on record.
    let retired = crate::mcp::dispatch_name(&catalog[0].0, &catalog[0].1.name);
    registry
        .reconcile_mcp_tools(crate::mcp::create_mcp_tools_from_cached_many(
            &catalog[..1],
            Arc::clone(&manager),
        ))
        .await;
    let aliases = crate::mcp::dispatch_names(&catalog);
    registry
        .reconcile_mcp_tools(crate::mcp::create_mcp_tools_from_cached_many(&catalog, manager))
        .await;
    assert_ne!(retired, aliases[0]);
    assert_eq!(
        known_names(&registry, &aliases[0]),
        names(&[retired.as_str(), aliases[0].as_str()])
    );

    let sets = [
        names(&[retired.as_str()]),
        names(&[aliases[0].as_str()]),
        names(&[aliases[1].as_str()]),
        names(&["mcp"]),
        names(&["bash"]),
        HashSet::new(),
    ];
    let asked = [
        retired.as_str(),
        aliases[0].as_str(),
        aliases[1].as_str(),
        "bash",
        "mcp_call",
        "unknown",
    ];
    for set in &sets {
        for name in asked {
            let known = known_names(&registry, name);
            assert_eq!(
                registry.tool_is_allowed(set, name),
                known.iter().any(|known| tool_name_is_allowed(set, known)),
                "allow answer for {name} against {set:?}"
            );
            assert_eq!(
                registry.tool_is_disabled(set, name),
                known.iter().any(|known| tool_name_is_disabled(set, known)),
                "deny answer for {name} against {set:?}"
            );
        }
    }

    // The retired name still decides for the tool it used to name.
    assert!(registry.tool_is_allowed(&names(&[retired.as_str()]), &aliases[0]));
    assert!(registry.tool_is_disabled(&names(&[retired.as_str()]), &aliases[0]));
    assert!(!registry.tool_is_allowed(&names(&["bash"]), &aliases[0]));
    assert!(!registry.tool_is_disabled(&HashSet::new(), &aliases[0]));
}
