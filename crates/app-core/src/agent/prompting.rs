use super::Agent;
use crate::logging;
use crate::message::{Message, ToolDefinition};

impl Agent {
    pub(crate) fn runtime_skills_snapshot(&self) -> serde_json::Value {
        let registry = self.current_skills_snapshot();
        let mut loaded = registry.list();
        loaded.sort_by(|a, b| a.name.cmp(&b.name));
        let skills: Vec<_> = loaded
            .iter()
            .map(|skill| {
                serde_json::json!({
                    "name": skill.name, "description": skill.description,
                    "path": skill.path, "allowed_tools": skill.allowed_tools,
                })
            })
            .collect();
        let endorsed: Vec<_> = crate::skill::endorsed_skills()
            .iter()
            .map(|skill| {
                serde_json::json!({
                    "name": skill.name, "description": skill.description, "category": skill.category,
                    "source": skill.source, "install": skill.install, "installed": registry.contains(skill.name),
                })
            })
            .collect();
        serde_json::json!({"skills": skills, "endorsed": endorsed, "active_skill": self.active_skill})
    }

    pub(crate) fn runtime_agent_model_overrides(&self) -> serde_json::Value {
        serde_json::json!(self.session.agent_model_overrides)
    }

    pub(crate) async fn runtime_context_snapshot(&self) -> serde_json::Value {
        let mut context = self.debug_context().await;
        context["messages"] = serde_json::json!(self.session.visible_conversation_messages());
        context["context_window"] = serde_json::json!(self.provider_context_window());
        context
    }

    /// Explicitly prepare/freeze the same tool surface used by provider turns.
    /// Unlike `debug_context`, this may update the tool cache. It never calls a provider.
    pub async fn prepare_debug_context(&mut self) -> serde_json::Value {
        let prepared_tools = self.tool_definitions().await;
        let mut context = self.debug_context().await;
        context["prepared_tools"] = serde_json::json!(&prepared_tools[..]);
        context
    }

    /// Inspect the next request's static context without inference, prewarming,
    /// or locking a new tool snapshot. Pending memory is deliberately not consumed.
    pub async fn debug_context(&self) -> serde_json::Value {
        self.ensure_listed_tools_index().await;
        let prompt = self.build_system_prompt_split(None);
        let current_tools = self.tool_definitions_for_debug().await;
        let effective_tools: &[ToolDefinition] = self.locked_tools.as_deref().unwrap_or(&current_tools);
        let locked_tool_names = self
            .locked_tools
            .as_ref()
            .map(|tools| tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>());
        serde_json::json!({
            "session_id": self.session.id,
            "working_dir": self.session.working_dir,
            "system_prompt": {
                "static": prompt.static_part,
                "dynamic": prompt.dynamic_part,
                "pending_memory_included": false,
            },
            "tools_locked": self.locked_tools.is_some(),
            "locked_tool_names": locked_tool_names,
            "effective_tools": effective_tools,
            "current_tools": current_tools,
        })
    }

    pub(super) fn log_prompt_prefix_accounting(
        &self,
        split: &crate::prompt::SplitSystemPrompt,
        tools: &[ToolDefinition],
    ) {
        let system_tokens = self.system_prompt_token_estimate(split);
        let tool_tokens = ToolDefinition::aggregate_prompt_token_estimate(tools);
        let prefix_tokens = system_tokens + tool_tokens;
        logging::info(&format!(
            "Prompt prefix estimate: total={} tokens (system={} tools={})",
            prefix_tokens, system_tokens, tool_tokens
        ));
    }

    pub(super) fn build_memory_prompt_nonblocking_shared(
        &self,
        messages: std::sync::Arc<[Message]>,
    ) -> Option<crate::memory::PendingMemory> {
        if !self.memory_enabled {
            return None;
        }

        let session_id = &self.session.id;

        let fresh_user_turn = crate::message::ends_with_fresh_user_turn(&messages);

        // Memory reaches a turn two ways, and only one is automatic. The
        // consolidation summary below is the durable, project-scoped half. The
        // other half is the agent's own `memory` tool call: recall is explicit,
        // which is why no background relevance pass is enqueued here. A pass
        // would have to guess the query from the turn and spend a provider call
        // on every user message, and the tool already answers the query the
        // agent actually wrote.
        if fresh_user_turn {
            crate::memory_agent::update_context_sync_with_dir(session_id, messages, self.session.working_dir.clone());
        }

        // The consolidated cross-session summary is the durable half of memory
        // guidance: one bounded digest per project, rewritten by the pass that
        // runs after extraction. It rides in the same injection slot as this
        // turn's relevance payload and adds nothing until a pass has run.
        let summary = if fresh_user_turn {
            self.session.working_dir.as_deref().and_then(|dir| {
                crate::memory::MemoryManager::new()
                    .with_project_dir(dir)
                    .consolidation_summary()
            })
        } else {
            None
        };

        memory_guidance(summary)
    }

