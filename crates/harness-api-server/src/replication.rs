//! Store-backed replication, head selection, and cursor history.
//!
//! Every request here is answered from the durable session store rather than
//! the daemon attachment, so a client can replicate and browse without
//! attaching. The store's exclusive writer guard is the cross-process
//! authority: an import or selection colliding with a live owner answers
//! `busy` instead of changing state behind a running turn.

use crate::translate::{Outbound, flatten_content};
use horon_agent_harness_api::{
    API_VERSION_MAJOR, ApiEvent, ApiRequest, CanonicalEventRecord, CanonicalRecordRef, ErrorCode, HistoryMessage,
    MAX_HISTORY_PAGE, MAX_REPLICATION_WIRE_BYTES, PayloadRef, ServerFrame, SessionBlobChunk, StoreErrorDetail,
};
use horon_agent_session_store as store;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub(crate) fn handle(request: &Value, envelope_bytes: Option<usize>) -> Vec<Outbound> {
    let api_id = request["id"].as_u64().unwrap_or(0);
    let req = request["req"].as_str().unwrap_or("");
    if matches!(req, "export_session_events" | "import_session_events") {
        // The bytes the client actually sent are the ones that occupied memory,
        // so they are what the envelope cap bounds. Only a caller without a
        // measured frame re-serializes the request to measure it.
        let measured = match envelope_bytes {
            Some(bytes) => Some(bytes),
            None => serde_json::to_vec(request).ok().map(|bytes| bytes.len()),
        };
        if measured.is_some_and(|bytes| bytes > MAX_REPLICATION_WIRE_BYTES) {
            return error_reply(
                api_id,
                store::StoreError::invalid_request(
                    store::StoreErrorCode::RecordTooLarge,
                    "replication request exceeds the 16 MiB wire envelope",
                ),
            );
        }
    }
    let parsed = match serde_json::from_value::<ApiRequest>(request.clone()) {
        Ok(parsed) => parsed,
        Err(error) => {
            return error_reply(
                api_id,
                store::StoreError::invalid_request(store::StoreErrorCode::InvalidRecord, error.to_string()),
            );
        }
    };
    let outcome = dispatch(parsed);
    match outcome {
        Ok(event) => match reply(api_id, event) {
            Ok(outbound) => vec![outbound],
            Err(error) => error_reply(api_id, error),
        },
        Err(error) => error_reply(api_id, error),
    }
}

/// One reply, serialized exactly once.
///
/// A replication page is the one reply large enough for its serialization to
/// matter, so its frame is serialized here, measured against the wire cap, and
/// handed to the writer as the bytes it sends. Every other reply stays a frame
/// the writer serializes on its way out.
fn reply(api_id: u64, event: ApiEvent) -> Result<Outbound, store::StoreError> {
    let frame = ServerFrame::reply(api_id, event);
    if !matches!(frame.event, ApiEvent::SessionEventPage { .. }) {
        return Ok(Outbound::Reply(frame));
    }
    let bytes = crate::writer::frame_bytes(&frame).map_err(|error| {
        store::StoreError::internal(
            store::StoreErrorCode::Internal,
            format!("serialize replication reply: {error}"),
        )
    })?;
    if page_event_bytes(&bytes, api_id) > MAX_REPLICATION_WIRE_BYTES {
        return Err(store::StoreError::invalid_request(
            store::StoreErrorCode::RecordTooLarge,
            "replication response exceeds the 16 MiB wire envelope",
        ));
    }
    Ok(Outbound::Serialized { frame, bytes })
}

/// The escaped length of the page event inside an already serialized frame.
///
/// The cap has always bounded the page event itself, not the envelope around
/// it, so the frame's prefix and closing brace are subtracted instead of
/// serializing the event a second time to measure it.
fn page_event_bytes(frame: &[u8], api_id: u64) -> usize {
    let envelope = format!("{{\"v\":{API_VERSION_MAJOR},\"reply_to\":{api_id},");
    frame.len().saturating_sub(1).saturating_sub(envelope.len() + 1)
}

