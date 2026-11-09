//! On-disk model-catalog cache shared by every catalog-backed runtime.
//!
//! A runtime that discovers its model list over HTTP persists it here so a
//! cold start can render the picker before the network answers. Entries are
//! namespaced per endpoint profile, keyed by the file's modification time so a
//! concurrent writer invalidates the in-process memo instead of serving stale
//! rows.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

const CACHE_TTL_SECS: u64 = 24 * 60 * 60;
const DEFAULT_CACHE_NAMESPACE: &str = "default";

/// One catalog model row as persisted on disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub context_length: Option<u64>,
    #[serde(default)]
    pub pricing: ModelPricing,
    #[serde(default)]
    pub created: Option<u64>,
    /// Declared input modalities, e.g. `["text", "image"]`. Empty when the
    /// catalog did not say, in which case callers keep their existing default
    /// rather than inferring a capability the source never claimed.
    #[serde(default)]
    pub input: Vec<String>,
}

/// Per-token prices as the catalog reports them (strings of USD per token).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelPricing {
    #[serde(default, deserialize_with = "deserialize_optional_string_or_number")]
    pub prompt: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_number")]
    pub completion: Option<String>,
    #[serde(
        default,
        rename = "input_cache_read",
        deserialize_with = "deserialize_optional_string_or_number"
    )]
    pub input_cache_read: Option<String>,
    #[serde(
        default,
        rename = "input_cache_write",
        deserialize_with = "deserialize_optional_string_or_number"
    )]
    pub input_cache_write: Option<String>,
}

fn deserialize_optional_string_or_number<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::String(value)) => Some(value),
        Some(serde_json::Value::Number(value)) => Some(value.to_string()),
        Some(serde_json::Value::Null) | None => None,
        Some(other) => {
            return Err(serde::de::Error::custom(format!(
                "expected string, number, or null for pricing value, got {other}"
            )));
        }
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskCache {
    pub cached_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_api_base: Option<String>,
    pub models: Vec<ModelInfo>,
}

#[derive(Debug, Clone)]
struct DiskCacheMemoEntry {
    modified_at: Option<SystemTime>,
    cache: Option<DiskCache>,
}

static DISK_CACHE_MEMO: LazyLock<Mutex<HashMap<PathBuf, DiskCacheMemoEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn current_unix_secs() -> Option<u64> {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

fn sanitize_cache_namespace(raw: &str) -> String {
    let sanitized: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if sanitized.is_empty() {
        DEFAULT_CACHE_NAMESPACE.to_string()
    } else {
        sanitized
    }
}

fn cache_path_for_namespace(namespace: &str) -> PathBuf {
    let namespace = sanitize_cache_namespace(namespace);
    if let Ok(path) = std::env::var("HORON_HOME") {
        return PathBuf::from(path)
            .join("cache")
            .join(format!("{}_models.json", namespace));
    }

    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".horon")
        .join("cache")
        .join(format!("{}_models.json", namespace))
}

fn disk_cache_modified_at(path: &PathBuf) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn fresh_disk_cache(cache: Option<DiskCache>) -> Option<DiskCache> {
    let now = current_unix_secs()?;
    let cache = cache?;
    if now.saturating_sub(cache.cached_at) < CACHE_TTL_SECS {
        Some(cache)
    } else {
        None
    }
}

fn load_disk_cache_entry_from_path(path: PathBuf) -> Option<DiskCache> {
    let modified_at = disk_cache_modified_at(&path);

    if let Ok(memo) = DISK_CACHE_MEMO.lock()
        && let Some(entry) = memo.get(&path)
        && entry.modified_at == modified_at
    {
        return fresh_disk_cache(entry.cache.clone());
    }

    let loaded = std::fs::read_to_string(&path)
        .ok()
        .and_then(|content| serde_json::from_str::<DiskCache>(&content).ok());

    if let Ok(mut memo) = DISK_CACHE_MEMO.lock() {
        memo.insert(
            path,
            DiskCacheMemoEntry {
                modified_at,
                cache: loaded.clone(),
            },
        );
    }

    fresh_disk_cache(loaded)
}

pub fn load_disk_cache_entry_for_namespace(namespace: &str) -> Option<DiskCache> {
    load_disk_cache_entry_from_path(cache_path_for_namespace(namespace))
}

#[cfg(test)]
pub fn save_disk_cache_with_source_for_namespace(namespace: &str, models: &[ModelInfo], source_api_base: Option<&str>) {
    let path = cache_path_for_namespace(namespace);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let now = current_unix_secs().unwrap_or(0);

    let cache = DiskCache {
        cached_at: now,
        source_api_base: source_api_base.map(ToString::to_string),
        models: models.to_vec(),
    };

    if let Ok(content) = serde_json::to_string(&cache) {
        let _ = std::fs::write(&path, content);
    }

    if let Ok(mut memo) = DISK_CACHE_MEMO.lock() {
        memo.insert(
            path.clone(),
            DiskCacheMemoEntry {
                modified_at: disk_cache_modified_at(&path),
                cache: Some(cache),
            },
        );
    }
}
