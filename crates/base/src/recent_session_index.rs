//! Recent-session display metadata from the common event catalog.

use anyhow::Result;
use horon_agent_session_store as store;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentSessionMetadata {
    pub session_id: String,
    pub working_dir: Option<String>,
    pub generated_title: Option<String>,
    pub custom_title: Option<String>,
    pub todo_title: Option<String>,
    pub saved: bool,
    pub save_label: Option<String>,
    pub updated_at_ms: i64,
    pub last_active_at_ms: Option<i64>,
}

impl RecentSessionMetadata {
    pub fn display_title(&self) -> Option<&str> {
        self.custom_title
            .as_deref()
            .and_then(non_empty)
            .or_else(|| {
                // Bookmarks labelled before labels doubled as titles.
                self.saved
                    .then(|| self.save_label.as_deref().and_then(non_empty))
                    .flatten()
            })
            .or_else(|| self.todo_title.as_deref().and_then(non_empty))
            .or_else(|| self.generated_title.as_deref().and_then(non_empty))
    }
}

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

pub fn recent(limit: usize) -> Result<Vec<RecentSessionMetadata>> {
    let home = crate::storage::horon_agent_dir()?;
    let home = if home.exists() { home.canonicalize()? } else { home };
    let paths = store::SessionPaths::new(home.join("sessions"));
    store::legacy::require_index_migration(&paths)?;
    if !paths.root().try_exists()? {
        return Ok(Vec::new());
    }
    let probe = store::ReadProbe::default();
    let rows = match store::catalog::list(&paths, probe.clone()) {
        Ok(rows) => rows,
        Err(error) => {
            // One unreadable original must not erase every other session from
            // the listing. Keep the sessions that can still be summarized and
            // leave the damaged one to the store's own integrity reporting.
            crate::logging::warn(&format!(
                "recent session listing degraded; skipping unreadable sessions: {error}"
            ));
            store::reader::sources(&paths)?
                .into_iter()
                .filter(|source| source.layout == store::reader::SourceLayout::Canonical)
                .filter_map(|source| store::catalog::row(&paths, &source.session_id, probe.clone()).ok())
                .collect()
        }
    };
    let mut entries = rows
        .into_iter()
        .map(|row| {
            let id = row.session_id.as_str();
            RecentSessionMetadata {
                session_id: id.to_owned(),
                working_dir: row.working_dir,
                generated_title: row.generated_title,
                custom_title: row.custom_title,
                todo_title: live_todo_title(&home, id, row.todo_title),
                saved: row.saved,
                save_label: row.save_label,
                updated_at_ms: row.metadata_updated_at.timestamp_millis(),
                last_active_at_ms: row.last_active_at_ms,
            }
        })
        .collect::<Vec<_>>();
    entries.extend(legacy_only_entries(&home, &paths, &entries));
    entries.sort_by(|a, b| {
        b.last_active_at_ms
            .unwrap_or(b.updated_at_ms)
            .cmp(&a.last_active_at_ms.unwrap_or(a.updated_at_ms))
            .then(a.session_id.cmp(&b.session_id))
    });
    Ok(entries
        .into_iter()
        .enumerate()
        .filter_map(|(index, entry)| (index < limit || entry.saved).then_some(entry))
        .collect())
}

fn live_todo_title(home: &std::path::Path, id: &str, stored: Option<String>) -> Option<String> {
    let live = ["", "-plan"]
        .iter()
        .any(|suffix| home.join(format!("todos/{id}{suffix}.json")).exists());
    if live {
        crate::todo::load_session_title(id)
    } else {
        stored
    }
}

/// Sessions that exist only in the preserved legacy layout.
///
/// The catalog lists canonical logs, so a session nobody has opened since the
/// migration would otherwise vanish from the picker. They are read through the
/// legacy module that owns that layout, never re-parsed here, and one unreadable
/// snapshot never removes the others.
fn legacy_only_entries(
    home: &std::path::Path,
    paths: &store::SessionPaths,
    known: &[RecentSessionMetadata],
) -> Vec<RecentSessionMetadata> {
    let Ok(sources) = store::reader::sources(paths) else {
        return Vec::new();
    };
    sources
        .into_iter()
        .filter(|source| {
            source.layout == store::reader::SourceLayout::Legacy
                && !known.iter().any(|entry| entry.session_id == source.session_id.as_str())
        })
        .filter_map(|source| {
            let id = source.session_id;
            let read = match store::legacy::LegacySource::new(home, id.clone()).and_then(|source| source.read()) {
                Ok(read) => read,
                Err(error) => {
                    crate::logging::warn(&format!(
                        "recent session listing skipped unreadable legacy session {}: {error}",
                        id.as_str()
                    ));
                    return None;
                }
            };
            let metadata = read.metadata;
            let last_active_at_ms = read
                .auxiliary
                .get("last_active_at")
                .and_then(|value| serde_json::from_value::<chrono::DateTime<chrono::Utc>>(value.clone()).ok())
                .map(|at| at.timestamp_millis());
            Some(RecentSessionMetadata {
                todo_title: live_todo_title(home, id.as_str(), None),
                session_id: id.as_str().to_owned(),
                working_dir: metadata.working_dir,
                generated_title: metadata.title,
                custom_title: metadata.custom_title,
                saved: metadata.saved,
                save_label: metadata.save_label,
                updated_at_ms: metadata.updated_at.timestamp_millis(),
                last_active_at_ms,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_title_uses_custom_then_todo_then_generated() {
        let mut entry = RecentSessionMetadata {
            session_id: "session_test".into(),
            working_dir: None,
            generated_title: Some("Generated".into()),
            custom_title: None,
            todo_title: Some("Todo goal".into()),
            saved: false,
            save_label: None,
            updated_at_ms: 1,
            last_active_at_ms: None,
        };
        assert_eq!(entry.display_title(), Some("Todo goal"));
        entry.custom_title = Some("Renamed".into());
        assert_eq!(entry.display_title(), Some("Renamed"));
    }

    #[test]
    fn display_title_prefers_save_label_over_derived_titles() {
        let mut entry = RecentSessionMetadata {
            session_id: "session_test".into(),
            working_dir: None,
            generated_title: Some("Generated".into()),
            custom_title: None,
            todo_title: Some("Todo goal".into()),
            saved: true,
            save_label: Some("yc mcp".into()),
            updated_at_ms: 1,
            last_active_at_ms: None,
        };
        assert_eq!(entry.display_title(), Some("yc mcp"));
        entry.custom_title = Some("Renamed".into());
        assert_eq!(entry.display_title(), Some("Renamed"));
        entry.custom_title = None;
        entry.saved = false;
        assert_eq!(entry.display_title(), Some("Todo goal"));
    }
}
