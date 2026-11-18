//! A live takeover saves the conversation before asking Claude to exit. Hold
//! that writer and a fingerprint of the complete source records across the stop.

use super::{
    ClaudeCodeEntry, Session, claude_message, import_claude_entries, normalize_imported_messages,
    ordered_claude_code_message_entries, read_claude_entries,
};
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, path::Path};

struct SourcePrefix {
    rows: usize,
    digest: [u8; 32],
}

pub(super) struct PreparedTakeover {
    pub(super) session: Session,
    // Cleared once the verified tail is in memory. A failed save must retry
    // the same messages and the store's reserved event IDs.
    source: Option<SourcePrefix>,
}

impl PreparedTakeover {
    pub(super) fn prepare(path: &Path, source_id: &str, target_id: String) -> Result<Self> {
        let (entries, digest) = read_source(path, source_id, usize::MAX)?;
        let ordered = ordered_claude_code_message_entries(&entries);
        let session = import_claude_entries(&ordered, source_id, target_id)?;
        Ok(Self {
            session,
            source: Some(SourcePrefix {
                rows: entries.len(),
                digest,
            }),
        })
    }

    pub(super) fn refresh(&mut self, path: &Path, source_id: &str) -> Result<()> {
        if let Some(prefix) = &self.source {
            let (entries, digest) = read_source(path, source_id, prefix.rows)?;
            ensure!(
                entries.len() >= prefix.rows && digest == prefix.digest,
                "the staged Claude transcript changed before its final messages could be imported"
            );
            if entries.len() == prefix.rows {
                return Ok(());
            }
            reject_repeated_tail_ids(&entries, prefix.rows)?;
            // Parent links and root timestamps determine conversation order.
            // Even an append to the file can insert a row before a staged reply.
            let staged = ordered_claude_code_message_entries(&entries[..prefix.rows]);
            let ordered = ordered_claude_code_message_entries(&entries);
            ensure!(
                ordered.len() >= staged.len() && staged.iter().zip(&ordered).all(|(a, b)| std::ptr::eq(*a, *b)),
                "the final Claude messages changed the order of the staged conversation"
            );
            let tail = ordered[staged.len()..]
                .iter()
                .filter_map(|entry| claude_message(entry))
                .collect();
            let (tail, _) = normalize_imported_messages(tail, true);
            if tail.is_empty() {
                return Ok(());
            }
            for message in tail {
                self.session.append_stored_message(message);
            }
            self.source = None;
        }
        self.session.save()
    }
}

fn read_source(path: &Path, source_id: &str, prefix_rows: usize) -> Result<(Vec<ClaudeCodeEntry>, [u8; 32])> {
    let mut digest = Sha256::new();
    let mut remaining = prefix_rows;
    let entries = read_claude_entries(path, |line| {
        if remaining != 0 {
            digest.update(line.as_bytes());
            digest.update(b"\n");
            remaining -= 1;
        }
    })?;
    let mut identified = false;
    for id in entries.iter().filter_map(|entry| entry.session_id.as_deref()) {
        ensure!(
            id == source_id,
            "Claude transcript {} belongs to session {id}, not live session {source_id}",
            path.display()
        );
        identified = true;
    }
    ensure!(
        identified,
        "Claude transcript {} does not identify live session {source_id}",
        path.display()
    );
    Ok((entries, digest.finalize().into()))
}

fn reject_repeated_tail_ids(entries: &[ClaudeCodeEntry], prefix_rows: usize) -> Result<()> {
    let mut seen = HashSet::new();
    for (index, entry) in entries.iter().enumerate() {
        if !entry.is_sidechain
            && matches!(entry.entry_type.as_str(), "user" | "assistant")
            && entry.message.is_some()
            && let Some(id) = entry.uuid.as_deref()
        {
            ensure!(
                seen.insert(id) || index < prefix_rows,
                "Claude transcript repeats message id {id}"
            );
        }
    }
    Ok(())
}