    fn append_current_turn_system_reminder(&self, split: &mut crate::prompt::SplitSystemPrompt) {
        // Read through the bounded accessor, so a reminder a writer assigned
        // directly cannot carry more text than the slot's caps allow.
        let Some(reminder) = self.bounded_turn_reminder() else {
            return;
        };

        if !split.dynamic_part.is_empty() {
            split.dynamic_part.push_str("\n\n");
        }
        split.dynamic_part.push_str("# System Reminder\n\n");
        split.dynamic_part.push_str(&reminder);
    }

    /// Append this session's working notes to the turn context.
    ///
    /// They live in the dynamic part, never the cached prefix: the model
    /// rewrites them mid-session, and a prefix that changed every time the
    /// notes did would invalidate the provider's cache on every request.
    fn append_context_notes(&self, split: &mut crate::prompt::SplitSystemPrompt) {
        if !crate::config::config().tools.context_notes {
            return;
        }
        let Ok(Some(notes)) = crate::context_notes::load(&self.session.id) else {
            return;
        };
        let notes = notes.trim();
        if notes.is_empty() {
            return;
        }
        if !split.dynamic_part.is_empty() {
            split.dynamic_part.push_str("\n\n");
        }
        split.dynamic_part.push_str("# Context Notes\n\n");
        split.dynamic_part.push_str(
            "Your working notes for this session (update them with the context_notes tool; keep one line per item and replace stale entries):\n\n",
        );
        split.dynamic_part.push_str(notes);
    }

    /// Build split system prompt for better caching
    /// Returns static (cacheable) and dynamic (not cached) parts separately
    pub(super) fn build_system_prompt_split(&self, memory_prompt: Option<&str>) -> crate::prompt::SplitSystemPrompt {
        // Read the frozen index once: it is the same rows on every turn, and an
        // SDK system-prompt override carries it too, so those sessions keep the
        // tool names a listed session would have.
        let listed_tools = self.listed_tools_section();
        if let Some(ref override_prompt) = self.session.system_prompt {
            let mut static_part = override_prompt.clone();
            if let Some(section) = listed_tools {
                Self::append_static_section(&mut static_part, &section);
            }
            return crate::prompt::SplitSystemPrompt {
                static_part,
                dynamic_part: String::new(),
            };
        }

        let skills = self.current_skills_snapshot();
        // A skill whose file is unreadable drops out of this request's active
        // section instead of failing the whole prompt: the transcript keeps
        // going and one log line names the skill and error.
        let skill_prompt = self.active_skill.as_ref().and_then(|name| {
            let skill = skills.get(name)?;
            match skill.get_prompt() {
                Ok(prompt) => Some(prompt),
                Err(error) => {
                    logging::warn(&format!(
                        "Active skill '{name}' could not be read; omitting it from this request: {error}"
                    ));
                    None
                }
            }
        });

        // Frozen per session so skill installs never rewrite the cached
        // system prefix. Later installs are announced in the transcript.
        let skills_section = self.prompt_skills_section.as_deref();

        let working_dir = self.session.working_dir.as_ref().map(std::path::PathBuf::from);

        let (mut split, _context_info) = crate::prompt::build_system_prompt_split_with_snapshot(
            skill_prompt.as_deref(),
            skills_section,
            memory_prompt,
            working_dir.as_deref(),
            &self.agents_md_snapshot,
            &self.static_prompt_sources,
        );

        self.append_current_turn_system_reminder(&mut split);
        self.append_context_notes(&mut split);
        self.append_pinned_plan(&mut split);
        crate::prompt::append_swarm_effort_directive(&mut split, self.provider.reasoning_effort().as_deref());
        self.append_todo_hint(&mut split);
        if let Some(section) = listed_tools {
            Self::append_static_section(&mut split.static_part, &section);
        }
        self.append_yield_instruction(&mut split);

        split
    }

    /// Append one cacheable system-prompt section, blank-line separated.
    fn append_static_section(static_part: &mut String, section: &str) {
        if !static_part.is_empty() {
            static_part.push_str("\n\n");
        }
        static_part.push_str(section);
    }

