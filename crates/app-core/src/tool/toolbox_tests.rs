// Every async test here holds the process-wide environment lock while it
// awaits a tool call. The lock is the point: it serializes tests that mutate
// `HORON_HOME` and the hook variables. The same allowance is used by
// `concurrency_class_tests.rs` and `hook_serialization_tests.rs`.
#![cfg_attr(test, allow(clippy::await_holding_lock))]

//! Tests for the `toolbox` dispatcher.
//!
//! The dispatcher exists only to reach tools the session did not carry
//! eagerly. These tests pin the three properties that matter: a `run` is
//! indistinguishable from a direct call (same output, same hooks, same policy),
//! every `describe`/`run` result names the tool so the transcript emits a
//! reference, and a tool whose load mode is `Off` stays unreachable while no
//! promotion action exists at all.

use super::*;
use crate::config::{CORE_TOOLS, ESSENTIAL_TOOLS, LoadMode, TOOLBOX_TOOL, ToolConfig, ToolLoadTiers};
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

const SESSION: &str = "toolbox-tests";

struct EchoTool {
    name: &'static str,
    description: &'static str,
    concurrency: ConcurrencyClass,
    /// What this probe declares it may touch, so plan mode can be exercised
    /// against a read-only target and a write-shaped one.
    effects: ToolEffects,
}

#[async_trait::async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.description
    }

    fn concurrency_class(&self) -> ConcurrencyClass {
        self.concurrency
    }

    fn effects(&self) -> ToolEffects {
        self.effects
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"]
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        Ok(ToolOutput::new(format!("echo:{}", input["text"].as_str().unwrap_or_default())).with_title("Echo"))
    }
}

fn tiers(overrides: &[(&str, LoadMode)]) -> ToolLoadTiers {
    let mut load_mode = BTreeMap::new();
    for (name, mode) in overrides {
        load_mode.insert((*name).to_string(), *mode);
    }
    ToolConfig {
        profile: "full".to_string(),
        load_mode,
        ..ToolConfig::default()
    }
    .load_tiers(None, false)
}

/// Installs a session policy whose frozen tiers are exactly `tiers`, and clears
/// it when the test ends so one test cannot leak into the next.
struct PolicyGuard;

impl PolicyGuard {
    fn install(tiers: ToolLoadTiers) -> Self {
        set_session_tool_policy(SESSION, None, HashSet::new());
        register_session_load_tiers(SESSION, tiers);
        Self
    }
}

impl Drop for PolicyGuard {
    fn drop(&mut self) {
        clear_session_tool_policy(SESSION);
    }
}

