//! Disposable event locations. No original payload is duplicated here.
use crate::cache_io::{ReadProbe, decode, encode};
use crate::*;
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Location {
    pub id: EventId,
    pub hash: String,
    pub offset: u64,
    pub length: u64,
    pub ordinal: usize,
    pub sparse: bool,
    pub message: bool,
    pub blob: Option<PayloadRef>,
    pub active: Option<ActiveMessage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RecordRef {
    pub id: EventId,
    pub hash: String,
    pub offset: u64,
    pub length: u64,
    pub blob: Option<PayloadRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ActiveMessage {
    pub order: usize,
    pub source: RecordRef,
    pub revised: bool,
}

impl Location {
    pub fn reference(&self) -> RecordRef {
        RecordRef {
            id: self.id.clone(),
            hash: self.hash.clone(),
            offset: self.offset,
            length: self.length,
            blob: self.blob.clone(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Index {
    pub rows: Vec<Location>,
}
impl Index {
    pub fn from_events(events: &[EventEnvelope]) -> Result<Self, StoreError> {
        let mut index = Self::default();
        for event in events {
            index.push(event)?;
        }
        Ok(index)
    }
    pub fn push(&mut self, event: &EventEnvelope) -> Result<(), StoreError> {
        let ordinal = self.rows.len();
        let offset = self.rows.last().map_or(0, |r| r.offset.saturating_add(r.length));
        let length = u64::try_from(encode_record_line(event)?.len())
            .map_err(|_| crate::recovery::corrupt("index length overflow"))?;
        self.rows.push(Location {
            id: event.event_id.clone(),
            hash: event.content_hash.clone(),
            offset,
            length,
            ordinal,
            sparse: ordinal.is_multiple_of(128),
            message: matches!(event.kind.known(), Some(KnownEventKind::MessageAppended)),
            blob: event.payload_ref.clone(),
            active: None,
        });
        Ok(())
    }
    pub fn bytes(&self) -> Result<Vec<u8>, StoreError> {
        let mut bytes = Vec::new();
        for row in &self.rows {
            bytes.extend(encode(row)?);
            bytes.push(b'\n');
        }
        Ok(bytes)
    }
    pub fn parse(bytes: &[u8]) -> Result<Self, StoreError> {
        if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
            return Err(crate::recovery::corrupt("truncated index"));
        }
        let mut index = Self::default();
        let mut offset = 0;
        for line in bytes.split_inclusive(|b| *b == b'\n') {
            let row: Location = decode(line)?;
            if row.ordinal != index.rows.len()
                || row.offset != offset
                || row.sparse != row.ordinal.is_multiple_of(128)
                || row.length == 0
            {
                return Err(crate::recovery::corrupt("invalid index location"));
            }
            offset = offset
                .checked_add(row.length)
                .ok_or_else(|| crate::recovery::corrupt("index offset overflow"))?;
            index.rows.push(row);
        }
        Ok(index)
    }

    pub fn get(&self, id: &EventId) -> Result<&Location, StoreError> {
        self.rows
            .iter()
            .find(|row| row.id == *id)
            .ok_or_else(|| crate::recovery::corrupt("indexed event absent"))
    }

    pub fn project(&mut self, graph: &EventGraph, projection: &Projection) -> Result<(), StoreError> {
        let mut revisions = std::collections::BTreeMap::new();
        for id in graph.ancestry(projection.head())? {
            if let crate::projection_payload::Payload::Revised(payload) = &graph.payloads[&id] {
                revisions.insert(payload.target_event_id.clone(), id);
            }
        }
        for row in &mut self.rows {
            row.active = None;
        }
        for (order, message) in projection.active_messages().iter().enumerate() {
            let effective = revisions.get(&message.event_id).unwrap_or(&message.event_id);
            let source = self.get(effective)?.reference();
            let row = self
                .rows
                .iter_mut()
                .find(|r| r.id == message.event_id)
                .ok_or_else(|| crate::recovery::corrupt("active append absent"))?;
            row.active = Some(ActiveMessage {
                order,
                source,
                revised: effective != &message.event_id,
            });
        }
        Ok(())
    }

    pub fn activate_append(&mut self, id: &EventId, order: usize) -> Result<(), StoreError> {
        let row = self
            .rows
            .iter_mut()
            .find(|r| r.id == *id)
            .ok_or_else(|| crate::recovery::corrupt("appended event absent"))?;
        row.active = Some(ActiveMessage {
            order,
            source: row.reference(),
            revised: false,
        });
        Ok(())
    }
}

pub(crate) fn at(
    source: (&SessionPaths, &SessionId),
    location: &Location,
    probe: &ReadProbe,
) -> Result<EventEnvelope, StoreError> {
    read_record(source, &location.reference(), probe)
}

pub(crate) fn read_record(
    source: (&SessionPaths, &SessionId),
    location: &RecordRef,
    probe: &ReadProbe,
) -> Result<EventEnvelope, StoreError> {
    let (paths, id) = source;
    let mut file = probe.file(&paths.events_log(id)?)?;
    file.seek(SeekFrom::Start(location.offset))?;
    let mut bytes = Vec::new();
    file.take(location.length).read_to_end(&mut bytes)?;
    let event = decode_record_line(&bytes)?;
    if event.session_id != *id
        || event.event_id != location.id
        || event.content_hash != location.hash
        || encode_record_line(&event)? != bytes
    {
        return Err(crate::recovery::corrupt("indexed original mismatch"));
    }
    if let Some(reference) = &event.payload_ref {
        event.hydrate_from_blob(&probe.read(&paths.blob_file(id, &reference.sha256)?)?)?;
    }
    Ok(event)
}
