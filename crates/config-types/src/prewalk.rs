//! `[prewalk]`: a one-shot handoff from the planning model to an execution model.
//!
//! The switch happens once per session, at the first successful `todo` write or
//! at plan approval, and it is applied through the same path as `SetModel`. The
//! section is off by default because the handoff trades plan-following quality
//! for cost.

use serde::{Deserialize, Serialize};

/// The `[prewalk]` table.
///
/// `into` has no default: enabling the feature without naming an execution
/// model fails config validation instead of silently picking one, because the
/// model role chain may resolve to the very model the session is already on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrewalkConfig {
    /// Whether a session hands off to `into` after planning.
    pub enabled: bool,
    /// The execution model: a bare model id, or `role:<name>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub into: Option<String>,
    /// Only switch when the session currently runs this model.
    ///
    /// Unlike omp's `from`, this is an applicability condition: it never names
    /// a model to return to, and an unset value applies to every session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

impl PrewalkConfig {
    /// Whether the session hands off to [`Self::into`].
    pub fn is_active(&self) -> bool {
        self.enabled && self.into_model().is_some()
    }

    /// The trimmed `into` value, if one was configured.
    pub fn into_model(&self) -> Option<&str> {
        trimmed(self.into.as_deref())
    }

    /// The trimmed `from` value, if one was configured.
    pub fn from_model(&self) -> Option<&str> {
        trimmed(self.from.as_deref())
    }

    /// Reject an enabled section that names no execution model.
    ///
    /// The message names the key a user has to add, because this error is what
    /// the strict config load reports and it is the only diagnostic they see.
    pub fn validate(&self) -> Result<(), PrewalkConfigError> {
        if !self.enabled {
            return Ok(());
        }
        match self.into_model() {
            Some(_) => Ok(()),
            None => Err(PrewalkConfigError::MissingInto),
        }
    }
}

/// Why a `[prewalk]` section is unusable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrewalkConfigError {
    /// `enabled = true` without a usable `into`.
    MissingInto,
}

impl std::fmt::Display for PrewalkConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingInto => formatter.write_str(
                "`[prewalk] enabled = true` requires `prewalk.into` to name an execution model \
                 (a model id or `role:<name>`); the key is missing or empty",
            ),
        }
    }
}

impl std::error::Error for PrewalkConfigError {}

/// A trimmed value, where blank counts as unset.
///
/// A blank `into` is not "no model": it is a missing key, so validation reports
/// it the same way as an absent one.
fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// Split a configured model value into a role reference or a plain model id.
///
/// Only `role:<name>` is a reference; every other spelling is a model id, so a
/// value such as `openai:gpt-5` keeps working as the model id it already is.
pub fn parse_model_reference(value: &str) -> ModelReference<'_> {
    let value = value.trim();
    match value.strip_prefix("role:") {
        Some(role) => ModelReference::Role(role.trim()),
        None => ModelReference::Model(value),
    }
}

/// A `[prewalk]` model value after parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelReference<'a> {
    /// A plain model id, used verbatim.
    Model(&'a str),
    /// A `role:<name>` reference resolved through the provider profile's roles.
    Role(&'a str),
}
