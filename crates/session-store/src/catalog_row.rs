//! Bounded listing records; public summaries resolve metadata, never conversation bodies.
use crate::cache_io::Stamp;
use crate::cache_values::CachedValue;
use crate::context_cache::CachedContext;
use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Summary {
    pub session_id: SessionId,
    pub head: EventId,
    pub title: Option<String>,
    pub custom_title: Option<String>,
    pub working_dir: Option<String>,
    pub saved: bool,
    pub save_label: Option<String>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub metadata_updated_at: chrono::DateTime<chrono::Utc>,
    pub last_active_at_ms: Option<i64>,
    pub generated_title: Option<String>,
    pub todo_title: Option<String>,
    pub archived: bool,
    pub(crate) source: Option<Stamp>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CatalogEntry {
    pub session_id: SessionId,
    pub head: EventId,
    pub metadata: CachedValue<SessionMetadata>,
    pub archive: Option<CachedValue<ArchiveChangedPayload>>,
    pub activity_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub first_prompt_title: Option<String>,
    pub source: Stamp,
}

impl CatalogEntry {
    pub fn new(context: &CachedContext, source: Stamp) -> Self {
        Self {
            session_id: context.session_id.clone(),
            head: context.head.clone(),
            metadata: context.metadata.clone(),
            archive: context.archive.clone(),
            activity_at: context.activity_at,
            first_prompt_title: context.first_prompt_title.clone(),
            source,
        }
    }

    pub fn materialize(&self, paths: &SessionPaths, probe: &ReadProbe) -> Result<Summary, StoreError> {
        let source = (paths, &self.session_id);
        let metadata = self.metadata.resolve(source, probe)?;
        let archive = match &self.archive {
            Some(value) => Some(value.resolve(source, probe)?),
            None => None,
        };
        let imported = metadata.legacy_extensions.get("legacy_index_display");
        let imported_text = |key: &str| {
            imported
                .and_then(|row| row.get(key))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        let imported_display = imported.and_then(|row| {
            ["custom_title", "save_label", "todo_title", "generated_title"]
                .into_iter()
                .filter(|key| *key != "save_label" || row["saved"].as_i64() == Some(1))
                .filter_map(|key| row[key].as_str())
                .map(str::trim)
                .find(|value| !value.is_empty())
        });
        let generated_title = if imported.is_some() && metadata.title.as_deref() == imported_display {
            imported_text("generated_title")
        } else {
            metadata.title.clone()
        }
        .or_else(|| self.first_prompt_title.clone());
        let historical_activity = metadata
            .legacy_extensions
            .get("last_active_at_ms")
            .and_then(serde_json::Value::as_i64);
        let local_path = paths.session_dir(&self.session_id)?.join("local-runtime.json");
        let last_active_at_ms = match std::fs::symlink_metadata(&local_path) {
            Ok(_) => {
                #[derive(Deserialize)]
                struct LocalActivity {
                    last_active_at: Option<chrono::DateTime<chrono::Utc>>,
                }
                let activity: LocalActivity =
                    serde_json::from_slice(&probe.bounded(&local_path)?).map_err(crate::cache_io::json_error)?;
                activity
                    .last_active_at
                    .map(|time| time.timestamp_millis())
                    .or(historical_activity)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => historical_activity,
            Err(error) => return Err(error.into()),
        };
        Ok(Summary {
            session_id: self.session_id.clone(),
            head: self.head.clone(),
            metadata_updated_at: metadata.updated_at,
            last_active_at_ms,
            generated_title,
            todo_title: imported_text("todo_title"),
            title: metadata.title,
            custom_title: metadata.custom_title,
            working_dir: metadata.working_dir,
            saved: metadata.saved,
            save_label: metadata.save_label,
            updated_at: metadata.updated_at.max(self.activity_at),
            archived: archive.is_some_and(|a| a.archived),
            source: Some(self.source.clone()),
        })
    }
}
