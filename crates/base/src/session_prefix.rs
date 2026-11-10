//! Resolve a session id prefix to one full session id.
//!
//! A full id is `session_<name>_<millis>_<hex>` and operators paste it by hand.
//! When they paste only a unique leading part, every surface that takes an
//! existing session id resolves it here instead of failing on an id that is not
//! on disk. An exact id always wins, an ambiguous prefix never guesses, and a
//! prefix below [`MIN_SESSION_ID_PREFIX`] is refused rather than resolved.
//!
//! The scan reads directory entry names only. It never opens a session record,
//! so the expensive name/title scan `find_session_by_name_or_id` avoids for
//! generated full-id shapes stays avoided here too.

use anyhow::Result;

/// Shortest prefix accepted as a session id reference.
///
/// Six characters distinguishes a deliberate prefix from a stray fragment while
/// still letting an operator type `session_` plus one discriminating character.
pub const MIN_SESSION_ID_PREFIX: usize = 6;

/// Candidates reported for an ambiguous prefix. A caller gets enough to
/// disambiguate without reading a truncated list.
pub const MAX_PREFIX_CANDIDATES: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionPrefixError {
    /// The query is a prefix of stored sessions but too short to identify one.
    TooShort,
    /// More than one session starts with the query.
    Ambiguous { candidates: Vec<String> },
    /// No session starts with the query.
    NotFound,
}

impl SessionPrefixError {
    /// The candidates an ambiguous prefix matched, for a reply that lets the
    /// caller pick one.
    pub fn candidates(&self) -> &[String] {
        match self {
            Self::Ambiguous { candidates } => candidates,
            _ => &[],
        }
    }
}

impl std::fmt::Display for SessionPrefixError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort => write!(
                formatter,
                "session id prefix must be at least {MIN_SESSION_ID_PREFIX} characters"
            ),
            Self::Ambiguous { candidates } => write!(
                formatter,
                "session id prefix matches {} sessions: {}",
                candidates.len(),
                candidates.join(", ")
            ),
            Self::NotFound => write!(formatter, "no session matches that id prefix"),
        }
    }
}

impl std::error::Error for SessionPrefixError {}

/// Choose the one session a query names, or say why it cannot.
///
/// An exact candidate always wins: a prefix never overrides a session that
/// literally carries that id. Duplicates collapse, so the same session
/// discovered from disk and from a live map does not read as ambiguous.
pub fn choose_session_prefix_match(
    query: &str,
    candidates: impl IntoIterator<Item = String>,
) -> Result<String, SessionPrefixError> {
    if query.is_empty() {
        return Err(SessionPrefixError::NotFound);
    }
    let mut matching: Vec<String> = Vec::new();
    let mut exact = false;
    for candidate in candidates {
        if candidate == query {
            exact = true;
        }
        if candidate.starts_with(query) && !matching.iter().any(|seen| *seen == candidate) {
            matching.push(candidate);
        }
    }
    if exact {
        return Ok(query.to_string());
    }
    if matching.is_empty() {
        return Err(SessionPrefixError::NotFound);
    }
    if query.len() < MIN_SESSION_ID_PREFIX {
        return Err(SessionPrefixError::TooShort);
    }
    match matching.len() {
        1 => Ok(matching.remove(0)),
        _ => {
            matching.sort();
            matching.truncate(MAX_PREFIX_CANDIDATES);
            Err(SessionPrefixError::Ambiguous { candidates: matching })
        }
    }
}

