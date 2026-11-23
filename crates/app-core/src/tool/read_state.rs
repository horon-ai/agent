//! What `read` already put in front of the model, per session.
//!
//! A repeated read of lines whose content has not changed is answered with a
//! one-line notice, and a whole-file re-read of a changed file with a diff
//! against the earlier read. Both are only correct while the earlier output is
//! still verbatim in the model's context, so every path that rewrites history
//! (compaction, transcript mutation, emergency truncation, tool-result aging,
//! session close) calls [`forget_session`].
//!
//! Change detection is a SHA-256 of the file bytes, so writes by `bash`,
//! `undo`, or another process are caught without subscribing to file events.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// Largest file whose whole-file read keeps a diff baseline snapshot.
pub(crate) const READ_SNAPSHOT_MAX_BYTES: usize = 1024 * 1024;
/// Artifact name of a diff baseline in the session artifact store.
pub(crate) const READ_SNAPSHOT_TOOL: &str = "read-snapshot";
pub(crate) const READ_STATE_MAX_SESSIONS: usize = 64;
pub(crate) const READ_STATE_MAX_PATHS: usize = 512;

#[derive(Clone, Debug)]
pub(crate) struct ReadRecord {
    /// SHA-256 of the file bytes at the read.
    pub(crate) hash: [u8; 32],
    pub(crate) edit_evidence: bool,
    /// Emitted `[start, end)` 0-based line ranges, sorted and merged.
    pub(crate) ranges: Vec<(usize, usize)>,
    pub(crate) hashline: bool,
    pub(crate) total_lines: usize,
    pub(crate) tool_call_id: String,
    /// Diff baseline (session artifact id), kept when the whole file was read
    /// and fits [`READ_SNAPSHOT_MAX_BYTES`].
    pub(crate) snapshot_id: Option<u64>,
}

impl ReadRecord {
    /// Whether `[start, end)` lies inside one emitted range.
    pub(crate) fn covers(&self, start: usize, end: usize) -> bool {
        start < end && self.ranges.iter().any(|&(from, to)| from <= start && end <= to)
    }

    /// Add `[start, end)` and merge overlapping or adjacent ranges.
    pub(crate) fn merge_range(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        self.ranges.push((start, end));
        self.ranges.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(self.ranges.len());
        for &(from, to) in &self.ranges {
            match merged.last_mut() {
                Some(last) if from <= last.1 => last.1 = last.1.max(to),
                _ => merged.push((from, to)),
            }
        }
        self.ranges = merged;
    }
}

#[derive(Default)]
struct SessionReads {
    records: HashMap<PathBuf, ReadRecord>,
    /// Insertion order; the oldest path is dropped past [`READ_STATE_MAX_PATHS`].
    order: VecDeque<PathBuf>,
}

#[derive(Default)]
struct ReadState {
    sessions: HashMap<String, SessionReads>,
    order: VecDeque<String>,
}

static STATE: LazyLock<Mutex<ReadState>> = LazyLock::new(|| Mutex::new(ReadState::default()));

fn state() -> std::sync::MutexGuard<'static, ReadState> {
    STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Stable key for a resolved path: canonical when it exists.
pub(crate) fn key_for(resolved: &Path) -> PathBuf {
    std::fs::canonicalize(resolved).unwrap_or_else(|_| resolved.to_path_buf())
}

pub(crate) fn lookup(session_id: &str, path: &Path) -> Option<ReadRecord> {
    state().sessions.get(session_id)?.records.get(path).cloned()
}

/// Byte positions of fully shown lines in the text the edit is applying to.
pub(super) struct EditEvidence {
    ranges: Vec<(usize, usize)>,
    stale: bool,
}

impl EditEvidence {
    pub(super) fn for_text(session: &str, path: &Path, text: &str) -> Self {
        use sha2::{Digest, Sha256};
        let mut evidence = Self {
            ranges: Vec::new(),
            stale: false,
        };
        let Some(read) = lookup(session, &key_for(path)).filter(|record| record.edit_evidence) else {
            return evidence;
        };
        let current: [u8; 32] = Sha256::digest(text.as_bytes()).into();
        if read.hash != current {
            evidence.stale = true;
            return evidence;
        }
        let mut spans = read.ranges.into_iter().peekable();
        let mut offset = 0;
        for (line, part) in text.split_inclusive('\n').enumerate() {
            let end = offset + part.len();
            if spans.peek().is_some_and(|&(_, stop)| line == stop) {
                spans.next();
            }
            if spans.peek().is_some_and(|&(start, _)| line >= start)
                && part
                    .lines()
                    .next()
                    .is_none_or(|body| body.len() <= super::read_selector::MAX_LINE_LEN)
            {
                match evidence.ranges.last_mut() {
                    Some(last) if last.1 == offset => last.1 = end,
                    _ => evidence.ranges.push((offset, end)),
                }
            }
            offset = end;
        }
        evidence
    }