    /// The `todo` tool is what an editor renders as the plan, so the prompt only
    /// mentions it when this session can actually call it: `core` and `acp`
    /// allow it, an SDK `enabled` list that omits it does not, and
    /// `[tools.load_mode] todo = "off"` removes it too. The answer follows the
    /// session-start tool selection, so the hint is stable for the session.
    fn append_todo_hint(&self, split: &mut crate::prompt::SplitSystemPrompt) {
        const TODO_TOOL: &str = "todo";
        // An SDK `enabled` list is the client's exact tool surface; a client
        // that did not name `todo` gets no hint about it.
        if crate::tool::sdk::config(&self.session.id).is_some_and(|config| {
            config
                .enabled
                .is_some_and(|enabled| !enabled.iter().any(|tool| tool == TODO_TOOL))
        }) {
            return;
        }
        let allowed = self
            .allowed_tools
            .as_ref()
            .is_none_or(|allowed| crate::tool::tool_name_is_allowed(allowed, TODO_TOOL));
        if !allowed
            || crate::tool::tool_name_is_disabled(&self.disabled_tools, TODO_TOOL)
            || self.load_tiers.tier(TODO_TOOL) == crate::config::LoadMode::Off
        {
            return;
        }
        if !split.static_part.is_empty() {
            split.static_part.push_str("\n\n");
        }
        split.static_part.push_str(crate::prompt::TODO_HINT);
    }

    /// Tell a schema-spawned child how to hand back its structured result.
    ///
    /// Only the child holding a yield slot is told; every other session's
    /// prompt is unchanged. The schema itself reaches the model in the tool
    /// description, so this sentence only has to name the call.
    fn append_yield_instruction(&self, split: &mut crate::prompt::SplitSystemPrompt) {
        if crate::tool::yield_result::schema_for(&self.session.id).is_none() {
            return;
        }
        if !split.dynamic_part.is_empty() {
            split.dynamic_part.push_str("\n\n");
        }
        split
            .dynamic_part
            .push_str(crate::tool::yield_result::YIELD_INSTRUCTION);
    }

    /// Carry an approved plan into every request as its own pinned section.
    ///
    /// It goes in the turn context, never in the static prefix: the plan text
    /// changes mid-session when the user approves one, and putting it in
    /// `static_part` would invalidate the provider's cached prefix at exactly
    /// the moment the session resumes work.
    fn append_pinned_plan(&self, split: &mut crate::prompt::SplitSystemPrompt) {
        let Some(plan) = crate::plan_mode::approved_plan(&self.session.id) else {
            return;
        };
        let plan = plan.trim();
        if plan.is_empty() {
            return;
        }
        if !split.dynamic_part.is_empty() {
            split.dynamic_part.push_str("\n\n");
        }
        split.dynamic_part.push_str("# Approved plan (pinned)\n\n");
        split
            .dynamic_part
            .push_str("The user approved this plan for this session. Execute it as written; it stays in context for the rest of the session.\n\n");
        split.dynamic_part.push_str(plan);
    }

    /// The system prompt this session would send, for debug introspection.
    ///
    /// The static and dynamic parts are concatenated exactly as the provider would receive them,
    /// so a caller can check what the model is told without starting a turn.
    pub fn system_prompt_for_debug(&self) -> String {
        let split = self.build_system_prompt_split(None);
        if split.dynamic_part.is_empty() {
            return split.static_part;
        }
        format!("{}\n\n{}", split.static_part, split.dynamic_part)
    }

    /// This turn's memory guidance; extracted so a test can call it directly.
    #[cfg(test)]
    pub(super) fn build_memory_prompt_nonblocking(&self, messages: &[Message]) -> Option<crate::memory::PendingMemory> {
        self.build_memory_prompt_nonblocking_shared(messages.to_vec().into())
    }
}

/// This turn's memory guidance, or nothing when no digest has been written yet.
///
/// The consolidated cross-session summary is the only automatic memory text a
/// turn carries; relevance comes from the agent's own `memory` tool call, so
/// there is no second payload to fold in.
fn memory_guidance(summary: Option<(String, usize)>) -> Option<crate::memory::PendingMemory> {
    summary.map(|(summary, entries)| crate::memory::PendingMemory {
        prompt: summary,
        display_prompt: None,
        computed_at: std::time::Instant::now(),
        count: entries,
        memory_ids: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_consolidation_summary_is_the_whole_injection() {
        let guidance = memory_guidance(Some(("# Memory Summary".to_string(), 3))).expect("summary is guidance");
        assert_eq!(guidance.prompt, "# Memory Summary");
        assert_eq!(guidance.count, 3, "every memory the digest speaks for is counted");
        assert!(guidance.memory_ids.is_empty(), "a digest names no individual ids");
        assert!(guidance.display_prompt.is_none(), "a digest has no UI rendering");
    }

    #[test]
    fn no_summary_means_no_automatic_memory_text() {
        assert!(
            memory_guidance(None).is_none(),
            "with no digest written yet, a turn carries no memory guidance of its own"
        );
    }
}
