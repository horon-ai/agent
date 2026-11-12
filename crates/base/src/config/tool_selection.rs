//! Derives a tool allow-list from the tool and ambient switches.

use super::{
    AmbientConfig, CHECKPOINT_TOOLS, CLIENTLESS_APPROVAL_TOOL, IRC_TOOL, PLAN_TOOL, ToolConfig, ToolSelection,
    normalize_tool_name,
};
use std::collections::HashSet;

impl ToolConfig {
    /// [`Self::selection`] with the ambient settings supplied by the caller.
    ///
    /// `request_permission` is registered by the ambient runner, but registration
    /// alone does not make it reachable: the agent applies this allow-list to
    /// the tool surface and rejects calls outside it. An unattended run that has
    /// no attached client cannot be an ambient session in the old sense, so
    /// `ambient.approval_for_clientless_runs` joins the tool into the computed
    /// allow-list. [`super::CORE_TOOLS`] stays the core set it is today.
    /// An empty or absent allow-list is left alone: `None` already exposes every
    /// registered tool, and an explicit `none` profile must stay empty.
    pub fn selection_with_ambient(&self, ambient: &AmbientConfig) -> ToolSelection {
        let mut allowed_tools = self.base_allowed_tools();
        let disabled_tools: HashSet<String> = self
            .disabled
            .iter()
            .map(|name| normalize_tool_name(name))
            .filter(|name| !name.is_empty())
            .collect();

        if ambient.approval_for_clientless_runs
            && let Some(allowed) = allowed_tools.as_mut()
            && !allowed.is_empty()
        {
            allowed.insert(CLIENTLESS_APPROVAL_TOOL.to_string());
        }

        // Plan mode is registered on the same terms: the tool is registered only
        // when the switch is on, and registration alone does not make it
        // reachable past this allow-list.
        if self.plan_mode
            && let Some(allowed) = allowed_tools.as_mut()
            && !allowed.is_empty()
        {
            allowed.insert(PLAN_TOOL.to_string());
        }

        // The IRC tool follows plan mode: it joins every non-empty allow-list
        // when the switch is on and leaves the empty `none` profile alone.
        if self.irc
            && let Some(allowed) = allowed_tools.as_mut()
            && !allowed.is_empty()
        {
            allowed.insert(IRC_TOOL.to_string());
        }

        // The checkpoint pair follows the same terms: both tools are registered
        // only when the switch is on, and registration alone does not make them
        // reachable past this allow-list.
        if self.checkpoint
            && let Some(allowed) = allowed_tools.as_mut()
            && !allowed.is_empty()
        {
            for name in CHECKPOINT_TOOLS {
                allowed.insert((*name).to_string());
            }
        }

        if let Some(allowed) = allowed_tools.as_mut() {
            for name in &disabled_tools {
                allowed.remove(name);
            }
        }

        ToolSelection {
            allowed_tools,
            disabled_tools,
        }
    }
}

#[cfg(test)]
#[path = "irc_tests.rs"]
mod irc_tests;
