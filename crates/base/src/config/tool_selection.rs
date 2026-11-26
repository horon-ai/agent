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

        // The IRC tool and the checkpoint pair are part of the open default
        // surface: they join the `core` profile's list while their switches are
        // on. A list the operator or an editor profile spelled out (`acp`,
        // `minimal`, an explicit `enabled`) stays exactly as written, so naming
        // a tool there is how it joins one. Registration alone never makes them
        // reachable past this allow-list.
        if self.is_core_surface()
            && let Some(allowed) = allowed_tools.as_mut()
        {
            if self.irc {
                allowed.insert(IRC_TOOL.to_string());
            }
            if self.checkpoint {
                for name in CHECKPOINT_TOOLS {
                    allowed.insert((*name).to_string());
                }
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

    /// Whether this configuration is the untouched default `core` surface: the
    /// `core` profile with no list the operator spelled out. Only that surface
    /// is open; every other list stays exactly as written.
    pub fn is_core_surface(&self) -> bool {
        self.normalized_enabled_tools().0.is_empty()
            && !self.disable_base_tools
            && self.profile.trim().eq_ignore_ascii_case(super::CORE_TOOL_PROFILE)
    }
}

#[cfg(test)]
#[path = "irc_tests.rs"]
mod irc_tests;
