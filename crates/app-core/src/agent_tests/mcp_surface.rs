//! The MCP fixed surface: eager only when a server is configured (IS-5).
//!
//! On a provider without native deferred loading, `mcp_search` and `mcp_call`
//! are how the model discovers and calls MCP servers. With no server configured
//! they are two schemas that can never return anything, so a `core`-listed
//! session names them in its `# Additional tools` index and reaches them
//! through `toolbox` instead. A configured server keeps them eager, because the
//! model needs the discovery path without a listing step.

use super::*;
use crate::config::{ESSENTIAL_TOOLS, TOOLBOX_TOOL};

struct CoreEnv {
    _home: tempfile::TempDir,
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl CoreEnv {
    fn new(profile: &str) -> Self {
        let previous = ["HORON_HOME", "HORON_AGENT_TOOL_PROFILE", "HORON_AGENT_TOOLS"]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();
        let home = tempfile::tempdir().expect("temp home");
        crate::env::set_var("HORON_HOME", home.path());
        crate::env::set_var("HORON_AGENT_TOOL_PROFILE", profile);
        crate::env::remove_var("HORON_AGENT_TOOLS");
        crate::config::Config::invalidate_cache();
        Self { _home: home, previous }
    }
}

impl Drop for CoreEnv {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..) {
            if let Some(value) = value {
                crate::env::set_var(key, value);
            } else {
                crate::env::remove_var(key);
            }
        }
        crate::config::Config::invalidate_cache();
    }
}

fn definition<'a>(definitions: &'a [ToolDefinition], name: &str) -> &'a ToolDefinition {
    definitions
        .iter()
        .find(|definition| definition.name == name)
        .unwrap_or_else(|| {
            let names: Vec<&str> = definitions.iter().map(|d| d.name.as_str()).collect();
            panic!("{name} is not in the definition list: {names:?}")
        })
}

fn context(agent: &Agent) -> crate::tool::ToolContext {
    crate::tool::ToolContext {
        session_id: agent.session_id().to_string(),
        message_id: "message".to_string(),
        tool_call_id: "toolbox-call".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    }
}

/// A `core` session on a provider that drops deferred definitions, carrying the
/// fixed MCP surface and `configured_servers` servers.
async fn core_agent(configured_servers: usize) -> Agent {
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    register_fake_deferred_mcp_surface(&registry).await;
    registry.set_configured_mcp_servers_for_test(configured_servers);
    let agent = Agent::new(provider, registry);
    assert!(
        !agent.provider.supports_deferred_tools(),
        "this module pins the provider that drops deferred definitions"
    );
    assert!(agent.load_tiers.core_listed(), "the default core session lists tools");
    agent
}

async fn eager_names(agent: &mut Agent) -> Vec<String> {
    let definitions = agent.tool_definitions().await;
    let mut names: Vec<String> = definitions
        .iter()
        .filter(|definition| !definition.defer_loading)
        .map(|definition| definition.name.clone())
        .collect();
    names.sort();
    names
}

/// With no server configured the two fixed-surface schemas cost the cached
/// prefix nothing: they travel as listed tools and the index names them.
#[tokio::test]
async fn a_core_session_without_mcp_servers_lists_the_fixed_surface() {
    let _guard = crate::storage::lock_test_env();
    let _env = CoreEnv::new(crate::config::CORE_TOOL_PROFILE);
    let mut agent = core_agent(0).await;
    let registry = agent.registry();

    let definitions = agent.tool_definitions().await;
    for name in ["mcp_search", "mcp_call"] {
        assert!(
            definition(&definitions, name).defer_loading,
            "{name} must be listed, never eager, with no server to discover"
        );
    }
    let mut expected: Vec<String> = ESSENTIAL_TOOLS.iter().map(|name| (*name).to_string()).collect();
    expected.push(TOOLBOX_TOOL.to_string());
    expected.sort();
    assert_eq!(
        eager_names(&mut agent).await,
        expected,
        "the eager set is the seven essential tools plus the dispatcher"
    );

    let prompt = agent.system_prompt_for_debug();
    for name in ["mcp_call", "mcp_search"] {
        assert!(
            prompt.contains(&format!("- {name}: ")),
            "{name} must be named in the index: {prompt}"
        );
    }

    let listed = registry
        .execute(TOOLBOX_TOOL, serde_json::json!({"action": "list"}), context(&agent))
        .await
        .expect("toolbox list succeeds");
    for name in ["mcp_call", "mcp_search"] {
        assert!(
            listed.output.contains(&format!("- {name}: ")),
            "{name} must be reachable through the dispatcher: {}",
            listed.output
        );
    }
}

