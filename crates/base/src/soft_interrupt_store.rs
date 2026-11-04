use anyhow::Result;
use horon_agent_agent_runtime::{SoftInterruptMessage, SoftInterruptSource};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedSoftInterrupt {
    content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    images: Vec<(String, String)>,
    urgent: bool,
    source: PersistedSoftInterruptSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    input_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PersistedSoftInterruptSource {
    User,
    System,
    BackgroundTask,
}

impl From<SoftInterruptSource> for PersistedSoftInterruptSource {
    fn from(value: SoftInterruptSource) -> Self {
        match value {
            SoftInterruptSource::User => Self::User,
            SoftInterruptSource::System => Self::System,
            SoftInterruptSource::BackgroundTask => Self::BackgroundTask,
        }
    }
}

impl From<PersistedSoftInterruptSource> for SoftInterruptSource {
    fn from(value: PersistedSoftInterruptSource) -> Self {
        match value {
            PersistedSoftInterruptSource::User => Self::User,
            PersistedSoftInterruptSource::System => Self::System,
            PersistedSoftInterruptSource::BackgroundTask => Self::BackgroundTask,
        }
    }
}

impl From<SoftInterruptMessage> for PersistedSoftInterrupt {
    fn from(value: SoftInterruptMessage) -> Self {
        Self {
            content: value.content,
            images: value.images,
            urgent: value.urgent,
            source: value.source.into(),
            client_id: value.client_id,
            input_id: value.input_id,
        }
    }
}

impl From<PersistedSoftInterrupt> for SoftInterruptMessage {
    fn from(value: PersistedSoftInterrupt) -> Self {
        Self {
            content: value.content,
            images: value.images,
            urgent: value.urgent,
            source: value.source.into(),
            client_id: value.client_id,
            input_id: value.input_id,
        }
    }
}

fn dir_path() -> Result<PathBuf> {
    Ok(crate::storage::horon_agent_dir()?.join("pending-soft-interrupts"))
}

/// The compatibility mirror file for one session.
///
/// Public because the durable input layer reports which file a legacy export
/// wrote, and because the migration path needs the same path it will read.
pub fn path_for_session(session_id: &str) -> Result<PathBuf> {
    Ok(dir_path()?.join(format!("{}.json", session_id)))
}

pub fn load(session_id: &str) -> Result<Vec<SoftInterruptMessage>> {
    let path = path_for_session(session_id)?;
    if !path.exists() {
        return Ok(Vec::new());
    }

    let persisted: Vec<PersistedSoftInterrupt> = crate::storage::read_json(&path)?;
    Ok(persisted.into_iter().map(SoftInterruptMessage::from).collect())
}

pub fn take(session_id: &str) -> Result<Vec<SoftInterruptMessage>> {
    let path = path_for_session(session_id)?;
    let loaded = load(session_id)?;
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    Ok(loaded)
}

pub fn overwrite(session_id: &str, interrupts: &[SoftInterruptMessage]) -> Result<()> {
    let path = path_for_session(session_id)?;
    if interrupts.is_empty() {
        // A caller that clears the copy relies on `Ok` meaning "the disk no
        // longer holds these messages"; a swallowed failure would let the next
        // restore inject them a second time.
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let persisted: Vec<PersistedSoftInterrupt> = interrupts.iter().cloned().map(Into::into).collect();
    crate::storage::write_json_fast(&path, &persisted)
}

/// Write the mirror and sync it before returning.
///
/// The durable input layer mirrors after every normal transition, and an older
/// build reading the same home has to see the whole queue or none of it, so
/// this uses the fsyncing JSON writer rather than the fast one.
pub fn overwrite_durable(session_id: &str, interrupts: &[SoftInterruptMessage]) -> Result<()> {
    let path = path_for_session(session_id)?;
    if interrupts.is_empty() {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let persisted: Vec<PersistedSoftInterrupt> = interrupts.iter().cloned().map(Into::into).collect();
    crate::storage::write_json(&path, &persisted)
}

pub fn append(session_id: &str, interrupt: SoftInterruptMessage) -> Result<()> {
    let mut current = load(session_id)?;
    current.push(interrupt);
    overwrite(session_id, &current)
}

pub fn clear(session_id: &str) -> Result<()> {
    overwrite(session_id, &[])
}

/// Drop persisted interrupts whose caller id is in `client_ids`, returning the
/// ids that were removed.
pub fn remove_by_client_ids(session_id: &str, client_ids: &[String]) -> Result<Vec<String>> {
    let mut current = load(session_id)?;
    let mut removed = Vec::new();
    current.retain(|message| match &message.client_id {
        Some(id) if client_ids.contains(id) => {
            removed.push(id.clone());
            false
        }
        _ => true,
    });
    if !removed.is_empty() {
        overwrite(session_id, &current)?;
    }
    Ok(removed)
}

#[cfg(test)]
#[path = "soft_interrupt_store_tests.rs"]
mod soft_interrupt_store_tests;
