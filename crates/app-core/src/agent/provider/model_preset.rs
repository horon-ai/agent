use super::Agent;
use crate::config::SessionModelPreset;
use anyhow::{Result, bail};

impl Agent {
    pub(crate) fn model_preset(&mut self, preset: &SessionModelPreset, apply: bool) -> Result<()> {
        let models = self.available_models_for_switching();
        let current = self.provider.model();
        let validate_model = |model: &str| {
            if model.is_empty() || (model != current && !models.iter().any(|available| available == model)) {
                bail!("Model '{model}' is unavailable in the active provider profile");
            }
            Ok(())
        };
        for (role, model) in &preset.roles {
            if !matches!(role.as_str(), "swarm" | "review" | "judge" | "memory" | "ambient") {
                bail!("Unknown model preset role: {role}");
            }
            if model != "inherit" {
                validate_model(model)?;
            }
        }
        if let Some(model) = preset.model.as_deref() {
            validate_model(model)?;
        }
        if preset.model.is_some() || preset.effort.is_some() {
            // Validate model-dependent effort on an isolated provider before changing the session.
            let candidate = self.provider_fork();
            if let Some(model) = preset.model.as_deref() {
                candidate.set_model(model)?;
                if candidate.model() != model {
                    bail!("Model '{model}' cannot be selected inside the active provider profile");
                }
            }
            if let Some(effort) = preset.effort.as_deref() {
                if !candidate.available_efforts().contains(&effort) {
                    bail!("Reasoning effort '{effort}' is unavailable for the preset model");
                }
                candidate.set_reasoning_effort(effort)?;
            }
        }
        if apply {
            if let Some(model) = preset.model.as_deref() {
                self.set_model(model)?;
            }
            if let Some(effort) = preset.effort.as_deref() {
                self.set_reasoning_effort(effort)?;
            }
            self.session.save_prepared()?;
            self.session.apply_agent_model_overrides(&preset.roles)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
