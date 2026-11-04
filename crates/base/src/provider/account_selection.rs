use super::*;

impl MultiProvider {
    /// Auto prefers saved subscriptions, but provider-level Auto can silently
    /// fall back to an API key when refresh fails. Bind the subscription before
    /// sending anything so only the pool/countdown router may change billing.
    pub(super) fn bind_auto_account_for_request(&self, target: ActiveProvider) -> Result<()> {
        if ProviderRegistry::new(self).active_compatible_profile_id().is_some() {
            // A direct profile has no OAuth account pool; binding one would
            // silently replace the built-in slot while the profile still runs.
            return Ok(());
        }
        let runtime = self.openai_provider();
        let Some(runtime) = runtime.filter(|runtime| runtime.credential_mode() == CredentialMode::Auto) else {
            return Ok(());
        };
        let Some(label) = active_account_label_for_provider(target) else {
            return Ok(());
        };
        let saved = crate::auth::codex::list_accounts()?
            .iter()
            .any(|account| account.label == label);
        if !saved {
            return Ok(());
        }
        let replacement = runtime.fork();
        replacement.set_oauth_account(Some(&label))?;
        replacement.set_credential_mode(CredentialMode::OAuth)?;
        let slot = &self.openai;
        let mut current = slot.write().unwrap_or_else(|p| p.into_inner());
        if current.as_ref().is_some_and(|current| Arc::ptr_eq(current, &runtime)) {
            *current = Some(replacement);
            self.invalidate_routes_memo();
        }
        Ok(())
    }

    /// Select a saved login without rewriting the process-wide account preference.
    /// Build the replacement off to the side so validation errors leave the old
    /// runtime, model and account untouched.
    pub(super) fn set_model_on_saved_account(&self, _method: &str, label: &str, model: &str) -> Result<()> {
        if label.is_empty() || model.trim().is_empty() {
            anyhow::bail!("Account label and model cannot be empty");
        }
        let target = {
            if !crate::auth::codex::list_accounts()?
                .iter()
                .any(|account| account.label == label)
            {
                anyhow::bail!("No OpenAI account with label '{}' found", label);
            }
            ActiveProvider::OpenAI
        };
        self.reconcile_auth_if_provider_missing(target);
        let runtime = self
            .openai_provider()
            .ok_or_else(|| anyhow!("{} runtime is not configured", Self::provider_label(target)))?;
        let replacement = runtime.fork();
        replacement.set_oauth_account(Some(label))?;
        replacement.set_credential_mode(CredentialMode::OAuth)?;
        replacement.set_model(&model_name_for_provider(target, model.trim()))?;
        self.clear_active_compatible_profile();
        *self.openai.write().unwrap_or_else(|p| p.into_inner()) = Some(replacement);
        self.set_active_provider(target);
        self.invalidate_routes_memo();
        Ok(())
    }

    pub(super) fn selected_account_for_provider(&self, _target: ActiveProvider) -> Option<String> {
        let runtime = self.openai_provider()?;
        (runtime.credential_mode() == CredentialMode::OAuth)
            .then(|| runtime.oauth_account_label())
            .flatten()
    }
}
