use super::turn_failover::ProviderErrorAction;
use super::*;

/// Per-turn model fallback state: the model the session selected before the
/// first switch, and the fallback entries this turn already spent.
#[derive(Debug, Default)]
pub(super) struct FallbackState {
    original: Option<String>,
    tried: Vec<String>,
}

impl Agent {
    /// Switch to the next configured fallback model for the rest of this turn.
    ///
    /// Only an error the shared classifier accepts starts a switch, so a
    /// credential, request-shape, or request-size failure never spends the
    /// list. The switch stays inside the active named provider profile and is
    /// not persisted: the session keeps the model the user selected, and the
    /// next turn restores it through [`Self::restore_model_fallback`].
    pub(super) fn try_model_fallback(&mut self, error: &anyhow::Error) -> Option<ProviderErrorAction> {
        if !horon_agent_provider_core::failover::classify_model_fallback_error_message(&error.to_string()) {
            return None;
        }
        let entries = active_profile_fallback_models()?;
        let current = self.provider_model();
        let next = entries
            .iter()
            .find(|entry| entry.as_str() != current && !self.model_fallback.tried.contains(entry))
            .cloned()?;
        if self.model_fallback.original.is_none() {
            self.model_fallback.original = Some(current);
        }
        // Recorded before the switch so a refused switch still spends the
        // entry: retrying the same refusal has no bound otherwise.
        self.model_fallback.tried.push(next.clone());
        if let Err(switch_error) = crate::provider::set_model_with_auth_refresh(self.provider.as_ref(), &next) {
            logging::warn(&format!("Model fallback to '{next}' was not applied: {switch_error}"));
            return None;
        }
        self.refresh_compaction_budget();
        logging::info(&format!(
            "Model fallback: switched to '{next}' for the rest of this turn ({}/{}) after: {error}",
            self.model_fallback.tried.len(),
            entries.len()
        ));
        Some(ProviderErrorAction::Resend {
            attempt: self.model_fallback.tried.len() as u32,
            max: entries.len() as u32,
        })
    }

    /// The model fallback step, or the loop's existing handling when it does
    /// not apply. A profile without `fallback_models` behaves exactly as it did
    /// before this step existed.
    pub(super) fn model_fallback_or_proceed(&mut self, error: &anyhow::Error) -> ProviderErrorAction {
        self.try_model_fallback(error).unwrap_or(ProviderErrorAction::Proceed)
    }

    /// Put the session's own model back after a turn that fell back.
    ///
    /// Called at the next turn start and on a turn that failed, so a switch
    /// never survives the turn that needed it and never becomes the session's
    /// selection on disk.
    pub(super) fn restore_model_fallback(&mut self) {
        let Some(original) = self.model_fallback.original.take() else {
            return;
        };
        self.model_fallback.tried.clear();
        if self.provider_model() == original {
            return;
        }
        match crate::provider::set_model_with_auth_refresh(self.provider.as_ref(), &original) {
            Ok(()) => {
                self.refresh_compaction_budget();
                logging::info(&format!("Model fallback: restored the session model '{original}'"));
            }
            Err(error) => {
                logging::warn(&format!("Model fallback could not restore '{original}': {error}"));
                self.model_fallback.original = Some(original);
            }
        }
    }
}

/// The fallback entries of the active named provider profile.
///
/// A session that is not on a named profile has no list this step may spend,
/// so it keeps its existing error path.
fn active_profile_fallback_models() -> Option<Vec<String>> {
    let profile_name = [
        "HORON_AGENT_NAMED_PROVIDER_PROFILE",
        "HORON_AGENT_PROVIDER_PROFILE_NAME",
    ]
    .iter()
    .find_map(|key| match std::env::var(key) {
        Ok(value) => Some(value.trim().to_string()).filter(|value| !value.is_empty()),
        Err(_) => None,
    })?;
    let config = crate::config::config();
    let profile = config.providers.get(&profile_name)?;
    let entries: Vec<String> = profile
        .model_roles
        .fallback_models()
        .into_iter()
        .map(str::to_string)
        .collect();
    (!entries.is_empty()).then_some(entries)
}
