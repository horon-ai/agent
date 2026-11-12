//! Dispatch for typed judgments: which model answers, and one bounded attempt.
//!
//! [`crates::provider::judge`]-style contracts live one layer down in
//! [`horon_agent_provider_core::judge`]: it owns the prompt, the response shape
//! and the strict parser, and it is pure. This module owns the two facts that
//! pure layer must not know: which model answers a judgment (role resolution
//! against the live configuration) and how one attempt is spent (a fork of the
//! caller's provider, a bounded timeout, and provider usage kept on the outcome).
//!
//! ## One model path
//!
//! A judgment is an ordinary chat completion: [`judge_with`] calls
//! [`Provider::complete_simple_with_usage`], the same method the memory sidecar
//! and compaction summary use. There is no second endpoint, no second
//! credential and no router process, so nothing here contradicts the build's
//! "one model path" rule.
//!
//! ## Naming
//!
//! The role is `judge` and so is this module. The `/judge` command is a
//! different thing: it spawns a reviewer worker inside a swarm and resolves its
//! model through the session agent-model overrides. Both read the same
//! `[autojudge] model` setting so a user has one place to name their judge
//! model, but they are separate workloads; `docs/runtime/TURNS.md` states the
//! difference.

use crate::config::{AutoJudgeConfig, NamedProviderConfig};
use crate::provider::Provider;
use crate::session::Session;
use horon_agent_config_types::model_roles::{ModelRole, ModelRoles, RoleResolutionError, resolve_role_model};
use horon_agent_provider_core::SimpleCompletionUsage;
use horon_agent_provider_core::judge::{JudgeAnswers, JudgeError, JudgeRequest, MAX_STATE_BYTES};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The role a judgment resolves through.
pub const JUDGE_ROLE: ModelRole = ModelRole::Judge;

/// What one successful judgment attempt produced.
///
/// Usage is carried rather than dropped so a caller can account for the tokens a
/// judgment spent. Nothing here writes a spend ledger: this build has none for
/// side tasks, and inventing one for judgments alone would report a partial
/// total as if it were whole.
#[derive(Debug, Clone)]
pub struct JudgeOutcome {
    /// The typed answers, already validated against the request.
    pub answers: JudgeAnswers,
    /// The model the answer came from, as the provider reports it.
    pub model: String,
    /// Provider-reported usage for the attempt.
    pub usage: SimpleCompletionUsage,
    /// Wall time the attempt spent, from selection to parsed answer.
    pub elapsed: Duration,
}

/// Why a judgment could not run here, as opposed to failing once it ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JudgeUnavailable {
    /// No named provider profile is active, so there is no role table to read.
    NoProfile,
    /// The active profile is not in this configuration.
    UnknownProfile { profile: String },
    /// Every step of the resolution chain was empty.
    Unresolved(RoleResolutionError),
}

impl std::fmt::Display for JudgeUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoProfile => formatter.write_str(
                "no named provider profile is active, so no `judge` role can resolve; \
                 select one with --provider-profile <name>",
            ),
            Self::UnknownProfile { profile } => {
                write!(
                    formatter,
                    "the active provider profile `{profile}` is not in this configuration"
                )
            }
            Self::Unresolved(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for JudgeUnavailable {}

impl From<JudgeUnavailable> for JudgeError {
    fn from(error: JudgeUnavailable) -> Self {
        JudgeError::Unavailable(error.to_string())
    }
}

/// The resolved model a judgment should use, plus why it is that model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeTarget {
    pub model: String,
    /// The profile whose role table was read, when one was active.
    pub profile: Option<String>,
}