fn context() -> ToolContext {
    ToolContext {
        session_id: SESSION.to_string(),
        message_id: "message".to_string(),
        tool_call_id: "toolbox-call".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

/// A registry holding the dispatcher plus three fake tools: a read-only one
/// that full defers, a write-shaped one that the hook test blocks, and one an
/// override turns `Off`.
async fn registry() -> Registry {
    let registry = Registry::empty();
    let mut tools = registry.tools.write().await;
    tools.insert(
        "toolbox".to_string(),
        Arc::new(toolbox::ToolboxTool::new(registry.downgrade())) as Arc<dyn Tool>,
    );
    tools.insert(
        "readonly_probe".to_string(),
        Arc::new(EchoTool {
            name: "readonly_probe",
            description: "Read-only probe that echoes its input.\nSecond line is never listed.",
            concurrency: ConcurrencyClass::Concurrent,
            effects: ToolEffects::read(),
        }) as Arc<dyn Tool>,
    );
    tools.insert(
        "write_probe".to_string(),
        Arc::new(EchoTool {
            name: "write_probe",
            description: "Write-shaped probe.",
            concurrency: ConcurrencyClass::Ordered,
            effects: ToolEffects::write(),
        }) as Arc<dyn Tool>,
    );
    tools.insert(
        "off_probe".to_string(),
        Arc::new(EchoTool {
            name: "off_probe",
            description: "Probe an override removes.",
            concurrency: ConcurrencyClass::Concurrent,
            effects: ToolEffects::read(),
        }) as Arc<dyn Tool>,
    );
    drop(tools);
    registry
}

async fn call(registry: &Registry, input: Value) -> Result<ToolOutput> {
    registry.execute(TOOLBOX_TOOL, input, context()).await
}

#[test]
fn the_schema_has_no_promote_action() {
    let tool = toolbox::ToolboxTool::new(Registry::empty().downgrade());
    let schema = tool.parameters_schema();
    let actions: Vec<&str> = schema["properties"]["action"]["enum"]
        .as_array()
        .expect("action is an enum")
        .iter()
        .filter_map(|value| value.as_str())
        .collect();
    assert_eq!(actions, ["list", "describe", "run"]);
    assert!(
        !actions.contains(&"promote"),
        "promotion is deliberately absent: the dispatcher loads a schema by reference"
    );
    assert_eq!(tool.name(), TOOLBOX_TOOL);
    assert_eq!(
        tool.concurrency_class(),
        ConcurrencyClass::Ordered,
        "the dispatcher can reach a write, so it must never overlap another call"
    );
}

#[tokio::test]
async fn toolbox_run_matches_direct_call() {
    let _guard = crate::storage::lock_test_env();
    let _policy = PolicyGuard::install(tiers(&[]));
    let registry = registry().await;
    let args = json!({"text": "same bytes"});

    let direct = registry
        .execute("readonly_probe", args.clone(), context())
        .await
        .expect("direct call succeeds");
    let through = call(
        &registry,
        json!({"action": "run", "tool": "readonly_probe", "args": args}),
    )
    .await
    .expect("toolbox run succeeds");

    assert_eq!(through.output, direct.output);
    assert_eq!(through.title, direct.title);
    assert_eq!(
        through.metadata.as_ref().unwrap()["tool_references"],
        json!(["readonly_probe"]),
        "a run must name its target so the deferred schema loads"
    );
}

#[tokio::test]
async fn toolbox_describe_returns_the_schema_and_references_the_tool() {
    let _guard = crate::storage::lock_test_env();
    let _policy = PolicyGuard::install(tiers(&[]));
    let registry = registry().await;

    let output = call(&registry, json!({"action": "describe", "tool": "readonly_probe"}))
        .await
        .expect("describe succeeds");
    assert!(output.output.contains("readonly_probe"), "{}", output.output);
    assert!(output.output.contains("\"text\""), "{}", output.output);
    assert_eq!(
        output.metadata.as_ref().unwrap()["tool_references"],
        json!(["readonly_probe"])
    );
}

#[tokio::test]
async fn toolbox_list_names_only_discoverable_tools() {
    let _guard = crate::storage::lock_test_env();
    let _policy = PolicyGuard::install(tiers(&[("off_probe", LoadMode::Off)]));
    let registry = registry().await;

    let output = call(&registry, json!({"action": "list"})).await.expect("list succeeds");
    assert!(
        output
            .output
            .contains("readonly_probe: Read-only probe that echoes its input."),
        "{}",
        output.output
    );
    assert!(
        !output.output.contains("Second line"),
        "list prints one line per tool: {}",
        output.output
    );
    assert!(
        !output.output.contains("off_probe"),
        "an off tool is not reachable: {}",
        output.output
    );
    assert!(
        !output.output.contains("- toolbox"),
        "the dispatcher does not list itself: {}",
        output.output
    );
}

#[tokio::test]
async fn toolbox_list_clips_a_description_like_the_index_does() {
    let _guard = crate::storage::lock_test_env();
    let _policy = PolicyGuard::install(tiers(&[]));
    let registry = Registry::empty();
    let sentence = "Names one tool. Everything after it belongs to the schema.";
    // One sentence, no terminator, longer than the bound: the index and the list
    // both clip it at the bound instead of at a sentence end.
    let unbounded = "Watches one path across restarts and reports every change once per turn \
                     boundary with a digest compared against the previous content each second";
    {
        let mut tools = registry.tools.write().await;
        tools.insert(
            "toolbox".to_string(),
            Arc::new(toolbox::ToolboxTool::new(registry.downgrade())) as Arc<dyn Tool>,
        );
        for (name, description) in [("sentence_probe", sentence), ("bound_probe", unbounded)] {
            tools.insert(
                name.to_string(),
                Arc::new(EchoTool {
                    name,
                    description,
                    concurrency: ConcurrencyClass::Concurrent,
                    effects: ToolEffects::read(),
                }) as Arc<dyn Tool>,
            );
        }
    }

    let output = call(&registry, json!({"action": "list"})).await.expect("list succeeds");
    let clipped = crate::prompt::one_line_description(sentence);
    assert!(
        output.output.contains(&format!("sentence_probe: {clipped}")),
        "{clipped} must be what the index shows: {}",
        output.output
    );
    assert!(
        !output.output.contains("belongs to the schema"),
        "the second sentence is left to describe: {}",
        output.output
    );
    let bound = crate::prompt::one_line_description(unbounded);
    assert_eq!(
        bound.chars().count(),
        crate::prompt::ONE_LINE_DESCRIPTION_MAX_CHARS,
        "a description without a terminator is clipped at the bound"
    );
    assert!(bound.ends_with("..."), "{bound}");
    assert!(
        output.output.contains(&format!("bound_probe: {bound}")),
        "{}",
        output.output
    );
}

#[tokio::test]
async fn toolbox_list_hides_a_policy_disabled_tool() {
    let _guard = crate::storage::lock_test_env();
    set_session_tool_policy(SESSION, None, HashSet::from(["write_probe".to_string()]));
    register_session_load_tiers(SESSION, tiers(&[]));
    let _policy = PolicyGuard;
    let registry = registry().await;

    let output = call(&registry, json!({"action": "list"})).await.expect("list succeeds");
    assert!(
        !output.output.contains("write_probe"),
        "a tool the session policy disables is not reachable: {}",
        output.output
    );
    assert!(
        output.output.contains("readonly_probe"),
        "an untouched tool is still listed: {}",
        output.output
    );
}

#[tokio::test]
async fn toolbox_rejects_off_tool() {
    let _guard = crate::storage::lock_test_env();
    let _policy = PolicyGuard::install(tiers(&[("off_probe", LoadMode::Off)]));
    let registry = registry().await;

    let error = call(
        &registry,
        json!({"action": "run", "tool": "off_probe", "args": {"text": "no"}}),
    )
    .await
    .expect_err("an off tool must be refused")
    .to_string();
    assert!(error.contains("off_probe"), "{error}");
    assert!(error.contains("off"), "{error}");
}

#[tokio::test]
async fn toolbox_rejects_bad_requests() {
    let _guard = crate::storage::lock_test_env();
    let _policy = PolicyGuard::install(tiers(&[]));
    let registry = registry().await;

    let unknown = call(&registry, json!({"action": "promote", "tool": "readonly_probe"}))
        .await
        .expect_err("unknown actions are refused")
        .to_string();
    assert!(unknown.contains("Unknown toolbox action"), "{unknown}");

    let missing_args = call(&registry, json!({"action": "run", "tool": "readonly_probe"}))
        .await
        .expect_err("run needs args")
        .to_string();
    assert!(missing_args.contains("needs `args`"), "{missing_args}");

    let missing_tool = call(&registry, json!({"action": "describe"}))
        .await
        .expect_err("describe needs a tool")
        .to_string();
    assert!(missing_tool.contains("needs `tool`"), "{missing_tool}");

    let itself = call(&registry, json!({"action": "run", "tool": "toolbox", "args": {}}))
        .await
        .expect_err("the dispatcher cannot run itself")
        .to_string();
    assert!(itself.contains("cannot run itself"), "{itself}");

    let bad_args = call(
        &registry,
        json!({"action": "run", "tool": "readonly_probe", "args": "not an object"}),
    )
    .await
    .expect_err("args must be an object")
    .to_string();
    assert!(bad_args.contains("to be a JSON object"), "{bad_args}");
}

#[cfg(unix)]
struct HookEnvGuard {
    previous: Option<std::ffi::OsString>,
}

#[cfg(unix)]
impl HookEnvGuard {
    fn install(key: &'static str, value: String) -> Self {
        let previous = std::env::var_os(key);
        crate::env::set_var(key, value);
        crate::config::invalidate_config_cache();
        Self { previous }
    }
}

#[cfg(unix)]
impl Drop for HookEnvGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => crate::env::set_var("HORON_AGENT_HOOK_PRE_TOOL", value),
            None => crate::env::remove_var("HORON_AGENT_HOOK_PRE_TOOL"),
        }
        crate::config::invalidate_config_cache();
    }
}