/// One configured server keeps the discovery path eager: it is what the model
/// uses to find and call that server, so it must not sit behind a listing step.
#[tokio::test]
async fn a_configured_mcp_server_keeps_the_fixed_surface_eager() {
    let _guard = crate::storage::lock_test_env();
    let _env = CoreEnv::new(crate::config::CORE_TOOL_PROFILE);
    let mut agent = core_agent(1).await;
    let registry = agent.registry();

    let definitions = agent.tool_definitions().await;
    for name in ["mcp_search", "mcp_call"] {
        assert!(
            !definition(&definitions, name).defer_loading,
            "{name} stays eager while a server is configured"
        );
    }
    let mut expected: Vec<String> = ESSENTIAL_TOOLS.iter().map(|name| (*name).to_string()).collect();
    expected.extend([
        "mcp_call".to_string(),
        "mcp_search".to_string(),
        TOOLBOX_TOOL.to_string(),
    ]);
    expected.sort();
    assert_eq!(eager_names(&mut agent).await, expected);

    let prompt = agent.system_prompt_for_debug();
    for name in ["mcp_call", "mcp_search"] {
        assert!(
            !prompt.contains(&format!("\n- {name}: ")),
            "an eager tool is not listed again: {prompt}"
        );
    }

    let listed = registry
        .execute(TOOLBOX_TOOL, serde_json::json!({"action": "list"}), context(&agent))
        .await
        .expect("toolbox list succeeds");
    for name in ["mcp_call", "mcp_search"] {
        assert!(
            !listed.output.contains(&format!("- {name}: ")),
            "an eager tool is not listed by the dispatcher: {}",
            listed.output
        );
    }
}

/// The decision is taken once, at the first tool-list lock. A server that
/// connects later, a configuration reload, an unlock and a compaction must not
/// move the two schemas between the eager list and the index.
#[tokio::test]
async fn the_mcp_surface_decision_is_frozen_for_the_session() {
    let _guard = crate::storage::lock_test_env();
    let _env = CoreEnv::new(crate::config::CORE_TOOL_PROFILE);

    let mut listed_first = core_agent(0).await;
    let first = serde_json::to_string(&listed_first.tool_definitions().await).expect("serialize");
    let first_prompt = listed_first.system_prompt_for_debug();
    listed_first.registry.set_configured_mcp_servers_for_test(3);
    listed_first.unlock_tools();
    listed_first.note_compaction_applied();
    let second = serde_json::to_string(&listed_first.tool_definitions().await).expect("serialize");
    assert_eq!(
        second, first,
        "a later server never makes the surface eager mid-session"
    );
    assert_eq!(
        listed_first.system_prompt_for_debug(),
        first_prompt,
        "the index keeps naming the fixed surface"
    );

    let mut eager_first = core_agent(1).await;
    let first = serde_json::to_string(&eager_first.tool_definitions().await).expect("serialize");
    eager_first.registry.set_configured_mcp_servers_for_test(0);
    eager_first.unlock_tools();
    eager_first.note_compaction_applied();
    let second = serde_json::to_string(&eager_first.tool_definitions().await).expect("serialize");
    assert_eq!(second, first, "the surface stays eager once it was resolved eager");
}

