//! ACP `session/list` over the host-local session catalog, and the catalog
//! lookup `session/delete` uses.

use super::*;
use crate::session::event_store_api as store;
use chrono::{DateTime, SecondsFormat, Utc};
use std::path::Path;

const PAGE_SIZE: usize = 50;
const CURSOR_PREFIX: &str = "o:";

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ListRow {
    pub(super) session_id: String,
    pub(super) cwd: Option<String>,
    pub(super) title: Option<String>,
    pub(super) updated_at: DateTime<Utc>,
    pub(super) last_active_at_ms: Option<i64>,
    pub(super) archived: bool,
}

impl ListRow {
    fn from_summary(summary: store::catalog::Summary) -> Self {
        let title = [
            summary.custom_title.as_deref(),
            summary.save_label.as_deref(),
            summary.todo_title.as_deref(),
            summary.generated_title.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|title| !title.is_empty())
        .map(str::to_string);
        Self {
            session_id: summary.session_id.as_str().to_string(),
            cwd: summary.working_dir,
            title,
            updated_at: summary.updated_at,
            last_active_at_ms: summary.last_active_at_ms,
            archived: summary.archived,
        }
    }

    fn activity_ms(&self) -> i64 {
        self.last_active_at_ms
            .unwrap_or_else(|| self.updated_at.timestamp_millis())
    }

    fn listed_cwd(&self) -> Option<&str> {
        self.cwd.as_deref().filter(|cwd| Path::new(cwd).is_absolute())
    }

    /// IS-13: the v2 row. The catalog keeps no additional directories, so
    /// none are reported.
    fn to_session_info_v2(&self, cwd: &str) -> Value {
        v2::shapes::session_info(
            &self.session_id,
            Path::new(cwd),
            self.title.as_deref(),
            Some(&self.updated_at_rfc3339()),
            &[],
        )
    }

    fn updated_at_rfc3339(&self) -> String {
        self.updated_at.to_rfc3339_opts(SecondsFormat::Millis, true)
    }

    fn to_session_info(&self, cwd: &str) -> Value {
        let mut info = json!({
            "sessionId": self.session_id,
            "cwd": cwd,
            "updatedAt": self.updated_at_rfc3339(),
        });
        if let (Some(title), Some(object)) = (&self.title, info.as_object_mut()) {
            object.insert("title".to_string(), json!(title));
        }
        info
    }
}

fn store_paths() -> Result<store::SessionPaths> {
    let home = crate::storage::horon_agent_dir()?;
    let home = if home.exists() { home.canonicalize()? } else { home };
    Ok(store::SessionPaths::new(home.join("sessions")))
}

/// Every catalog row. One unreadable session drops out instead of failing
/// the whole listing, as the recent-session picker does.
pub(super) fn catalog_rows() -> Result<Vec<ListRow>> {
    let paths = store_paths()?;
    store::legacy::require_index_migration(&paths)?;
    if !paths.root().try_exists()? {
        return Ok(Vec::new());
    }
    let probe = store::ReadProbe::default();
    let summaries = match store::catalog::list(&paths, probe.clone()) {
        Ok(summaries) => summaries,
        Err(error) => {
            crate::logging::warn(&format!(
                "ACP: session/list degraded; skipping unreadable sessions: {error}"
            ));
            store::reader::sources(&paths)?
                .into_iter()
                .filter(|source| source.layout == store::reader::SourceLayout::Canonical)
                .filter_map(|source| store::catalog::row(&paths, &source.session_id, probe.clone()).ok())
                .collect()
        }
    };
    Ok(summaries.into_iter().map(ListRow::from_summary).collect())
}

/// The title and last activity of one session, for a v2 resume's
/// `session_info_update`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CatalogInfo {
    pub(crate) title: Option<String>,
    /// RFC 3339.
    pub(crate) updated_at: String,
}