/// The active named provider profile name, if this process has one.
///
/// `HORON_AGENT_NAMED_PROVIDER_PROFILE` is the presence marker
/// `apply_named_provider_profile_env` sets; `HORON_AGENT_PROVIDER_PROFILE_NAME`
/// is the older spelling still honoured elsewhere in the tree.
pub fn active_profile_name() -> Option<String> {
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

/// Resolve the model a judgment uses from one profile, without reading the
/// environment: the pure step, so the chain is testable on plain values.
///
/// `run_override` is the session's `judge` agent-model selection; it wins over
/// the role entry exactly as it does for every other role, so `/judge <model>`
/// and `/agents judge <model>` keep their meaning.
pub fn resolve_judge_model(
    run_override: Option<&str>,
    roles: &ModelRoles,
    profile_default_model: Option<&str>,
    provider_default_model: Option<&str>,
) -> Result<String, RoleResolutionError> {
    resolve_role_model(
        JUDGE_ROLE,
        run_override,
        roles,
        profile_default_model,
        provider_default_model,
    )
}

/// The `judge` model this process would use, resolved against the live config.
///
/// A judgment needs a model the session did not necessarily select, so role
/// resolution is the answer and a missing model is a typed refusal rather than
/// a silent fall back to the session model: quietly judging with the model
/// being judged is exactly the second opinion this must not be.
pub fn configured_judge_model(run_override: Option<&str>) -> Result<JudgeTarget, JudgeUnavailable> {
    let config = crate::config::config();
    let profile_name = active_profile_name();
    let profile: Option<&NamedProviderConfig> = match profile_name.as_deref() {
        Some(name) => Some(
            config
                .providers
                .get(name)
                .ok_or_else(|| JudgeUnavailable::UnknownProfile {
                    profile: name.to_string(),
                })?,
        ),
        None => None,
    };
    let empty = ModelRoles::default();
    let roles = profile.map(|profile| &profile.model_roles.roles).unwrap_or(&empty);
    let model = resolve_judge_model(
        run_override,
        roles,
        profile.and_then(|profile| profile.default_model.as_deref()),
        config.provider.default_model.as_deref(),
    )
    .map_err(JudgeUnavailable::Unresolved)?;
    Ok(JudgeTarget {
        model,
        profile: profile_name,
    })
}

/// The judge model and attempt budget a session would use right now, or the
/// reason it has none. This is the whole configuration read, so a caller that
/// wants to skip the work when nothing is configured can ask once.
pub fn judge_target_for_session(
    session: &Session,
    autojudge: &AutoJudgeConfig,
) -> Result<JudgeTarget, JudgeUnavailable> {
    let run_override = session.effective_agent_model(JUDGE_ROLE.as_str(), autojudge.model.clone());
    configured_judge_model(run_override.as_deref())
}

/// Ask one model for typed judgments, inside a bounded attempt.
///
/// The provider is the caller's, forked here so a model selection made for one
/// judgment cannot leak back into the session's provider.
///
/// `timeout_ms` is the whole attempt, not just the request: model selection, the
/// request and parsing all happen inside it. A budget that only covered the
/// network wait would leave a provider that blocks in `set_model` outside the
/// bound it was given. `0` is not a way to ask for no deadline — it means the
/// default — because a judgment stalls the turn it is judging.
///
/// Every failure is typed: [`JudgeError::Input`] before any request,
/// [`JudgeError::Timeout`] when the budget ran out, [`JudgeError::Malformed`] or
/// [`JudgeError::InvalidAnswer`] when the reply left the contract, and
/// [`JudgeError::Backend`] when the provider refused or failed. The caller
/// decides what a failure means; this function never guesses an answer.
pub async fn judge_with(
    provider: &Arc<dyn Provider>,
    model: &str,
    timeout_ms: u64,
    request: &JudgeRequest,
) -> Result<JudgeOutcome, JudgeError> {
    request.validate()?;
    let prompt = request.prompt()?;
    if let Some(reason) = provider.unavailable_reason() {
        return Err(JudgeError::Unavailable(reason));
    }

    let started = Instant::now();
    let budget = Duration::from_millis(timeout_ms.max(1));
    let attempt = async {
        let judge_provider = provider.fork();
        // A judge model that cannot be selected is a configuration error, not a
        // reason to judge with whatever the session happens to be running: the
        // whole point is a second model's opinion.
        judge_provider.set_model(model).map_err(|error| {
            JudgeError::Backend(format!("the judge model '{model}' could not be selected: {error}"))
        })?;

        let (reply, usage) = judge_provider
            .complete_simple_with_usage(&prompt, JudgeRequest::SYSTEM)
            .await
            .map_err(backend_error)?;
        Ok::<_, JudgeError>((judge_provider.model(), reply, usage))
    };

    let (model_used, reply, usage) = match tokio::time::timeout(budget, attempt).await {
        Ok(result) => result?,
        Err(_) => {
            return Err(JudgeError::Timeout {
                after_ms: budget.as_millis() as u64,
            });
        }
    };

    let answers = request.parse_answers(&reply)?;
    Ok(JudgeOutcome {
        answers,
        model: model_used,
        usage,
        elapsed: started.elapsed(),
    })
}

fn backend_error(error: anyhow::Error) -> JudgeError {
    JudgeError::Backend(error.to_string())
}

/// The state a turn-recovery judgment sends, capped to the contract's budget.
///
/// The last user message and the stopped assistant text are what a reader needs
/// to judge whether the stop was premature, so they are the whole state. Cutting
/// is by characters and on a boundary, because the cap is bytes and a multibyte
/// character must not be split into an invalid string.
pub fn turn_recovery_state(last_user_message: &str, stopped_text: &str) -> serde_json::Value {
    let head = "## Last user message\n";
    let divider = "\n\n## Assistant text at the stop\n";
    let budget = MAX_STATE_BYTES.saturating_sub(head.len() + divider.len());
    let user_share = budget / 3;
    let mut state = String::with_capacity(budget);
    state.push_str(head);
    state.push_str(&clip_to_bytes(last_user_message, user_share));
    state.push_str(divider);
    state.push_str(&clip_to_bytes(stopped_text, budget - user_share));
    serde_json::Value::String(state)
}

/// Keep the head of `text` within `max_bytes`, never splitting a character.
///
/// The ellipsis that marks a cut counts against the budget, so the result is
/// never longer than the caller asked for.
fn clip_to_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    const ELLIPSIS: &str = "…";
    let room = max_bytes.saturating_sub(ELLIPSIS.len());
    let mut end = room;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut clipped = text[..end].to_string();
    clipped.push_str(ELLIPSIS);
    clipped
}

#[cfg(test)]
#[path = "judge_tests.rs"]
mod tests;