/// A server that connects mid-session is announced in the transcript. When the
/// fixed surface is listed, the note has to send the model through `toolbox`,
/// because `mcp_call` is not in its tool list.
#[tokio::test]
async fn a_late_mcp_tool_is_announced_through_toolbox_when_the_surface_is_listed() {
    let _guard = crate::storage::lock_test_env();
    let _env = CoreEnv::new(crate::config::CORE_TOOL_PROFILE);

    for (configured_servers, expects_toolbox) in [(0, true), (1, false)] {
        let mut agent = core_agent(configured_servers).await;
        agent.mcp_tools_mode = crate::config::McpToolsMode::Auto;
        agent.tool_definitions().await;
        agent
            .registry
            .register(
                "mcp__fake__echo".to_string(),
                Arc::new(FakeMcpTool {
                    name: "echo".to_string(),
                }) as Arc<dyn crate::tool::Tool>,
            )
            .await;

        agent.announce_late_mcp_tools().await;
        let text = transcript_texts(&agent)
            .into_iter()
            .find(|text| text.contains("New MCP tools are available."))
            .unwrap_or_else(|| panic!("no announcement with {configured_servers} configured server(s)"));
        assert!(text.contains("mcp__fake__echo"), "{text}");
        assert_eq!(
            text.contains("toolbox"),
            expects_toolbox,
            "the note must point at the surface the session actually has: {text}"
        );
        if expects_toolbox {
            assert!(text.contains("mcp_call"), "{text}");
        }
    }
}

/// A session that resolved on a provider with native deferred loading froze an
/// index without the fixed surface. Switching to a provider that drops deferred
/// definitions must not leave `mcp_call` advertised nowhere: the two tools fall
/// back to the eager surface that path sent before core listing existed.
#[tokio::test]
async fn a_provider_switch_to_non_native_keeps_the_fixed_surface_reachable() {
    let _guard = crate::storage::lock_test_env();
    let _env = CoreEnv::new(crate::config::CORE_TOOL_PROFILE);
    let native = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let provider: Arc<dyn Provider> = Arc::new(SwitchableDeferredProvider(Arc::clone(&native)));
    let registry = Registry::new(provider.clone()).await;
    register_fake_deferred_mcp_surface(&registry).await;
    let mut agent = Agent::new(provider, registry.clone());

    let on_native = agent.tool_definitions().await;
    assert!(
        !definition(&on_native, "mcp_search").defer_loading,
        "discovery stays eager"
    );
    assert!(
        definition(&on_native, "mcp_call").defer_loading,
        "the native placeholder"
    );
    let native_prompt = agent.system_prompt_for_debug();
    assert!(
        !native_prompt.contains("\n- mcp_search: "),
        "the native index names no fixed surface: {native_prompt}"
    );

    native.store(false, std::sync::atomic::Ordering::SeqCst);
    let fallback = agent.tool_definitions().await;
    for name in ["mcp_search", "mcp_call"] {
        assert!(
            !definition(&fallback, name).defer_loading,
            "{name} must be sent on the provider that drops deferred definitions"
        );
    }
    let prompt = agent.system_prompt_for_debug();
    assert_eq!(
        prompt, native_prompt,
        "the frozen index never gains a row for a surface it does not carry"
    );

    let listed = registry
        .execute(TOOLBOX_TOOL, serde_json::json!({"action": "list"}), context(&agent))
        .await
        .expect("toolbox list succeeds");
    for name in ["mcp_call", "mcp_search"] {
        assert!(
            !listed.output.contains(&format!("- {name}: ")),
            "an eager surface is not also listed: {}",
            listed.output
        );
    }
}