fn dispatch(request: ApiRequest) -> Result<ApiEvent, store::StoreError> {
    let paths = home_paths()?;
    match request {
        ApiRequest::ListSessionHeads { session_id } => {
            let (paths, id) = existing_session(&paths, &session_id)?;
            list_heads(&paths, &id)
        }
        ApiRequest::SelectSessionHead {
            session_id,
            head_event_id,
        } => {
            let (paths, id) = existing_session(&paths, &session_id)?;
            select_head(&paths, &id, &head_event_id)
        }
        ApiRequest::GetHistoryPage {
            session_id,
            head_event_id,
            before_event_id,
            limit,
        } => {
            let (paths, id) = existing_session(&paths, &session_id)?;
            history_page(&paths, &id, head_event_id.as_deref(), before_event_id.as_deref(), limit)
        }
        ApiRequest::ExportSessionEvents {
            session_id,
            protocol_version,
            heads,
            known_ids,
            cursor,
        } => {
            let (paths, id) = existing_session(&paths, &session_id)?;
            export_events(
                &paths,
                store::replication::ExportRequest {
                    protocol_version,
                    session_id: id,
                    heads: parse_ids(&heads)?,
                    known_ids: parse_ids(&known_ids)?,
                    cursor,
                },
            )
        }
        ApiRequest::ImportSessionEvents {
            session_id,
            protocol_version,
            heads,
            events,
            next_cursor,
            blob_refs,
            record_refs,
            retry_cursor,
        } => {
            let (paths, id) = parse_session(&paths, &session_id)?;
            let page = store::replication::SessionEventPage {
                protocol_version,
                session_id: id,
                heads: parse_ids(&heads)?,
                events: events.into_iter().map(store_record).collect::<Result<_, _>>()?,
                next_cursor,
                blob_refs: blob_refs.into_iter().map(store_payload_ref).collect(),
                record_refs: record_refs
                    .into_iter()
                    .map(store_record_ref)
                    .collect::<Result<_, _>>()?,
            };
            import_events(&paths, &page, retry_cursor.as_deref())
        }
        ApiRequest::ExportSessionBlob {
            session_id,
            sha256,
            total_size,
            offset,
        } => {
            let (paths, id) = existing_session(&paths, &session_id)?;
            export_blob(&paths, &id, &sha256, total_size, offset)
        }
        ApiRequest::ImportSessionBlob { session_id, chunk } => {
            let (paths, id) = parse_session(&paths, &session_id)?;
            import_blob(&paths, &id, chunk)
        }
        ApiRequest::GetSessionBlobStatus {
            session_id,
            sha256,
            total_size,
        } => {
            let (paths, id) = existing_session_dir(&paths, &session_id)?;
            blob_status(&paths, &id, &sha256, total_size)
        }
        ApiRequest::ExportSessionRecordBlob {
            session_id,
            reference,
            offset,
        } => {
            let (paths, id) = existing_session(&paths, &session_id)?;
            export_record_blob(&paths, &id, reference, offset)
        }
        ApiRequest::ImportSessionRecordBlob {
            session_id,
            reference,
            chunk,
        } => {
            let (paths, id) = parse_session(&paths, &session_id)?;
            import_record_blob(&paths, &id, reference, chunk)
        }
        ApiRequest::GetSessionRecordBlobStatus { session_id, reference } => {
            let (paths, id) = existing_session_dir(&paths, &session_id)?;
            record_blob_status(&paths, &id, reference)
        }
        _ => Err(store::StoreError::internal(
            store::StoreErrorCode::Internal,
            "replication dispatcher received another request",
        )),
    }
}

