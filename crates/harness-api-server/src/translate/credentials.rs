//! Secure credential binding and atomic owner-only persistence.

use std::io::Write;

use super::*;

impl BridgeState {
    /// Map a client-facing API-key provider name to the OpenAI-compatible
    /// profile the daemon can actually activate, its env key, and its env file.
    ///
    /// The canonical name returned here travels in the auth-changed
    /// notification, so it must be a profile id from
    /// `horon_agent_provider_metadata` (`anthropic-api`, `gemini-api`, …).
    /// Legacy aliases (`claude-api`, `gemini`, …) are accepted and normalized;
    /// providers with no surviving profile (for example `cursor`) are rejected.
    pub(super) fn credential_binding(provider: &str) -> Option<(&'static str, &'static [&'static str], &'static str)> {
        match provider.trim().to_ascii_lowercase().as_str() {
            "claude-api" | "anthropic-api" | "anthropic-key" | "claude-key" => {
                Some(("anthropic-api", &["ANTHROPIC_API_KEY"], "anthropic.env"))
            }
            "openai-api" | "openai-key" | "openai-apikey" => Some(("openai-api", &["OPENAI_API_KEY"], "openai.env")),
            "openrouter" => Some(("openrouter", &["OPENROUTER_API_KEY"], "openrouter.env")),
            "gemini" | "gemini-api" | "google-gemini" => {
                Some(("gemini-api", &["GEMINI_API_KEY", "GOOGLE_API_KEY"], "gemini.env"))
            }
            _ => None,
        }
    }

    pub(super) fn write_credential(file_name: &str, env_keys: &[&str], value: Option<&str>) -> Result<(), String> {
        let _write_guard = Self::state_write_guard();
        let dir = Self::app_config_dir().ok_or_else(|| "could not resolve horon-agent config directory".to_string())?;
        Self::ensure_owner_only_dir(&dir)?;
        let path = dir.join(file_name);
        Self::validate_owner_file_if_exists(&path)?;
        let existing = match std::fs::read_to_string(&path) {
            Ok(existing) => existing,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(format!("read credential file: {error}")),
        };
        let mut lines: Vec<String> = existing
            .lines()
            .filter(|line| {
                line.split_once('=')
                    .is_none_or(|(key, _)| !env_keys.contains(&key.trim()))
            })
            .map(str::to_string)
            .collect();
        if let Some(value) = value {
            if value.contains(['\n', '\r', '\0']) || value.trim() != value {
                return Err("API key must be a trimmed, non-NUL single line".into());
            }
            lines.push(format!("{}={value}", env_keys[0]));
        }
        let mut content = lines.join("\n");
        if !content.is_empty() {
            content.push('\n');
        }
        Self::atomic_owner_write(&path, content.as_bytes())
    }

    pub(super) fn atomic_owner_write(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
        let parent = path
            .parent()
            .ok_or_else(|| "owner-only write path has no parent".to_string())?;
        Self::ensure_owner_only_dir(parent)?;
        Self::validate_owner_file_if_exists(path)?;
        let temp = path.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| format!("credential write clock is before Unix epoch: {error}"))?
                .as_nanos()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        horon_agent_platform::fs::request_owner_only_mode(&mut options);
        let mut file = options.open(&temp).map_err(|error| error.to_string())?;
        let write_result = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        if let Err(error) = write_result {
            if let Err(cleanup_error) = std::fs::remove_file(&temp)
                && cleanup_error.kind() != std::io::ErrorKind::NotFound
            {
                return Err(format!(
                    "credential write failed: {error}; temporary-file cleanup failed: {cleanup_error}"
                ));
            }
            return Err(error.to_string());
        }
        horon_agent_platform::fs::atomic_replace(&temp, path).map_err(|error| match std::fs::remove_file(&temp) {
            Ok(()) => error.to_string(),
            Err(cleanup_error) if cleanup_error.kind() == std::io::ErrorKind::NotFound => error.to_string(),
            Err(cleanup_error) => {
                format!("credential replace failed: {error}; temporary-file cleanup failed: {cleanup_error}")
            }
        })?;
        horon_agent_platform::fs::set_file_permissions_owner_only(path).map_err(|error| error.to_string())?;
        horon_agent_platform::fs::sync_parent_directory(path)
            .map_err(|error| format!("sync owner-only directory: {error}"))?;
        Ok(())
    }

    pub(super) fn ensure_owner_only_dir(path: &std::path::Path) -> Result<(), String> {
        std::fs::create_dir_all(path).map_err(|error| error.to_string())?;
        let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!("{} must be a non-symlink directory", path.display()));
        }
        horon_agent_platform::fs::verify_private_dir_metadata(&metadata)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        horon_agent_platform::fs::set_dir_permissions_owner_only(path).map_err(|error| error.to_string())?;
        Ok(())
    }

    pub(super) fn validate_owner_file_if_exists(path: &std::path::Path) -> Result<(), String> {
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!("{} must be a regular, non-symlink file", path.display()));
        }
        // Report the condition that actually failed: the check also refuses a
        // hard link and a non-regular file, and calling either of those an
        // ownership problem sends the user looking at the wrong thing.
        horon_agent_platform::fs::verify_private_file_metadata(&metadata)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(())
    }
}
