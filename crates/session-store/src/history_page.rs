//! Head-bound seek index for full-history pages.
//!
//! A history page must cost the page, not the session. The projection stays the
//! authority for what a page contains, so this index records only *where* each
//! projected message lives: its event identity, the log byte range of the record
//! that carries its body (the append, or the revision that replaced it), and its
//! stored message id. No message body, metadata value or payload is copied here.
//!
//! The index is bound to the session identity, the exact head it was projected
//! for, and the original log's identity stamp (device, inode, length, mtime,
//! ctime). A different head - a branch selection, a rewind, a compaction, an
//! import, a reset - or any change to the log's bytes makes the header not match,
//! and the next request rebuilds from one full replay: exactly what every page
//! request used to cost. Selection files are re-read on every request, so a
//! selection change is never served from a stale projection.
//!
//! Reading a page touches the header block, one binary-search probe per cursor,
//! the page's own rows, and the page's own log records plus their blobs.

use crate::blobs::Disk;
use crate::cache_io::{ReadProbe, Stamp, decode, encode, publish};
use crate::projection_payload::Payload;
use crate::*;
use horon_agent_session_types::StoredMessage;
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

const INDEX_FILE: &str = "history-page-index.bin";
const HEADER_BYTES: usize = 4096;
const ROW_BYTES: usize = 40;
const BY_ID_BYTES: usize = 24;
const BY_MESSAGE_BYTES: usize = 12;
const VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Header {
    version: u32,
    session_id: SessionId,
    head: EventId,
    heads: Vec<EventId>,
    log: Stamp,
    count: u64,
    active_start: u64,
    strings_len: u64,
    rows_at: u64,
    strings_at: u64,
    by_id_at: u64,
    by_message_at: u64,
}

#[derive(Clone, Debug)]
struct Row {
    offset: u64,
    length: u64,
    event: [u8; 16],
    message_offset: u32,
    message_length: u32,
}

impl Row {
    fn encode(&self) -> [u8; ROW_BYTES] {
        let mut bytes = [0u8; ROW_BYTES];
        bytes[0..8].copy_from_slice(&self.offset.to_be_bytes());
        bytes[8..16].copy_from_slice(&self.length.to_be_bytes());
        bytes[16..32].copy_from_slice(&self.event);
        bytes[32..36].copy_from_slice(&self.message_offset.to_be_bytes());
        bytes[36..40].copy_from_slice(&self.message_length.to_be_bytes());
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<Self, StoreError> {
        let mut event = [0u8; 16];
        event.copy_from_slice(&bytes[16..32]);
        Ok(Self {
            offset: u64::from_be_bytes(fixed(bytes, 0)?),
            length: u64::from_be_bytes(fixed(bytes, 8)?),
            event,
            message_offset: u32::from_be_bytes(fixed(bytes, 32)?),
            message_length: u32::from_be_bytes(fixed(bytes, 36)?),
        })
    }

    fn event_string(&self) -> String {
        uuid::Uuid::from_bytes(self.event).to_string()
    }
}

fn fixed<const N: usize>(bytes: &[u8], at: usize) -> Result<[u8; N], StoreError> {
    let slice = bytes
        .get(at..at + N)
        .ok_or_else(|| crate::recovery::corrupt("truncated history index record"))?;
    let mut out = [0u8; N];
    out.copy_from_slice(slice);
    Ok(out)
}

fn be32(bytes: &[u8], at: usize) -> Result<u32, StoreError> {
    Ok(u32::from_be_bytes(fixed(bytes, at)?))
}

/// A loaded page index. Every accessor reads from the published file, so a page
/// pays only for the rows it returns.
pub struct HistoryPages {
    paths: SessionPaths,
    id: SessionId,
    probe: ReadProbe,
    header: Header,
    path: PathBuf,
    built: bool,
}

impl HistoryPages {
    /// Loads the index for `head`, rebuilding it when the head, the selection or
    /// the original log no longer match.
    ///
    /// # Errors
    /// Propagates original damage, an unresolvable head and index I/O failures.
    pub fn open(
        paths: &SessionPaths,
        id: &SessionId,
        head: Option<&EventId>,
        probe: &ReadProbe,
    ) -> Result<Self, StoreError> {
        let log = paths.events_log(id)?;
        let Some(stamp) = Stamp::optional(&log)? else {
            return Err(StoreError::unknown_session("empty event graph"));
        };
        let path = paths.session_dir(id)?.join(INDEX_FILE);
        if let Some(header) = load_header(&path, id, probe)?
            && header.log == stamp
            && matches_head(&header, head, paths, id)?
        {
            return Ok(Self {
                paths: paths.clone(),
                id: id.clone(),
                probe: probe.clone(),
                header,
                path,
                built: false,
            });
        }
        Self::build(paths, id, head, probe, stamp)
    }