/// A `pre_tool` gate blocks the target of `toolbox run` exactly as it blocks a
/// direct call, because the dispatcher goes through `Registry::execute`.
#[cfg(unix)]
#[tokio::test]
async fn toolbox_run_applies_the_pre_tool_hook() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = crate::storage::lock_test_env();
    let dir = tempfile::tempdir().expect("temp dir");
    let script = dir.path().join("gate.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         cat >/dev/null\n\
         case \"$HORON_AGENT_HOOK_TOOL_NAME\" in\n\
         \x20 write_probe) echo 'probe is blocked' >&2; exit 2 ;;\n\
         esac\n\
         exit 0\n",
    )
    .expect("write hook script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let _hooks = HookEnvGuard::install("HORON_AGENT_HOOK_PRE_TOOL", script.display().to_string());

    let _policy = PolicyGuard::install(tiers(&[]));
    let registry = registry().await;

    let error = call(
        &registry,
        json!({"action": "run", "tool": "write_probe", "args": {"text": "blocked"}}),
    )
    .await
    .expect_err("the pre_tool gate must block a dispatched call")
    .to_string();
    assert!(error.contains("blocked by pre_tool hook"), "{error}");
    assert!(error.contains("probe is blocked"), "{error}");

    let allowed = call(
        &registry,
        json!({"action": "run", "tool": "readonly_probe", "args": {"text": "fine"}}),
    )
    .await
    .expect("an unblocked tool still runs");
    assert_eq!(allowed.output, "echo:fine");
}

