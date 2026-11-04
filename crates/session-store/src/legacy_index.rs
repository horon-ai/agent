//! Optional SQLite read on a guarded staging copy only.
use crate::legacy_bundle::Bundle;
use crate::legacy_replay::Replay;
use crate::*;
use std::path::Path;

#[cfg(not(feature = "legacy-session-index-import"))]
pub(crate) fn import(
    bundle: &Bundle,
    _stage: &Path,
    _state: &mut Replay,
) -> Result<Option<Vec<SessionId>>, StoreError> {
    if bundle.has_index() {
        return Err(crate::legacy::migration_required());
    }
    Ok(None)
}

#[cfg(feature = "legacy-session-index-import")]
pub(crate) fn import(bundle: &Bundle, stage: &Path, state: &mut Replay) -> Result<Option<Vec<SessionId>>, StoreError> {
    use crate::legacy::corrupt;
    use rusqlite::types::ValueRef;
    use serde_json::{Map, Value, json};
    if !bundle.has_index() {
        return Ok(None);
    }
    let mut connection = open_copy(bundle, stage)?;
    let sql_error = |e: rusqlite::Error| corrupt(format!("legacy index copy: {e}"));
    let transaction = connection.transaction().map_err(sql_error)?;
    let mut inventory_statement = transaction
        .prepare("SELECT session_id FROM recent_sessions ORDER BY session_id")
        .map_err(sql_error)?;
    let inventory = inventory_statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sql_error)?
        .map(|row| SessionId::parse(row.map_err(sql_error)?))
        .collect::<Result<Vec<_>, _>>()?;
    drop(inventory_statement);
    let mut statement = transaction
        .prepare("SELECT * FROM recent_sessions WHERE session_id = ?1")
        .map_err(sql_error)?;
    let names = statement
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let mut rows = statement
        .query([stage
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| corrupt("stage session identity absent"))?])
        .map_err(sql_error)?;
    let mut values = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        let mut value = Map::new();
        for (index, name) in names.iter().enumerate() {
            let item = match row.get_ref(index).map_err(sql_error)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(n) => json!(n),
                ValueRef::Real(n) => {
                    let number =
                        serde_json::Number::from_f64(n).ok_or_else(|| corrupt("non-finite legacy SQLite real"))?;
                    Value::Number(number)
                }
                ValueRef::Text(bytes) => {
                    json!(std::str::from_utf8(bytes).map_err(|e| corrupt(e.to_string()))?)
                }
                ValueRef::Blob(bytes) => json!({"sqlite_blob_bytes":bytes}),
            };
            value.insert(name.clone(), item);
        }
        values.push(Value::Object(value));
    }
    drop(rows);
    drop(statement);
    transaction.commit().map_err(sql_error)?;
    values.sort_by_key(|v| v.to_string());
    if values.len() > 1 {
        return Err(corrupt("duplicate legacy index session rows"));
    }
    let id = stage
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| corrupt("stage identity absent"))?;
    if values.is_empty() && !bundle.files.contains_key(&format!("sessions/{id}.json")) {
        return Err(StoreError::unknown_session("index-only session row absent"));
    }
    if let Some(row) = values.first() {
        fn text(row: &Value, key: &str) -> Result<Option<String>, StoreError> {
            match row.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(s)) => Ok(Some(s.clone())),
                Some(_) => Err(crate::legacy::corrupt(format!("invalid index text {key}"))),
            }
        }
        state.metadata.custom_title = text(row, "custom_title")?;
        state.metadata.working_dir = text(row, "working_dir")?;
        state.metadata.saved = match row.get("saved") {
            None => false,
            Some(v) => match v.as_i64() {
                Some(0) => false,
                Some(1) => true,
                _ => return Err(corrupt("invalid index saved flag")),
            },
        };
        state.metadata.save_label = text(row, "save_label")?;
        let generated = text(row, "generated_title")?;
        let todo = text(row, "todo_title")?;
        state.metadata.title = [
            state.metadata.custom_title.as_deref(),
            state.metadata.save_label.as_deref().filter(|_| state.metadata.saved),
            todo.as_deref(),
            generated.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(str::to_owned);
        for key in ["updated_at_ms", "last_active_at_ms"] {
            if let Some(value) = row.get(key).filter(|v| !v.is_null()) {
                let ms = value.as_i64().ok_or_else(|| corrupt("invalid index time"))?;
                let time =
                    chrono::DateTime::from_timestamp_millis(ms).ok_or_else(|| corrupt("index time out of range"))?;
                if key == "updated_at_ms" {
                    state.metadata.updated_at = time;
                }
                state.metadata.legacy_extensions.insert(key.into(), value.clone());
            }
        }
        state
            .metadata
            .legacy_extensions
            .insert("legacy_index_display".into(), row.clone());
    }
    state.auxiliary.insert("legacy_index_rows".into(), json!(values));
    Ok(Some(inventory))
}

#[cfg(feature = "legacy-session-index-import")]
fn open_copy(bundle: &Bundle, stage: &Path) -> Result<rusqlite::Connection, StoreError> {
    use crate::legacy::corrupt;
    use crate::legacy_bundle::INDEX;
    use rusqlite::{Connection, OpenFlags};
    use std::io::Write;
    let working = stage.join("index-copy");
    let mut disk = crate::blobs::Disk {
        hook: Box::new(|_| Ok(())),
    };
    disk.directory(&working)?;
    for name in [INDEX.to_owned(), format!("{INDEX}-wal"), format!("{INDEX}-shm")] {
        if let Some(bytes) = bundle.files.get(&name) {
            let mut file = crate::blobs::new_file(&working.join(name))?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
    }
    let sql_error = |e: rusqlite::Error| corrupt(format!("legacy index copy: {e}"));
    let connection = Connection::open_with_flags(
        working.join(INDEX),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sql_error)?;
    connection
        .busy_timeout(std::time::Duration::from_secs(2))
        .map_err(sql_error)?;
    Ok(connection)
}

#[cfg(not(feature = "legacy-session-index-import"))]
pub(crate) fn inventory(_bundle: &Bundle, _stage: &Path) -> Result<Vec<SessionId>, StoreError> {
    Err(crate::legacy::migration_required())
}

#[cfg(feature = "legacy-session-index-import")]
pub(crate) fn inventory(bundle: &Bundle, stage: &Path) -> Result<Vec<SessionId>, StoreError> {
    let error = |e: rusqlite::Error| crate::legacy::corrupt(format!("legacy index copy: {e}"));
    let mut connection = open_copy(bundle, stage)?;
    let transaction = connection.transaction().map_err(error)?;
    let mut query = transaction
        .prepare("SELECT session_id FROM recent_sessions ORDER BY session_id")
        .map_err(error)?;
    let ids = query
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(error)?
        .map(|row| SessionId::parse(row.map_err(error)?))
        .collect::<Result<Vec<_>, _>>()?;
    drop(query);
    transaction.commit().map_err(error)?;
    if ids.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(crate::legacy::corrupt("duplicate legacy inventory identity"));
    }
    Ok(ids)
}