    /// True when this load had to replay the session to rebuild the index.
    pub fn built(&self) -> bool {
        self.built
    }

    /// The head this page index was projected for.
    pub fn head(&self) -> &EventId {
        &self.header.head
    }

    /// Projected message count for the head.
    pub fn len(&self) -> usize {
        usize::try_from(self.header.count).unwrap_or(usize::MAX)
    }

    /// True when the head projects no message at all.
    pub fn is_empty(&self) -> bool {
        self.header.count == 0
    }

    /// First projected position that is not covered by a compaction.
    pub fn active_start(&self) -> usize {
        usize::try_from(self.header.active_start).unwrap_or(usize::MAX)
    }

    /// Position of a projected message identity, if it is on this head.
    ///
    /// # Errors
    /// Propagates index read failures.
    pub fn position_of_event(&self, event: &EventId) -> Result<Option<usize>, StoreError> {
        self.position_of_event_str(event.as_str())
    }

    /// Position of a projected message identity given in its stored string form.
    ///
    /// # Errors
    /// Propagates index read failures.
    pub fn position_of_event_str(&self, event: &str) -> Result<Option<usize>, StoreError> {
        let mut low = 0usize;
        let mut high = self.len();
        while low < high {
            let middle = low + (high - low) / 2;
            let bytes = self.read_at(self.header.by_id_at, middle, BY_ID_BYTES)?;
            let mut identity = [0u8; 16];
            identity.copy_from_slice(&bytes[0..16]);
            let found = uuid::Uuid::from_bytes(identity).to_string();
            match found.as_str().cmp(event) {
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => {
                    return Ok(Some(u32::from_be_bytes(fixed(&bytes, 16)?) as usize));
                }
            }
        }
        Ok(None)
    }

    /// Position of a projected stored-message identity, if it is on this head.
    ///
    /// # Errors
    /// Propagates index read failures.
    pub fn position_of_message(&self, message_id: &str) -> Result<Option<usize>, StoreError> {
        let mut low = 0usize;
        let mut high = self.len();
        while low < high {
            let middle = low + (high - low) / 2;
            let bytes = self.read_at(self.header.by_message_at, middle, BY_MESSAGE_BYTES)?;
            let found = self.message_at(&bytes)?;
            match found.as_bytes().cmp(message_id.as_bytes()) {
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => {
                    return Ok(Some(u32::from_be_bytes(fixed(&bytes, 8)?) as usize));
                }
            }
        }
        Ok(None)
    }

    /// Materializes only the projected messages in `[start, end)`, in order.
    ///
    /// # Errors
    /// Refuses out-of-range windows, a stale row (record identity or canonical
    /// bytes no longer match) and damaged originals or blobs.
    pub fn page(&self, start: usize, end: usize) -> Result<Vec<ProjectedMessage>, StoreError> {
        if start > end || end > self.len() {
            return Err(StoreError::invalid_request(
                StoreErrorCode::InvalidRecord,
                "history page window is outside the projection",
            ));
        }
        let mut messages = Vec::with_capacity(end - start);
        for position in start..end {
            let bytes = self.read_at(self.header.rows_at, position, ROW_BYTES)?;
            let row = Row::decode(&bytes)?;
            let event = self.read_record(&row)?;
            let message = self.message_of(&event, &row)?;
            messages.push(ProjectedMessage {
                event_id: EventId::parse(row.event_string())?,
                message,
            });
        }
        Ok(messages)
    }

    /// Event identity of the projected message at `position`.
    ///
    /// # Errors
    /// Refuses a position outside the projection and index read failures.
    pub fn event_id_at(&self, position: usize) -> Result<EventId, StoreError> {
        if position >= self.len() {
            return Err(StoreError::invalid_request(
                StoreErrorCode::InvalidRecord,
                "history position is outside the projection",
            ));
        }
        let bytes = self.read_at(self.header.rows_at, position, ROW_BYTES)?;
        EventId::parse(Row::decode(&bytes)?.event_string())
    }

