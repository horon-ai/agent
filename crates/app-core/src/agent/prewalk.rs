//! `[prewalk]`: one handoff from the planning model to an execution model.
//!
//! The switch happens once per session at the first successful `todo` write, or
//! at plan approval when plan mode is on. It uses the same path as `SetModel`,
//! so the session stores the new model and every later request - including
//! after a resume - runs on it. The decision itself is a replay event, which is
//! what makes the handoff one-shot across processes.

use super::*;

use horon_agent_config_types::model_roles::{ModelRole, ModelRoles, resolve_role_model};
use horon_agent_config_types::{ModelReference, PrewalkConfig, parse_model_reference};

/// Why a configured `into` could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PrewalkRefusal {
    /// The value is not a model this profile can serve.
    UnknownModel(String),
    /// A `role:<name>` reference did not resolve.
    UnresolvedRole(String),
    /// `role:<name>` names a role that does not exist.
    UnknownRole(String),
}

impl std::fmt::Display for PrewalkRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownModel(model) => write!(
                formatter,
                "`prewalk.into` names the model `{model}`, which the active provider profile does not offer",
            ),
            Self::UnresolvedRole(role) => write!(
                formatter,
                "`prewalk.into` refers to the model role `{role}`, which resolves to no model",
            ),
            Self::UnknownRole(role) => write!(
                formatter,
                "`prewalk.into` refers to `role:{role}`, but the only roles are `default`, `plan`, and `aux`",
            ),
        }
    }
}

impl Agent {
    /// Apply the `[prewalk]` handoff if this session has reached its trigger.
    ///
    /// Called at the tool-round boundary - before the next provider request - and
    /// at turn start, so an approval that arrived between turns is also caught.
    pub(crate) fn apply_prewalk(&mut self) {
        let config = crate::config::config();
        if !config.prewalk.is_active() {
            return;
        }
        // A session that already decided this, in this process or in the stored
        // file, never decides again.
        if self.session.prewalk_event().is_some() {
            return;
        }
        if !self.prewalk_applies(&config.prewalk) {
            return;
        }
        if !self.prewalk_triggered() {
            return;
        }
        let Some(into) = config.prewalk.into_model().map(str::to_string) else {
            return;
        };
        let current = self.provider_model();
        match self.resolve_prewalk_model(&into, &current) {
            Ok(target) if target == current => {
                // Already the execution model: nothing to switch, but the
                // handoff is spent so later rounds stop checking.
                self.session.record_prewalk_event(
                    Some(&current),
                    Some(&target),
                    Some("already on the execution model"),
                );
                self.persist_session_best_effort("prewalk");
            }
            Ok(target) => match self.set_model(&target) {
                Ok(()) => {
                    self.session.record_prewalk_event(Some(&current), Some(&target), None);
                    self.persist_session_best_effort("prewalk");
                    logging::info(&format!(
                        "prewalk: switched from '{current}' to '{target}' for execution"
                    ));
                }
                Err(error) => {
                    let reason = format!("the switch to `{target}` failed: {error}");
                    self.session
                        .record_prewalk_event(Some(&current), Some(&target), Some(&reason));
                    self.persist_session_best_effort("prewalk");
                    logging::warn(&format!("prewalk: {reason}; the session keeps '{current}'"));
                }
            },
            Err(refusal) => {
                let reason = refusal.to_string();
                self.session
                    .record_prewalk_event(Some(&current), Some(into.as_str()), Some(&reason));
                self.persist_session_best_effort("prewalk");
                logging::warn(&format!("prewalk: {reason}; the session keeps '{current}'"));
            }
        }
    }

    /// Whether `from`, when set, names the model this session currently runs.
    fn prewalk_applies(&self, config: &PrewalkConfig) -> bool {
        let Some(from) = config.from_model() else {
            return true;
        };
        let current = self.provider_model();
        match self.resolve_prewalk_model(from, &current) {
            Ok(expected) => expected == current,
            // An unusable `from` cannot match, so the session keeps its model
            // instead of switching on a condition nobody configured.
            Err(_) => false,
        }
    }

    /// Whether this session reached the handoff point.
    ///
    /// With plan mode on, only an approved plan hands off: a todo write during
    /// planning must not switch the model out from under the plan. With plan
    /// mode off, the first todo write with at least one item does.
    fn prewalk_triggered(&self) -> bool {
        if crate::config::config().tools.plan_mode {
            return matches!(
                crate::plan_mode::state_for(&self.session.id),
                crate::plan_mode::PlanState::Approved
            );
        }
        crate::todo::load_todos(&self.session.id).is_ok_and(|todos| !todos.is_empty())
    }

    /// Resolve a configured `into`/`from` value to a model id this profile serves.
    fn resolve_prewalk_model(&self, value: &str, current: &str) -> Result<String, PrewalkRefusal> {
        let model = match parse_model_reference(value) {
            ModelReference::Model(model) => model.to_string(),
            ModelReference::Role(role) => self.resolve_prewalk_role(role)?,
        };
        let offered = self.available_models_for_switching();
        // A profile without a catalog cannot prove the model unknown, so the
        // switch proceeds the same way an explicit `/model` would.
        if offered.is_empty() || offered.iter().any(|entry| entry == &model) {
            return Ok(model);
        }
        // The session may already run a model the switch list hides (an aliased
        // route); re-selecting it is never a downgrade.
        if model == current {
            return Ok(model);
        }
        Err(PrewalkRefusal::UnknownModel(model))
    }

    /// Resolve `role:<name>` through the active profile's role chain.
    fn resolve_prewalk_role(&self, name: &str) -> Result<String, PrewalkRefusal> {
        let role = match name {
            "default" => ModelRole::Default,
            "plan" => ModelRole::Plan,
            "aux" => ModelRole::Aux,
            other => return Err(PrewalkRefusal::UnknownRole(other.to_string())),
        };
        let config = crate::config::config();
        let profile = active_profile_name().and_then(|name| config.providers.get(&name));
        let empty = ModelRoles::default();
        let roles = profile.map(|profile| &profile.model_roles.roles).unwrap_or(&empty);
        let profile_default = profile.and_then(|profile| profile.default_model.as_deref());
        resolve_role_model(
            role,
            None,
            roles,
            profile_default,
            config.provider.default_model.as_deref(),
        )
        .map_err(|_| PrewalkRefusal::UnresolvedRole(name.to_string()))
    }
}

/// The named provider profile this process is running under, if any.
fn active_profile_name() -> Option<String> {
    [
        "HORON_AGENT_NAMED_PROVIDER_PROFILE",
        "HORON_AGENT_PROVIDER_PROFILE_NAME",
    ]
    .iter()
    .find_map(|key| match std::env::var(key) {
        Ok(value) => Some(value.trim().to_string()).filter(|value| !value.is_empty()),
        Err(_) => None,
    })
}
