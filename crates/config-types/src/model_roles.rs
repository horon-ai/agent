//! Model roles and the per-profile model fallback list.
//!
//! A role is a named workload-to-model mapping inside one provider profile
//! (`[providers.<name>.roles]`). Roles never select a second profile, endpoint
//! or credential; the resolution order below is the single chain every caller
//! uses, as described in `docs/provider/MODEL.md` under `### Resolution order`.

use serde::{Deserialize, Serialize};

#[cfg(test)]
#[path = "model_roles_tests.rs"]
mod tests;

/// A workload that can name its own model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelRole {
    /// Every turn of the session unless a more specific role applies.
    Default,
    /// Planning turns, written before execution hands off.
    Plan,
    /// Cheap auxiliary work: compaction summaries, titles, classification.
    Aux,
    /// Typed judgments: a calibrated probability, label or level instead of
    /// free text. Falls back to `aux` first because a judgment is the same
    /// class of cheap side task, then to `default`.
    Judge,
}

impl ModelRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Plan => "plan",
            Self::Aux => "aux",
            Self::Judge => "judge",
        }
    }

    /// The roles this role inherits from, closest first, excluding itself.
    ///
    /// Resolving through this list is what keeps one chain in
    /// [`resolve_role_model`]: a role only declares which other role entries
    /// stand in for it, and the shared function keeps the order.
    pub fn fallback_roles(self) -> &'static [Self] {
        match self {
            Self::Default => &[],
            Self::Plan => &[Self::Default],
            Self::Aux => &[Self::Default],
            Self::Judge => &[Self::Aux, Self::Default],
        }
    }
}

impl std::fmt::Display for ModelRole {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The `[providers.<name>.roles]` table.
///
/// An unset entry inherits from the next step of the resolution chain instead of
/// failing, so a profile without a role table resolves every role to the model
/// this build already picks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelRoles {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aux: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub judge: Option<String>,
}

impl ModelRoles {
    pub fn is_empty(&self) -> bool {
        self.default.is_none() && self.plan.is_none() && self.aux.is_none() && self.judge.is_none()
    }

    fn value_for(&self, role: ModelRole) -> Option<&str> {
        match role {
            ModelRole::Default => self.default.as_deref(),
            ModelRole::Plan => self.plan.as_deref(),
            ModelRole::Aux => self.aux.as_deref(),
            ModelRole::Judge => self.judge.as_deref(),
        }
    }
}

/// The role keys and the model fallback list of one provider profile.
///
/// It is flattened into `NamedProviderConfig`, so both keys sit directly on
/// `[providers.<name>]` and a configuration file that mentions neither
/// serializes and resolves exactly as before.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelRoleConfig {
    /// Ordered model ids tried after the retry budget of a retryable error is
    /// spent, once per entry, before account and provider failover.
    #[serde(default, alias = "fallback-models", skip_serializing_if = "Vec::is_empty")]
    pub fallback_models: Vec<String>,
    #[serde(default, skip_serializing_if = "ModelRoles::is_empty")]
    pub roles: ModelRoles,
}

impl ModelRoleConfig {
    /// The fallback entries that name a model, with surrounding whitespace removed.
    ///
    /// Blank entries are dropped rather than attempted: an empty string is
    /// "unset" everywhere else in this configuration too.
    pub fn fallback_models(&self) -> Vec<&str> {
        self.fallback_models
            .iter()
            .map(|model| model.trim())
            .filter(|model| !model.is_empty())
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.fallback_models.is_empty() && self.roles.is_empty()
    }
}

/// Why a role could not be resolved to a bare model id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleResolutionError {
    /// A role entry carried a route prefix such as `openai:` instead of a bare
    /// model id. A role must not move the request to another profile.
    RoutePrefix {
        role: ModelRole,
        value: String,
        prefix: String,
    },
    /// Every step of the chain was empty.
    Unresolved { role: ModelRole },
}

impl std::fmt::Display for RoleResolutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RoutePrefix { role, value, prefix } => write!(
                formatter,
                "model role `{role}` must name a bare model id inside its provider profile, but `{value}` carries the route prefix `{prefix}:`",
            ),
            Self::Unresolved { role } => {
                let inherited = role
                    .fallback_roles()
                    .iter()
                    .map(|role| format!("`roles.{role}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let inherited = if inherited.is_empty() {
                    String::new()
                } else {
                    format!("{inherited}, ")
                };
                write!(
                    formatter,
                    "no model resolved for role `{role}`: the run override, `roles.{role}`, {inherited}the profile `default_model`, and `[provider] default_model` were all empty",
                )
            }
        }
    }
}

impl std::error::Error for RoleResolutionError {}

/// Resolve the model a role asks for.
///
/// The first step that yields a non-empty value wins: the run override, the
/// role's own entry, each role in [`ModelRole::fallback_roles`] in order, the
/// profile `default_model`, then `[provider] default_model`. An empty string
/// counts as unset at every step, and a step that resolves nothing ends in an
/// error naming the role.
///
/// Only the `roles` steps refuse a route prefix. The other three are the
/// chain this build already resolves, where a `--model` value such as
/// `<profile>:<model>` is consumed by the existing prefix handling before it
/// reaches a role, so refusing it here would change today's behaviour.
pub fn resolve_role_model(
    role: ModelRole,
    run_override: Option<&str>,
    roles: &ModelRoles,
    profile_default_model: Option<&str>,
    provider_default_model: Option<&str>,
) -> Result<String, RoleResolutionError> {
    if let Some(value) = usable(run_override) {
        return Ok(value.to_string());
    }
    if let Some(value) = usable(roles.value_for(role)) {
        return role_model(role, value);
    }
    // The role's own entry already had its turn above, so the fallback walk
    // never reads it twice: a role that inherits from itself is unrepresentable.
    for inherited in role.fallback_roles() {
        if let Some(value) = usable(roles.value_for(*inherited)) {
            return role_model(role, value);
        }
    }
    if let Some(value) = usable(profile_default_model) {
        return Ok(value.to_string());
    }
    if let Some(value) = usable(provider_default_model) {
        return Ok(value.to_string());
    }
    Err(RoleResolutionError::Unresolved { role })
}

fn role_model(role: ModelRole, value: &str) -> Result<String, RoleResolutionError> {
    match route_prefix(value) {
        Some(prefix) => Err(RoleResolutionError::RoutePrefix {
            role,
            value: value.to_string(),
            prefix: prefix.to_string(),
        }),
        None => Ok(value.to_string()),
    }
}

/// The `<name>:` prefix of a route spec, if the value carries one.
fn route_prefix(value: &str) -> Option<&str> {
    let (prefix, _rest) = value.split_once(':')?;
    let named = !prefix.is_empty()
        && prefix
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'));
    named.then_some(prefix)
}

/// A trimmed value that is not empty; an empty string counts as unset.
fn usable(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