    /// Resolve a row's span in the string table, refusing anything the header does not
    /// cover.
    ///
    /// The header proves the region layout, not the contents of an individual row: a
    /// corrupt row can name a length up to 4 GiB, and `read_bytes` allocates that before
    /// it reads. Bounding the span here makes a damaged index a named error instead of an
    /// unbounded allocation, and covers both readers (`message_at` and `message_bytes`).
    fn string_span(&self, offset: u64, length: usize) -> Result<u64, StoreError> {
        let end = offset
            .checked_add(length as u64)
            .ok_or_else(|| crate::recovery::corrupt("history index row offset overflow"))?;
        if end > self.header.strings_len {
            return Err(crate::recovery::corrupt(
                "history index row points outside the string table",
            ));
        }
        self.header
            .strings_at
            .checked_add(offset)
            .ok_or_else(|| crate::recovery::corrupt("history index row offset overflow"))
    }

    fn message_at(&self, record: &[u8]) -> Result<String, StoreError> {
        let offset = u64::from(be32(record, 0)?);
        let length = be32(record, 4)? as usize;
        let at = self.string_span(offset, length)?;
        let bytes = self.read_bytes(at, length)?;
        String::from_utf8(bytes).map_err(|_| crate::recovery::corrupt("history index holds a non-UTF-8 message id"))
    }
    fn read_at(&self, section: u64, index: usize, width: usize) -> Result<Vec<u8>, StoreError> {
        let offset = u64::try_from(index)
            .ok()
            .and_then(|index| u64::try_from(width).ok().and_then(|width| index.checked_mul(width)))
            .and_then(|offset| section.checked_add(offset))
            .ok_or_else(|| crate::recovery::corrupt("history index offset overflow"))?;
        self.read_bytes(offset, width)
    }

    fn read_bytes(&self, offset: u64, length: usize) -> Result<Vec<u8>, StoreError> {
        let mut file = self.probe.file(&self.path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0u8; length];
        file.read_exact(&mut bytes)
            .map_err(|_| crate::recovery::corrupt("history index is truncated"))?;
        Ok(bytes)
    }

    fn read_record(&self, row: &Row) -> Result<EventEnvelope, StoreError> {
        let log = self.paths.events_log(&self.id)?;
        let mut file = self.probe.file(&log)?;
        file.seek(SeekFrom::Start(row.offset))?;
        let mut bytes = Vec::new();
        let bound =
            u64::try_from(crate::MAX_RECORD_BYTES).map_err(|_| crate::recovery::corrupt("record bound overflow"))?;
        if row.length > bound {
            return Err(crate::recovery::corrupt("history index row exceeds the record bound"));
        }
        (&mut file).take(row.length).read_to_end(&mut bytes)?;
        let event = decode_record_line(&bytes)?;
        if event.session_id != self.id
            || event.event_id.as_str() != row.event_string()
            || encode_record_line(&event)? != bytes
        {
            return Err(crate::recovery::corrupt("indexed original mismatch"));
        }
        Ok(event)
    }

    fn message_of(&self, event: &EventEnvelope, row: &Row) -> Result<StoredMessage, StoreError> {
        let value = match event.body()? {
            EventBody::Inline => event.payload_value()?.clone(),
            EventBody::Blob => {
                let reference = event
                    .payload_ref
                    .as_ref()
                    .ok_or_else(|| crate::recovery::corrupt("blob reference absent"))?;
                let bytes = self.probe.read(&self.paths.blob_file(&self.id, &reference.sha256)?)?;
                horon_agent_perf_counters::record_blob_bytes_read(bytes.len() as u64);
                event.hydrate_from_blob(&bytes)?.payload
            }
        };
        let message = match Payload::parse(event, &value)? {
            Payload::Appended(payload) => payload.message,
            Payload::Revised(payload) => payload.replacement,
            _ => return Err(crate::recovery::corrupt("indexed row is not a message record")),
        };
        if message.id.as_bytes() != self.message_bytes(row)? {
            return Err(crate::recovery::corrupt("indexed message identity changed"));
        }
        Ok(message)
    }

    fn message_bytes(&self, row: &Row) -> Result<Vec<u8>, StoreError> {
        let at = self.string_span(u64::from(row.message_offset), row.message_length as usize)?;
        self.read_bytes(at, row.message_length as usize)
    }

