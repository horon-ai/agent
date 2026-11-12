use super::*;
use crate::{terminal_eprintln as eprintln, terminal_println as println};

pub(in crate::agent) mod audio;
mod turn_token_usage;
use turn_token_usage::*;

impl Agent {
    /// Refuse to run a model turn when this session moved to another machine
    /// or this in-memory copy is older than the transcript on disk.
    pub(crate) fn ensure_session_lease(&self) -> Result<()> {
        self.session.ensure_event_current()?;
        let account_route = self
            .session
            .route_api_method
            .as_deref()
            .or(self.session.provider_key.as_deref());
        if let Some(expected) = account_route.and_then(horon_agent_provider_core::oauth_account_label)
            && self.provider.oauth_account_label().as_deref() != Some(expected)
        {
            anyhow::bail!(
                "Saved account '{}' could not be restored for this session. Select an available account using /model or /login before sending a request.",
                expected
            );
        }
        match self.session.migration_lease_block() {
            Some(block) => Err(anyhow::anyhow!("Session is not runnable here: {block}")),
            None => Ok(()),
        }
    }

    /// True when a newer migrated transcript replaced this copy on disk.
    pub(crate) fn session_copy_is_stale(&self) -> bool {
        matches!(
            self.session.migration_lease_block(),
            Some(crate::storage::SessionLeaseBlock::StaleCopy { .. })
        )
    }

    /// Run a single turn with the given user message
    pub async fn run_once(&mut self, user_message: &str) -> Result<()> {
        self.resolve_armed_effort(false, None);
        self.announce_late_mcp_tools().await;
        self.announce_late_skills();
        let input_id = self.add_turn_start_message(
            Role::User,
            vec![ContentBlock::Text {
                text: user_message.to_string(),
                cache_control: None,
            }],
            None,
        );
        if !user_message.trim().is_empty() {
            self.begin_model_usage_turn(&input_id);
        }
        self.save_first_commit()?;
        if trace_enabled() {
            eprintln!("[trace] session_id {}", self.session.id);
        }
        let turn_started_at = Instant::now();
        let start_message_index = self.message_count();
        self.fire_turn_start_hook("run");
        let result = self.run_turn(true).await.map(|_| ());
        // `horon-agent run` uses this path, so headless turns fire the same
        // lifecycle hooks as streaming turns (docs/runtime/HOOKS.md).
        self.fire_turn_end_hook(&result, turn_started_at, start_message_index);
        result
    }

    pub async fn run_once_capture(&mut self, user_message: &str) -> Result<String> {
        self.run_once_capture_with_display_role(user_message, None).await
    }

    pub(crate) async fn run_once_capture_with_display_role(
        &mut self,
        user_message: &str,
        display_role: Option<crate::session::StoredDisplayRole>,
    ) -> Result<String> {
        self.resolve_armed_effort(false, display_role);
        self.announce_late_mcp_tools().await;
        self.announce_late_skills();
        let input_id = self.add_turn_start_message(
            Role::User,
            vec![ContentBlock::Text {
                text: user_message.to_string(),
                cache_control: None,
            }],
            display_role,
        );
        if !user_message.trim().is_empty() {
            self.begin_model_usage_turn(&input_id);
        }
        self.save_first_commit()?;
        if trace_enabled() {
            eprintln!("[trace] session_id {}", self.session.id);
        }
        self.run_turn(false).await
    }

    /// Run one conversation turn with streaming events via mpsc channel (per-client)
    pub async fn run_once_streaming_mpsc(
        &mut self,
        user_message: &str,
        images: Vec<(String, String)>,
        system_reminder: Option<String>,
        event_tx: mpsc::UnboundedSender<ServerEvent>,
    ) -> Result<()> {
        self.run_once_streaming_mpsc_with_display_role(user_message, images, system_reminder, event_tx, None, None)
            .await
    }

    pub(crate) async fn run_once_streaming_mpsc_with_display_role(
        &mut self,
        user_message: &str,
        images: Vec<(String, String)>,
        system_reminder: Option<String>,
        event_tx: mpsc::UnboundedSender<ServerEvent>,
        display_role: Option<crate::session::StoredDisplayRole>,
        mut report: Option<crate::server::foreground_state::TurnReport>,
    ) -> Result<()> {
        // Inject any pending notifications before the user message
        let alerts = self.take_alerts();
        if !alerts.is_empty() {
            let alert_text = format!(
                "[NOTIFICATION]\nYou received {} notification(s) from other agents working in this codebase:\n\n{}\n\nUse the communicate tool to coordinate with other agents (prefer dm; broadcast reaches only your spawned subtree).",
                alerts.len(),
                alerts.join("\n\n---\n\n")
            );
            self.add_message(
                Role::User,
                vec![ContentBlock::Text {
                    text: alert_text,
                    cache_control: None,
                }],
            );
        }

        let mut system_reminder = system_reminder.filter(|value| !value.trim().is_empty());
        // A directory list that changed after the session context was captured
        // reaches the model on the next prompt and never again.
        if let Some(reminder) = self.session.take_pending_additional_dirs_reminder() {
            Self::merge_system_reminder(&mut system_reminder, &reminder);
            self.persist_session_best_effort("additional directories reminder");
        }
        // A reminder-only turn (e.g. reload recovery) used to persist an empty
        // user message, so the model saw a blank prompt and asked about it.
        // Carry the reminder as the turn's message instead.
        let (user_message, system_reminder, display_role, continues_turn) = match system_reminder {
            Some(reminder) if user_message.trim().is_empty() && images.is_empty() => (
                std::borrow::Cow::Owned(format!("[auto] {}", reminder.trim())),
                None,
                display_role.or(Some(crate::session::StoredDisplayRole::System)),
                true,
            ),
            reminder => (std::borrow::Cow::Borrowed(user_message), reminder, display_role, false),
        };
        self.current_turn_system_reminder = system_reminder;

        self.resolve_armed_effort(continues_turn, display_role);
        self.announce_late_mcp_tools().await;
        self.announce_late_skills();
        let stored_as_user = display_role.is_none();
        let start_message_id = self.insert_user_context_message_with_display_role(
            &user_message,
            images,
            display_role,
            continues_turn,
            true,
        );
        // The stored id exists now and nothing is persisted yet: an editor that
        // waits for the insertion learns it before anything can fail, and a
        // persistence failure below is a failure after insertion.
        if let Some(report) = report.as_mut() {
            report.inserted(&self.session.id, &start_message_id);
        }
        self.save_session()?;
        let turn_started_at = Instant::now();
        let start_message_index = self.message_count();
        self.fire_turn_start_hook("chat");
        // A reload recovery continues the turn that was already running, so it
        // keeps that run's id. Any other message is new external input and begins
        // a run of its own. `begin_run` dispatches the start write without
        // awaiting: this entry point must reach its turn without yielding first,
        // because a yield here lets the caller run before the turn publishes
        // anything and starves a client already waiting for the first delta.
        let run = match continues_turn.then(|| self.current_run()).flatten() {
            Some(existing) => existing,
            None => self.begin_run(),
        };
        if let Some(report) = report.as_ref() {
            // Only text the user actually typed names the turn: an empty or
            // whitespace-only continuation (a reload reminder with no prompt, a
            // bare image) has no user message to report.
            let user_content = (stored_as_user && !user_message.trim().is_empty()).then(|| user_message.to_string());
            report.started(&event_tx, &self.session.id, run.run_id(), user_content);
        }
        let result = self.run_turn_streaming_mpsc(event_tx).await;
        self.current_turn_system_reminder = None;
        self.fire_turn_end_hook(&result, turn_started_at, start_message_index);
        let cause = self.stop_cause_for(result.as_ref().err().map(|error| error.to_string()).as_deref());
        if let Some(report) = report.as_ref() {
            report.stopped(cause);
        }
        self.settle_run_start(&run).await;
        self.note_turn_conversation_save(result.is_ok());
        self.finish_run(run.run_id(), cause).await;
        result
    }

    /// Append and persist a user message without starting a model turn.
    pub(crate) fn append_user_context_message(
        &mut self,
        user_message: &str,
        images: Vec<(String, String)>,
    ) -> Result<()> {
        self.append_user_context_message_with_display_role(user_message, images, None, false)
    }

    fn append_user_context_message_with_display_role(
        &mut self,
        user_message: &str,
        images: Vec<(String, String)>,
        display_role: Option<crate::session::StoredDisplayRole>,
        // A synthetic continuation (reload recovery) resumes the previous
        // turn, so it must not start a new model-usage turn.
        continues_turn: bool,
    ) -> Result<()> {
        // A restored session may still owe its commit; land it before this
        // message, so the message never runs ahead of the state it extends.
        self.finish_resume_persistence()?;
        self.insert_user_context_message_with_display_role(user_message, images, display_role, continues_turn, false);
        self.save_session()
    }

