//! Credential activation for auth changes: pick the profile an auth change
//! names so the next request uses its endpoint and freshly saved credential.

use crate::protocol::AuthChanged;

/// Whether an auth change plausibly wrote an API key into the provider's env
/// file. OAuth-style logins never touch API-key env files, and a
/// process-env-preseeded credential means the env var itself is the intended
/// source of truth. Legacy hint-only notifications carry no source metadata;
/// syncing is still the safe default because it only changes anything when the
/// saved file and the process env var differ.
pub(super) fn auth_change_saved_credentials(auth: Option<&AuthChanged>) -> bool {
    match auth {
        Some(auth) => {
            matches!(
                auth.credential_source,
                Some(crate::protocol::AuthCredentialSource::ApiKeyFile) | None
            ) && !matches!(
                auth.auth_method,
                Some(crate::protocol::AuthMethod::ProcessEnvPreseeded)
                    | Some(crate::protocol::AuthMethod::OAuthBrowser)
                    | Some(crate::protocol::AuthMethod::DeviceCode)
            )
        }
        None => true,
    }
}

/// Make a credential saved to the profile's env file win over a stale value
/// inherited through this process's environment (issue #453).
///
/// `SetApiKey` writes the key to the per-profile env file, but credential
/// resolution ([`crate::provider_catalog::load_api_key_from_env_or_config`])
/// prefers the process env var. A long-lived daemon that inherited an old key
/// would keep failing with 401 even though the new key is on disk, so an auth
/// change replaces the env var with the saved value and removes it when the
/// saved key was cleared.
fn sync_process_env_from_saved_api_key(profile: crate::provider_catalog::OpenAiCompatibleProfile) {
    let resolved = crate::provider_catalog::resolve_openai_compatible_profile(profile);
    let saved = crate::provider_catalog::load_env_value_from_config_file(&resolved.api_key_env, &resolved.env_file);
    let current = match std::env::var(&resolved.api_key_env) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            crate::logging::warn(&format!(
                "Invalid process credential variable {}: non-Unicode value",
                resolved.api_key_env
            ));
            None
        }
    };
    match saved {
        Some(saved) => {
            if current.as_deref() == Some(saved.as_str()) {
                return;
            }
            let replaced = if current.is_some() {
                "stale_process_env"
            } else {
                "unset_process_env"
            };
            crate::env::set_var(&resolved.api_key_env, &saved);
            crate::logging::auth_event(
                "auth_changed_env_synced_from_file",
                &resolved.id,
                &[
                    ("env_key", resolved.api_key_env.as_str()),
                    ("env_file", resolved.env_file.as_str()),
                    ("replaced", replaced),
                ],
            );
        }
        None => {
            if current.is_none() {
                return;
            }
            crate::env::remove_var(&resolved.api_key_env);
            crate::logging::auth_event(
                "auth_changed_env_cleared_from_file",
                &resolved.id,
                &[
                    ("env_key", resolved.api_key_env.as_str()),
                    ("env_file", resolved.env_file.as_str()),
                    ("reason", "saved_credential_removed"),
                ],
            );
        }
    }
}

/// Apply the profile named by an auth change so the next request uses its
/// endpoint and credential; unknown names are ignored. When the change saved a
/// credential to the profile's env file, that value replaces a stale process
/// env var first so the just-saved key actually wins (issue #453).
pub(super) fn activate_auth_profile(provider_id: &str, saved_credentials_changed: bool) {
    if crate::config::config().providers.contains_key(provider_id) {
        if crate::provider_catalog::apply_named_provider_profile_env(provider_id).is_err() {
            return;
        }
    } else if let Some(profile) = crate::provider_catalog::openai_compatible_profile_by_id(provider_id) {
        if saved_credentials_changed {
            sync_process_env_from_saved_api_key(profile);
        }
        crate::provider_catalog::force_apply_openai_compatible_profile_env(Some(profile));
    } else {
        return;
    }
    crate::env::set_var("HORON_AGENT_PROVIDER_PROFILE_ACTIVE", "1");
}