    fn build(
        paths: &SessionPaths,
        id: &SessionId,
        requested: Option<&EventId>,
        probe: &ReadProbe,
        stamp: Stamp,
    ) -> Result<Self, StoreError> {
        let report = crate::recovery::scan_observed(paths, id, probe)?;
        if let Some(error) = report.damage {
            return Err(error);
        }
        if !report.torn_tail.is_empty() {
            return Err(crate::recovery::corrupt("torn original"));
        }
        let graph = EventGraph::with_blobs(id, &report.events, |event| {
            let reference = event
                .payload_ref
                .as_ref()
                .ok_or_else(|| crate::recovery::corrupt("blob reference absent"))?;
            probe.read(&paths.blob_file(id, &reference.sha256)?)
        })?;
        let head = match requested {
            Some(head) => head.clone(),
            None => match selected_head(graph.heads(), paths, id)? {
                Some(head) if graph.event(&head).is_some() => head,
                _ => match graph.heads() {
                    [only] => only.clone(),
                    _ => return Err(conflict_heads(&graph)),
                },
            },
        };
        let projection = graph.replay(Some(&head))?;
        let mut offsets = Vec::with_capacity(report.events.len());
        let mut offset = 0u64;
        for length in &report.lengths {
            offsets.push(offset);
            offset = offset
                .checked_add(*length)
                .ok_or_else(|| crate::recovery::corrupt("log offset overflow"))?;
        }
        let mut located = std::collections::BTreeMap::new();
        for (position, event) in report.events.iter().enumerate() {
            located.insert(event.event_id.clone(), position);
        }
        let mut revisions = std::collections::BTreeMap::new();
        for id in graph.ancestry(projection.head())? {
            if let Payload::Revised(payload) = &graph.payloads[&id] {
                revisions.insert(payload.target_event_id.clone(), id);
            }
        }
        let mut rows = Vec::with_capacity(projection.messages().len());
        let mut strings = Vec::new();
        let mut messages: Vec<(Vec<u8>, usize)> = Vec::with_capacity(projection.messages().len());
        for (position, message) in projection.messages().iter().enumerate() {
            let source = revisions.get(&message.event_id).unwrap_or(&message.event_id);
            let located = located
                .get(source)
                .ok_or_else(|| crate::recovery::corrupt("projected message has no original record"))?;
            let message_offset =
                u32::try_from(strings.len()).map_err(|_| oversized("history index string table exceeds 4 GiB"))?;
            let bytes = message.message.id.as_bytes();
            let message_length =
                u32::try_from(bytes.len()).map_err(|_| oversized("history index message id is too long"))?;
            strings.extend_from_slice(bytes);
            messages.push((bytes.to_vec(), position));
            rows.push(Row {
                offset: offsets[*located],
                length: report.lengths[*located],
                event: *message.event_id.as_uuid().as_bytes(),
                message_offset,
                message_length,
            });
        }
        if strings.len() > u32::MAX as usize {
            return Err(oversized("history index string table exceeds 4 GiB"));
        }
        let mut by_id: Vec<(String, u32)> = rows
            .iter()
            .enumerate()
            .map(|(position, row)| (row.event_string(), position as u32))
            .collect();
        by_id.sort_by(|left, right| left.0.cmp(&right.0));
        messages.sort_by(|left, right| left.0.cmp(&right.0));
        let header = Header {
            version: VERSION,
            session_id: id.clone(),
            head: projection.head().clone(),
            heads: graph.heads().to_vec(),
            log: stamp,
            count: rows.len() as u64,
            active_start: (projection.messages().len() - projection.active_messages().len()) as u64,
            strings_len: strings.len() as u64,
            rows_at: HEADER_BYTES as u64,
            strings_at: HEADER_BYTES as u64 + (rows.len() * ROW_BYTES) as u64,
            by_id_at: HEADER_BYTES as u64 + (rows.len() * ROW_BYTES) as u64 + strings.len() as u64,
            by_message_at: HEADER_BYTES as u64
                + (rows.len() * ROW_BYTES) as u64
                + strings.len() as u64
                + (by_id.len() * BY_ID_BYTES) as u64,
        };
        let path = paths.session_dir(id)?.join(INDEX_FILE);
        let bytes = encode_index(&header, &rows, &strings, &by_id, &messages)?;
        // An append adds events *after* the head this projection was built for; it does not change
        // what that head projects to, so these rows still describe it exactly. Publishing them
        // under the stamp the replay started with therefore serves the page the caller asked for,
        // and the next request compares that stamp against the log, sees the append and rebuilds.
        // Reuse is guarded by that comparison, never by this function, so no stale page is served.
        publish(&mut Disk::new(Box::new(|_| Ok(()))), &path, &bytes, probe)?;
        Ok(Self {
            paths: paths.clone(),
            id: id.clone(),
            probe: probe.clone(),
            header,
            path,
            built: true,
        })
    }
}

fn encode_index(
    header: &Header,
    rows: &[Row],
    strings: &[u8],
    by_id: &[(String, u32)],
    by_message: &[(Vec<u8>, usize)],
) -> Result<Vec<u8>, StoreError> {
    let header_bytes = encode(header)?;
    if header_bytes.len() > HEADER_BYTES {
        return Err(oversized("history index header exceeds its fixed block"));
    }
    let mut bytes = vec![b' '; HEADER_BYTES];
    bytes[..header_bytes.len()].copy_from_slice(&header_bytes);
    for row in rows {
        bytes.extend_from_slice(&row.encode());
    }
    bytes.extend_from_slice(strings);
    for (event, position) in by_id {
        let parsed = EventId::parse(event)?;
        bytes.extend_from_slice(parsed.as_uuid().as_bytes());
        bytes.extend_from_slice(&position.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 4]);
    }
    for (_message, position) in by_message {
        let row = &rows[*position];
        bytes.extend_from_slice(&row.message_offset.to_be_bytes());
        bytes.extend_from_slice(&row.message_length.to_be_bytes());
        bytes.extend_from_slice(
            &u32::try_from(*position)
                .map_err(|_| oversized("history position overflow"))?
                .to_be_bytes(),
        );
    }
    Ok(bytes)
}