    /// Move evidence with the edit so a later operation can use its output.
    pub(super) fn replace(
        &mut self,
        text: &str,
        path: &str,
        start: usize,
        end: usize,
        replacement: &str,
    ) -> anyhow::Result<()> {
        let joins_next =
            end > start && end < text.len() && text.as_bytes()[end - 1] == b'\n' && !replacement.ends_with('\n');
        // An empty slice (fuzzy match of a blank line) must still require that
        // the line at `start` was read, rather than accepting `to == start`
        // from an earlier line.
        let required_end = if end == start {
            (start + 1).min(text.len())
        } else {
            end + usize::from(joins_next)
        };
        let Some(index) = self
            .ranges
            .iter()
            .position(|&(from, to)| from <= start && required_end <= to)
        else {
            let first = text.as_bytes()[..start].iter().filter(|&&byte| byte == b'\n').count() + 1;
            let span_end = required_end.max((start + 1).min(text.len()));
            let last = if span_end > start {
                first
                    + text.as_bytes()[start..span_end - 1]
                        .iter()
                        .filter(|&&byte| byte == b'\n')
                        .count()
            } else {
                first
            };
            let status = if self.stale { "stale read" } else { "unread lines" };
            anyhow::bail!(
                "Cannot edit {path} lines {first}-{last}: {status}. Read the current text with start_line={first}, end_line={last}, full=true, then retry."
            );
        };
        let removed = end - start;
        let added = replacement.len();
        self.ranges[index].1 = self.ranges[index].1 - removed + added;
        for (from, to) in &mut self.ranges[index + 1..] {
            *from = *from - removed + added;
            *to = *to - removed + added;
        }
        Ok(())
    }
}

pub(crate) fn record(session_id: &str, path: PathBuf, record: ReadRecord) {
    let mut state = state();
    if !state.sessions.contains_key(session_id) {
        state.order.push_back(session_id.to_string());
        while state.order.len() > READ_STATE_MAX_SESSIONS {
            if let Some(oldest) = state.order.pop_front() {
                state.sessions.remove(&oldest);
            }
        }
    }
    let session = state.sessions.entry(session_id.to_string()).or_default();
    if session.records.insert(path.clone(), record).is_none() {
        session.order.push_back(path);
        while session.order.len() > READ_STATE_MAX_PATHS {
            if let Some(oldest) = session.order.pop_front() {
                session.records.remove(&oldest);
            }
        }
    }
}

/// Drop everything known about `session_id`'s earlier reads.
pub(crate) fn forget_session(session_id: &str) {
    let mut state = state();
    if state.sessions.remove(session_id).is_some() {
        state.order.retain(|id| id != session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_with(ranges: &[(usize, usize)]) -> ReadRecord {
        let mut record = ReadRecord {
            hash: [0; 32],
            edit_evidence: false,
            ranges: Vec::new(),
            hashline: false,
            total_lines: 100,
            tool_call_id: "c".into(),
            snapshot_id: None,
        };
        for &(start, end) in ranges {
            record.merge_range(start, end);
        }
        record
    }

    #[test]
    fn merged_ranges_cover_spans_across_adjacent_reads() {
        let record = record_with(&[(0, 10), (10, 20), (30, 40)]);
        assert_eq!(record.ranges, vec![(0, 20), (30, 40)]);
        assert!(record.covers(5, 15));
        assert!(!record.covers(15, 35), "the gap 20..30 was never shown");
        assert!(!record.covers(5, 5), "an empty request is never a hit");
    }

    #[test]
    fn paths_and_sessions_are_bounded() {
        let session = "read-state-bounds";
        forget_session(session);
        for index in 0..=READ_STATE_MAX_PATHS {
            record(session, PathBuf::from(format!("/p/{index}")), record_with(&[(0, 1)]));
        }
        assert!(lookup(session, Path::new("/p/0")).is_none(), "oldest path evicted");
        assert!(lookup(session, Path::new(&format!("/p/{READ_STATE_MAX_PATHS}"))).is_some());
        forget_session(session);
        assert!(lookup(session, Path::new(&format!("/p/{READ_STATE_MAX_PATHS}"))).is_none());
    }

    #[test]
    fn empty_span_blank_line_edit_rejects_unread_line_without_underflow_and_accepts_read_line() {
        use sha2::{Digest, Sha256};

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("blank.txt");
        let text = "\n\nalpha\n";
        std::fs::write(&file, text).unwrap();
        let hash: [u8; 32] = Sha256::digest(text.as_bytes()).into();
        let key = key_for(&file);

        // Only line 3 (`alpha\n`) was read: editing unread blank line 1 `(0, 0)` must fail cleanly.
        let session_a = "read-state-blank-a";
        forget_session(session_a);
        let mut rec_a = record_with(&[(2, 3)]);
        rec_a.hash = hash;
        rec_a.edit_evidence = true;
        record(session_a, key.clone(), rec_a);
        let mut ev_a = EditEvidence::for_text(session_a, &file, text);
        let err_a = ev_a
            .replace(text, "blank.txt", 0, 0, "beta\n")
            .expect_err("unread line 1 must be refused");
        assert!(err_a.to_string().contains("lines 1-1: unread lines"), "{err_a}");
        forget_session(session_a);

        // Only line 1 `(0, 1)` was read: editing unread blank line 2 `(1, 1)` must also fail cleanly.
        let session_b = "read-state-blank-b";
        forget_session(session_b);
        let mut rec_b = record_with(&[(0, 1)]);
        rec_b.hash = hash;
        rec_b.edit_evidence = true;
        record(session_b, key, rec_b);
        let mut ev_b = EditEvidence::for_text(session_b, &file, text);
        let err_b = ev_b
            .replace(text, "blank.txt", 1, 1, "beta\n")
            .expect_err("unread line 2 must not satisfy via line 1 end");
        assert!(err_b.to_string().contains("lines 2-2: unread lines"), "{err_b}");
        // Line 1 itself `(0, 0)` was read and succeeds.
        ev_b.replace(text, "blank.txt", 0, 0, "beta\n")
            .expect("read line 1 accepts empty-span fuzzy edit");
        forget_session(session_b);
    }
}
