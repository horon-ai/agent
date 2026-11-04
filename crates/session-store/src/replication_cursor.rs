//! Self-checking fixed-closure continuations, never local log offsets.
use crate::replication_codec::{decode, encode};
use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Position {
    version: u32,
    session_id: SessionId,
    heads: Vec<EventId>,
    closure_digest: String,
    index: usize,
    purpose: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    position: Position,
    checksum: String,
}

pub(crate) fn invalid() -> StoreError {
    StoreError::invalid_request(StoreErrorCode::InvalidCursor, "invalid replica cursor")
}

pub(crate) fn make(
    id: &SessionId,
    heads: &[EventId],
    digest: &str,
    index: usize,
    purpose: &str,
) -> Result<String, StoreError> {
    let position = Position {
        version: 1,
        session_id: id.clone(),
        heads: heads.to_vec(),
        closure_digest: digest.into(),
        index,
        purpose: purpose.into(),
    };
    let checksum = sha256_hex(&serde_json::to_vec(&position).map_err(|_| invalid())?);
    Ok(encode(
        &serde_json::to_vec(&Cursor { position, checksum }).map_err(|_| invalid())?,
        true,
    ))
}

pub(crate) fn check(
    token: &str,
    id: &SessionId,
    heads: &[EventId],
    digest: &str,
    len: usize,
    purpose: &str,
) -> Result<usize, StoreError> {
    let bytes = decode(token, true, 32 * 1024).map_err(|_| invalid())?;
    let cursor: Cursor = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    let position = &cursor.position;
    if position.version != 1
        || position.session_id != *id
        || position.heads != heads
        || position.closure_digest != digest
        || position.index > len
        || position.purpose != purpose
        || cursor.checksum != sha256_hex(&serde_json::to_vec(position).map_err(|_| invalid())?)
    {
        return Err(invalid());
    }
    Ok(position.index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modified_cursor_fields_are_rejected_before_resuming() {
        let id = SessionId::parse("cursor").unwrap();
        let heads = vec![EventId::new_v4()];
        let digest = "a".repeat(64);
        let token = make(&id, &heads, &digest, 256, "export").unwrap();
        let value: serde_json::Value = serde_json::from_slice(&decode(&token, true, 32 * 1024).unwrap()).unwrap();
        for (field, replacement) in [
            ("version", serde_json::json!(2)),
            ("session_id", serde_json::json!("another")),
            ("heads", serde_json::json!([EventId::new_v4()])),
            ("closure_digest", serde_json::json!("b".repeat(64))),
            ("index", serde_json::json!(0)),
            ("purpose", serde_json::json!("import")),
        ] {
            let mut changed = value.clone();
            changed["position"][field] = replacement;
            let changed = encode(&serde_json::to_vec(&changed).unwrap(), true);
            assert_eq!(
                check(&changed, &id, &heads, &digest, 300, "export").unwrap_err().code,
                StoreErrorCode::InvalidCursor
            );
        }
        assert_eq!(check(&token, &id, &heads, &digest, 300, "export").unwrap(), 256);
    }
}