/// Every stored session id that starts with `query`, read from directory
/// entry names only.
///
/// Both layouts are covered: a canonical `sessions/<id>/` directory and a
/// legacy `sessions/<id>.json` file. Nothing is opened, so a prefix query costs
/// one directory listing rather than one transcript parse per session.
///
/// # Errors
/// Propagates an unreadable sessions root. A missing root is no matches.
pub fn stored_session_id_prefix_matches(query: &str) -> Result<Vec<String>> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let paths = crate::session::store_paths()?;
    let root = paths.root();
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut matches: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let file_type = entry.file_type()?;
        let id = if file_type.is_dir() {
            name
        } else if file_type.is_file() && name.ends_with(".json") {
            name.trim_end_matches(".json")
        } else {
            continue;
        };
        if !id.starts_with(query) {
            continue;
        }
        // Only names the store accepts are sessions; a stray file that happens
        // to share the prefix is not a candidate.
        if horon_agent_session_store::SessionId::parse(id).is_err() {
            continue;
        }
        matches.push(id.to_string());
    }
    matches.sort();
    matches.dedup();
    matches.truncate(MAX_PREFIX_CANDIDATES);
    Ok(matches)
}

/// Resolve a query against stored sessions only.
///
/// Live sessions that were never persisted are not on disk; callers holding
/// them pass their ids to [`choose_session_prefix_match`] instead.
pub fn resolve_stored_session_prefix(query: &str) -> Result<String, SessionPrefixError> {
    // A store that cannot be read contributes no candidates; the caller then
    // reports the query as unknown rather than as a resolution failure.
    let stored = match stored_session_id_prefix_matches(query) {
        Ok(ids) => ids,
        Err(_) => Vec::new(),
    };
    choose_session_prefix_match(query, stored)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    #[test]
    fn an_exact_id_wins_over_prefix_matching() {
        let chosen = choose_session_prefix_match(
            "session_fox_1790191044353",
            stored(&["session_fox_1790191044353", "session_fox_1790191044353_621ff7b9"]),
        )
        .expect("exact id resolves");
        assert_eq!(chosen, "session_fox_1790191044353");
    }

    #[test]
    fn one_matching_prefix_resolves_and_duplicates_do_not_ambiguate() {
        let id = "session_fox_1790191044353_621ff7b9";
        let chosen =
            choose_session_prefix_match("session_fox", stored(&[id, id, "session_owl_1790191044353_621ff7b9"]))
                .expect("one distinct candidate");
        assert_eq!(chosen, id);
    }

    #[test]
    fn two_matching_prefixes_are_ambiguous_and_list_both() {
        let error = choose_session_prefix_match(
            "session_fox",
            stored(&[
                "session_fox_1790191044353_621ff7b9",
                "session_fox_1790191044354_621ff7b9",
            ]),
        )
        .expect_err("two candidates cannot resolve");
        assert_eq!(
            error,
            SessionPrefixError::Ambiguous {
                candidates: stored(&[
                    "session_fox_1790191044353_621ff7b9",
                    "session_fox_1790191044354_621ff7b9",
                ])
            }
        );
    }

    #[test]
    fn ambiguity_stops_at_ten_candidates() {
        let candidates: Vec<String> = (0..14)
            .map(|index| format!("session_fox_17901910443{index:02}_621ff7b9"))
            .collect();
        match choose_session_prefix_match("session_fox", candidates) {
            Err(SessionPrefixError::Ambiguous { candidates }) => {
                assert_eq!(candidates.len(), MAX_PREFIX_CANDIDATES)
            }
            other => panic!("expected an ambiguous prefix, got {other:?}"),
        }
    }

    #[test]
    fn a_prefix_below_the_minimum_is_refused_even_when_it_matches() {
        // The query is a real prefix of a stored session, so the refusal is
        // about the length, not about a missing session.
        assert_eq!(
            choose_session_prefix_match("sessi", stored(&["session_fox_1790191044353_621ff7b9"])),
            Err(SessionPrefixError::TooShort)
        );
        assert_eq!(
            choose_session_prefix_match("s", stored(&["session_fox_1790191044353", "session_owl_1"])),
            Err(SessionPrefixError::TooShort)
        );
    }

    #[test]
    fn a_query_that_matches_nothing_is_not_found_at_any_length() {
        assert_eq!(
            choose_session_prefix_match("fox", stored(&["session_fox_1790191044353"])),
            Err(SessionPrefixError::NotFound)
        );
        assert_eq!(
            choose_session_prefix_match("session_none", stored(&["session_fox_1790191044353"])),
            Err(SessionPrefixError::NotFound)
        );
    }
}
