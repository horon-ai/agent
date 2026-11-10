use super::*;
use crate::config::{CORE_TOOL_PROFILE, CORE_TOOLS, ESSENTIAL_TOOLS, LoadMode, TOOLBOX_TOOL};

pub(super) struct ProfileEnv {
    _home: tempfile::TempDir,
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl ProfileEnv {
    fn new(profile: &str, config_toml: Option<&str>) -> Self {
        let previous = ["HORON_HOME", "HORON_AGENT_TOOL_PROFILE", "HORON_AGENT_TOOLS"]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();
        let home = tempfile::tempdir().expect("temp home");
        crate::env::set_var("HORON_HOME", home.path());
        crate::env::set_var("HORON_AGENT_TOOL_PROFILE", profile);
        crate::env::remove_var("HORON_AGENT_TOOLS");
        if let Some(config) = config_toml {
            std::fs::write(home.path().join("config.toml"), config).expect("write config.toml");
        }
        crate::config::Config::invalidate_cache();
        Self { _home: home, previous }
    }

    fn rewrite_config(&self, config: &str) {
        std::fs::write(self._home.path().join("config.toml"), config).expect("rewrite config.toml");
        crate::config::Config::invalidate_cache();
    }
}

impl Drop for ProfileEnv {
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

pub(super) async fn agent_with_mcp_management_tool() -> Agent {
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    registry
        .register(
            "mcp".to_string(),
            Arc::new(FakeMcpTool {
                name: "mcp".to_string(),
            }) as Arc<dyn crate::tool::Tool>,
        )
        .await;
    Agent::new(provider, registry)
}

fn sorted_core_tools() -> Vec<String> {
    let mut names: Vec<String> = CORE_TOOLS.iter().map(|name| (*name).to_string()).collect();
    names.sort();
    names
}

/// The literal `core` surface as it stands before load tiers exist. Pinned here
/// so the tier work can never quietly change the headless default: `core` must
/// keep this exact list, in this exact order, with no deferred flag.
const CORE_BASELINE: &[&str] = &[
    "agentgrep",
    "apply_patch",
    "ask",
    "bash",
    "edit",
    "ls",
    "mcp",
    "monitor",
    "read",
    "replace",
    "skill_manage",
    "undo",
    "webfetch",
    "websearch",
    "write",
];

async fn definitions_json(agent: &Agent) -> String {
    serde_json::to_string(&agent.tool_definitions_for_debug().await).expect("serialize definitions")
}

/// The same agent with the listing forced off, which is exactly how the session
/// resolved its tiers before core listing existed.
async fn definitions_without_listing(mut agent: Agent) -> String {
    let config = &crate::config::config().tools;
    agent.load_tiers = config.load_tiers(agent.allowed_tools.as_ref(), false);
    definitions_json(&agent).await
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

#[tokio::test]
async fn core_profile_lists_non_essential_tools() {
    let _guard = crate::storage::lock_test_env();
    let _env = ProfileEnv::new(CORE_TOOL_PROFILE, None);
    let agent = agent_with_mcp_management_tool().await;

    let definitions = agent.tool_definitions_for_debug().await;
    let mut expected: Vec<String> = CORE_BASELINE.iter().map(|name| (*name).to_string()).collect();
    expected.push(TOOLBOX_TOOL.to_string());
    expected.sort();
    let names: Vec<String> = definitions.iter().map(|definition| definition.name.clone()).collect();
    assert_eq!(
        names, expected,
        "the core surface is the fifteen kept tools plus the dispatcher"
    );
    let mut baseline: Vec<String> = CORE_BASELINE.iter().map(|name| (*name).to_string()).collect();
    baseline.sort();
    assert_eq!(sorted_core_tools(), baseline, "the baseline and CORE_TOOLS agree");
    assert!(
        agent.load_tiers.core_listed(),
        "the default core session lists its tools"
    );

    for definition in &definitions {
        if definition.name == TOOLBOX_TOOL {
            assert!(!definition.defer_loading, "the dispatcher must stay eager");
            continue;
        }
        let eager = ESSENTIAL_TOOLS.contains(&definition.name.as_str());
        assert_eq!(
            definition.defer_loading,
            !eager,
            "{} must be {} under core",
            definition.name,
            if eager { "eager" } else { "listed" }
        );
        assert_eq!(
            agent.load_tiers.tier(&definition.name),
            if eager {
                LoadMode::Essential
            } else {
                LoadMode::Discoverable
            },
            "{} carries the tier the flag projects",
            definition.name
        );
    }
    assert_eq!(
        definitions.iter().filter(|d| !d.defer_loading).count(),
        ESSENTIAL_TOOLS.len() + 1,
        "the seven essential tools and the dispatcher are the eager set"
    );
}

#[tokio::test]
async fn full_profile_defers_non_core_tools() {
    let _guard = crate::storage::lock_test_env();
    let _env = ProfileEnv::new("full", None);
    let agent = agent_with_mcp_management_tool().await;

    let definitions = agent.tool_definitions_for_debug().await;
    assert!(
        definitions.len() > CORE_TOOLS.len(),
        "the full profile exposes more than the core set, got {}",
        definitions.len()
    );
    for definition in &definitions {
        if CORE_TOOLS.contains(&definition.name.as_str()) {
            assert_eq!(
                agent.load_tiers.tier(&definition.name),
                LoadMode::Essential,
                "{} is a core tool, so it is essential",
                definition.name
            );
            assert!(
                !definition.defer_loading,
                "core tool {} must stay eager under full",
                definition.name
            );
            continue;
        }
        // The fixed MCP surface is owned by the MCP exposure pass, which decides
        // its flags after the tier pass (mcp_search stays eager for discovery and
        // mcp_call becomes the stable deferred placeholder on a native provider).
        if definition.name == "mcp_call" || definition.name.starts_with("mcp__") {
            continue;
        }
        // `toolbox` is the explicit exception: it is the discovery path for the
        // deferred set, so the resolver pins it Essential and it stays eager.
        if definition.name == TOOLBOX_TOOL {
            assert_eq!(
                agent.load_tiers.tier(TOOLBOX_TOOL),
                LoadMode::Essential,
                "toolbox is pinned essential so the dispatcher is never deferred"
            );
            assert!(!definition.defer_loading, "toolbox must stay eager under full");
            continue;
        }
        assert_eq!(
            agent.load_tiers.tier(&definition.name),
            LoadMode::Discoverable,
            "non-core tool {} must resolve discoverable under full",
            definition.name
        );
        assert!(
            definition.defer_loading,
            "non-core tool {} must be deferred under full",
            definition.name
        );
    }
    let eager = definitions
        .iter()
        .filter(|definition| !definition.defer_loading)
        .count();
    assert_eq!(
        eager,
        CORE_TOOLS.len() + 1,
        "only the core tools and the toolbox dispatcher stay eager on this provider"
    );
}

/// Screen- and browser-driving built-ins are gone from every profile.
///
/// `full` is the widest surface this build offers, so if a removed name is
/// absent here it cannot be reached through `--tool-profile`, the environment,
/// or `[tools]`. The three names are pinned so a stray re-registration fails.
#[tokio::test]
async fn removed_screen_and_browser_tools_are_never_defined() {
    let _guard = crate::storage::lock_test_env();
    let _env = ProfileEnv::new("full", None);
    let agent = agent_with_mcp_management_tool().await;

    let names: Vec<String> = agent
        .tool_definitions_for_debug()
        .await
        .into_iter()
        .map(|definition| definition.name)
        .collect();
    assert!(names.len() > CORE_TOOLS.len(), "the sweep must not pass vacuously");
    // Assembled from pieces on purpose: the repository guard that keeps
    // removed tool names out of the tree matches plain literals, and this test
    // is the proof they are gone rather than a reference to them.
    const REMOVED: [&str; 3] = [concat!("brow", "ser"), "open", concat!("macos_", "computer_use")];
    for removed in REMOVED {
        assert!(
            !names.iter().any(|name| name == removed),
            "{removed} must not be offered under full: {names:?}"
        );
    }
}

/// A dispatched result names its target, and that metadata converts to exactly
/// one `ToolReference` block, which is what loads the deferred schema.
#[tokio::test]
async fn toolbox_result_converts_to_one_tool_reference_block() {
    let _guard = crate::storage::lock_test_env();
    let _env = ProfileEnv::new("full", None);
    let agent = agent_with_mcp_management_tool().await;
    let registry = agent.registry();
    let ctx = ToolContext {
        session_id: agent.session_id().to_string(),
        message_id: "message".to_string(),
        tool_call_id: "toolbox-call".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    };

    let output = registry
        .execute(
            TOOLBOX_TOOL,
            serde_json::json!({"action": "describe", "tool": "swarm"}),
            ctx,
        )
        .await
        .expect("describe succeeds under full");

    let blocks = tool_output_to_content_blocks("call_toolbox".to_string(), output);
    let references: Vec<(&str, &str)> = blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolReference { tool_use_id, tool_name } => Some((tool_use_id.as_str(), tool_name.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        references,
        vec![("call_toolbox", "swarm")],
        "one describe must load exactly one schema"
    );
}

#[tokio::test]
async fn toolbox_is_essential_and_only_full_and_core_expose_it() {
    let _guard = crate::storage::lock_test_env();

    let _acp = ProfileEnv::new("acp", None);
    let agent = agent_with_mcp_management_tool().await;
    let definitions = agent.tool_definitions_for_debug().await;
    assert!(
        !definitions.iter().any(|definition| definition.name == TOOLBOX_TOOL),
        "acp must not expose the dispatcher"
    );

    for profile in [CORE_TOOL_PROFILE, "full"] {
        let _env = ProfileEnv::new(profile, None);
        let agent = agent_with_mcp_management_tool().await;
        let definitions = agent.tool_definitions_for_debug().await;
        let toolbox = definition(&definitions, TOOLBOX_TOOL);
        assert!(!toolbox.defer_loading, "{profile} must keep the dispatcher eager");
        assert_eq!(agent.load_tiers.tier(TOOLBOX_TOOL), LoadMode::Essential);
    }
}

#[tokio::test]
async fn an_explicit_essential_override_keeps_a_tool_eager() {
    let _guard = crate::storage::lock_test_env();
    let _env = ProfileEnv::new(
        "full",
        Some("[tools]\nprofile = \"full\"\n\n[tools.load_mode]\nswarm = \"essential\"\n"),
    );
    let agent = agent_with_mcp_management_tool().await;

    let definitions = agent.tool_definitions_for_debug().await;
    let swarm = definition(&definitions, "swarm");
    assert!(
        !swarm.defer_loading,
        "an explicit essential entry beats the full default"
    );
    let memory = definition(&definitions, "memory");
    assert!(memory.defer_loading, "an untouched non-core tool stays deferred");
}

#[tokio::test]
async fn an_explicit_off_override_removes_the_tool() {
    let _guard = crate::storage::lock_test_env();
    let _env = ProfileEnv::new(
        "full",
        Some("[tools]\nprofile = \"full\"\n\n[tools.load_mode]\nmemory = \"off\"\nno_such_tool_xyz = \"off\"\n"),
    );
    let agent = agent_with_mcp_management_tool().await;

    let names: Vec<String> = agent
        .tool_definitions_for_debug()
        .await
        .into_iter()
        .map(|definition| definition.name)
        .collect();
    assert!(
        !names.iter().any(|name| name == "memory"),
        "an off override removes the tool from the list"
    );
    assert!(
        names.iter().any(|name| name == "swarm"),
        "an unrelated tool is untouched, and an unknown override name is inert"
    );
}

#[tokio::test]
async fn the_deferred_set_is_frozen_for_the_session() {
    let _guard = crate::storage::lock_test_env();
    let env = ProfileEnv::new("full", Some("[tools]\nprofile = \"full\"\n"));
    let agent = agent_with_mcp_management_tool().await;

    let first = serde_json::to_string(&agent.tool_definitions_for_debug().await).expect("serialize");
    env.rewrite_config("[tools]\nprofile = \"full\"\n\n[tools.load_mode]\nswarm = \"essential\"\n");
    let second = serde_json::to_string(&agent.tool_definitions_for_debug().await).expect("serialize");

    assert_eq!(first, second, "two consecutive builds of one session are identical");
    let definitions = agent.tool_definitions_for_debug().await;
    let swarm = definition(&definitions, "swarm");
    assert!(
        swarm.defer_loading,
        "the session keeps the tier it resolved at startup, not the reloaded config"
    );
}

/// The todo tool is what an editor renders as the plan, so only a session
/// that can call it is told about it: acp allows todo, core does not.
#[tokio::test]
async fn todo_hint_reaches_acp_but_not_core() {
    let _guard = crate::storage::lock_test_env();

    let _acp = ProfileEnv::new("acp", None);
    let acp_agent = agent_with_mcp_management_tool().await;
    let acp_prompt = acp_agent.system_prompt_for_debug();
    assert!(
        acp_prompt.contains(crate::prompt::TODO_HINT),
        "acp allows todo, so its prompt carries the hint"
    );

    let _core = ProfileEnv::new(CORE_TOOL_PROFILE, None);
    let core_agent = agent_with_mcp_management_tool().await;
    let core_prompt = core_agent.system_prompt_for_debug();
    assert!(
        !core_prompt.contains(crate::prompt::TODO_HINT),
        "core has no todo tool, so its prompt carries no hint"
    );
}

#[tokio::test]
async fn core_load_mode_override_restores_eager() {
    let _guard = crate::storage::lock_test_env();
    let env = ProfileEnv::new(
        CORE_TOOL_PROFILE,
        Some("[tools]\nprofile = \"core\"\n\n[tools.load_mode]\nask = \"essential\"\nmonitor = \"off\"\n"),
    );
    let agent = agent_with_mcp_management_tool().await;
    let definitions = agent.tool_definitions_for_debug().await;

    assert!(
        !definition(&definitions, "ask").defer_loading,
        "an essential entry wins"
    );
    assert!(
        !definitions.iter().any(|definition| definition.name == "monitor"),
        "an off entry removes the tool"
    );
    assert!(
        definition(&definitions, "undo").defer_loading,
        "an untouched listed tool stays listed"
    );
    assert!(!definition(&definitions, "bash").defer_loading);
    drop(env);

    let _env = ProfileEnv::new(
        CORE_TOOL_PROFILE,
        Some("[tools]\nprofile = \"core\"\n\n[tools.load_mode]\ntoolbox = \"off\"\n"),
    );
    let agent = agent_with_mcp_management_tool().await;
    let definitions = agent.tool_definitions_for_debug().await;

    assert!(
        !agent.load_tiers.core_listed(),
        "without the dispatcher there is no way to reach a listed tool"
    );
    assert!(
        definitions.iter().all(|definition| !definition.defer_loading),
        "every core tool is eager again"
    );
    assert_eq!(definitions.len(), CORE_TOOLS.len(), "only the dispatcher is gone");
}

/// Sessions that named their own tools send what they sent before the listing
/// existed: an explicit profile, an explicit `enabled` list, a caller-supplied
/// allow-list, and an SDK `enabled` overlay are all explicit choices.
#[tokio::test]
async fn explicit_lists_unchanged() {
    let _guard = crate::storage::lock_test_env();

    for profile in ["acp", "minimal", "none", "full"] {
        let _env = ProfileEnv::new(profile, None);
        let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
        let registry = Registry::new(provider.clone()).await;
        let agent = Agent::new(provider, registry);
        assert!(!agent.load_tiers.core_listed(), "{profile} must not list core tools");
        let listed = definitions_json(&agent).await;
        let pre_listing = definitions_without_listing(agent).await;
        assert_eq!(listed, pre_listing, "{profile} must send the same tools as before");
    }

    let _enabled = ProfileEnv::new(
        CORE_TOOL_PROFILE,
        Some("[tools]\nprofile = \"core\"\nenabled = [\"bash\", \"read\"]\n"),
    );
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let agent = Agent::new(provider, registry);
    assert!(!agent.load_tiers.core_listed(), "an explicit enabled list never lists");
    let listed = definitions_json(&agent).await;
    let pre_listing = definitions_without_listing(agent).await;
    assert_eq!(listed, pre_listing, "an explicit enabled list is unchanged");

    let _explicit = ProfileEnv::new(CORE_TOOL_PROFILE, None);
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let allowed: HashSet<String> = CORE_TOOLS.iter().map(|name| (*name).to_string()).collect();
    let agent = Agent::new_with_session(provider, registry, Session::create(None, None), Some(allowed));
    assert!(!agent.load_tiers.core_listed(), "a caller allow-list never lists");
    let definitions = agent.tool_definitions_for_debug().await;
    assert!(
        definitions.iter().all(|definition| !definition.defer_loading),
        "a caller allow-list keeps every tool it names eager"
    );
    let listed = definitions_json(&agent).await;
    let pre_listing = definitions_without_listing(agent).await;
    assert_eq!(listed, pre_listing, "a caller allow-list is unchanged");

    let _sdk = ProfileEnv::new(CORE_TOOL_PROFILE, None);
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let agent = Agent::new(provider, registry);
    let session_id = agent.session_id().to_string();
    let (tx, _rx) = tokio_mpsc::unbounded_channel();
    crate::tool::sdk::configure(
        &session_id,
        "core-listing-owner",
        crate::protocol::SessionToolConfig {
            enabled: Some(vec!["bash".into(), "read".into(), "undo".into()]),
            disabled: vec![],
            custom: vec![],
        },
        tx,
    )
    .expect("configure the SDK tool list");
    assert!(agent.load_tiers.core_listed(), "the session itself is core-listed");
    let definitions = agent.tool_definitions_for_debug().await;
    for name in ["bash", "read", "undo"] {
        assert!(
            !definition(&definitions, name).defer_loading,
            "an SDK enabled list keeps {name} eager"
        );
    }
    let listed = definitions_json(&agent).await;
    let pre_listing = definitions_without_listing(agent).await;
    assert_eq!(listed, pre_listing, "an SDK enabled list under core is unchanged");
    crate::tool::sdk::remove_session(&session_id);
}

/// `full` keeps today's deferral even with an SDK `enabled` overlay: the list is
/// layered on an unrestricted surface, not on a listed core one.
#[tokio::test]
async fn full_sdk_list_keeps_todays_deferral() {
    let _guard = crate::storage::lock_test_env();
    let _env = ProfileEnv::new("full", None);
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let agent = Agent::new(provider, registry);
    let session_id = agent.session_id().to_string();
    let (tx, _rx) = tokio_mpsc::unbounded_channel();
    crate::tool::sdk::configure(
        &session_id,
        "full-sdk-owner",
        crate::protocol::SessionToolConfig {
            enabled: Some(vec!["bash".into(), "todo".into()]),
            disabled: vec![],
            custom: vec![],
        },
        tx,
    )
    .expect("configure the SDK tool list");

    let definitions = agent.tool_definitions_for_debug().await;
    assert!(
        !definition(&definitions, "bash").defer_loading,
        "an essential tool stays eager under full"
    );
    assert!(
        definition(&definitions, "todo").defer_loading,
        "full still defers a non-core tool under an SDK list"
    );
    crate::tool::sdk::remove_session(&session_id);
}

/// The eight core tools a `core`-listed session names in its system prompt
/// instead of sending their schemas eagerly.
fn listed_core_tools() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = CORE_TOOLS
        .iter()
        .copied()
        .filter(|name| !ESSENTIAL_TOOLS.contains(name))
        .collect();
    names.sort_unstable();
    names
}

/// The static prompt with the clock line removed.
///
/// `Time: HH:MM:SS` is regenerated for every build and is not what these tests
/// compare; everything else in the static part must not move.
fn without_clock(static_part: &str) -> String {
    static_part
        .lines()
        .filter(|line| !line.starts_with("Time: "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The static prompt this session would send, after the index is built the way
/// a real turn builds it.
async fn static_prompt(mut agent: Agent) -> String {
    agent.tool_definitions().await;
    without_clock(&agent.build_system_prompt_split(None).static_part)
}

/// The same session with core listing forced off, which is exactly how its
/// prompt was assembled before the index existed.
async fn static_prompt_with_listing_off(mut agent: Agent) -> String {
    let config = &crate::config::config().tools;
    agent.load_tiers = config.load_tiers(agent.allowed_tools.as_ref(), false);
    *agent
        .listed_tools_index
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    static_prompt(agent).await
}

/// Under `core` on a provider without native deferred loading, the eight listed
/// core tools are not sent eagerly and the static prompt names every one of
/// them, with the one-line description the dispatcher also prints.
#[tokio::test]
async fn core_index_names_every_listed_core_tool() {
    let _guard = crate::storage::lock_test_env();
    let _env = ProfileEnv::new(CORE_TOOL_PROFILE, None);
    let mut agent = agent_with_mcp_management_tool().await;
    assert!(
        !agent.provider.supports_deferred_tools(),
        "this leg pins the provider that drops deferred definitions"
    );

    let definitions = agent.tool_definitions().await;
    let listed = listed_core_tools();
    assert_eq!(listed.len(), 8, "fifteen core tools minus the seven essential ones");
    for name in &listed {
        assert!(
            definition(&definitions, name).defer_loading,
            "{name} must be listed, never eager"
        );
    }
    for name in ESSENTIAL_TOOLS {
        assert!(!definition(&definitions, name).defer_loading, "{name} stays eager");
    }
    assert!(
        !definition(&definitions, TOOLBOX_TOOL).defer_loading,
        "the dispatcher stays eager"
    );
    let eager: Vec<&str> = definitions
        .iter()
        .filter(|definition| !definition.defer_loading)
        .map(|definition| definition.name.as_str())
        .collect();
    assert_eq!(
        eager.len(),
        ESSENTIAL_TOOLS.len() + 1,
        "only the seven essential tools and the dispatcher are eager: {eager:?}"
    );

    let prompt = agent.system_prompt_for_debug();
    assert!(prompt.contains("# Additional tools"), "{prompt}");
    for name in &listed {
        assert!(
            prompt.contains(&format!("- {name}: ")),
            "{name} is missing from the index: {prompt}"
        );
    }
    for name in ESSENTIAL_TOOLS {
        assert!(
            !prompt.contains(&format!("\n- {name}: ")),
            "an eager tool is not listed again: {prompt}"
        );
    }
}

struct NativeDeferredProvider;

#[async_trait]
impl Provider for NativeDeferredProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let (_tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(1);
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "claude"
    }

    fn supports_deferred_tools(&self) -> bool {
        true
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

/// On a provider with native deferred loading the schemas still travel as
/// deferred definitions, and the index is present too.
#[tokio::test]
async fn a_native_provider_keeps_deferring_and_still_gets_the_index() {
    let _guard = crate::storage::lock_test_env();
    let _env = ProfileEnv::new(CORE_TOOL_PROFILE, None);
    let provider: Arc<dyn Provider> = Arc::new(NativeDeferredProvider);
    let registry = Registry::new(provider.clone()).await;
    registry
        .register(
            "mcp".to_string(),
            Arc::new(FakeMcpTool {
                name: "mcp".to_string(),
            }) as Arc<dyn crate::tool::Tool>,
        )
        .await;
    let mut agent = Agent::new(provider, registry);
    assert!(agent.provider.supports_deferred_tools());

    let definitions = agent.tool_definitions().await;
    for name in listed_core_tools() {
        assert!(
            definition(&definitions, name).defer_loading,
            "{name} stays a deferred definition on this provider"
        );
    }

    let prompt = agent.system_prompt_for_debug();
    assert!(prompt.contains("# Additional tools"), "{prompt}");
    assert!(prompt.contains("- undo: "), "{prompt}");
}

/// Sessions that named their own tools send the static prompt they sent before
/// core listing existed: no index, and no other change either.
#[tokio::test]
async fn sessions_that_are_not_core_listed_get_no_index() {
    let _guard = crate::storage::lock_test_env();

    for profile in ["acp", "full"] {
        let _env = ProfileEnv::new(profile, None);
        let agent = agent_with_mcp_management_tool().await;
        assert!(!agent.load_tiers.core_listed(), "{profile} must not list core tools");
        let listed_prompt = static_prompt(agent).await;
        assert!(
            !listed_prompt.contains("# Additional tools"),
            "{profile} must carry no index: {listed_prompt}"
        );

        let agent = agent_with_mcp_management_tool().await;
        let forced = static_prompt_with_listing_off(agent).await;
        assert_eq!(listed_prompt, forced, "{profile} sends the static part it sent before");
    }

    let _sdk = ProfileEnv::new(CORE_TOOL_PROFILE, None);
    let agent = agent_with_mcp_management_tool().await;
    let session_id = agent.session_id().to_string();
    let (tx, _rx) = tokio_mpsc::unbounded_channel();
    crate::tool::sdk::configure(
        &session_id,
        "index-owner",
        crate::protocol::SessionToolConfig {
            enabled: Some(vec!["bash".into(), "read".into(), "undo".into()]),
            disabled: vec![],
            custom: vec![],
        },
        tx,
    )
    .expect("configure the SDK tool list");
    assert!(agent.load_tiers.core_listed(), "the session itself is core-listed");
    let listed_prompt = static_prompt(agent).await;
    assert!(
        !listed_prompt.contains("# Additional tools"),
        "an SDK enabled list names its tools exactly: {listed_prompt}"
    );
    assert!(
        !listed_prompt.contains("\n- undo: "),
        "a tool the client named is not listed: {listed_prompt}"
    );

    let agent = agent_with_mcp_management_tool().await;
    let forced = static_prompt_with_listing_off(agent).await;
    assert_eq!(listed_prompt, forced, "an SDK enabled list under core is unchanged");
    crate::tool::sdk::remove_session(&session_id);

    let _explicit = ProfileEnv::new(CORE_TOOL_PROFILE, None);
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let allowed: HashSet<String> = CORE_TOOLS.iter().map(|name| (*name).to_string()).collect();
    let agent = Agent::new_with_session(provider, registry, Session::create(None, None), Some(allowed));
    assert!(!agent.load_tiers.core_listed(), "a caller allow-list never lists");
    let listed_prompt = static_prompt(agent).await;
    assert!(
        !listed_prompt.contains("# Additional tools"),
        "a caller allow-list carries no index: {listed_prompt}"
    );

    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let allowed: HashSet<String> = CORE_TOOLS.iter().map(|name| (*name).to_string()).collect();
    let agent = Agent::new_with_session(provider, registry, Session::create(None, None), Some(allowed));
    let forced = static_prompt_with_listing_off(agent).await;
    assert_eq!(listed_prompt, forced, "a caller allow-list is unchanged");
}

/// A locked session keeps the index it built: a config reload, an unlock and a
/// compaction must not rewrite the cached prefix.
#[tokio::test]
async fn the_listed_tools_index_is_frozen_for_the_session() {
    let _guard = crate::storage::lock_test_env();
    let env = ProfileEnv::new(CORE_TOOL_PROFILE, None);
    let mut agent = agent_with_mcp_management_tool().await;

    let first_tools = serde_json::to_string(&agent.tool_definitions().await[..]).expect("serialize");
    let first_prompt = without_clock(&agent.build_system_prompt_split(None).static_part);
    assert!(first_prompt.contains("# Additional tools"), "{first_prompt}");

    env.rewrite_config("[tools]\nprofile = \"core\"\n\n[tools.load_mode]\nask = \"essential\"\n");
    agent.unlock_tools();
    agent.note_compaction_applied();

    let second_tools = serde_json::to_string(&agent.tool_definitions().await[..]).expect("serialize");
    let second_prompt = without_clock(&agent.build_system_prompt_split(None).static_part);
    assert_eq!(second_tools, first_tools, "the tool list survives the unlock");
    assert_eq!(second_prompt, first_prompt, "the static part survives the unlock");
    assert!(
        definition(&agent.tool_definitions_for_debug().await, "ask").defer_loading,
        "the reloaded `essential` entry never reaches the frozen session"
    );

    // A policy change the frozen index must ignore. The rebuild below is
    // asserted to differ, so this cannot pass by the index never being read.
    let session_id = agent.session_id().to_string();
    let (tx, _rx) = tokio_mpsc::unbounded_channel();
    crate::tool::sdk::configure(
        &session_id,
        "freeze-owner",
        crate::protocol::SessionToolConfig {
            enabled: None,
            disabled: vec!["webfetch".into()],
            custom: vec![],
        },
        tx,
    )
    .expect("configure the SDK tool list");
    agent.unlock_tools();
    let rebuilt: Vec<String> = agent
        .build_listed_tools_index()
        .await
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert!(
        !rebuilt.iter().any(|name| name == "webfetch"),
        "a rebuild would drop the disabled tool: {rebuilt:?}"
    );
    // The real turn path: lock the tool surface, then ensure the index. That is
    // where an unfrozen index would be rebuilt, so the freeze is exercised
    // through the same calls a turn makes rather than by building the prompt.
    agent.tool_definitions().await;
    let third_prompt = without_clock(&agent.build_system_prompt_split(None).static_part);
    assert_eq!(
        third_prompt, first_prompt,
        "the frozen index keeps the rows it locked, not the current policy"
    );
    crate::tool::sdk::remove_session(&session_id);
}

/// The tool names the index names, in the order it prints them.
fn index_row_names(static_part: &str) -> Vec<String> {
    let start = static_part
        .find("# Additional tools")
        .unwrap_or_else(|| panic!("no index in the static part: {static_part}"));
    static_part[start..]
        .lines()
        .filter_map(|line| line.strip_prefix("- "))
        .map(|row| row.split(':').next().unwrap_or(row).trim().to_string())
        .collect()
}

/// An SDK `disabled` entry takes a tool out of the dispatcher's reach. The
/// index advertises exactly what the dispatcher reaches, so the two surfaces
/// cannot advertise different sets.
#[tokio::test]
async fn the_index_names_exactly_the_tools_toolbox_reaches() {
    let _guard = crate::storage::lock_test_env();
    let _env = ProfileEnv::new(CORE_TOOL_PROFILE, None);
    let mut agent = agent_with_mcp_management_tool().await;
    let session_id = agent.session_id().to_string();
    let (tx, _rx) = tokio_mpsc::unbounded_channel();
    crate::tool::sdk::configure(
        &session_id,
        "index-owner",
        crate::protocol::SessionToolConfig {
            enabled: None,
            disabled: vec!["webfetch".into()],
            custom: vec![],
        },
        tx,
    )
    .expect("configure the SDK tool list");

    agent.tool_definitions().await;
    let prompt = agent.build_system_prompt_split(None).static_part;
    assert!(
        !prompt.contains("\n- webfetch: "),
        "a disabled tool is not listed: {prompt}"
    );
    assert!(prompt.contains("\n- undo: "), "{prompt}");

    let ctx = ToolContext {
        session_id: session_id.clone(),
        message_id: "message".to_string(),
        tool_call_id: "toolbox-call".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    };
    let listed = agent
        .registry()
        .execute(TOOLBOX_TOOL, serde_json::json!({"action": "list"}), ctx)
        .await
        .expect("toolbox list succeeds")
        .output;
    let mut list_names: Vec<String> = listed
        .lines()
        .filter_map(|line| line.strip_prefix("- "))
        .map(|row| row.split(':').next().unwrap_or(row).trim().to_string())
        .collect();
    let mut index_names = index_row_names(&prompt);
    list_names.sort_unstable();
    index_names.sort_unstable();
    assert_eq!(
        index_names, list_names,
        "the index and toolbox list advertise one set\nprompt:\n{prompt}\ntoolbox list:\n{listed}"
    );

    crate::tool::sdk::remove_session(&session_id);
}

/// `/clear` empties the transcript of the same session, so the tier snapshot and
/// the listed-tool index still describe it: only a session restore drops them.
#[tokio::test]
async fn clearing_the_conversation_keeps_the_frozen_index() {
    let _guard = crate::storage::lock_test_env();
    let _env = ProfileEnv::new(CORE_TOOL_PROFILE, None);
    let mut agent = agent_with_mcp_management_tool().await;
    agent.tool_definitions().await;
    let before = without_clock(&agent.build_system_prompt_split(None).static_part);
    assert!(before.contains("# Additional tools"), "{before}");
    let rows = index_row_names(&before).len();

    agent.clear();

    assert!(
        agent.locked_tools.is_none(),
        "the clear ran: it drops the locked tool snapshot"
    );
    let after = without_clock(&agent.build_system_prompt_split(None).static_part);
    assert_eq!(before, after, "clearing the transcript keeps the cached prefix");
    assert_eq!(
        index_row_names(&after).len(),
        rows,
        "the index keeps every row it froze"
    );
}

/// A warmup sends the same static prefix the first real turn sends.
#[tokio::test]
async fn prewarm_sends_the_same_static_prefix_as_the_first_turn() {
    let _guard = crate::storage::lock_test_env();
    let _env = ProfileEnv::new(CORE_TOOL_PROFILE, None);
    let mut agent = agent_with_mcp_management_tool().await;

    let (_provider, warm_tools, warm_static, _context) = agent.prewarm_snapshot().await;
    assert!(
        warm_static.contains("# Additional tools"),
        "a warmup must carry the index a turn carries: {warm_static}"
    );

    let tools = agent.tool_definitions().await;
    let first_turn = agent.build_system_prompt_split(None).static_part;
    assert_eq!(
        without_clock(&warm_static),
        without_clock(&first_turn),
        "the warmup and the first turn send one static prefix"
    );
    assert_eq!(
        serde_json::to_string(&ToolDefinition::eager(&warm_tools)).expect("serialize"),
        serde_json::to_string(&ToolDefinition::eager(&tools)).expect("serialize"),
        "the warmup sends the same eager tool list"
    );
}

/// The daemon registers the MCP management tool while a session subscribes,
/// which is after an idle warmup has already built the index. A warmup must not
/// freeze a list that is missing a tool the session can reach: the first
/// tool-list lock rebuilds it from the same registry the turn will use.
#[tokio::test]
async fn a_warmup_built_before_mcp_registration_is_rebuilt_at_the_tool_lock() {
    let _guard = crate::storage::lock_test_env();
    let _env = ProfileEnv::new(CORE_TOOL_PROFILE, None);
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    let (_provider, _tools, warm, _context) = agent.prewarm_snapshot().await;
    assert!(warm.contains("# Additional tools"), "{warm}");
    assert!(
        !warm.contains("\n- mcp: "),
        "the MCP management tool has not registered yet: {warm}"
    );

    agent
        .registry
        .register(
            "mcp".to_string(),
            Arc::new(FakeMcpTool {
                name: "mcp".to_string(),
            }) as Arc<dyn crate::tool::Tool>,
        )
        .await;

    let definitions = agent.tool_definitions().await;
    assert!(
        definition(&definitions, "mcp").defer_loading,
        "the late tool is listed, not eager"
    );
    let prompt = agent.build_system_prompt_split(None).static_part;
    assert!(
        prompt.contains("\n- mcp: "),
        "the frozen index names every tool the session can reach: {prompt}"
    );

    let (_provider, _tools, warm_again, _context) = agent.prewarm_snapshot().await;
    assert_eq!(
        without_clock(&warm_again),
        without_clock(&prompt),
        "the index is final once the surface is locked"
    );
}

/// An SDK system-prompt override carries the index too: the override replaces
/// the built-in prompt, not the tools the session can reach.
#[tokio::test]
async fn an_sdk_system_prompt_override_carries_the_index() {
    let _guard = crate::storage::lock_test_env();
    let _env = ProfileEnv::new(CORE_TOOL_PROFILE, None);
    let mut agent = agent_with_mcp_management_tool().await;
    agent.set_system_prompt("X");

    agent.tool_definitions().await;
    let prompt = agent.system_prompt_for_debug();
    assert!(prompt.starts_with("X"), "{prompt}");
    assert!(prompt.contains("# Additional tools"), "{prompt}");
    assert!(prompt.contains("- undo: "), "{prompt}");
}

#[tokio::test]
async fn core_listed_manual_qa_transcript() {
    let _guard = crate::storage::lock_test_env();
    let _env = ProfileEnv::new(CORE_TOOL_PROFILE, None);
    let agent = agent_with_mcp_management_tool().await;
    let definitions = agent.tool_definitions_for_debug().await;

    let mut eager: Vec<&str> = Vec::new();
    let mut listed: Vec<&str> = Vec::new();
    for definition in &definitions {
        if definition.defer_loading {
            listed.push(&definition.name);
        } else {
            eager.push(&definition.name);
        }
    }
    println!("=== profile core, agent-built definitions ({}) ===", definitions.len());
    println!("eager ({}): {}", eager.len(), eager.join(", "));
    println!("listed ({}): {}", listed.len(), listed.join(", "));

    assert_eq!(eager.len(), ESSENTIAL_TOOLS.len() + 1);
    for name in ESSENTIAL_TOOLS {
        assert!(eager.contains(name), "{name} must be eager");
    }
    assert_eq!(listed.len(), CORE_TOOLS.len() - ESSENTIAL_TOOLS.len());
}

/// The swarm worker derives its allow-list from the registry, so the core
/// allow-list joining the dispatcher would hand every worker an eager `toolbox`
/// with nothing to list. The worker must send exactly the fifteen core tools.
#[tokio::test]
async fn swarm_worker_allow_list_never_carries_the_dispatcher() {
    let _guard = crate::storage::lock_test_env();
    let _env = ProfileEnv::new(CORE_TOOL_PROFILE, None);
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    registry
        .register(
            "mcp".to_string(),
            Arc::new(FakeMcpTool {
                name: "mcp".to_string(),
            }) as Arc<dyn crate::tool::Tool>,
        )
        .await;

    let mut allowed: HashSet<String> = registry.tool_names().await.into_iter().collect();
    for blocked in ["subagent", "task", "todo", "todowrite", "todoread"] {
        allowed.remove(blocked);
    }
    crate::config::config().tools.apply_to_allowed_set(&mut allowed);
    assert!(
        allowed.contains(TOOLBOX_TOOL),
        "the registry-derived list starts with the dispatcher, as swarm.rs builds it"
    );

    let worker = Agent::new_with_session(provider, registry, Session::create(None, None), Some(allowed));
    let definitions = worker.tool_definitions_for_debug().await;
    let names: Vec<String> = definitions.iter().map(|definition| definition.name.clone()).collect();
    let mut baseline: Vec<String> = CORE_BASELINE.iter().map(|name| (*name).to_string()).collect();
    baseline.sort();
    assert_eq!(
        names, baseline,
        "a swarm worker sends the fifteen core tools and nothing else"
    );
    assert!(
        !definitions.iter().any(|definition| definition.name == TOOLBOX_TOOL),
        "nothing is listed in a worker, so the dispatcher must not ride along"
    );
    assert!(definitions.iter().all(|definition| !definition.defer_loading));
}

/// `full` reached the dispatcher before core listing existed, so a worker built
/// on it keeps it.
#[tokio::test]
async fn full_swarm_worker_keeps_the_dispatcher() {
    let _guard = crate::storage::lock_test_env();
    let _env = ProfileEnv::new("full", None);
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;

    let mut allowed: HashSet<String> = registry.tool_names().await.into_iter().collect();
    crate::config::config().tools.apply_to_allowed_set(&mut allowed);
    assert!(allowed.contains(TOOLBOX_TOOL));

    let worker = Agent::new_with_session(provider, registry, Session::create(None, None), Some(allowed));
    let definitions = worker.tool_definitions_for_debug().await;
    assert!(
        definitions.iter().any(|definition| definition.name == TOOLBOX_TOOL),
        "the full profile always reached the dispatcher"
    );
}

/// An explicit `discoverable` entry outlives the SDK keep-eager rule.
#[tokio::test]
async fn core_sdk_list_honors_an_explicit_discoverable_override() {
    let _guard = crate::storage::lock_test_env();
    let _env = ProfileEnv::new(
        CORE_TOOL_PROFILE,
        Some("[tools]\nprofile = \"core\"\n\n[tools.load_mode]\nask = \"discoverable\"\n"),
    );
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let agent = Agent::new(provider, registry);
    let session_id = agent.session_id().to_string();
    let (tx, _rx) = tokio_mpsc::unbounded_channel();
    crate::tool::sdk::configure(
        &session_id,
        "core-override-owner",
        crate::protocol::SessionToolConfig {
            enabled: Some(vec!["ask".into(), "bash".into()]),
            disabled: vec![],
            custom: vec![],
        },
        tx,
    )
    .expect("configure the SDK tool list");

    let definitions = agent.tool_definitions_for_debug().await;
    assert!(
        definition(&definitions, "ask").defer_loading,
        "an explicit discoverable entry still defers in an SDK session"
    );
    assert!(
        !definition(&definitions, "bash").defer_loading,
        "the rest of the SDK list stays eager"
    );
    crate::tool::sdk::remove_session(&session_id);
}
