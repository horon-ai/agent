use super::{Session, event_store};
use anyhow::{Context, Result};
use horon_agent_session_store as store;
use std::io::Read;

impl Session {
    pub(super) fn flush_events(&mut self, paths: &store::SessionPaths, id: &store::SessionId) -> Result<()> {
        let writer = self
            .event_state
            .writer
            .as_ref()
            .context("session writer missing")?
            .clone();
        let events = self.event_state.pending.clone();
        let selected = events
            .last()
            .context("empty pending commit")?
            .envelope()
            .event_id
            .clone();
        let commit_paths = paths.clone();
        let commit_id = id.clone();
        let expected_prefix = self.event_state.source_hash.clone();
        let prefix_len = self.event_state.pending_base_len;
        let explicit_head = self.event_state.explicit_head;
        // Consumed before any I/O so a failed commit cannot leave the flag
        // armed for a later, unrelated commit: the next one rebuilds normally.
        let skip_cache = std::mem::take(&mut self.event_state.skip_cache_once) && self.event_state.defer_final_cache;
        let external_source = self.event_state.external_source.clone();
        let runtime = super::event_wire::LocalRuntime::capture(self);
        writer.call(move |writer| {
            writer.batched(|writer| -> Result<()> {
                if let Some(source) = &external_source {
                    event_store::validate_external(source)?;
                }
                let intended = intended_bytes(&events)?;
                // Validation streams the log through one fixed buffer instead of
                // reading it whole: the same bytes are hashed and compared, but the
                // peak temporary allocation no longer follows the log size.
                let mut opened = match std::fs::File::open(commit_paths.events_log(&commit_id)?) {
                    Ok(log) => Some(log),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => return Err(error.into()),
                };
                let log: &mut dyn Read = match opened.as_mut() {
                    Some(file) => file,
                    None => &mut std::io::empty(),
                };
                validate_commit_prefix(log, prefix_len, expected_prefix.as_deref())?;
                compare_reserved_tail(log, &intended)?;
                for event in events {
                    writer.append(&event)?;
                }
                // One flush confirms every original this commit wrote, and it runs
                // before any head or binding below may name one of them: a batched
                // append defers its flush, so this is the commit's one flush.
                writer.flush_pending_log()?;
                if let Some(source) = external_source {
                    event_store::publish_local_durable(
                        &commit_paths.session_dir(&commit_id)?.join("local-explicit-source.json"),
                        &source,
                        commit_paths.sync_mode(),
                    )?;
                }
                // Selection and the runtime binding are local, durable, and never
                // exported as replica state. Their files are flushed individually
                // and the session directory once, after both.
                let runtime_changed = runtime.publish(&commit_paths, &commit_id)?;
                let selected_changed = event_store::select(&commit_paths, &commit_id, &selected, explicit_head)?;
                if runtime_changed || selected_changed {
                    event_store::sync_local_state(&commit_paths, &commit_id)?;
                }
                // One derived rebuild for the whole commit: every original above is
                // durable and the head is already selected, so the checkpoint and the
                // shared catalog are published once here instead of once per event.
                //
                // A one-shot run leaves the final commit's rebuild to the next
                // reader. Every reader validates the cached stamp against this log,
                // so the untouched cache is refused and rebuilt there; the write
                // this saves is the one nobody reads before the process exits.
                let cached_head = explicit_head.then_some(selected.clone());
                if !skip_cache {
                    if let Err(error) = writer.rebuild_cache(cached_head.as_ref()) {
                        crate::logging::warn(&format!(
                            "Session originals committed; derived cache needs repair: {error}"
                        ));
                    } else if let Some(warning) = writer.cache_warning() {
                        crate::logging::warn(&format!("Session originals committed; {warning}"));
                    }
                }
                Ok(())
            })
        })?;
        self.event_state.head = self.event_state.pending.last().map(|e| e.envelope().event_id.clone());
        self.event_state.source_hash = event_store::source_hash(paths, id)?;
        self.event_state.pending.clear();
        Ok(())
    }
}

/// The bytes this commit would append, in one buffer of their own.
///
/// The size follows the pending events, never the log, so the validation's peak
/// stays independent of how large the durable log already is.
pub(super) fn intended_bytes(events: &[store::SealedEvent]) -> Result<Vec<u8>> {
    let mut intended = Vec::new();
    for event in events {
        intended.extend(store::encode_record_line(event.envelope())?);
    }
    Ok(intended)
}

/// Hash the captured prefix of the durable log this commit is about to extend,
/// through one fixed buffer instead of a whole-file read.
///
/// Every prefix byte is still hashed and the digest is still compared with the
/// captured one, so the decision is identical; only the peak temporary
/// allocation changed, to one [`store::HASH_STREAM_BUFFER`] (64 KiB).
pub(super) fn validate_commit_prefix(
    log: &mut dyn Read,
    prefix_len: usize,
    expected_prefix: Option<&str>,
) -> Result<()> {
    let prefix_len = u64::try_from(prefix_len)?;
    let mut prefix_reader = (&mut *log).take(prefix_len);
    let prefix = store::sha256_hex_stream(&mut prefix_reader)?;
    // A log shorter than the captured prefix is truncation, and it is reported
    // before any digest comparison can call the same bytes a change.
    anyhow::ensure!(
        prefix.bytes == prefix_len,
        "SourceConflict: original prefix was truncated"
    );
    if let Some(expected) = expected_prefix {
        anyhow::ensure!(prefix.hex == expected, "SourceConflict: original prefix was changed");
    }
    Ok(())
}

/// Stream-compare the bytes after the captured prefix against the bytes this
/// commit would write.
///
/// The durable tail must be a prefix of `intended`: an exact copy, which the
/// retry adopts, or a partial write this commit completes. A tail that runs past
/// `intended`, or differs inside it, is a source conflict and the log is left
/// alone. The comparison buffer is one [`store::HASH_STREAM_BUFFER`], and it is
/// taken after the prefix hash released its own, so the two never coexist.
pub(super) fn compare_reserved_tail(log: &mut dyn Read, intended: &[u8]) -> Result<()> {
    let mut buffer = vec![0u8; store::HASH_STREAM_BUFFER];
    let mut offset = 0usize;
    loop {
        let read = match log.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        anyhow::ensure!(
            intended.get(offset..offset + read) == Some(&buffer[..read]),
            "SourceConflict: non-reserved bytes followed the captured prefix"
        );
        offset += read;
    }
}
