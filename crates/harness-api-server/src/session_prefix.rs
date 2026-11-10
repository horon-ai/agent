//! Resolve a session id prefix before a session-addressed request is translated.
//!
//! The harness API takes an existing session id in eight requests. An operator
//! pasting a unique leading part of an id should not have to paste all of it, so
//! this step rewrites the request to the full id first. It runs in the bridge
//! rather than the daemon because the bridge already enumerates stored sessions
//! for `list_sessions`, and because the legacy daemon protocol has no prefix
//! concept to extend.
//!
//! Nothing here changes a request that already names a session the connection
//! knows. An id the caller sent verbatim keeps travelling untouched, so the
//! daemon's own unknown-session answer is unchanged for every real id, and only
//! a query that names no session on disk is a candidate for prefix matching.

use horon_agent_base::session_prefix::SessionPrefixError;
use horon_agent_harness_api::{ApiEvent, ErrorCode, ServerFrame, StoreErrorDetail};
use serde_json::{Value, json};

/// Requests that take an existing session id. A new session (`create_session`)
/// has no id to resolve, and a request addressed to the attached session is
/// covered by the exact-id check below.
const SESSION_ADDRESSED: &[&str] = &[
    "attach_session",
    "fork_session",
    "transfer_session",
    "rewind",
    "export_session_markdown",
    "peek_session",
    "archive_session",
    "restore_session",
];

/// Rewrite `request["session_id"]` to the full id it names, or answer locally.
///
/// Returns `Some(frame)` only when the request must be answered here: an
/// ambiguous prefix, or a prefix too short to identify anything. `None` means
/// the request continues, rewritten or untouched.
///
/// `attached` is this connection's session and `known` is every id the daemon
/// has reported. A live session that was never persisted exists only in those
/// two, so they take part in the match.
///
/// The candidates of an ambiguous prefix travel in `details` under the existing
/// `unknown_session` code, because `ErrorCode` keeps its five stable values for
/// older clients. A client that wants to distinguish the case reads
/// `details.store_code == "ambiguous_session"`.
pub(crate) fn resolve(request: &mut Value, attached: Option<&str>, known: &[String]) -> Option<ServerFrame> {
    let req = request["req"].as_str()?;
    if !SESSION_ADDRESSED.contains(&req) {
        return None;
    }
    let reply_to = request["id"].as_u64().unwrap_or(0);
    let query = request["session_id"].as_str()?.to_string();
    if query.is_empty() || attached == Some(query.as_str()) || known.iter().any(|id| *id == query) {
        return None;
    }

    let mut candidates: Vec<String> =
        horon_agent_base::session_prefix::stored_session_id_prefix_matches(&query).unwrap_or_default();
    for id in attached.into_iter().chain(known.iter().map(String::as_str)) {
        if id.starts_with(&query) && !candidates.iter().any(|seen| seen == id) {
            candidates.push(id.to_string());
        }
    }

    match horon_agent_base::session_prefix::choose_session_prefix_match(&query, candidates) {
        Ok(full_id) => {
            request["session_id"] = json!(full_id);
            None
        }
        Err(SessionPrefixError::Ambiguous { candidates }) => {
            let message = format!(
                "session id prefix `{query}` matches {} sessions: {}; send a longer prefix or a full id",
                candidates.len(),
                candidates.join(", ")
            );
            Some(ServerFrame::reply(
                reply_to,
                ApiEvent::Error {
                    code: ErrorCode::UnknownSession,
                    message,
                    details: Some(StoreErrorDetail {
                        store_code: "ambiguous_session".into(),
                        retryable: false,
                        accepted_ids: candidates,
                        ..StoreErrorDetail::default()
                    }),
                },
            ))
        }
        Err(SessionPrefixError::TooShort) => Some(ServerFrame::reply(
            reply_to,
            ApiEvent::Error {
                code: ErrorCode::InvalidRequest,
                message: SessionPrefixError::TooShort.to_string(),
                details: None,
            },
        )),
        // No stored session starts with this query. Leave the request as it is
        // so the daemon answers with its own unknown-session error, which is the
        // answer an operator already knows how to read.
        Err(SessionPrefixError::NotFound) => None,
    }
}