    /// Store a user message without persisting it, returning its stored id.
    /// With `starts_run`, the message is the one the turn about to run starts
    /// with, and is recorded as its start.
    fn insert_user_context_message_with_display_role(
        &mut self,
        user_message: &str,
        images: Vec<(String, String)>,
        display_role: Option<crate::session::StoredDisplayRole>,
        continues_turn: bool,
        starts_run: bool,
    ) -> String {
        let mut blocks = audio::attachment_blocks(images);
        blocks.push(ContentBlock::Text {
            text: user_message.to_string(),
            cache_control: None,
        });

        if blocks.len() > 1 {
            crate::logging::info(&format!("Agent received message with {} image(s)", blocks.len() - 1));
        }

        let starts_turn = !continues_turn && (blocks.len() > 1 || !user_message.trim().is_empty());
        let input_id = if starts_run {
            self.add_turn_start_message(Role::User, blocks, display_role)
        } else {
            self.add_message_with_display_role(Role::User, blocks, display_role)
        };
        if starts_turn {
            self.begin_model_usage_turn(&input_id);
        }
        input_id
    }

    /// Fire the `turn_start` observer hook when a turn begins, before the model
    /// starts generating (and before the first `pre_tool`). This lets external
    /// integrations (terminal multiplexers, status bars) detect that the agent
    /// is actively working during the otherwise-invisible window between prompt
    /// submission and the first tool call. No-op (without building the payload)
    /// when the hook is not configured.
    fn fire_turn_start_hook(&self, source: &str) {
        if !crate::hooks::hook_configured("turn_start") {
            return;
        }
        let mut event = crate::hooks::HookEvent::new("turn_start")
            .session_id(self.session.id.clone())
            .field("MODEL", self.provider_model())
            .field("SOURCE", source.to_string());
        if let Some(cwd) = self.working_dir() {
            event = event.cwd(cwd);
        }
        crate::hooks::dispatch_observer(event);
    }

    /// Fire the `turn_end` observer hook with turn outcome metadata.
    /// No-op (without building the payload) when the hook is not configured.
    fn fire_turn_end_hook(&self, result: &Result<()>, started_at: Instant, start_message_index: usize) {
        if !crate::hooks::hook_configured("turn_end") {
            return;
        }
        let status = if result.is_ok() { "ok" } else { "error" };
        let mut event = crate::hooks::HookEvent::new("turn_end")
            .session_id(self.session.id.clone())
            .field("STATUS", status)
            .field("DURATION_MS", started_at.elapsed().as_millis().to_string())
            .field("MODEL", self.provider_model());
        if let Some(cwd) = self.working_dir() {
            event = event.cwd(cwd);
        }
        if let Some(text) = self.latest_assistant_text_after(start_message_index) {
            const LAST_TEXT_LIMIT: usize = 4000;
            let snippet: String = text.chars().take(LAST_TEXT_LIMIT).collect();
            event = event.field("LAST_ASSISTANT_TEXT", snippet);
        }
        if let Some(usage) = session_turn_token_usage_after(&self.session, start_message_index) {
            if let Some(value) = usage.prompt_tokens {
                event = event.field("PROMPT_TOKENS", value.to_string());
            }
            event = event
                .field("INPUT_TOKENS", usage.input_tokens.to_string())
                .field("OUTPUT_TOKENS", usage.output_tokens.to_string());
            if let Some(value) = usage.cache_read_input_tokens {
                event = event.field("CACHE_READ_TOKENS", value.to_string());
            }
            if let Some(value) = usage.cache_creation_input_tokens {
                event = event.field("CACHE_WRITE_TOKENS", value.to_string());
            }
        }
        if let Err(error) = result {
            const ERROR_LIMIT: usize = 1000;
            let message: String = error.to_string().chars().take(ERROR_LIMIT).collect();
            event = event.field("ERROR", message);
        }
        crate::hooks::dispatch_observer(event);
    }

    /// Unlock the tool list so the next API request picks up any new tools.
    /// Called after MCP reload or when the user explicitly wants new tools.
    pub fn unlock_tools(&mut self) {
        if self.locked_tools.is_some() {
            logging::info("Tool list unlocked — next request will pick up current tools");
            self.locked_tools = None;
            self.cache_tracker.reset();
            self.kv_cache_monitor.reset();
        }
        // Allow the late-MCP-registration recheck to fire once for the next
        // snapshot (e.g. after an explicit `mcp` reload).
        self.mcp_late_register_resolved = false;
    }

    /// Unlock tools if a tool execution may have changed the registry
    /// (e.g., mcp connect/disconnect/reload)
    pub(super) fn unlock_tools_if_needed(&mut self, tool_name: &str, input: &serde_json::Value) {
        // The dispatcher is policy-transparent, so an `mcp` call made through it
        // is an `mcp` call: connecting servers there must unlock the same way.
        let is_mcp_call = if tool_name == crate::config::TOOLBOX_TOOL {
            input
                .get("tool")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .is_some_and(|target| crate::tool::Registry::resolve_tool_name(target) == "mcp")
        } else {
            tool_name == "mcp"
        };
        // Only explicit `eager` exposes per-server definitions in the cached
        // tool list. Every other mode keeps that list fixed (native deferred
        // loading or the `mcp_search`/`mcp_call` surface), so an `mcp`
        // connect/disconnect must not unlock it: that would be a needless
        // full prompt-cache miss.
        if is_mcp_call && self.mcp_tools_mode == crate::config::McpToolsMode::Eager {
            self.unlock_tools();
        }
    }

    pub fn is_debug(&self) -> bool {
        self.session.is_debug
    }

    /// Set a persisted custom system prompt override (also used by ambient mode).
    /// When set, this replaces the normal system prompt entirely.
    pub fn set_system_prompt(&mut self, prompt: &str) {
        self.session.system_prompt = Some(prompt.to_string());
        self.persist_session_best_effort("system prompt override");
    }

    pub fn set_debug(&mut self, is_debug: bool) {
        self.session.set_debug(is_debug);
        if let Err(err) = self.save_session() {
            logging::error(&format!("Failed to persist debug session state: {}", err));
        }
    }

    /// Enable or disable memory features for this session.
    pub fn set_memory_enabled(&mut self, enabled: bool) {
        self.memory_enabled = enabled;
        if !enabled {
            crate::memory::clear_pending_memory(&self.session.id);
        }
    }

    /// Mark this session as an inline swarm worker. When enabled, the streaming
    /// loop publishes a throttled output tail to the global bus so a
    /// coordinator can render a live inline gallery viewport for it.
    pub fn set_inline_output_tap(&mut self, enabled: bool) {
        self.inline_output_tap = enabled;
    }

    /// Whether this session streams an inline output tail to the bus.
    pub(crate) fn inline_output_tap(&self) -> bool {
        self.inline_output_tap
    }

    /// Publish the current rolling activity tail to the bus for the
    /// coordinator's inline gallery. No-op unless the inline tap is enabled.
    pub(crate) fn publish_inline_tail(&self) {
        if !self.inline_output_tap {
            return;
        }
        crate::bus::Bus::global().publish(crate::bus::BusEvent::SwarmOutputTail(crate::bus::SwarmOutputTail {
            session_id: self.session.id.clone(),
            tail: self.inline_tail.render(),
        }));
    }

    /// Check whether memory features are enabled for this session.
    pub fn memory_enabled(&self) -> bool {
        self.memory_enabled
    }

    /// Set the stdin request channel for interactive stdin forwarding
    pub fn set_stdin_request_tx(&mut self, tx: tokio::sync::mpsc::UnboundedSender<crate::tool::StdinInputRequest>) {
        self.stdin_request_tx = Some(tx);
    }

    /// Prepare the static provider prefix while a client is idle. Unlike
    /// `tool_definitions`, this does not pin the tool snapshot or consume the
    /// one-shot late-MCP-discovery check before the first real turn.
    pub(crate) async fn prewarm_provider(&self) {
        self.ensure_listed_tools_index().await;
        let tools: Arc<[ToolDefinition]> = match &self.locked_tools {
            Some(tools) => Arc::clone(tools),
            None => self.build_filtered_tool_definitions().await.into(),
        };
        let prompt = self.build_system_prompt_split(None);
        self.provider
            .prewarm_with_context(&tools, &prompt.static_part, &self.provider_request_context())
            .await;
    }

