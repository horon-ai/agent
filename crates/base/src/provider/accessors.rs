use super::*;

impl MultiProvider {
    pub(super) fn openai_provider(&self) -> Option<Arc<dyn Provider>> {
        self.openai
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Runtime that executes the shared OpenAI-compatible slot: the active
    /// direct profile when one is selected, otherwise the built-in OpenAI
    /// runtime.
    pub(super) fn active_openrouter_execution_provider(&self) -> Option<Arc<dyn Provider>> {
        ProviderRegistry::new(self).active_openrouter_execution()
    }

    /// The runtime that will serve the next request.
    ///
    /// A direct OpenAI-compatible profile takes precedence over the built-in
    /// OpenAI runtime because selecting a profile is what the user asked for.
    pub(super) fn execution_provider(&self) -> Option<Arc<dyn Provider>> {
        self.active_openrouter_execution_provider()
            .or_else(|| self.openai_provider())
    }

    /// Drop the active direct-profile selection.
    ///
    /// Called when a built-in OpenAI route is pinned explicitly. Without this,
    /// `execution_provider()` would keep preferring the stale profile and send
    /// an OpenAI model id to the profile endpoint.
    pub(super) fn clear_active_compatible_profile(&self) {
        *self
            .active_openai_compatible_profile
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        self.invalidate_routes_memo();
    }

    pub(super) fn provider_slot_available(&self, provider: ActiveProvider) -> bool {
        match provider {
            // The shared OpenAI-compatible slot executes through the active
            // direct profile when one is selected. Checking only the built-in
            // slot made dispatch treat an active compatible profile as
            // "not configured" (issue #358).
            ActiveProvider::OpenAI => self.execution_provider().is_some(),
        }
    }

    pub(super) fn reconcile_auth_if_provider_missing(&self, provider: ActiveProvider) -> bool {
        if self.provider_slot_available(provider) {
            return true;
        }

        crate::logging::info(&format!(
            "Provider {} missing at use site; reconciling auth from disk",
            Self::provider_label(provider)
        ));
        Provider::on_auth_changed(self);
        self.provider_slot_available(provider)
    }
}