fn home_paths() -> Result<store::SessionPaths, store::StoreError> {
    let home = std::env::var_os("HORON_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".horon")))
        .ok_or_else(|| {
            store::StoreError::internal(store::StoreErrorCode::Internal, "no horon-agent home is configured")
        })?;
    let home = if home.exists() {
        home.canonicalize().map_err(store::StoreError::from)?
    } else {
        home
    };
    // A replica's blobs and receipts are originals under this home, so they are
    // flushed at the level the operator configured, like the daemon's own.
    Ok(store::SessionPaths::new(home.join("sessions"))
        .with_sync_mode(horon_agent_base::config::config().storage_sync_mode()))
}

fn existing_session(
    paths: &store::SessionPaths,
    session_id: &str,
) -> Result<(store::SessionPaths, store::SessionId), store::StoreError> {
    let (paths, id) = parse_session(paths, session_id)?;
    if !paths.events_log(&id)?.is_file() {
        return Err(store::StoreError::unknown_session("session has no event log"));
    }
    Ok((paths, id))
}

fn existing_session_dir(
    paths: &store::SessionPaths,
    session_id: &str,
) -> Result<(store::SessionPaths, store::SessionId), store::StoreError> {
    let (paths, id) = parse_session(paths, session_id)?;
    if !paths.session_dir(&id)?.is_dir() {
        return Err(store::StoreError::unknown_session("session has no local replica"));
    }
    Ok((paths, id))
}

fn parse_session(
    paths: &store::SessionPaths,
    session_id: &str,
) -> Result<(store::SessionPaths, store::SessionId), store::StoreError> {
    Ok((paths.clone(), store::SessionId::parse(session_id)?))
}

fn parse_ids(ids: &[String]) -> Result<Vec<store::EventId>, store::StoreError> {
    ids.iter().map(store::EventId::parse).collect()
}

fn list_heads(paths: &store::SessionPaths, id: &store::SessionId) -> Result<ApiEvent, store::StoreError> {
    let graph = store::EventGraph::open(paths, id)?;
    let heads: Vec<String> = graph.heads().iter().map(|head| head.as_str().to_owned()).collect();
    let selected = selected_head(paths, id, &graph)?;
    Ok(ApiEvent::SessionHeads {
        session_id: id.as_str().to_owned(),
        heads,
        selected_head: selected.map(|head| head.as_str().to_owned()),
    })
}

fn selected_head(
    paths: &store::SessionPaths,
    id: &store::SessionId,
    graph: &store::EventGraph,
) -> Result<Option<store::EventId>, store::StoreError> {
    let directory = paths.session_dir(id)?;
    let explicit = match std::fs::read(directory.join("local-head-explicit.json")) {
        Ok(bytes) => serde_json::from_slice::<bool>(&bytes).map_err(json_error)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    let stored = if explicit {
        match std::fs::read(directory.join("local-head.json")) {
            Ok(bytes) => Some(serde_json::from_slice::<store::EventId>(&bytes).map_err(json_error)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        }
    } else {
        None
    };
    if let Some(head) = stored
        && graph.event(&head).is_some()
    {
        return Ok(Some(head));
    }
    match graph.heads() {
        [only] => Ok(Some(only.clone())),
        _ => Ok(None),
    }
}

fn select_head(
    paths: &store::SessionPaths,
    id: &store::SessionId,
    head_event_id: &str,
) -> Result<ApiEvent, store::StoreError> {
    let head = store::EventId::parse(head_event_id)?;
    let graph = store::EventGraph::open(paths, id)?;
    graph.replay(Some(&head))?;
    let _writer = store::BlockingWriter::lock_for_exchange(
        paths.clone(),
        id.clone(),
        (Box::new(|_| Ok(())), store::ReadProbe::default()),
    )?;
    let directory = paths.session_dir(id)?;
    publish(&directory.join("local-head.json"), &head)?;
    publish(&directory.join("local-head-explicit.json"), &true)?;
    std::fs::File::open(&directory)?.sync_all()?;
    let graph = store::EventGraph::open(paths, id)?;
    Ok(ApiEvent::SessionHeads {
        session_id: id.as_str().to_owned(),
        heads: graph.heads().iter().map(|head| head.as_str().to_owned()).collect(),
        selected_head: Some(head.as_str().to_owned()),
    })
}

fn publish(path: &Path, value: &impl Serialize) -> Result<(), store::StoreError> {
    let directory = path
        .parent()
        .ok_or_else(|| store::StoreError::internal(store::StoreErrorCode::Io, "local head has no parent"))?;
    let temporary = directory.join(".local-head.tmp");
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(&serde_json::to_vec(value).map_err(json_error)?)?;
        file.sync_all()?;
    }
    std::fs::rename(&temporary, path)?;
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

fn history_page(
    paths: &store::SessionPaths,
    id: &store::SessionId,
    head_event_id: Option<&str>,
    before_event_id: Option<&str>,
    limit: Option<u32>,
) -> Result<ApiEvent, store::StoreError> {
    let limit = limit.unwrap_or(horon_agent_harness_api::DEFAULT_HISTORY_PAGE);
    if limit == 0 || limit > MAX_HISTORY_PAGE {
        return Err(store::StoreError::invalid_request(
            store::StoreErrorCode::InvalidRecord,
            format!("history limit must be 1..={MAX_HISTORY_PAGE}"),
        ));
    }
    let head = head_event_id.map(store::EventId::parse).transpose()?;
    let pages = store::HistoryPages::open(paths, id, head.as_ref(), &store::ReadProbe::default())?;
    if pages.built() {
        horon_agent_perf_counters::record_history_index_build(pages.len() as u64);
    }
    let end = match before_event_id {
        Some(cursor) => pages.position_of_event_str(cursor)?.ok_or_else(|| {
            store::StoreError::invalid_request(
                store::StoreErrorCode::InvalidCursor,
                "history cursor is not on this head",
            )
        })?,
        None => pages.len(),
    };
    let start = end.saturating_sub(limit as usize);
    let next_cursor = if start > 0 {
        Some(pages.event_id_at(start)?.as_str().to_owned())
    } else {
        None
    };
    let mut messages = Vec::with_capacity(end - start);
    for message in pages.page(start, end)? {
        let value = serde_json::to_value(&message.message).map_err(json_error)?;
        messages.push(HistoryMessage {
            response_stats: None,
            role: value["role"].as_str().unwrap_or_default().to_owned(),
            content: flatten_content(&value["content"]),
        });
    }
    let event = ApiEvent::SessionHistoryPage {
        session_id: id.as_str().to_owned(),
        head: pages.head().as_str().to_owned(),
        messages,
        next_cursor,
    };
    let wire = serde_json::to_vec(&event).map_or(0, |bytes| bytes.len() as u64);
    horon_agent_perf_counters::record_history_page(0, (end - start) as u64, wire);
    Ok(event)
}

fn export_events(
    paths: &store::SessionPaths,
    request: store::replication::ExportRequest,
) -> Result<ApiEvent, store::StoreError> {
    let page = store::replication::export_events(paths, &request)?;
    Ok(ApiEvent::SessionEventPage {
        session_id: page.session_id.as_str().to_owned(),
        heads: page.heads.iter().map(|head| head.as_str().to_owned()).collect(),
        events: page.events.iter().map(api_record).collect(),
        next_cursor: page.next_cursor,
        blob_refs: page.blob_refs.iter().map(api_payload_ref).collect(),
        record_refs: page.record_refs.iter().map(api_record_ref).collect(),
    })
}

fn import_events(
    paths: &store::SessionPaths,
    page: &store::replication::SessionEventPage,
    retry_cursor: Option<&str>,
) -> Result<ApiEvent, store::StoreError> {
    let mut writer = store::BlockingWriter::lock_for_exchange(
        paths.clone(),
        page.session_id.clone(),
        (Box::new(|_| Ok(())), store::ReadProbe::default()),
    )?;
    let result = writer.import_events_resuming(page, retry_cursor)?;
    Ok(ApiEvent::SessionImportResult {
        session_id: result.session_id.as_str().to_owned(),
        accepted_ids: result.accepted_ids.iter().map(|id| id.as_str().to_owned()).collect(),
        heads: result.heads.iter().map(|head| head.as_str().to_owned()).collect(),
        retry_cursor: result.retry_cursor,
        conflict_heads: result
            .conflict_heads
            .iter()
            .map(|head| head.as_str().to_owned())
            .collect(),
    })
}

fn export_blob(
    paths: &store::SessionPaths,
    id: &store::SessionId,
    sha256: &str,
    total_size: u64,
    offset: u64,
) -> Result<ApiEvent, store::StoreError> {
    let reference = store::PayloadRef {
        sha256: sha256.to_owned(),
        size: total_size,
        encoding: store::PayloadEncoding::CanonicalJson,
    };
    let chunk = store::blob_exchange::export_blob(paths, id, &reference, offset)?;
    Ok(api_blob_chunk(id, chunk))
}

fn import_blob(
    paths: &store::SessionPaths,
    id: &store::SessionId,
    chunk: SessionBlobChunk,
) -> Result<ApiEvent, store::StoreError> {
    let status = store::blob_exchange::import_blob(paths, id, &store_blob_chunk(chunk))?;
    Ok(ApiEvent::SessionBlobStatus {
        session_id: id.as_str().to_owned(),
        sha256: status.sha256,
        received: status.received,
        total_size: status.total_size,
        complete: status.complete,
    })
}

fn blob_status(
    paths: &store::SessionPaths,
    id: &store::SessionId,
    sha256: &str,
    total_size: u64,
) -> Result<ApiEvent, store::StoreError> {
    let reference = store::PayloadRef {
        sha256: sha256.to_owned(),
        size: total_size,
        encoding: store::PayloadEncoding::CanonicalJson,
    };
    let status = store::blob_exchange::blob_status(paths, id, &reference)?;
    Ok(ApiEvent::SessionBlobStatus {
        session_id: id.as_str().to_owned(),
        sha256: status.sha256,
        received: status.received,
        total_size: status.total_size,
        complete: status.complete,
    })
}

fn export_record_blob(
    paths: &store::SessionPaths,
    id: &store::SessionId,
    reference: CanonicalRecordRef,
    offset: u64,
) -> Result<ApiEvent, store::StoreError> {
    let reference = store_record_ref(reference)?;
    let chunk = store::record_exchange::export_record_blob(paths, id, &reference, offset)?;
    Ok(api_blob_chunk(id, chunk.chunk))
}

fn import_record_blob(
    paths: &store::SessionPaths,
    id: &store::SessionId,
    reference: CanonicalRecordRef,
    chunk: SessionBlobChunk,
) -> Result<ApiEvent, store::StoreError> {
    let reference = store_record_ref(reference)?;
    let status = store::record_exchange::import_record_blob(
        paths,
        id,
        &store::record_exchange::RecordBlobChunk {
            reference,
            chunk: store_blob_chunk(chunk),
        },
    )?;
    Ok(ApiEvent::SessionRecordBlobStatus {
        session_id: id.as_str().to_owned(),
        reference: api_record_ref(&status.reference),
        received: status.status.received,
        total_size: status.status.total_size,
        complete: status.status.complete,
    })
}

fn record_blob_status(
    paths: &store::SessionPaths,
    id: &store::SessionId,
    reference: CanonicalRecordRef,
) -> Result<ApiEvent, store::StoreError> {
    let reference = store_record_ref(reference)?;
    let status = store::record_exchange::record_blob_status(paths, id, &reference)?;
    Ok(ApiEvent::SessionRecordBlobStatus {
        session_id: id.as_str().to_owned(),
        reference: api_record_ref(&status.reference),
        received: status.status.received,
        total_size: status.status.total_size,
        complete: status.status.complete,
    })
}

fn api_record(record: &store::CanonicalEventRecord) -> CanonicalEventRecord {
    CanonicalEventRecord {
        event_id: record.event_id.as_str().to_owned(),
        content_hash: record.content_hash.clone(),
        canonical_json: record.canonical_json.clone(),
    }
}

fn store_record(record: CanonicalEventRecord) -> Result<store::CanonicalEventRecord, store::StoreError> {
    Ok(store::CanonicalEventRecord {
        event_id: store::EventId::parse(&record.event_id)?,
        content_hash: record.content_hash,
        canonical_json: record.canonical_json,
    })
}

fn api_payload_ref(reference: &store::PayloadRef) -> PayloadRef {
    PayloadRef {
        sha256: reference.sha256.clone(),
        size: reference.size,
        encoding: "canonical_json".to_owned(),
    }
}

fn store_payload_ref(reference: PayloadRef) -> store::PayloadRef {
    store::PayloadRef {
        sha256: reference.sha256,
        size: reference.size,
        encoding: store::PayloadEncoding::CanonicalJson,
    }
}

fn api_record_ref(reference: &store::replication::CanonicalRecordRef) -> CanonicalRecordRef {
    CanonicalRecordRef {
        position: reference.position,
        event_id: reference.event_id.as_str().to_owned(),
        content_hash: reference.content_hash.clone(),
        sha256: reference.sha256.clone(),
        size: reference.size,
    }
}

fn store_record_ref(
    reference: CanonicalRecordRef,
) -> Result<store::replication::CanonicalRecordRef, store::StoreError> {
    Ok(store::replication::CanonicalRecordRef {
        position: reference.position,
        event_id: store::EventId::parse(&reference.event_id)?,
        content_hash: reference.content_hash,
        sha256: reference.sha256,
        size: reference.size,
    })
}

fn api_blob_chunk(id: &store::SessionId, chunk: store::blob_exchange::SessionBlobChunk) -> ApiEvent {
    ApiEvent::SessionBlobChunk {
        session_id: id.as_str().to_owned(),
        sha256: chunk.sha256,
        offset: chunk.offset,
        data_base64: chunk.data_base64,
        next_offset: chunk.next_offset,
        total_size: chunk.total_size,
        complete: chunk.complete,
    }
}

fn store_blob_chunk(chunk: SessionBlobChunk) -> store::blob_exchange::SessionBlobChunk {
    store::blob_exchange::SessionBlobChunk {
        sha256: chunk.sha256,
        offset: chunk.offset,
        data_base64: chunk.data_base64,
        next_offset: chunk.next_offset,
        total_size: chunk.total_size,
        complete: chunk.complete,
    }
}

fn error_reply(api_id: u64, error: store::StoreError) -> Vec<Outbound> {
    let code = match error.category {
        store::StoreErrorCategory::InvalidRequest => ErrorCode::InvalidRequest,
        store::StoreErrorCategory::Internal => ErrorCode::Internal,
        store::StoreErrorCategory::UnknownSession => ErrorCode::UnknownSession,
    };
    let details = StoreErrorDetail::from_wire_detail(&error.wire_detail());
    vec![Outbound::Reply(ServerFrame::reply(
        api_id,
        ApiEvent::Error {
            code,
            message: error.message,
            details,
        },
    ))]
}

fn json_error(error: serde_json::Error) -> store::StoreError {
    store::StoreError::internal(store::StoreErrorCode::Internal, error.to_string())
}