    pub(super) async fn tool_definitions(&mut self) -> Arc<[ToolDefinition]> {
        let tools = self.lock_tool_definitions().await;
        // The surface is now authoritative, so the index is built (or rebuilt
        // once, when a warmup built it before the lock) from the same registry
        // state and frozen. This always happens before the request is sent.
        self.ensure_listed_tools_index().await;
        tools
    }

    async fn lock_tool_definitions(&mut self) -> Arc<[ToolDefinition]> {
        // The MCP fixed-surface decision is taken here, once, before any list
        // is built: every later rebuild reads the frozen value. The published
        // flag is refreshed on every lock because a provider switch can change
        // whether the frozen index can carry the surface at all.
        self.freeze_mcp_surface();
        self.publish_listed_mcp_surface();

        // Provider-native deferred MCP loading: MCP definitions live outside
        // the cached prefix, so the eager snapshot never changes when servers
        // connect, reconnect, or register late. Refresh the deferred subset
        // every turn instead of unlocking.
        let native = self.native_deferred_mcp();
        if self.locked_tools.is_some() && self.locked_tools_native_deferred != native {
            // The active provider's deferred-loading capability changed (model
            // or provider switch). The prefix changes with the provider anyway,
            // so rebuilding costs nothing extra and keeps MCP usable.
            logging::info(&format!(
                "Rebuilding tool snapshot: native deferred MCP loading {} after provider change",
                if native { "enabled" } else { "disabled" }
            ));
            self.locked_tools = None;
            self.mcp_late_register_resolved = false;
            self.cache_tracker.reset();
            self.kv_cache_monitor.reset();
        }
        if native {
            return self.native_deferred_tool_definitions().await;
        }

        // Return locked tools if available (prevents cache invalidation from
        // tools arriving asynchronously after the first API request).
        //
        // Exception: MCP servers connect on a background task and register
        // `mcp__*` tools seconds after the session starts — typically *after*
        // the first turn has already locked the snapshot. We deliberately do
        // NOT block the first turn on MCP connection: servers can be slow or
        // hang, and we want the user to be able to talk to the agent the moment
        // the session spawns. The price is that the first locked snapshot is
        // missing MCP tools, and the only other unlock path fires when the model
        // calls the `mcp` management tool — which it cannot do without first
        // seeing MCP tools (#206).
        //
        // So, exactly once per locked snapshot, if MCP tools have since appeared
        // in the registry, we rebuild. This is a single intentional provider
        // prompt-cache miss (the turn MCP tools first appear). The
        // `mcp_late_register_resolved` flag makes this a one-shot check so we do
        // not rescan the registry on every subsequent turn.
        // Every mode except explicit `eager` keeps a cache-stable MCP surface:
        // per-server tools registering later never change the locked list.
        let locked_uses_fixed_mcp_surface = self.mcp_tools_mode != crate::config::McpToolsMode::Eager
            || self.locked_tools.as_ref().is_some_and(|locked| {
                locked
                    .iter()
                    .any(|tool| matches!(tool.name.as_str(), "mcp_search" | "mcp_call"))
                    && !locked.iter().any(|tool| tool.name.starts_with("mcp__"))
            });
        if locked_uses_fixed_mcp_surface && let Some(locked) = &self.locked_tools {
            // Per-server tools may continue registering in the background, but
            // the fixed surface cannot change as a result. Avoid a provider
            // cache reset and registry scan; new tools are announced in the
            // transcript instead (see `announce_late_mcp_tools`).
            self.mcp_late_register_resolved = true;
            return Arc::clone(locked);
        }
        if let Some(ref locked) = self.locked_tools {
            if self.mcp_late_register_resolved {
                return Arc::clone(locked);
            }
            if self.registry_has_new_mcp_tools(locked).await {
                logging::info(
                    "MCP tools registered after first turn locked the tool snapshot — \
                     rebuilding once to expose them. This is one intentional prompt-cache \
                     miss; we accept it so the agent is reachable immediately at spawn \
                     instead of blocking on MCP connection (#206).",
                );
                // Latch the one-shot guard and drop the stale snapshot directly.
                // We intentionally do NOT call `unlock_tools()` here, because that
                // re-arms the guard (it is the explicit-reload path) and would let
                // the recheck fire again on every later turn.
                self.mcp_late_register_resolved = true;
                self.locked_tools = None;
                self.cache_tracker.reset();
                self.kv_cache_monitor.reset();
            } else {
                // No MCP tools have appeared. They may still be connecting, so
                // leave the guard unset and re-check on the next turn. Once they
                // appear (or never do, after the registry settles) we stop.
                return Arc::clone(locked);
            }
        }

        let tools: Arc<[ToolDefinition]> = self.build_filtered_tool_definitions().await.into();

        // Lock the tool list to prevent cache invalidation when more tools
        // arrive asynchronously mid-session.
        logging::info(&format!(
            "Locking tool list at {} tools for cache stability",
            tools.len()
        ));
        self.locked_tools = Some(Arc::clone(&tools));
        self.locked_tools_native_deferred = false;
        // Per-server definitions in the snapshot itself (eager mode) are
        // already described to the model; never announce them again.
        self.announced_mcp_tools.extend(
            tools
                .iter()
                .filter(|tool| tool.name.starts_with("mcp__"))
                .map(|tool| tool.name.clone()),
        );
        tools
    }

    /// Whether this session exposes MCP tools through provider-native deferred
    /// loading (see [`ToolDefinition::defer_loading`]). Eager mode keeps
    /// top-level definitions; every other mode prefers the native path when
    /// the active provider supports it, and falls back to the fixed
    /// `mcp_search`/`mcp_call` surface otherwise.
    pub(crate) fn native_deferred_mcp(&self) -> bool {
        self.mcp_tools_mode != crate::config::McpToolsMode::Eager && self.provider.supports_deferred_tools()
    }

    /// Whether this session lists the core tools it does not send eagerly: a
    /// `core` allow-list with no SDK `enabled` overlay naming the tools exactly.
    ///
    /// The same condition the tier pass and the index build use.
    fn lists_core_tools(&self) -> bool {
        self.load_tiers.core_listed()
            && crate::tool::sdk::config(&self.session.id)
                .and_then(|config| config.enabled)
                .is_none()
    }

    /// The frozen MCP fixed-surface decision: `true` when at least one MCP
    /// server was configured when the surface resolved.
    ///
    /// Before the freeze the live registry count answers, because nothing has
    /// committed to a list yet.
    fn mcp_surface_eager(&self) -> bool {
        match *self
            .mcp_surface_eager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
        {
            Some(value) => value,
            None => self.registry.configured_mcp_servers() > 0,
        }
    }

    /// Freeze the MCP fixed-surface decision for the session and publish it.
    ///
    /// Called where the tool surface locks, before the index is frozen, so the
    /// definition list, the index and the dispatcher all read one value. A
    /// later unlock, compaction, server connection or provider switch reuses it:
    /// moving `mcp_search`/`mcp_call` between the eager list and the index
    /// mid-session would rewrite the cached prefix.
    fn freeze_mcp_surface(&self) {
        {
            let mut slot = self
                .mcp_surface_eager
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if slot.is_some() {
                return;
            }
            *slot = Some(self.registry.configured_mcp_servers() > 0);
        }
        self.publish_listed_mcp_surface();
    }

    /// Publish the session's listing decision to the tool policy, where
    /// `toolbox` and the `mcp` management tool read it.
    fn publish_listed_mcp_surface(&self) {
        crate::tool::register_session_listed_mcp(&self.session.id, self.mcp_surface_listed());
    }

    /// Whether `mcp_search`/`mcp_call` reach this session through `toolbox`
    /// rather than as eager definitions.
    ///
    /// Only the fixed surface on a provider without native deferred loading:
    /// with native loading `mcp_search` stays eager and `mcp_call` is the
    /// session's deferred placeholder, and explicit `eager` mode sends the
    /// per-server definitions instead. A session that lists no tools, or whose
    /// SDK `enabled` overlay names its tools exactly, keeps both eager.
    ///
    /// The frozen index has the last word. A session that resolved on a provider
    /// with native deferred loading froze an index without the two rows; when
    /// its provider later switches to one that drops deferred definitions,
    /// listing them would advertise them nowhere, so they stay eager instead.
    fn mcp_surface_listed(&self) -> bool {
        self.mcp_tools_mode != crate::config::McpToolsMode::Eager
            && !self.provider.supports_deferred_tools()
            && self.lists_core_tools()
            && !self.mcp_surface_eager()
            && self.listed_tools_index_names_mcp_surface()
    }

