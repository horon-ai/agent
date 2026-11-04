//! Read-only full validation and exclusive tail recovery.
use crate::blobs::{Disk, checked_file, io_error, new_file, read_file};
use crate::*;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};

/// A damaged log retains its validated prefix for diagnosis, never for silent execution.
#[derive(Debug)]
pub struct ScanReport {
    pub events: Vec<EventEnvelope>,
    /// Byte length of each complete record line, in the same order as `events`.
    pub lengths: Vec<u64>,
    pub complete_offset: u64,
    pub torn_tail: Vec<u8>,
    pub damage: Option<StoreError>,
}

/// Blocking doctor surface. Only newline-complete records are applied.
pub fn full_scan(paths: &SessionPaths, id: &SessionId) -> Result<ScanReport, StoreError> {
    scan_observed(paths, id, &crate::ReadProbe::default())
}

pub(crate) fn scan_observed(
    paths: &SessionPaths,
    id: &SessionId,
    probe: &crate::ReadProbe,
) -> Result<ScanReport, StoreError> {
    let mut report = ScanReport {
        events: Vec::new(),
        lengths: Vec::new(),
        complete_offset: 0,
        torn_tail: Vec::new(),
        damage: None,
    };
    let log = paths.events_log(id)?;
    match checked_file(&log, false) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(error) => return Err(error.into()),
    }
    let file = probe.file(&log)?;
    let mut reader = BufReader::new(file);
    let mut known = BTreeMap::new();
    loop {
        let mut line = Vec::new();
        let bound = u64::try_from(MAX_RECORD_BYTES).map_err(|_| corrupt("record bound overflow"))?;
        if (&mut reader).take(bound + 1).read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if line.len() > MAX_RECORD_BYTES {
            report.damage = Some(corrupt("oversized record"));
            break;
        }
        if line.last() != Some(&b'\n') {
            report.torn_tail = line;
            break;
        }
        let parsed = validate_line(&line, id, &known).and_then(|event| {
            if let Some(reference) = &event.payload_ref {
                event.hydrate_from_blob(&probe.read(&paths.blob_file(id, &reference.sha256)?)?)?;
            }
            Ok(event)
        });
        match parsed {
            Ok(event) => {
                known.insert(event.event_id.clone(), event.content_hash.clone());
                report.complete_offset = report
                    .complete_offset
                    .checked_add(u64::try_from(line.len()).map_err(|_| corrupt("record length overflow"))?)
                    .ok_or_else(|| corrupt("log offset overflow"))?;
                report
                    .lengths
                    .push(u64::try_from(line.len()).map_err(|_| corrupt("record length overflow"))?);
                report.events.push(event);
            }
            Err(error) => {
                report.damage = Some(error);
                break;
            }
        }
    }
    Ok(report)
}

fn validate_line(line: &[u8], id: &SessionId, known: &BTreeMap<EventId, String>) -> Result<EventEnvelope, StoreError> {
    if line.len() > MAX_RECORD_BYTES {
        return Err(corrupt("oversized record"));
    }
    let event = decode_record_line(line)?;
    if event.session_id != *id {
        return Err(corrupt("record belongs to another session"));
    }
    if encode_record_line(&event)? != line {
        return Err(corrupt("record is not canonical"));
    }
    if known.contains_key(&event.event_id) {
        return Err(corrupt("duplicate event identity in original log"));
    }
    if let Some(parent) = &event.parent_event_id
        && !known.contains_key(parent)
    {
        return Err(StoreError::invalid_request(
            StoreErrorCode::MissingDependency,
            "parent is absent",
        ));
    }
    Ok(event)
}

pub(crate) fn corrupt(message: &str) -> StoreError {
    StoreError::internal(StoreErrorCode::CorruptLog, message)
}

/// The caller must hold the stable session writer lock.
pub(crate) fn quarantine(disk: &mut Disk, log: &std::path::Path, report: &ScanReport) -> Result<(), StoreError> {
    if report.torn_tail.is_empty() {
        return Ok(());
    }
    let parent = log.parent().ok_or_else(|| corrupt("log has no parent"))?;
    let path = parent.join(format!(".quarantine-{}.raw", uuid::Uuid::new_v4()));
    let mut copy = new_file(&path)?;
    copy.write_all(&report.torn_tail)?;
    disk.hit(IoPoint::QuarantineFileSync)?;
    copy.sync_all().map_err(io_error)?;
    disk.sync_dir(parent, IoPoint::QuarantineDirectorySync)?;
    disk.hit(IoPoint::QuarantinePublished)?;
    let original = checked_file(log, true)?;
    original.set_len(report.complete_offset)?;
    disk.hit(IoPoint::TailTruncated)?;
    original.sync_all().map_err(io_error)?;
    disk.sync_dir(parent, IoPoint::DirectorySync)
}

/// Explicit salvage to a new store root. The source is locked but never altered.
/// Only its verified complete prefix is copied; damage is returned in the report.
/// A failed destination remains inspectable and is never treated as a completed recovery.
pub fn recover_to(source: &SessionPaths, id: &SessionId, destination: &SessionPaths) -> Result<ScanReport, StoreError> {
    let session = source.session_dir(id)?;
    let lock_path = session.join(".writer.lock");
    let lock = checked_file(&lock_path, true)?;
    crate::writer::lock_exclusive(&lock)?;
    let report = full_scan(source, id)?;
    match fs::symlink_metadata(destination.root()) {
        Ok(_) => {
            return Err(StoreError::invalid_request(
                StoreErrorCode::UnsafePath,
                "recovery requires a new destination root",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut writer = BlockingWriter::open(destination.clone(), id.clone())?;
    for event in &report.events {
        let sealed = match &event.payload_ref {
            Some(reference) => SealedEvent::Blob {
                envelope: event.clone(),
                blob: read_file(&source.blob_file(id, &reference.sha256)?)?,
                payload_ref: reference.clone(),
            },
            None => SealedEvent::Inline(event.clone()),
        };
        writer.append(&sealed)?;
    }
    Ok(report)
}