/// The catalog row of `session_id`, `None` when the store has none or cannot
/// read it while its writer is owned.
pub(super) fn catalog_info(session_id: &str) -> Result<Option<CatalogInfo>> {
    let Ok(id) = store::SessionId::parse(session_id) else {
        return Ok(None);
    };
    let paths = store_paths()?;
    if !paths.root().try_exists()? {
        return Ok(None);
    }
    if store::reader::layout(&paths, &id)? != Some(store::reader::SourceLayout::Canonical) {
        return Ok(None);
    }
    match store::catalog::row(&paths, &id, store::ReadProbe::default()) {
        Ok(summary) => {
            let row = ListRow::from_summary(summary);
            Ok(Some(CatalogInfo {
                updated_at: row.updated_at_rfc3339(),
                title: row.title,
            }))
        }
        Err(error) if error.code == store::StoreErrorCode::Busy => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// What the catalog knows about one session, for `session/delete`.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum CatalogLookup {
    /// No canonical log: never created, or not in this store.
    Missing,
    Archived,
    Present {
        cwd: Option<String>,
    },
}

pub(super) fn catalog_lookup(session_id: &str) -> Result<CatalogLookup> {
    let Ok(id) = store::SessionId::parse(session_id) else {
        return Ok(CatalogLookup::Missing);
    };
    let paths = store_paths()?;
    if !paths.root().try_exists()? {
        return Ok(CatalogLookup::Missing);
    }
    if store::reader::layout(&paths, &id)? != Some(store::reader::SourceLayout::Canonical) {
        return Ok(CatalogLookup::Missing);
    }
    lookup_from_row(store::catalog::row(&paths, &id, store::ReadProbe::default()))
}

/// A Busy row exists but its writer is owned, normally by the daemon that
/// still holds the session; the daemon then archives it, and archiving an
/// archived session is a no-op, so delete stays idempotent.
pub(super) fn lookup_from_row(
    row: std::result::Result<store::catalog::Summary, store::StoreError>,
) -> Result<CatalogLookup> {
    match row {
        Ok(summary) if summary.archived => Ok(CatalogLookup::Archived),
        Ok(summary) => Ok(CatalogLookup::Present {
            cwd: summary.working_dir,
        }),
        Err(error) if error.code == store::StoreErrorCode::Busy => Ok(CatalogLookup::Present { cwd: None }),
        Err(error) => Err(error.into()),
    }
}

/// One `session/list` page: unarchived rows with an absolute cwd, filtered
/// by exact cwd, newest activity first.
/// The rows take the shape of the protocol `version` the connection speaks.
pub(super) fn session_list_page(
    rows: Vec<ListRow>,
    cwd: Option<&str>,
    cursor: Option<&str>,
    version: AcpVersion,
) -> std::result::Result<Value, String> {
    let offset = match cursor {
        Some(cursor) => decode_cursor(cursor).ok_or_else(|| format!("Invalid session/list cursor: {cursor:?}"))?,
        None => 0,
    };
    let mut rows: Vec<ListRow> = rows
        .into_iter()
        .filter(|row| !row.archived)
        .filter(|row| {
            row.listed_cwd()
                .is_some_and(|row_cwd| cwd.is_none_or(|cwd| cwd == row_cwd))
        })
        .collect();
    rows.sort_by(|a, b| {
        b.activity_ms()
            .cmp(&a.activity_ms())
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    let sessions: Vec<Value> = rows
        .iter()
        .skip(offset)
        .take(PAGE_SIZE)
        .filter_map(|row| {
            row.listed_cwd().map(|cwd| match version {
                AcpVersion::V2 => row.to_session_info_v2(cwd),
                AcpVersion::V1 => row.to_session_info(cwd),
            })
        })
        .collect();
    let mut page = json!({ "sessions": sessions });
    let next = offset.saturating_add(PAGE_SIZE);
    if next < rows.len()
        && let Some(object) = page.as_object_mut()
    {
        object.insert("nextCursor".to_string(), json!(encode_cursor(next)));
    }
    Ok(page)
}

pub(super) fn encode_cursor(offset: usize) -> String {
    base64url_encode(format!("{CURSOR_PREFIX}{offset}").as_bytes())
}

pub(super) fn decode_cursor(cursor: &str) -> Option<usize> {
    let decoded = String::from_utf8(base64url_decode(cursor)?).ok()?;
    let digits = decoded.strip_prefix(CURSOR_PREFIX)?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

const BASE64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64url_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let triple = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (index, byte)| acc | (u32::from(*byte) << (16 - 8 * index)));
        for index in 0..=chunk.len() {
            out.push(char::from(BASE64URL[((triple >> (18 - 6 * index)) & 0x3f) as usize]));
        }
    }
    out
}

fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    if text.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    for chunk in text.as_bytes().chunks(4) {
        let mut triple = 0u32;
        for (index, symbol) in chunk.iter().enumerate() {
            let value = BASE64URL.iter().position(|candidate| candidate == symbol)?;
            triple |= (value as u32) << (18 - 6 * index);
        }
        for index in 0..chunk.len() - 1 {
            out.push(((triple >> (16 - 8 * index)) & 0xff) as u8);
        }
    }
    Some(out)
}

fn optional_string(params: &Value, field: &str) -> std::result::Result<Option<String>, String> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("session/list {field} must be a string")),
    }
}

impl AcpRuntime {
    pub(super) async fn handle_session_list(&self, message: IncomingRequest) -> Result<()> {
        let id = message.id;
        let params = &message.params;
        if !(params.is_null() || params.is_object()) {
            return self
                .write_error_value(
                    id,
                    JSONRPC_INVALID_PARAMS,
                    "session/list params must be an object".to_string(),
                )
                .await;
        }
        let (cwd, cursor) = match (optional_string(params, "cwd"), optional_string(params, "cursor")) {
            (Ok(cwd), Ok(cursor)) => (cwd, cursor),
            (Err(err), _) | (_, Err(err)) => return self.write_error_value(id, JSONRPC_INVALID_PARAMS, err).await,
        };
        if let Some(cursor) = cursor.as_deref()
            && decode_cursor(cursor).is_none()
        {
            return self
                .write_error_value(
                    id,
                    JSONRPC_INVALID_PARAMS,
                    format!("Invalid session/list cursor: {cursor:?}"),
                )
                .await;
        }
        let rows = tokio::task::spawn_blocking(catalog_rows)
            .await
            .map_err(anyhow::Error::from)
            .and_then(|rows| rows);
        match rows {
            Ok(rows) => match session_list_page(
                rows,
                cwd.as_deref(),
                cursor.as_deref(),
                self.spoken_version().unwrap_or(AcpVersion::V1),
            ) {
                Ok(page) => self.write_result(id, page).await,
                Err(err) => self.write_error_value(id, JSONRPC_INVALID_PARAMS, err).await,
            },
            Err(err) => {
                self.write_error_value(
                    id,
                    JSONRPC_INTERNAL_ERROR,
                    format!("Failed to read the session catalog: {err:#}"),
                )
                .await
            }
        }
    }
}