/// The manual QA transcript: a real registry, the real dispatcher, the real
/// `Registry::execute` seam. Run with `--nocapture` to read it; it also asserts
/// so a broken build cannot print a happy transcript.
#[tokio::test]
async fn toolbox_manual_qa_transcript() {
    struct StubProvider;

    #[async_trait::async_trait]
    impl crate::provider::Provider for StubProvider {
        async fn complete(
            &self,
            _messages: &[crate::message::Message],
            _tools: &[crate::message::ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> anyhow::Result<crate::provider::EventStream> {
            anyhow::bail!("stub provider must never be called")
        }

        fn name(&self) -> &str {
            "stub"
        }

        fn fork(&self) -> Arc<dyn crate::provider::Provider> {
            Arc::new(StubProvider)
        }
    }

    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("temp home");
    let previous_home = std::env::var_os("HORON_HOME");
    let previous_profile = std::env::var_os("HORON_AGENT_TOOL_PROFILE");
    crate::env::set_var("HORON_HOME", home.path());
    crate::env::set_var("HORON_AGENT_TOOL_PROFILE", "full");
    crate::config::invalidate_config_cache();

    let registry = Registry::new(Arc::new(StubProvider) as Arc<dyn crate::provider::Provider>).await;
    registry
        .register(
            "manual_probe".to_string(),
            Arc::new(EchoTool {
                name: "manual_probe",
                description: "Read-only manual QA probe that echoes its input.",
                concurrency: ConcurrencyClass::Concurrent,
                effects: ToolEffects::read(),
            }) as Arc<dyn Tool>,
        )
        .await;
    set_session_tool_policy(SESSION, None, HashSet::new());
    register_session_load_tiers(SESSION, tiers(&[]));

    let definitions = registry.definitions(None).await;
    println!("=== toolbox list ===");
    let list = call(&registry, json!({"action": "list"})).await.expect("list succeeds");
    println!("{}", list.output);
    println!("metadata: {:?}", list.metadata);

    println!("\n=== toolbox describe manual_probe ===");
    let describe = call(&registry, json!({"action": "describe", "tool": "manual_probe"}))
        .await
        .expect("describe succeeds");
    println!("{}", describe.output);
    println!("metadata: {:?}", describe.metadata);

    println!("\n=== toolbox run manual_probe ===");
    let run = call(
        &registry,
        json!({"action": "run", "tool": "manual_probe", "args": {"text": "hello from toolbox"}}),
    )
    .await
    .expect("run succeeds");
    println!("output: {}", run.output);
    println!("metadata: {:?}", run.metadata);

    // `Registry::definitions` returns the raw list; the session tier pass is
    // what decides eager vs deferred, so classify through the same snapshot.
    let session_tiers = tiers(&[]);
    let eager: Vec<&str> = definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .filter(|name| session_tiers.tier(name) == LoadMode::Essential)
        .collect();
    let deferred = definitions.len() - eager.len();
    println!(
        "\ntoolbox present: {}",
        definitions.iter().any(|d| d.name == TOOLBOX_TOOL)
    );
    println!("tier of toolbox: {:?}", session_tiers.tier(TOOLBOX_TOOL));
    println!("eager under full: {} tools, deferred: {} tools", eager.len(), deferred);
    println!("eager names: {eager:?}");

    assert!(definitions.iter().any(|d| d.name == TOOLBOX_TOOL));
    assert!(list.output.contains("manual_probe:"));
    assert_eq!(
        describe.metadata.as_ref().unwrap()["tool_references"],
        json!(["manual_probe"])
    );
    assert_eq!(run.output, "echo:hello from toolbox");
    assert_eq!(
        run.metadata.as_ref().unwrap()["tool_references"],
        json!(["manual_probe"])
    );

    clear_session_tool_policy(SESSION);
    match previous_home {
        Some(value) => crate::env::set_var("HORON_HOME", value),
        None => crate::env::remove_var("HORON_HOME"),
    }
    match previous_profile {
        Some(value) => crate::env::set_var("HORON_AGENT_TOOL_PROFILE", value),
        None => crate::env::remove_var("HORON_AGENT_TOOL_PROFILE"),
    }
    crate::config::invalidate_config_cache();
}

/// The `goal` tool is registered but outside `CORE_TOOLS`, so under `full` its
/// schema is deferred and the dispatcher is the only way to read it.
#[tokio::test]
async fn toolbox_describe_reaches_the_goal_tool_under_full() {
    struct StubProvider;

    #[async_trait::async_trait]
    impl crate::provider::Provider for StubProvider {
        async fn complete(
            &self,
            _messages: &[crate::message::Message],
            _tools: &[crate::message::ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> anyhow::Result<crate::provider::EventStream> {
            anyhow::bail!("stub provider must never be called")
        }

        fn name(&self) -> &str {
            "stub"
        }

        fn fork(&self) -> Arc<dyn crate::provider::Provider> {
            Arc::new(StubProvider)
        }
    }

    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("temp home");
    let previous_home = std::env::var_os("HORON_HOME");
    let previous_profile = std::env::var_os("HORON_AGENT_TOOL_PROFILE");
    crate::env::set_var("HORON_HOME", home.path());
    crate::env::set_var("HORON_AGENT_TOOL_PROFILE", "full");
    crate::config::invalidate_config_cache();

    let registry = Registry::new(Arc::new(StubProvider) as Arc<dyn crate::provider::Provider>).await;
    set_session_tool_policy(SESSION, None, HashSet::new());
    register_session_load_tiers(SESSION, tiers(&[]));

    let goal_definition = registry
        .definitions(None)
        .await
        .into_iter()
        .find(|definition| definition.name == "goal")
        .expect("the goal tool is registered");
    assert_eq!(goal_definition.name, "goal");
    assert!(
        !crate::config::CORE_TOOLS.contains(&"goal"),
        "goal stays outside the core set"
    );
    assert_eq!(
        tiers(&[]).tier("goal"),
        LoadMode::Discoverable,
        "outside the core set, full resolves goal to the deferred tier"
    );

    let describe = call(&registry, json!({"action": "describe", "tool": "goal"}))
        .await
        .expect("describe reaches the goal tool");
    assert!(describe.output.starts_with("goal\n"), "{}", describe.output);
    assert!(describe.output.contains("Manage durable goals"), "{}", describe.output);
    assert!(
        describe.output.contains("\"checkpoint\""),
        "describe returns the real input schema: {}",
        describe.output
    );
    assert_eq!(describe.metadata.as_ref().unwrap()["tool_references"], json!(["goal"]));

    clear_session_tool_policy(SESSION);
    match previous_home {
        Some(value) => crate::env::set_var("HORON_HOME", value),
        None => crate::env::remove_var("HORON_HOME"),
    }
    match previous_profile {
        Some(value) => crate::env::set_var("HORON_AGENT_TOOL_PROFILE", value),
        None => crate::env::remove_var("HORON_AGENT_TOOL_PROFILE"),
    }
    crate::config::invalidate_config_cache();
}

/// The goal tool is reachable two ways: `full`, and an explicit `enabled`
/// allow-list. The `full` leg is pinned above; this is the allow-list leg, in
/// both directions.
///
/// A tool that survived a restricted allow-list was enabled deliberately, so
/// its resolved tier is `Essential` and its definition reaches the model eager.
/// Without `goal` on the list the tool is absent rather than merely deferred,
/// because `core` is itself an allow-list.
#[tokio::test]
async fn enabled_allow_list_exposes_the_goal_tool() {
    struct StubProvider;

    #[async_trait::async_trait]
    impl crate::provider::Provider for StubProvider {
        async fn complete(
            &self,
            _messages: &[crate::message::Message],
            _tools: &[crate::message::ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> anyhow::Result<crate::provider::EventStream> {
            anyhow::bail!("stub provider must never be called")
        }

        fn name(&self) -> &str {
            "stub"
        }

        fn fork(&self) -> Arc<dyn crate::provider::Provider> {
            Arc::new(StubProvider)
        }
    }

    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("temp home");
    let previous_home = std::env::var_os("HORON_HOME");
    let previous_profile = std::env::var_os("HORON_AGENT_TOOL_PROFILE");
    let previous_enabled = std::env::var_os("HORON_AGENT_TOOLS");
    crate::env::set_var("HORON_HOME", home.path());
    crate::env::remove_var("HORON_AGENT_TOOL_PROFILE");
    crate::env::remove_var("HORON_AGENT_TOOLS");

    let write_config = |config: &str| {
        std::fs::write(home.path().join("config.toml"), config).expect("write config.toml");
        crate::config::invalidate_config_cache();
    };

    write_config("[tools]\nprofile = \"core\"\nenabled = [\"goal\"]\n");
    let tool_config = &crate::config::config().tools;
    let allowed = tool_config
        .selection()
        .allowed_tools
        .expect("an explicit enabled list is an allow-list");
    assert_eq!(
        allowed,
        HashSet::from(["goal".to_string()]),
        "the allow-list is exactly what the config names, so the profile adds nothing"
    );
    assert_eq!(
        tool_config.load_tiers(Some(&allowed), false).tier("goal"),
        LoadMode::Essential,
        "a hand-enabled tool is never deferred: only an explicit full profile is discoverable"
    );

    let provider: Arc<dyn crate::provider::Provider> = Arc::new(StubProvider);
    let agent = crate::agent::Agent::new(provider.clone(), Registry::new(provider).await);
    let definitions = agent.tool_definitions_for_debug().await;
    let goal = definitions
        .iter()
        .find(|definition| definition.name == "goal")
        .expect("the enabled allow-list exposes the goal tool");
    assert!(
        !goal.defer_loading,
        "the goal tool is exposed as essential, not deferred, under an enabled allow-list"
    );
    let exposed: Vec<&str> = definitions.iter().map(|definition| definition.name.as_str()).collect();
    assert!(
        !definitions.iter().any(|definition| definition.name == "bash"),
        "an explicit list replaces the profile's own tools: {exposed:?}"
    );

    write_config("[tools]\nprofile = \"core\"\nenabled = [\"bash\"]\n");
    let provider: Arc<dyn crate::provider::Provider> = Arc::new(StubProvider);
    let agent = crate::agent::Agent::new(provider.clone(), Registry::new(provider).await);
    let names: Vec<String> = agent
        .tool_definitions_for_debug()
        .await
        .into_iter()
        .map(|definition| definition.name)
        .collect();
    assert!(
        names.iter().any(|name| name == "bash"),
        "the listed tool is exposed: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name == "goal"),
        "goal stays absent unless the allow-list names it: {names:?}"
    );

    match previous_home {
        Some(value) => crate::env::set_var("HORON_HOME", value),
        None => crate::env::remove_var("HORON_HOME"),
    }
    match previous_profile {
        Some(value) => crate::env::set_var("HORON_AGENT_TOOL_PROFILE", value),
        None => crate::env::remove_var("HORON_AGENT_TOOL_PROFILE"),
    }
    match previous_enabled {
        Some(value) => crate::env::set_var("HORON_AGENT_TOOLS", value),
        None => crate::env::remove_var("HORON_AGENT_TOOLS"),
    }
    crate::config::invalidate_config_cache();
}

struct CoreStubProvider;

#[async_trait::async_trait]
impl crate::provider::Provider for CoreStubProvider {
    async fn complete(
        &self,
        _messages: &[crate::message::Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<crate::provider::EventStream> {
        anyhow::bail!("stub provider must never be called")
    }

    fn name(&self) -> &str {
        "stub"
    }

    fn fork(&self) -> Arc<dyn crate::provider::Provider> {
        Arc::new(CoreStubProvider)
    }
}

struct CoreHome {
    _home: tempfile::TempDir,
    previous_home: Option<std::ffi::OsString>,
    previous_profile: Option<std::ffi::OsString>,
}

impl CoreHome {
    fn new() -> Self {
        let previous_home = std::env::var_os("HORON_HOME");
        let previous_profile = std::env::var_os("HORON_AGENT_TOOL_PROFILE");
        let home = tempfile::tempdir().expect("temp home");
        crate::env::set_var("HORON_HOME", home.path());
        crate::env::set_var("HORON_AGENT_TOOL_PROFILE", "core");
        crate::config::invalidate_config_cache();
        Self {
            _home: home,
            previous_home,
            previous_profile,
        }
    }
}

impl Drop for CoreHome {
    fn drop(&mut self) {
        clear_session_tool_policy(SESSION);
        match self.previous_home.take() {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
        match self.previous_profile.take() {
            Some(value) => crate::env::set_var("HORON_AGENT_TOOL_PROFILE", value),
            None => crate::env::remove_var("HORON_AGENT_TOOL_PROFILE"),
        }
        crate::config::invalidate_config_cache();
    }
}

async fn core_registry() -> Registry {
    let provider: Arc<dyn crate::provider::Provider> = Arc::new(CoreStubProvider);
    let registry = Registry::new(provider).await;
    // `mcp` is the registry key the wildcard allow-list entry stands for; a
    // bare `Registry::new` has no MCP management tool until a server surface
    // registers one, so this stands in for it.
    registry
        .register(
            "mcp".to_string(),
            Arc::new(EchoTool {
                name: "mcp",
                description: "MCP server management.",
                concurrency: ConcurrencyClass::Concurrent,
                effects: ToolEffects::read(),
            }) as Arc<dyn Tool>,
        )
        .await;
    let config = &crate::config::config().tools;
    let allowed = config.selection().allowed_tools.expect("core is an allow-list");
    let tiers = config.load_tiers(Some(&allowed), true);
    set_session_tool_policy(SESSION, Some(allowed), HashSet::new());
    register_session_load_tiers(SESSION, tiers);
    registry
}

#[tokio::test]
async fn toolbox_list_names_the_listed_core_tools_under_core() {
    let _guard = crate::storage::lock_test_env();
    let _home = CoreHome::new();
    let registry = core_registry().await;

    let output = call(&registry, json!({"action": "list"})).await.expect("list succeeds");
    let mut expected: Vec<&str> = CORE_TOOLS
        .iter()
        .copied()
        .filter(|name| !ESSENTIAL_TOOLS.contains(name))
        .collect();
    expected.sort_unstable();
    assert_eq!(expected.len(), 11, "eighteen core tools minus the seven essential ones");

    let rows: Vec<&str> = output
        .output
        .lines()
        .filter_map(|line| line.strip_prefix("- "))
        .map(|row| row.split(':').next().unwrap_or(row))
        .collect();
    assert_eq!(rows, expected, "{}", output.output);
}

#[tokio::test]
async fn toolbox_run_reaches_undo_under_core() {
    let _guard = crate::storage::lock_test_env();
    let _home = CoreHome::new();
    let registry = core_registry().await;
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("notes.txt");
    std::fs::write(&file, b"original bytes\n").expect("seed file");

    let write_ctx = ToolContext {
        working_dir: Some(dir.path().to_path_buf()),
        ..context()
    };
    registry
        .execute(
            "write",
            json!({"file_path": "notes.txt", "content": "replaced bytes\n"}),
            write_ctx,
        )
        .await
        .expect("write through the direct seam");

    let output = call(&registry, json!({"action": "run", "tool": "undo", "args": {}}))
        .await
        .expect("toolbox reaches undo");
    assert_eq!(std::fs::read(&file).expect("read the file"), b"original bytes\n");
    assert_eq!(
        output.metadata.as_ref().unwrap()["tool_references"],
        json!(["undo"]),
        "a dispatched run names its target"
    );

    let error = call(&registry, json!({"action": "run", "tool": "undo", "args": {}}))
        .await
        .expect_err("nothing is left to undo");
    assert!(
        error.to_string().contains("Nothing to undo"),
        "the target's own error surfaces unchanged: {error}"
    );
}

/// Puts one session into plan mode for a test and returns it to `Off`.
struct PlanModeGuard {
    session: String,
}

impl PlanModeGuard {
    fn enter(session: &str) -> Self {
        crate::plan_mode::set_state(session, crate::plan_mode::PlanState::Planning);
        Self {
            session: session.to_string(),
        }
    }
}

impl Drop for PlanModeGuard {
    fn drop(&mut self) {
        crate::plan_mode::clear_state(&self.session);
    }
}

/// Plan mode is read-only, and the dispatcher's reach is its target's reach:
/// `list`/`describe` only read, a read-only target still runs, and a
/// write-shaped target is refused by its own effects. The refusal a model sees
/// through the dispatcher is the refusal a direct call would have produced.
#[tokio::test]
async fn toolbox_in_plan_mode_is_refused_only_through_its_target() {
    let _guard = crate::storage::lock_test_env();
    let _policy = PolicyGuard::install(tiers(&[]));
    let _plan = PlanModeGuard::enter(SESSION);
    let registry = registry().await;

    let list = call(&registry, json!({"action": "list"}))
        .await
        .expect("list only reads what the registry holds");
    assert!(list.output.contains("readonly_probe"), "{}", list.output);

    let describe = call(&registry, json!({"action": "describe", "tool": "readonly_probe"}))
        .await
        .expect("describe only reads a schema");
    assert!(describe.output.contains("readonly_probe"), "{}", describe.output);

    let read_only = call(
        &registry,
        json!({"action": "run", "tool": "readonly_probe", "args": {"text": "ok"}}),
    )
    .await
    .expect("a read-only target is allowed while planning");
    assert_eq!(read_only.output, "echo:ok");

    let direct = registry
        .execute("write_probe", json!({"text": "blocked"}), context())
        .await
        .expect_err("plan mode refuses a direct write");
    let through = call(
        &registry,
        json!({"action": "run", "tool": "write_probe", "args": {"text": "blocked"}}),
    )
    .await
    .expect_err("the same write reached through the dispatcher is refused too");
    assert_eq!(
        through.to_string(),
        direct.to_string(),
        "the dispatcher hands the model the target's own refusal"
    );
    assert!(through.to_string().contains("write_probe"), "{through}");
}

/// The dispatch gate that runs before a round must not refuse the dispatcher in
/// plan mode either, while a direct write on the same session is still refused
/// there by its own effects.
#[tokio::test]
async fn toolbox_passes_the_dispatch_gate_in_plan_mode_while_a_write_does_not() {
    let _guard = crate::storage::lock_test_env();
    let _home = CoreHome::new();
    let provider: Arc<dyn crate::provider::Provider> = Arc::new(CoreStubProvider);
    let agent = crate::agent::Agent::new(provider.clone(), Registry::new(provider).await);
    let _plan = PlanModeGuard::enter(agent.session_id());

    agent
        .validate_tool_allowed_for_test(TOOLBOX_TOOL)
        .expect("the dispatcher itself is never refused: plan mode judges its target");
    let refusal = agent
        .validate_tool_allowed_for_test("write")
        .expect_err("a direct write is still refused while planning");
    assert!(refusal.to_string().contains("plan mode"), "{refusal}");
}

/// `toolbox run` fires the `pre_tool` gate once, for the tool it runs: the hook
/// sees that tool's name and payload, never the dispatcher's.
#[cfg(unix)]
#[tokio::test]
async fn toolbox_run_fires_the_pre_tool_hook_once_for_the_target() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = crate::storage::lock_test_env();
    let _home = CoreHome::new();
    let dir = tempfile::tempdir().expect("temp dir");
    let log = dir.path().join("pre-tool.log");
    let script = dir.path().join("gate.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             cat >/dev/null\n\
             printf '%s|%s\\n' \"$HORON_AGENT_HOOK_SESSION_ID\" \"$HORON_AGENT_HOOK_TOOL_NAME\" >> \"{}\"\n\
             exit 0\n",
            log.display()
        ),
    )
    .expect("write hook script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let _hooks = HookEnvGuard::install("HORON_AGENT_HOOK_PRE_TOOL", script.display().to_string());

    let registry = core_registry().await;
    // `undo` answers its own "nothing to undo" here; the gate runs before the
    // tool either way, which is the invocation this test counts.
    let _ = call(&registry, json!({"action": "run", "tool": "undo", "args": {}})).await;

    let recorded = std::fs::read_to_string(&log).expect("the gate recorded its invocations");
    // The hook configuration is process-wide, so a call another running test
    // makes while this script is installed lands in the same log. Count only
    // this session's runs: exactly one, named after the target tool.
    let this_session: Vec<&str> = recorded
        .lines()
        .filter_map(|line| line.split_once('|'))
        .filter(|(session, _)| *session == SESSION)
        .map(|(_, tool)| tool)
        .collect();
    assert_eq!(
        this_session,
        ["undo"],
        "exactly one gate run, named after the tool that ran and not the dispatcher"
    );
}

/// A tool the session cannot call is refused through the dispatcher with the
/// same wording a direct call uses, for both `describe` and `run`. The
/// dispatcher itself is still held to the allow-list: a list that omits
/// `toolbox` cannot reach anything at all.
#[tokio::test]
async fn toolbox_run_refuses_a_tool_outside_the_session_allow_list() {
    let _guard = crate::storage::lock_test_env();
    set_session_tool_policy(
        SESSION,
        Some(HashSet::from(["readonly_probe".to_string(), TOOLBOX_TOOL.to_string()])),
        HashSet::new(),
    );
    let _policy = PolicyGuard;
    let registry = registry().await;

    for input in [
        json!({"action": "run", "tool": "write_probe", "args": {"text": "no"}}),
        json!({"action": "describe", "tool": "write_probe"}),
    ] {
        let error = call(&registry, input.clone())
            .await
            .expect_err("a tool outside the allow-list is unreachable")
            .to_string();
        assert!(error.contains("not allowed"), "{error}");
        assert!(error.contains("write_probe"), "{error}");
    }

    let allowed = call(
        &registry,
        json!({"action": "run", "tool": "readonly_probe", "args": {"text": "fine"}}),
    )
    .await
    .expect("a tool the session may call still runs");
    assert_eq!(allowed.output, "echo:fine");

    set_session_tool_policy(
        SESSION,
        Some(HashSet::from(["readonly_probe".to_string()])),
        HashSet::new(),
    );
    let error = call(&registry, json!({"action": "list"}))
        .await
        .expect_err("the dispatcher is held to the allow-list too")
        .to_string();
    assert!(error.contains("not allowed"), "{error}");
    assert!(error.contains(TOOLBOX_TOOL), "{error}");
}

/// Only `run` reaches another tool. A call that dispatches nothing, and one this
/// build does not know, must not look read-only to a planning session; a `run`
/// carries the target's own effects for the arguments it was handed.
#[tokio::test]
async fn effects_for_input_answers_only_run_through_the_target() {
    let empty = Registry::empty();
    let tool = toolbox::ToolboxTool::new(empty.downgrade());
    assert_eq!(
        tool.effects_for_input(&json!({"action": "list"})),
        ToolEffects::read(),
        "list only reads"
    );
    assert_eq!(
        tool.effects_for_input(&json!({"action": "describe", "tool": "readonly_probe"})),
        ToolEffects::read(),
        "describe only reads"
    );
    assert_eq!(
        tool.effects_for_input(&json!({})),
        ToolEffects::write(),
        "a missing action dispatches nothing, so it is the barrier"
    );
    assert_eq!(
        tool.effects_for_input(&json!({"action": "promote", "tool": "readonly_probe"})),
        ToolEffects::write(),
        "an unknown action is the barrier"
    );
    assert_eq!(
        tool.effects_for_input(&json!({"action": "run", "tool": "readonly_probe", "args": {"text": "x"}})),
        ToolEffects::write(),
        "a target this registry does not hold is the barrier"
    );

    let registry = registry().await;
    let tool = toolbox::ToolboxTool::new(registry.downgrade());
    assert_eq!(
        tool.effects_for_input(&json!({"action": "run", "tool": "readonly_probe", "args": {"text": "x"}})),
        ToolEffects::read(),
        "a run carries the target's own effects"
    );
    assert_eq!(
        tool.effects_for_input(&json!({"action": "run", "tool": "write_probe", "args": {"text": "x"}})),
        ToolEffects::write(),
        "a write-shaped target stays a barrier"
    );
    assert_eq!(
        tool.effects_for_input(&json!({"action": "run", "tool": "readonly_probe"})),
        ToolEffects::write(),
        "a run without args reaches nothing this hook can classify"
    );
}