impl crate::translate::BridgeState {
    /// Rewrite a session id prefix to the full id it names.
    ///
    /// Returns a frame only when the request must be answered here: an
    /// ambiguous prefix, or a prefix too short to identify anything. A request
    /// that already names a session this connection knows is untouched, so the
    /// daemon's own unknown-session answer is unchanged for every real id.
    pub fn resolve_session_prefix(&self, request: &mut Value) -> Option<ServerFrame> {
        resolve(request, self.session_id.as_deref(), &self.known_sessions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(req: &str, session_id: &str) -> Value {
        json!({"v": 1, "id": 7, "req": req, "session_id": session_id})
    }

    #[test]
    fn an_unknown_request_kind_is_left_alone() {
        let mut value = request("send_message", "session_fox_1790191044353_621ff7b9");
        assert!(resolve(&mut value, None, &[]).is_none());
        assert_eq!(value["session_id"], "session_fox_1790191044353_621ff7b9");
    }

    #[test]
    fn an_id_the_connection_knows_is_never_rewritten() {
        let mut value = request("attach_session", "session_live_empty");
        assert!(resolve(&mut value, None, &["session_live_empty".into()]).is_none());
        assert_eq!(value["session_id"], "session_live_empty");
    }

    #[test]
    fn the_attached_session_is_never_rewritten() {
        let mut value = request("fork_session", "session_fox_1790191044353_621ff7b9");
        assert!(resolve(&mut value, Some("session_fox_1790191044353_621ff7b9"), &[]).is_none());
    }

    #[test]
    fn a_query_matching_nothing_keeps_the_daemons_unknown_session_answer() {
        let mut value = request("attach_session", "session_none_1790191044353");
        assert!(resolve(&mut value, None, &[]).is_none());
        assert_eq!(value["session_id"], "session_none_1790191044353");
    }

    #[test]
    fn an_empty_session_id_is_not_resolved() {
        let mut value = request("attach_session", "");
        assert!(resolve(&mut value, None, &[]).is_none());
    }

    #[test]
    fn an_ambiguous_prefix_reports_unknown_session_with_the_candidates() {
        // Two live sessions share the prefix; no disk scan is needed.
        let known = vec![
            "session_fox_1790191044353_621ff7b9".to_string(),
            "session_fox_1790191044354_621ff7b9".to_string(),
        ];
        let mut value = request("attach_session", "session_fox");
        let frame = resolve(&mut value, None, &known).expect("ambiguous prefix answers locally");
        assert_eq!(frame.reply_to, Some(7));
        match frame.event {
            ApiEvent::Error { code, message, details } => {
                assert_eq!(code, ErrorCode::UnknownSession);
                assert!(message.contains("session_fox_1790191044353_621ff7b9"), "{message}");
                let details = details.expect("candidates travel in details");
                assert_eq!(details.store_code, "ambiguous_session");
                assert_eq!(details.accepted_ids.len(), 2);
            }
            other => panic!("expected an error frame, got {other:?}"),
        }
    }

    #[test]
    fn a_unique_live_prefix_resolves_to_the_full_id() {
        let known = vec!["session_fox_1790191044353_621ff7b9".to_string()];
        let mut value = request("attach_session", "session_fox_17");
        assert!(resolve(&mut value, None, &known).is_none());
        assert_eq!(value["session_id"], "session_fox_1790191044353_621ff7b9");
    }

    #[test]
    fn a_prefix_below_the_minimum_that_matches_is_refused() {
        let known = vec!["session_fox_1790191044353_621ff7b9".to_string()];
        let mut value = request("attach_session", "sessio");
        let frame = resolve(&mut value, None, &known).expect("short matching prefix is refused");
        match frame.event {
            ApiEvent::Error { code, message, .. } => {
                assert_eq!(code, ErrorCode::InvalidRequest);
                assert!(message.contains("at least 6 characters"), "{message}");
            }
            other => panic!("expected an error frame, got {other:?}"),
        }
        assert_eq!(value["session_id"], "sessio");
    }
}