    /// Whether the frozen index names the MCP fixed surface.
    ///
    /// `true` while the index is still free to be built: the lock that freezes
    /// it rebuilds the rows from the registry it will use, so those rows carry
    /// the surface whenever this session lists it.
    fn listed_tools_index_names_mcp_surface(&self) -> bool {
        if !self
            .listed_tools_index_frozen
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return true;
        }
        match self.listed_tools_rows() {
            Some(rows) => rows.iter().any(|(name, _)| name == "mcp_search"),
            None => true,
        }
    }

    /// Tool list for the native deferred MCP path.
    ///
    /// The eager part is locked exactly like the regular path. MCP
    /// definitions are appended as deferred on every turn from the live
    /// registry. They never enter the cached prefix, so adding, removing, or
    /// reconnecting servers costs no prompt-cache miss.
    async fn native_deferred_tool_definitions(&mut self) -> Arc<[ToolDefinition]> {
        let current = self.build_filtered_tool_definitions().await;
        let eager = match &self.locked_tools {
            // A snapshot locked before switching to the native path may still
            // carry eager MCP definitions; keep them eager for cache stability
            // (they are still callable) but never duplicate them as deferred.
            Some(locked) => Arc::clone(locked),
            None => {
                let eager: Arc<[ToolDefinition]> = ToolDefinition::eager(&current).into();
                logging::info(&format!(
                    "Locking eager tool list at {} tools for cache stability (MCP tools deferred)",
                    eager.len()
                ));
                self.locked_tools = Some(Arc::clone(&eager));
                self.locked_tools_native_deferred = true;
                eager
            }
        };
        self.mcp_late_register_resolved = true;
        // Deferred definitions grow from the live registry, so this path copies.
        let mut tools = eager.to_vec();
        let eager_names: HashSet<String> = tools.iter().map(|t| t.name.clone()).collect();
        tools.extend(
            current
                .into_iter()
                .filter(|t| t.defer_loading && !eager_names.contains(&t.name)),
        );
        tools.into()
    }

    /// Build the agent's tool definitions from the registry, applying the
    /// session's `allowed_tools`, `disabled_tools`, and self-dev filters.
    async fn build_filtered_tool_definitions(&self) -> Vec<ToolDefinition> {
        self.build_filtered_tool_definitions_with(true).await
    }

    /// Filtered definitions before MCP exposure: includes per-server
    /// `mcp__*` definitions (with policy filters applied) even when the
    /// provider sees only the fixed search/call surface.
    async fn build_filtered_tool_definitions_raw(&self) -> Vec<ToolDefinition> {
        self.build_filtered_tool_definitions_with(false).await
    }

    async fn build_filtered_tool_definitions_with(&self, apply_mcp_exposure: bool) -> Vec<ToolDefinition> {
        let sdk = crate::tool::sdk::config(&self.session.id);
        let enabled = sdk
            .as_ref()
            .and_then(|c| c.enabled.as_ref())
            .map(|names| names.iter().cloned().collect());
        let allowed = enabled.as_ref().or(self.allowed_tools.as_ref());
        let disabled = if enabled.is_none() {
            self.disabled_tools.clone()
        } else {
            HashSet::new()
        };
        let mut tools = self.registry.definitions_filtered(allowed, &disabled).await;
        self.apply_builtin_load_tiers(&mut tools);
        if apply_mcp_exposure {
            self.apply_mcp_tool_exposure(&mut tools);
        }
        let mut tools = crate::tool::sdk::apply_definitions(&self.session.id, tools);
        if let Some(config) = sdk.as_ref() {
            let disabled = config.disabled.iter().cloned().collect();
            tools.retain(|tool| !self.registry.tool_is_disabled(&disabled, &tool.name));
        }
        tools
    }

    /// Apply the session's built-in load tiers to the filtered definitions.
    ///
    /// The tiers were resolved once when the session started, so this pass is a
    /// pure projection of that snapshot: a configuration reload cannot move a
    /// tool between the eager and deferred sets mid-session and bust the prompt
    /// cache. `Off` entries are removed here; `Discoverable` entries reuse the
    /// provider-native deferred loading that already backs MCP tools, so their
    /// schemas stay out of the cached prefix. Providers without native support
    /// drop them at the request chokepoint, which is why the discoverable tier
    /// only reaches the model there through the transcript instead.
    ///
    /// A `core`-listed session that carries an SDK `enabled` overlay is an
    /// explicit allow-list: the client named exactly the tools it wants, so the
    /// core-listing default is suppressed and every one of them stays eager. An
    /// explicit `[tools.load_mode]` entry
    /// still wins over that default, because the operator asked for it directly.
    /// `full` is not gated on this: its SDK list is layered on an unrestricted
    /// surface and keeps deferring non-core tools.
    fn apply_builtin_load_tiers(&self, tools: &mut Vec<ToolDefinition>) {
        tools.retain(|tool| self.load_tiers.tier(&tool.name) != crate::config::LoadMode::Off);
        let sdk_enabled = self.load_tiers.core_listed()
            && crate::tool::sdk::config(&self.session.id)
                .and_then(|config| config.enabled)
                .is_some();
        if sdk_enabled {
            for tool in tools.iter_mut() {
                if self.load_tiers.explicit_tier(&tool.name) == Some(crate::config::LoadMode::Discoverable) {
                    tool.defer_loading = true;
                }
            }
            return;
        }
        for tool in tools.iter_mut() {
            if self.load_tiers.tier(&tool.name) == crate::config::LoadMode::Discoverable {
                tool.defer_loading = true;
            }
        }
    }

    /// Build the `# Additional tools` index of this session's listed tools.
    ///
    /// The answer depends only on the session-frozen tiers and the built-in
    /// registry, so once the tool surface has locked, every later caller reads
    /// the same rows and the cached system prefix stays byte-identical. Callers
    /// are the paths that assemble the prompt or hand out a tool list, never a
    /// tool execution.
    ///
    /// A warmup runs before the session locks its surface, and MCP servers
    /// finish registering around then, so a warmup build is not final: the lock
    /// rebuilds it once from the registry the session will actually use and
    /// freezes that. The rebuild happens strictly before the first request, so
    /// it costs no prompt-cache miss.
    pub(crate) async fn ensure_listed_tools_index(&self) {
        if self
            .listed_tools_index_frozen
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return;
        }
        if self.locked_tools.is_none() && self.listed_tools_rows().is_some() {
            return;
        }
        if self.locked_tools.is_some() {
            self.freeze_mcp_surface();
        }
        let rows = self.build_listed_tools_index().await;
        let mut slot = self
            .listed_tools_index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *slot = Some(rows);
        if self.locked_tools.is_some() {
            self.listed_tools_index_frozen
                .store(true, std::sync::atomic::Ordering::Release);
        }
        drop(slot);
        if self.locked_tools.is_some() {
            self.publish_listed_mcp_surface();
        }
    }

    /// The index rows, or `None` while the index has not been built.
    fn listed_tools_rows(&self) -> Option<Vec<(String, String)>> {
        self.listed_tools_index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// The rendered index section, when this session lists any tool.
    ///
    /// A session that reaches the prompt before anything built the index gets no
    /// section and a warning: guessing here would seed the cached prefix with a
    /// list the session never resolved, and staying silent would hide a missing
    /// call site.
    pub(crate) fn listed_tools_section(&self) -> Option<String> {
        match self.listed_tools_rows() {
            Some(rows) => crate::prompt::build_additional_tools_section(&rows),
            None => {
                logging::warn("listed tools index not built before prompt");
                None
            }
        }
    }

    /// One row per listed tool, `(name, one-line description)`.
    ///
    /// `mcp__*` proxies and the dispatcher itself are excluded: the first has a
    /// discovery surface of its own, the second is already eager. Each row also
    /// has to pass the reachability check `toolbox` applies, so the index and
    /// `toolbox list` name the same set; that check carries the session policy,
    /// including an SDK overlay's `enabled` and `disabled` entries, which means
    /// a tool the dispatcher refuses is not advertised either. An SDK `enabled`
    /// list is a client naming its tools exactly, so it lists nothing, the same
    /// condition [`Self::apply_builtin_load_tiers`] uses.
    pub(super) async fn build_listed_tools_index(&self) -> Vec<(String, String)> {
        let sdk_enabled = crate::tool::sdk::config(&self.session.id)
            .and_then(|config| config.enabled)
            .is_some();
        if !self.load_tiers.core_listed() || sdk_enabled {
            return Vec::new();
        }
        let listed_mcp_surface = self.mcp_surface_listed();
        let tools = self
            .registry
            .definitions_filtered(self.allowed_tools.as_ref(), &self.disabled_tools)
            .await;
        tools
            .into_iter()
            .filter(|tool| tool.name != crate::config::TOOLBOX_TOOL)
            .filter(|tool| !tool.name.starts_with("mcp__"))
            .filter(|tool| {
                self.load_tiers.tier(&tool.name) == crate::config::LoadMode::Discoverable
                    || (listed_mcp_surface && matches!(tool.name.as_str(), "mcp_search" | "mcp_call"))
            })
            .filter(|tool| crate::tool::session_can_reach(&self.registry, &self.session.id, &tool.name))
            .map(|tool| (tool.name, crate::prompt::one_line_description(&tool.description)))
            .collect()
    }

    /// Replace per-server MCP definitions with the fixed search/call surface
    /// according to the configured mode. Auto mode estimates the actual
    /// serialized, already-filtered definitions the provider would receive.
    fn apply_mcp_tool_exposure(&self, tools: &mut Vec<ToolDefinition>) {
        if self.native_deferred_mcp() {
            // Keep `mcp_search` eager for discovery (its results load the
            // matching definitions via tool references). Every MCP definition
            // is deferred, and so is `mcp_call`: discovered tools are called
            // natively, but a stable deferred entry matters for caching. Some
            // models (observed on Opus 4.8) rebuild the prefix when the
            // deferred set goes from empty to non-empty, while changes within
            // a non-empty deferred set keep the cache. `mcp_call` stays in the
            // set for the whole session, so the first server connecting later
            // is still cache-neutral.
            for tool in tools.iter_mut() {
                if tool.name.starts_with("mcp__") || tool.name == "mcp_call" {
                    tool.defer_loading = true;
                }
            }
            return;
        }
        // Only explicit `eager` puts per-server definitions in the cached
        // prefix. `auto` used to switch on a token threshold, but any MCP
        // change (late connect, `mcp connect`, crossing the threshold) then
        // rewrote the tool list and busted the whole prompt cache. On
        // providers without native deferred loading, `auto` therefore always
        // uses the fixed `mcp_search`/`mcp_call` surface; the definitions
        // reach the model through the transcript instead.
        let deferred = !matches!(self.mcp_tools_mode, crate::config::McpToolsMode::Eager);

        if deferred {
            tools.retain(|tool| !tool.name.starts_with("mcp__"));
            if self.mcp_surface_listed() {
                // No server was configured when the surface resolved, so the
                // fixed surface has nothing to discover: the session names the
                // two tools in its index and reaches them through `toolbox`.
                for tool in tools.iter_mut() {
                    if matches!(tool.name.as_str(), "mcp_search" | "mcp_call") {
                        tool.defer_loading = true;
                    }
                }
            }
        } else {
            tools.retain(|tool| !matches!(tool.name.as_str(), "mcp_search" | "mcp_call"));
        }
    }

    /// Announce MCP tools that became available after the tool list locked,
    /// by appending a note to the transcript before the next user message.
    ///
    /// Only used on the fixed `mcp_search`/`mcp_call` surface (providers
    /// without native deferred loading, in any mode but `eager`). The cached
    /// tool list must never change, so this appended note, which carries
    /// each tool's input schema, is how the model learns about servers that
    /// finished connecting mid-session. Each tool is announced once.
    pub(crate) async fn announce_late_mcp_tools(&mut self) {
        if self.mcp_tools_mode == crate::config::McpToolsMode::Eager || self.native_deferred_mcp() {
            return;
        }
        self.seed_announced_mcp_tools_from_transcript();
        let current = self.build_filtered_tool_definitions_raw().await;
        let fresh: Vec<ToolDefinition> = current
            .into_iter()
            .filter(|tool| tool.name.starts_with("mcp__") && !self.announced_mcp_tools.contains(&tool.name))
            .collect();
        if fresh.is_empty() {
            return;
        }
        let instruction = if self.mcp_surface_listed() {
            "call them through toolbox: action \"run\", tool \"mcp_call\", args with server, tool \
             and arguments matching input_schema (use tool \"mcp_search\" to find them by description)"
        } else {
            "call them with mcp_call (server, tool, arguments matching input_schema)"
        };
        let mut text = format!(
            "<system-reminder>\nNew MCP tools are available. They are not in your tool list; \
             {instruction}.\n"
        );
        for tool in fresh.iter().take(MAX_ANNOUNCED_MCP_TOOLS) {
            let (server, raw) = self.registry.mcp_identity_for_alias(&tool.name).unwrap_or_else(|| {
                let (server, raw) = split_mcp_dispatch_name(&tool.name);
                (server.to_string(), raw.to_string())
            });
            text.push_str(&format!(
                "- {}: server: {server}  tool: {raw}  ({})\n  input_schema: {}\n",
                tool.name,
                tool.description.trim(),
                serde_json::to_string(&tool.input_schema).unwrap_or_else(|_| "{}".to_string()),
            ));
        }
        if fresh.len() > MAX_ANNOUNCED_MCP_TOOLS {
            let more = fresh.len() - MAX_ANNOUNCED_MCP_TOOLS;
            if self.mcp_surface_listed() {
                text.push_str(&format!(
                    "... and {more} more; use toolbox (action \"run\", tool \"mcp_search\") to find them.\n"
                ));
            } else {
                text.push_str(&format!("... and {more} more; use mcp_search to find them.\n"));
            }
        }
        text.push_str("</system-reminder>");
        for tool in &fresh {
            self.announced_mcp_tools.insert(tool.name.clone());
        }
        logging::info(&format!(
            "Announcing {} late MCP tool(s) in the transcript (tool list stays cache-stable)",
            fresh.len()
        ));
        self.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text,
                cache_control: None,
            }],
        );
    }

    /// Announce skills installed after the session's system prompt was built.
    ///
    /// The "Available Skills" section is frozen per session (see
    /// `prompt_skills_snapshot`) because rewriting the system prompt forces a
    /// KV cache miss on the whole conversation. New skills are instead
    /// described once, in a note appended before the next user message.
    pub(crate) fn announce_late_skills(&mut self) {
        if self.session.system_prompt.is_some() {
            return;
        }
        self.seed_announced_skills_from_transcript();
        let fresh: Vec<crate::prompt::SkillInfo> = self
            .current_prompt_skill_infos()
            .into_iter()
            .filter(|skill| !self.announced_skills.contains(&skill.name))
            .collect();
        if fresh.is_empty() {
            return;
        }
        let mut text = format!("<system-reminder>\n{LATE_SKILL_ANNOUNCEMENT_MARKER} ");
        text.push_str(
            "They are not in the Available Skills list above but can be invoked with `/skillname` \
             the same way:\n",
        );
        for skill in &fresh {
            let description = skill.description.split_whitespace().collect::<Vec<_>>().join(" ");
            text.push_str(&format!("- `/{} ` - {}\n", skill.name, description));
        }
        text.push_str("</system-reminder>");
        for skill in &fresh {
            self.announced_skills.insert(skill.name.clone());
        }
        logging::info(&format!(
            "Announcing {} late skill(s) in the transcript (system prompt stays cache-stable)",
            fresh.len()
        ));
        self.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text,
                cache_control: None,
            }],
        );
        self.announced_skills_scan_index = self.session.active_context().len();
    }

    /// Mark skills named in earlier announcements as announced, so a
    /// restored session does not describe them again.
    fn seed_announced_skills_from_transcript(&mut self) {
        let start = self
            .announced_skills_scan_index
            .min(self.session.active_context().len());
        for message in &self.session.active_context()[start..] {
            for block in &message.content {
                if let ContentBlock::Text { text, .. } = block
                    && text.contains(LATE_SKILL_ANNOUNCEMENT_MARKER)
                {
                    for line in text.lines() {
                        if let Some(name) = line
                            .strip_prefix("- `/")
                            .and_then(|rest| rest.split(' ').next())
                            .filter(|name| !name.is_empty())
                        {
                            self.announced_skills.insert(name.to_string());
                        }
                    }
                }
            }
        }
        self.announced_skills_scan_index = self.session.active_context().len();
    }

    /// Mark MCP tools the transcript already describes as announced: tools
    /// loaded by a tool reference (`mcp connect` / `mcp_search` results carry
    /// their schemas) and tools named in earlier announcements (which matters
    /// after a session restore, when the in-memory set starts empty).
    fn seed_announced_mcp_tools_from_transcript(&mut self) {
        let start = self.announced_mcp_scan_index.min(self.session.active_context().len());
        for message in &self.session.active_context()[start..] {
            for block in &message.content {
                match block {
                    ContentBlock::ToolReference { tool_name, .. } => {
                        self.announced_mcp_tools.insert(tool_name.clone());
                    }
                    ContentBlock::Text { text, .. } if text.contains(LATE_MCP_ANNOUNCEMENT_MARKER) => {
                        for line in text.lines() {
                            if let Some(name) = line
                                .strip_prefix("- ")
                                .and_then(|rest| rest.split(':').next())
                                .filter(|name| name.starts_with("mcp__"))
                            {
                                self.announced_mcp_tools.insert(name.to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        self.announced_mcp_scan_index = self.session.active_context().len();
    }

    /// Returns true if the registry contains `mcp__*` tools (subject to the
    /// session's `allowed_tools` filter) that are not present in the currently
    /// locked snapshot. Used to detect the async MCP-registration race (#206).
    async fn registry_has_new_mcp_tools(&self, locked: &[ToolDefinition]) -> bool {
        let registry_names = self.registry.tool_names().await;
        let allowed = self.allowed_tools.as_ref();
        registry_names.iter().any(|name| {
            name.starts_with("mcp__")
                && allowed
                    .map(|set| self.registry.tool_is_allowed(set, name))
                    .unwrap_or(true)
                && !self.registry.tool_is_disabled(&self.disabled_tools, name)
                && !locked.iter().any(|t| &t.name == name)
        })
    }

    pub(crate) fn invalidate_sdk_tools(&mut self) {
        self.mcp_late_register_resolved = false;
        self.locked_tools = None;
        self.cache_tracker.reset();
        self.kv_cache_monitor.reset();
    }

    pub(crate) async fn set_desktop_response_capability(&mut self, enabled: bool) {
        let was_enabled = self.registry.tool_names().await.iter().any(|n| n == "user_response");
        if was_enabled != enabled {
            self.registry.set_user_response_enabled(enabled).await;
            self.invalidate_sdk_tools();
        }
    }

    pub async fn tool_names(&self) -> Vec<String> {
        self.tool_definitions_for_debug()
            .await
            .into_iter()
            .map(|tool| tool.name)
            .collect()
    }

    /// Per-server MCP tool names the session can see, for the client header's
    /// `mcp:` inventory.
    ///
    /// Equivalent to filtering [`Self::tool_names`] for `mcp__*`, but reads only
    /// registry keys: building full definitions (every schema, sorted, SDK
    /// overlay applied) just to count names took up to ~40ms on the bootstrap
    /// History path that gates the client's first settled frame.
    pub async fn visible_mcp_tool_names(&self) -> Vec<String> {
        // Mirrors `apply_mcp_tool_exposure`: on the fixed `mcp_search` /
        // `mcp_call` surface per-server definitions are not exposed.
        if !self.native_deferred_mcp() && !matches!(self.mcp_tools_mode, crate::config::McpToolsMode::Eager) {
            return Vec::new();
        }
        let sdk = crate::tool::sdk::config(&self.session.id);
        let enabled: Option<HashSet<String>> = sdk
            .as_ref()
            .and_then(|c| c.enabled.as_ref())
            .map(|names| names.iter().cloned().collect());
        let allowed = enabled.as_ref().or(self.allowed_tools.as_ref());
        let sdk_disabled: HashSet<String> = sdk
            .as_ref()
            .map(|c| c.disabled.iter().cloned().collect())
            .unwrap_or_default();
        let mut names: Vec<String> = self
            .registry
            .tool_names()
            .await
            .into_iter()
            .filter(|name| name.starts_with("mcp__"))
            .filter(|name| allowed.is_none_or(|set| self.registry.tool_is_allowed(set, name)))
            .filter(|name| {
                enabled.is_some()
                    || self.disabled_tools.is_empty()
                    || !self.registry.tool_is_disabled(&self.disabled_tools, name)
            })
            .filter(|name| sdk_disabled.is_empty() || !self.registry.tool_is_disabled(&sdk_disabled, name))
            .collect();
        names.sort();
        names
    }

    /// Get full tool definitions for debug introspection (bypasses lock)
    pub async fn tool_definitions_for_debug(&self) -> Vec<crate::message::ToolDefinition> {
        self.build_filtered_tool_definitions().await
    }

    pub async fn execute_tool(&self, name: &str, input: serde_json::Value) -> Result<crate::tool::ToolOutput> {
        self.ensure_resume_persistence_settled()?;
        self.validate_tool_allowed(name)?;

        let call_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| format!("debug-{}", d.as_millis()))
            .unwrap_or_else(|_| "debug".to_string());
        let ctx = ToolContext {
            session_id: self.session.id.clone(),
            message_id: self.session.id.clone(),
            tool_call_id: call_id,
            working_dir: self.working_dir().map(PathBuf::from),
            stdin_request_tx: self.stdin_request_tx.clone(),
            graceful_shutdown_signal: Some(self.graceful_shutdown.clone()),
            execution_mode: ToolExecutionMode::Direct,
        };
        self.registry.execute(name, input, ctx).await
    }

    pub fn add_manual_tool_use(
        &mut self,
        tool_call_id: String,
        tool_name: String,
        input: serde_json::Value,
    ) -> Result<String> {
        let message_id = self.add_message(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: tool_call_id,
                name: tool_name,
                input,
                thought_signature: None,
            }],
        );
        self.save_session()?;
        Ok(message_id)
    }

    pub fn add_manual_tool_result(
        &mut self,
        tool_call_id: String,
        output: crate::tool::ToolOutput,
        duration_ms: u64,
    ) -> Result<()> {
        let blocks = tool_output_to_history_blocks(tool_call_id, output, &self.session.id);
        self.add_message_with_duration(Role::User, blocks, Some(duration_ms));
        self.save_session()?;
        Ok(())
    }

    pub fn add_manual_tool_error(&mut self, tool_call_id: String, error: String, duration_ms: u64) -> Result<()> {
        self.add_message_with_duration(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: tool_call_id,
                content: error,
                is_error: Some(true),
                verdict: Some(crate::tool::ToolVerdict::Failed),
            }],
            Some(duration_ms),
        );
        self.save_session()?;
        Ok(())
    }

    pub(super) fn validate_tool_allowed(&self, name: &str) -> Result<()> {
        let unqualified_name = name.strip_prefix("functions.").unwrap_or(name);
        let is_custom = crate::tool::sdk::custom(&self.session.id, unqualified_name);
        let name = if is_custom {
            unqualified_name
        } else {
            Registry::resolve_tool_name(unqualified_name)
        };
        // Plan mode runs first so the SDK branches below cannot return early
        // past it: an SDK-enabled or callback tool is exactly the kind of call
        // whose effects this path cannot see, and the registry path refuses it
        // for the same reason. The dispatcher is the one exception, because its
        // reach is the reach of the tool it runs: `toolbox list`/`describe` are
        // read-only and `toolbox run` is refused by the inner registry call with
        // the target's own effects. An SDK callback whose name happens to be
        // `toolbox` is not the dispatcher and keeps the check.
        if is_custom || name != crate::config::TOOLBOX_TOOL {
            let effects = self
                .registry
                .declared_effects_sync(name)
                .unwrap_or(crate::tool::ToolEffects::write());
            if !crate::plan_mode::allows_effects(&self.session.id, effects) {
                return Err(anyhow::anyhow!(crate::plan_mode::block_message(name)));
            }
        }
        let mut sdk_enabled = false;
        if let Some(config) = crate::tool::sdk::config(&self.session.id) {
            let disabled = config.disabled.into_iter().collect();
            anyhow::ensure!(
                !self.registry.tool_is_disabled(&disabled, name),
                "Tool '{}' is disabled",
                name
            );
            if config.custom.iter().any(|t| t.name == name) {
                return Ok(());
            }
            if let Some(enabled) = config.enabled {
                let allowed = enabled.into_iter().collect();
                anyhow::ensure!(
                    self.registry.tool_is_allowed(&allowed, name),
                    "Tool '{}' is not allowed",
                    name
                );
                sdk_enabled = true;
            }
        }
        if sdk_enabled {
            return Ok(());
        }
        if let Some(allowed) = self.allowed_tools.as_ref()
            && !self.registry.tool_is_allowed(allowed, name)
        {
            return Err(anyhow::anyhow!("Tool '{}' is not allowed", name));
        }
        if self.registry.tool_is_disabled(&self.disabled_tools, name) {
            return Err(anyhow::anyhow!("Tool '{}' is disabled", name));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn validate_tool_allowed_for_test(&self, name: &str) -> Result<()> {
        self.validate_tool_allowed(name)
    }

    /// Restore a session by ID (loads from disk)
    pub fn restore_session(&mut self, session_id: &str) -> Result<SessionStatus> {
        self.restore_session_with_working_dir(session_id, None)
    }

    /// Restore and immediately persist, for callers that do not deliver History first.
    pub(crate) fn restore_session_with_working_dir(
        &mut self,
        session_id: &str,
        working_dir: Option<&str>,
    ) -> Result<SessionStatus> {
        let status = self.prepare_restored_session(session_id, working_dir)?;
        self.finish_resume_persistence()?;
        Ok(status)
    }

    /// Load a session and prepare everything a resumed turn needs, without saving.
    ///
    /// The resumed state stays non-durable until [`Self::finish_resume_persistence`]
    /// runs, and every external entry point refuses to start meanwhile. That is what
    /// lets an attachment deliver the already-saved transcript before paying for the
    /// write, while a new turn can never run ahead of the commit it depends on.
    pub(crate) fn prepare_restored_session(
        &mut self,
        session_id: &str,
        working_dir: Option<&str>,
    ) -> Result<SessionStatus> {
        let restore_start = Instant::now();
        let load_start = Instant::now();
        // Flush the outgoing session before reading the replacement. Closing it
        // afterwards writes its close state behind the replacement's snapshot,
        // and that tail is what the fresh facade refuses as a source conflict
        // ("session originals changed since this facade was loaded"). The load
        // still decides whether the Agent is closed, so a failed load leaves it
        // alive exactly as before.
        if !self.session.active_context().is_empty() {
            self.persist_session_best_effort("session close state");
        }
        let mut session = Session::load(session_id)?;
        if let Some(working_dir) = working_dir {
            session.working_dir = Some(working_dir.to_string());
            session.refresh_initial_session_context_message();
        }
        let load_ms = load_start.elapsed().as_millis();
        logging::info(&format!(
            "Restoring session '{}' with {} messages, provider_session_id: {:?}, status: {}",
            session_id,
            session.active_context().len(),
            session.provider_session_id,
            session.status.display()
        ));
        let previous_status = session.status.clone();

        let assign_start = Instant::now();
        // The outgoing state is already durable, so close without a second
        // write: the restore commit owns this session's next write.
        self.close_session_binding();
        self.fire_session_lifecycle_hook("session_end", "close");
        // Restore provider_session_id for Claude CLI session resume
        self.provider_session_id = session.provider_session_id.clone();
        self.install_restored_session(session);
        // The replacement session resolves its own allow-list, tiers, and SDK
        // overlay, so the listed-tools index has to be built again with them.
        self.reset_listed_tools_index();
        self.refresh_agents_md_snapshot();
        self._tool_policy_registration = crate::tool::register_session_tool_policy(
            &self.session.id,
            self.allowed_tools.clone(),
            self.disabled_tools.clone(),
        );
        crate::tool::register_session_load_tiers(&self.session.id, self.load_tiers.clone());
        // A restored session resolves its MCP fixed surface from the registry and
        // configuration it is attached to now, so the previous decision does not
        // carry over. A conversation reset keeps it: the session and its tool
        // list are unchanged, so the cached prefix still holds.
        *self
            .mcp_surface_eager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        crate::tool::register_session_listed_mcp(&self.session.id, false);
        let assign_ms = assign_start.elapsed().as_millis();

        let reset_start = Instant::now();
        self.reset_runtime_state_for_session_change();
        let restored_soft_interrupts = self.restore_persisted_soft_interrupts();
        let reset_ms = reset_start.elapsed().as_millis();

        let model_start = Instant::now();
        self.restore_session_model_from_session();
        self.restore_reasoning_effort_from_session();
        let model_ms = model_start.elapsed().as_millis();

        let mark_active_start = Instant::now();
        self.session.mark_active();
        let mark_active_ms = mark_active_start.elapsed().as_millis();
        self.sync_memory_dedup_state_from_session();

        logging::info(&format!(
            "restore_session: loaded session {} with {} messages, calling seed_compaction",
            session_id,
            self.session.active_context().len()
        ));
        let compaction_start = Instant::now();
        self.seed_compaction_from_session();
        let compaction_ms = compaction_start.elapsed().as_millis();

        // The commit is deferred, so the barrier closes here; only the resume
        // commit (`finish_resume_persistence`, or any save that funnels into
        // it) opens it again.
        self.begin_pending_resume();

        logging::info(&format!(
            "[TIMING] restore_prepare: session={}, messages={}, restored_soft_interrupts={}, load={}ms, assign={}ms, reset={}ms, model={}ms, mark_active={}ms, compaction={}ms, total={}ms",
            session_id,
            self.session.active_context().len(),
            restored_soft_interrupts,
            load_ms,
            assign_ms,
            reset_ms,
            model_ms,
            mark_active_ms,
            compaction_ms,
            restore_start.elapsed().as_millis(),
        ));
        logging::info(&format!(
            "Session restored: {} messages in session",
            self.session.active_context().len()
        ));
        Ok(previous_status)
    }

    /// Persist the state a restored session prepared, and announce it once durable.
    ///
    /// Resume folds the environment event and the restored state into one commit,
    /// recording the event here (not while preparing) so its git probe runs after
    /// the attachment has delivered History. `session_start` is that commit's
    /// success signal: a failed save must not announce a session whose state did
    /// not land. The barrier stays closed on failure, because work on a
    /// non-durable restore is what this forbids.
    pub(crate) fn finish_resume_persistence(&mut self) -> Result<()> {
        let Some(session_id) = self.pending_resume.as_ref().map(|pending| pending.session_id.clone()) else {
            return Ok(());
        };
        let env_snapshot_start = Instant::now();
        if let Some(pending) = self.pending_resume.as_mut()
            && !pending.env_snapshot_recorded
        {
            // Mark first: a failed save keeps the recorded event in memory, and the
            // retried commit must persist it once, not twice.
            pending.env_snapshot_recorded = true;
            self.prepare_env_snapshot("resume");
        }
        let env_snapshot_ms = env_snapshot_start.elapsed().as_millis();
        let save_start = Instant::now();
        let saved = self.session.save();
        let save_ms = save_start.elapsed().as_millis();
        if let Err(err) = &saved {
            logging::error(&format!(
                "Failed to persist resumed session state for {}: {}",
                session_id, err
            ));
        }
        #[cfg(test)]
        resume_probe::note_save(&self.session.id, saved.is_ok());
        if let Err(error) = saved {
            // Keep refusing external work: the state this session needs is absent.
            return Err(error.context(super::ResumeCommitFailed { session_id }));
        }
        self.clear_pending_resume();
        #[cfg(test)]
        resume_probe::note_session_start(&self.session.id);
        self.fire_session_lifecycle_hook("session_start", "resume");
        logging::info(&format!(
            "[TIMING] restore_persist: session={}, env_snapshot={}ms, save={}ms",
            session_id, env_snapshot_ms, save_ms
        ));
        Ok(())
    }

    /// Refuse new external work while a restored session's state is not durable.
    ///
    /// Only the `&self` tool entry uses this; `&mut self` entries commit instead.
    pub(crate) fn ensure_resume_persistence_settled(&self) -> Result<()> {
        match &self.pending_resume {
            Some(pending) => anyhow::bail!(
                "Session '{}' was restored but its access state is not saved yet; retry after it lands",
                pending.session_id
            ),
            None => Ok(()),
        }
    }

    /// Get conversation history for sync
    pub fn get_history(&self, include_message_ids: bool) -> Vec<HistoryMessage> {
        crate::session::render_messages(&self.session)
            .into_iter()
            .map(|msg| {
                crate::server::foreground_state::history_message(
                    msg,
                    self.session.active_context(),
                    include_message_ids,
                )
            })
            .collect()
    }

    pub fn get_history_and_rendered_images(
        &self,
        include_message_ids: bool,
    ) -> (Vec<HistoryMessage>, Vec<crate::session::RenderedImage>) {
        let (messages, images) = crate::session::render_messages_and_images(&self.session);
        let history = messages
            .into_iter()
            .map(|msg| {
                crate::server::foreground_state::history_message(
                    msg,
                    self.session.active_context(),
                    include_message_ids,
                )
            })
            .collect();
        (history, images)
    }

    pub fn get_history_and_rendered_images_with_compacted_history(
        &self,
        compacted_history_visible: usize,
        include_message_ids: bool,
    ) -> (
        Vec<HistoryMessage>,
        Vec<crate::session::RenderedImage>,
        Option<crate::session::RenderedCompactedHistoryInfo>,
    ) {
        let (messages, images, compacted_info) =
            crate::session::render_messages_and_images_with_compacted_history(&self.session, compacted_history_visible);
        let history = messages
            .into_iter()
            .map(|msg| {
                crate::server::foreground_state::history_message(
                    msg,
                    self.session.active_context(),
                    include_message_ids,
                )
            })
            .collect();
        (history, images, compacted_info)
    }

    pub fn get_tool_call_summaries(&self, limit: usize) -> Vec<crate::protocol::ToolCallSummary> {
        crate::session::summarize_tool_calls(&self.session, limit)
    }

    /// Start an interactive REPL
    pub async fn repl(&mut self) -> Result<()> {
        println!("horon-agent - Coding Agent");
        println!("Type your message, or 'quit' to exit.");

        // Show available skills
        let skills = self.current_skills_snapshot();
        let skill_list = skills.list();
        if !skill_list.is_empty() {
            println!(
                "Available skills: {}",
                skill_list
                    .iter()
                    .map(|s| format!("/{}", s.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        println!();

        loop {
            print!("> ");
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;

            let input = input.trim();
            if input.is_empty() {
                continue;
            }

            if input == "quit" || input == "exit" {
                break;
            }

            if input == "clear" {
                self.clear();
                println!("Conversation cleared.");
                continue;
            }

            // Check for skill invocation. Resolve against the registry (not
            // the bare tokenizer) so a `SKILL.md` `name:` field containing
            // spaces, e.g. "My Custom Skill", can still be matched: the
            // bare parse always stops at the first whitespace.
            if let Some(invocation) = skills.resolve_invocation(input) {
                if let Some(skill) = skills.get(invocation.name) {
                    println!("Activating skill: {}", skill.name);
                    println!("{}\n", skill.description);
                    self.active_skill = Some(invocation.name.to_string());
                    if let Some(prompt) = invocation.prompt {
                        self.arm_effort_keywords(prompt);
                        if let Err(e) = self.run_once(prompt).await {
                            eprintln!("\nError: {}\n", e);
                        }
                        println!();
                    }
                    continue;
                } else {
                    println!("Unknown skill: /{}", invocation.name);
                    println!(
                        "Available: {}",
                        skills
                            .list()
                            .iter()
                            .map(|s| format!("/{}", s.name))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    continue;
                }
            }

            self.arm_effort_keywords(input);
            if let Err(e) = self.run_once(input).await {
                eprintln!("\nError: {}\n", e);
            }

            println!();
        }

        // Extract memories from session before exiting
        self.extract_session_memories().await;

        Ok(())
    }

    /// Extract memories from the session transcript
    /// Returns the number of memories extracted, or 0 if none/skipped
    pub async fn extract_session_memories(&self) -> usize {
        if !self.memory_enabled {
            return 0;
        }

        // Need at least 4 messages for meaningful extraction
        if self.session.active_context().len() < 4 {
            return 0;
        }

        logging::info(&format!(
            "Extracting memories from {} messages",
            self.session.active_context().len()
        ));

        // Build transcript
        let mut transcript = String::new();
        for msg in self.session.active_context() {
            let role = match msg.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
            };
            transcript.push_str(&format!("**{}:**\n", role));
            for block in &msg.content {
                match block {
                    ContentBlock::Text { text, .. } => {
                        if text.trim_start().starts_with("<system-reminder>") {
                            continue;
                        }
                        transcript.push_str(text);
                        transcript.push('\n');
                    }
                    ContentBlock::ToolUse { name, .. } => {
                        transcript.push_str(&format!("[Used tool: {}]\n", name));
                    }
                    ContentBlock::ToolResult { content, .. } => {
                        let preview = if content.len() > 200 {
                            format!("{}...", crate::util::truncate_str(content, 200))
                        } else {
                            content.clone()
                        };
                        transcript.push_str(&format!("[Result: {}]\n", preview));
                    }
                    ContentBlock::Reasoning { .. }
                    | ContentBlock::ReasoningTrace { .. }
                    | ContentBlock::AnthropicThinking { .. }
                    | ContentBlock::OpenAIReasoning { .. }
                    | ContentBlock::ToolReference { .. }
                    | ContentBlock::ProviderNative { .. } => {}
                    ContentBlock::Image { .. } => transcript.push_str("[Image]\n"),
                    ContentBlock::Audio { .. } => transcript.push_str("[audio]\n"),
                    ContentBlock::OpenAICompaction { .. } => {
                        transcript.push_str("[OpenAI native compaction]\n");
                    }
                }
            }
            transcript.push('\n');
        }

        if !crate::memory::memory_llm_judge_available() {
            logging::info("Memory extraction skipped: LLM judge unavailable");
            return 0;
        }

        // Extract using sidecar
        let sidecar = crate::sidecar::Sidecar::for_session(&self.session);
        match sidecar.extract_memories(&transcript).await {
            Ok(extracted) if !extracted.is_empty() => {
                let manager = self
                    .session
                    .working_dir
                    .as_deref()
                    .map(|dir| crate::memory::MemoryManager::new().with_project_dir(dir))
                    .unwrap_or_default();
                let mut stored_count = 0;

                for memory in &extracted {
                    let category = crate::memory::MemoryCategory::from_extracted(&memory.category);

                    let trust = match memory.trust.as_str() {
                        "high" => crate::memory::TrustLevel::High,
                        "low" => crate::memory::TrustLevel::Low,
                        _ => crate::memory::TrustLevel::Medium,
                    };

                    let entry = crate::memory::MemoryEntry::new(category, &memory.content)
                        .with_source(&self.session.id)
                        .with_trust(trust);

                    if manager.remember_project(entry).is_ok() {
                        stored_count += 1;
                    }
                }

                if stored_count > 0 {
                    logging::info(&format!("Extracted {} memories from session", stored_count));
                }
                stored_count
            }
            Ok(_) => 0,
            Err(e) => {
                logging::info(&format!("Memory extraction skipped: {}", e));
                0
            }
        }
    }
}

/// Cap on tools listed in one late-MCP transcript announcement.
const MAX_ANNOUNCED_MCP_TOOLS: usize = 32;

/// Split a registry key `mcp__<server>__<tool>` into (server, tool).
/// Server names cannot contain `__`, so the first separator is the split.
fn split_mcp_dispatch_name(name: &str) -> (&str, &str) {
    name.strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__"))
        .unwrap_or(("", name))
}

/// Stable header of the late-MCP transcript announcement; used to recognize
/// earlier announcements when a session is restored.
const LATE_MCP_ANNOUNCEMENT_MARKER: &str = "New MCP tools are available.";

/// Stable header of the late-skill transcript announcement; used to recognize
/// earlier announcements when a session is restored.
const LATE_SKILL_ANNOUNCEMENT_MARKER: &str = "New skills were installed.";

#[cfg(test)]
#[path = "turn_token_usage_tests.rs"]
mod turn_token_usage_tests;

#[cfg(test)]
#[path = "turn_execution_resume_tests.rs"]
mod turn_execution_resume_tests;

/// Test-only record of what a resume attempt did, because the two facts the
/// resume invariant turns on (the commit landed, the hook followed it) leave no
/// other observable trace inside this crate.
#[cfg(test)]
pub(super) mod resume_probe {
    use std::sync::Mutex;

    #[derive(Clone, Debug)]
    pub(super) struct ResumeAttempt {
        pub session_id: String,
        pub save_succeeded: Option<bool>,
        pub session_start_fired: bool,
    }

    static ATTEMPTS: Mutex<Vec<ResumeAttempt>> = Mutex::new(Vec::new());

    pub(super) fn note_save(session_id: &str, save_succeeded: bool) {
        if let Ok(mut attempts) = ATTEMPTS.lock() {
            attempts.push(ResumeAttempt {
                session_id: session_id.to_string(),
                save_succeeded: Some(save_succeeded),
                session_start_fired: false,
            });
        }
    }

    pub(super) fn note_session_start(session_id: &str) {
        if let Ok(mut attempts) = ATTEMPTS.lock()
            && let Some(attempt) = attempts
                .iter_mut()
                .rev()
                .find(|attempt| attempt.session_id == session_id)
        {
            attempt.session_start_fired = true;
        }
    }

    pub(super) fn attempts_for(session_id: &str) -> Vec<ResumeAttempt> {
        ATTEMPTS
            .lock()
            .map(|attempts| {
                attempts
                    .iter()
                    .filter(|attempt| attempt.session_id == session_id)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}
