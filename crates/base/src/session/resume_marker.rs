//! The resume marker: "a resume turn was created for this request".
//!
//! The marker used to be a journal entry. `origin/dev` made the event log the
//! only durable original and turned the journal writer into a test fixture, so
//! the marker is now a `MetadataChanged` event that carries the session's own
//! metadata unchanged plus one optional `resume_key`. Reading it back is a scan
//! of the same log the session itself loads from: there is no second file a
//! checkpoint can replace, and no state that can drift from the transcript.
//!
//! The carrier is `MetadataChanged`, not `MessageAppended`, because the marker
//! is not a transcript entry. A `MessageAppended` marker would have to invent a
//! message, and every reader that replays the log — the provider transcript
//! included — would then see it. `MetadataChanged` replays to the metadata it
//! copies, so a marker is inert for every existing reader and only the scan
//! below reads the field.

use super::{Session, event_actor::Writer, event_origin, event_store};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use horon_agent_session_store as store;
use serde_json::json;
use std::time::{Duration, Instant};

/// How long an append waits for another process's writer to finish.
///
/// The old journal lock waited out a checkpoint or a concurrent append instead
/// of failing the resume, and a resume that fails on a busy store would spend an
/// attempt for a condition that clears by itself.
const WRITER_WAIT: Duration = Duration::from_secs(30);
/// Poll interval while another process owns the writer.
const WRITER_RETRY: Duration = Duration::from_millis(25);

impl Session {
    /// Record that a resume turn for `resume_key` was created in this session,
    /// and return only after the event is on disk.
    ///
    /// The marker is the single durable answer to "was this request already
    /// resumed": an event carrying the key means the turn exists, and its
    /// absence means it does not. A second call with the same key appends
    /// nothing, which makes a retried or concurrent resume idempotent.
    ///
    /// The dedupe read and the append run inside one writer job, so they are one
    /// step against the same exclusive session lock every other append takes.
    /// Without that, two processes could both read "no marker" and both append.
    pub fn append_resume_key(session_id: &str, resume_key: &str) -> Result<()> {
        let paths = event_store::paths()?;
        let id = store::SessionId::parse(session_id)?;
        let deadline = Instant::now() + WRITER_WAIT;
        loop {
            let writer = match Writer::registered(&paths, &id)? {
                Some(writer) => writer,
                None => match Writer::open(paths.clone(), id.clone()) {
                    Ok(writer) => writer,
                    Err(error) if is_busy(&error) && Instant::now() < deadline => {
                        std::thread::sleep(WRITER_RETRY);
                        continue;
                    }
                    Err(error) => return Err(error),
                },
            };
            let job_paths = paths.clone();
            let job_id = id.clone();
            let key = resume_key.to_string();
            return writer.call(move |writer| append_marker(writer, &job_paths, &job_id, &key));
        }
    }

    /// When this session first recorded a resume turn for `resume_key`.
    ///
    /// `None` means no such turn was ever created. The timestamp is the moment of
    /// creation, so a caller can tell a turn that already ran (the session moved
    /// on afterwards) from one that was created and never executed.
    pub fn resume_key_recorded_at(session_id: &str, resume_key: &str) -> Result<Option<DateTime<Utc>>> {
        let paths = event_store::paths()?;
        let id = store::SessionId::parse(session_id)?;
        if !paths.events_log(&id)?.exists() {
            return Ok(None);
        }
        Ok(resume_markers(&paths, &id)?
            .into_iter()
            .filter(|(key, _)| key == resume_key)
            .map(|(_, recorded_at)| recorded_at)
            .min())
    }

    /// Whether this session's event log already records a resume turn for `resume_key`.
    pub fn journal_has_resume_key(session_id: &str, resume_key: &str) -> Result<bool> {
        Ok(Self::resume_key_recorded_at(session_id, resume_key)?.is_some())
    }
}

/// Whether an append error means another process owns the writer right now.
fn is_busy(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<store::StoreError>()
        .is_some_and(|error| error.code == store::StoreErrorCode::Busy)
}

/// Append the marker unless the key is already recorded, on the writer thread.
fn append_marker(
    writer: &mut store::BlockingWriter,
    paths: &store::SessionPaths,
    id: &store::SessionId,
    resume_key: &str,
) -> Result<()> {
    if resume_markers(paths, id)?.iter().any(|(key, _)| key == resume_key) {
        return Ok(());
    }
    let report = store::full_scan(paths, id)?;
    if let Some(error) = report.damage {
        return Err(error.into());
    }
    if !report.torn_tail.is_empty() {
        anyhow::bail!("incomplete original tail requires writer recovery");
    }
    let graph = store::EventGraph::with_blobs(id, &report.events, |event| {
        let Some(reference) = event.payload_ref.as_ref() else {
            return Err(store::StoreError::unknown_session("missing original blob reference"));
        };
        Ok(std::fs::read(paths.blob_file(id, &reference.sha256)?)?)
    })?;
    let selected = event_store::selected(paths, id)?;
    let head = graph.selected_head(selected.as_ref())?;
    // The marker copies the metadata the head already projects, so replaying it
    // changes nothing a reader would see. A marker must never move the session's
    // own `updated_at`: the resume decision compares that against the recorded
    // moment to tell a turn that ran from one that only exists.
    let metadata = graph.replay(Some(&head))?.metadata().clone();
    let event = store::EventDraft::new(
        id.clone(),
        store::EventId::new_v4(),
        Some(head),
        event_origin::origin(paths)?,
        Utc::now(),
        store::EventKind::new(store::KnownEventKind::MetadataChanged),
        json!({"metadata": metadata, "resume_key": resume_key}),
    )
    .seal()?;
    writer.append(&event)?;
    Ok(())
}

/// Every resume key recorded in the log, with the moment its event was written.
///
/// A key repeated by a later marker keeps its earliest moment, so a retried
/// append cannot make a created turn look newer than the run it started.
fn resume_markers(paths: &store::SessionPaths, id: &store::SessionId) -> Result<Vec<(String, DateTime<Utc>)>> {
    let report = store::full_scan(paths, id)?;
    if let Some(error) = report.damage {
        return Err(error.into());
    }
    let mut markers: Vec<(String, DateTime<Utc>)> = Vec::new();
    for event in &report.events {
        if event.kind.as_str() != store::KnownEventKind::MetadataChanged.as_str() {
            continue;
        }
        let payload = match event.body()? {
            store::EventBody::Inline => event.payload_value()?.clone(),
            store::EventBody::Blob => {
                let reference = event.payload_ref.as_ref().context("marker payload reference missing")?;
                serde_json::from_slice(&std::fs::read(paths.blob_file(id, &reference.sha256)?)?)?
            }
        };
        let Some(resume_key) = payload.get("resume_key").and_then(serde_json::Value::as_str) else {
            continue;
        };
        match markers.iter_mut().find(|(key, _)| key == resume_key) {
            Some((_, recorded_at)) if *recorded_at <= event.timestamp => {}
            Some((_, recorded_at)) => *recorded_at = event.timestamp,
            None => markers.push((resume_key.to_string(), event.timestamp)),
        }
    }
    Ok(markers)
}