/// Before the MCP surface registers, a warmup builds an index that cannot name
/// it. The decision is taken at the tool lock, so a server that is configured by
/// then keeps the surface eager instead of hiding it behind a listing step.
#[tokio::test]
async fn a_warmup_before_mcp_registration_leaves_the_decision_to_the_lock() {
    let _guard = crate::storage::lock_test_env();
    let _env = CoreEnv::new(crate::config::CORE_TOOL_PROFILE);
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry.clone());

    let (_provider, _tools, warm, _context) = agent.prewarm_snapshot().await;
    assert!(!warm.contains("\n- mcp_search: "), "nothing registered yet: {warm}");

    register_fake_deferred_mcp_surface(&registry).await;
    registry.set_configured_mcp_servers_for_test(1);

    let definitions = agent.tool_definitions().await;
    for name in ["mcp_call", "mcp_search"] {
        assert!(
            !definition(&definitions, name).defer_loading,
            "the lock sees the configured server and keeps {name} eager"
        );
    }
    let prompt = agent.system_prompt_for_debug();
    assert!(
        !prompt.contains("\n- mcp_search: "),
        "the index was rebuilt at the lock: {prompt}"
    );
}

/// A batch larger than the announcement's inline limit ends on a pointer to the
/// way back. When the session lists the fixed surface, that pointer is the
/// dispatcher, because a direct `mcp_search` is not in its tool list.
#[tokio::test]
async fn a_large_late_batch_points_at_the_surface_the_session_has() {
    let _guard = crate::storage::lock_test_env();
    let _env = CoreEnv::new(crate::config::CORE_TOOL_PROFILE);

    for (configured_servers, expects_toolbox) in [(0, true), (1, false)] {
        let mut agent = core_agent(configured_servers).await;
        agent.mcp_tools_mode = crate::config::McpToolsMode::Auto;
        agent.tool_definitions().await;
        for index in 0..33 {
            agent
                .registry
                .register(
                    format!("mcp__bulk__tool{index}"),
                    Arc::new(FakeMcpTool {
                        name: format!("tool{index}"),
                    }) as Arc<dyn crate::tool::Tool>,
                )
                .await;
        }

        agent.announce_late_mcp_tools().await;
        let text = transcript_texts(&agent)
            .into_iter()
            .find(|text| text.contains("New MCP tools are available."))
            .unwrap_or_else(|| panic!("no announcement with {configured_servers} configured server(s)"));
        assert!(text.contains("mcp__bulk__tool0"), "the note names what arrived: {text}");
        let tail = text
            .split("</system-reminder>")
            .next()
            .and_then(|body| body.lines().map(str::trim).rfind(|line| !line.is_empty()))
            .unwrap_or_else(|| panic!("the announcement has no body: {text}"));
        assert!(
            tail.contains("mcp_search"),
            "the overflow names the search tool: {tail}"
        );
        assert_eq!(
            tail.contains("toolbox"),
            expects_toolbox,
            "the overflow tail names the surface the session has: {tail}"
        );
    }
}

/// The dispatcher is policy-transparent for the MCP management tool too: a
/// `toolbox` call that runs `mcp` unlocks the tool list exactly as a direct
/// `mcp` call does in eager mode.
#[tokio::test]
async fn a_toolbox_mcp_call_unlocks_like_a_direct_mcp_call() {
    let _guard = crate::storage::lock_test_env();
    let _env = CoreEnv::new(crate::config::CORE_TOOL_PROFILE);
    let mut agent = core_agent(1).await;
    agent.mcp_tools_mode = crate::config::McpToolsMode::Eager;
    agent.tool_definitions().await;
    assert!(agent.locked_tools.is_some(), "the first turn locks the tool list");

    agent.unlock_tools_if_needed(
        TOOLBOX_TOOL,
        &serde_json::json!({"action": "run", "tool": "mcp", "args": {"action": "list"}}),
    );
    assert!(
        agent.locked_tools.is_none(),
        "an `mcp` call through the dispatcher unlocks the tool list"
    );

    agent.tool_definitions().await;
    agent.unlock_tools_if_needed(
        TOOLBOX_TOOL,
        &serde_json::json!({"action": "run", "tool": "undo", "args": {}}),
    );
    assert!(
        agent.locked_tools.is_some(),
        "a dispatcher call that does not reach `mcp` leaves the list locked"
    );
}