fn oversized(message: &str) -> StoreError {
    StoreError::invalid_request(StoreErrorCode::RecordTooLarge, message)
}

fn conflict_heads(graph: &EventGraph) -> StoreError {
    StoreError::invalid_request(StoreErrorCode::ConflictHeads, "select a head before paging history").with_details(
        StoreErrorDetails {
            conflict_heads: graph.heads().iter().map(|head| head.as_str().to_owned()).collect(),
            ..StoreErrorDetails::default()
        },
    )
}

/// The local selection, mirroring the replication surface: an explicit selection
/// wins, otherwise a single retained head is the head.
fn selected_head(heads: &[EventId], paths: &SessionPaths, id: &SessionId) -> Result<Option<EventId>, StoreError> {
    let directory = paths.session_dir(id)?;
    let explicit = match std::fs::read(directory.join("local-head-explicit.json")) {
        Ok(bytes) => Some(serde_json::from_slice::<bool>(&bytes).map_err(crate::cache_io::json_error)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if explicit == Some(true)
        && let Ok(bytes) = std::fs::read(directory.join("local-head.json"))
        && let Ok(head) = serde_json::from_slice::<EventId>(&bytes)
    {
        return Ok(Some(head));
    }
    match heads {
        [only] => Ok(Some(only.clone())),
        _ => Ok(None),
    }
}

fn matches_head(
    header: &Header,
    requested: Option<&EventId>,
    paths: &SessionPaths,
    id: &SessionId,
) -> Result<bool, StoreError> {
    match requested {
        Some(head) => Ok(&header.head == head),
        None => Ok(selected_head(&header.heads, paths, id)?.as_ref() == Some(&header.head)),
    }
}

fn load_header(path: &std::path::Path, id: &SessionId, probe: &ReadProbe) -> Result<Option<Header>, StoreError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let mut file = probe.file(path)?;
    let mut bytes = vec![b' '; HEADER_BYTES];
    if file.read_exact(&mut bytes).is_err() {
        return Ok(None);
    }
    let Ok(header) = decode::<Header>(&bytes) else {
        return Ok(None);
    };
    if header.version != VERSION || header.session_id != *id {
        return Ok(None);
    }
    let expected = header.by_message_at + header.count * BY_MESSAGE_BYTES as u64;
    if header.strings_at + header.strings_len > header.by_id_at
        || header.rows_at + header.count * ROW_BYTES as u64 != header.strings_at
    {
        return Ok(None);
    }
    let actual = std::fs::metadata(path).map(|metadata| metadata.len()).unwrap_or(0);
    if actual != expected {
        return Ok(None);
    }
    Ok(Some(header))
}
