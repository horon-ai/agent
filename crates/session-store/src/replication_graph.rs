//! Closure and dependency ordering shared by export and prevalidated import.
use crate::projection_payload::Payload;
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn dependencies(graph: &EventGraph, id: &EventId) -> BTreeSet<EventId> {
    let mut refs = BTreeSet::new();
    refs.extend(graph.events[id].parent_event_id.iter().cloned());
    match &graph.payloads[id] {
        Payload::Created(p) => {
            if let Some(reset) = &p.reset_provenance {
                refs.extend(reset.cleared_head_event_id.iter().cloned());
            }
        }
        Payload::Revised(p) => {
            refs.insert(p.target_event_id.clone());
        }
        Payload::Compacted(p) => {
            refs.extend(p.source_head_id.iter().cloned());
            refs.insert(p.covered_through_event_id.clone());
            refs.extend(p.first_kept_event_id.iter().cloned());
            refs.extend(p.prior_compaction_id.iter().cloned());
        }
        Payload::Blocked(p) => {
            refs.extend(p.source_head_id.iter().cloned());
        }
        Payload::Appended(_)
        | Payload::Metadata(_)
        | Payload::Archive(_)
        | Payload::Auxiliary(_)
        | Payload::Unknown => {}
    }
    refs
}

pub(crate) fn closure(graph: &EventGraph, heads: &[EventId]) -> Result<Vec<EventId>, StoreError> {
    let mut included = BTreeSet::new();
    let mut pending = heads.to_vec();
    while let Some(id) = pending.pop() {
        if !graph.events.contains_key(&id) {
            return Err(StoreError::invalid_request(
                StoreErrorCode::MissingDependency,
                "missing referenced event",
            ));
        }
        if included.insert(id.clone()) {
            pending.extend(dependencies(graph, &id));
        }
    }
    let mut degrees = BTreeMap::new();
    let mut children: BTreeMap<EventId, Vec<EventId>> = BTreeMap::new();
    let mut ready = BTreeSet::new();
    for id in &included {
        let deps = dependencies(graph, id);
        degrees.insert(id.clone(), deps.len());
        if deps.is_empty() {
            ready.insert(id.clone());
        }
        for dep in deps {
            children.entry(dep).or_default().push(id.clone());
        }
    }
    let mut ordered = Vec::new();
    while let Some(id) = ready.pop_first() {
        if let Some(next) = children.get(&id) {
            for child in next {
                let degree = degrees
                    .get_mut(child)
                    .ok_or_else(|| StoreError::internal(StoreErrorCode::Internal, "missing closure degree"))?;
                *degree -= 1;
                if *degree == 0 {
                    ready.insert(child.clone());
                }
            }
        }
        ordered.push(id);
    }
    if ordered.len() != included.len() {
        return Err(StoreError::invalid_request(
            StoreErrorCode::CyclicHistory,
            "reference cycle",
        ));
    }
    Ok(ordered)
}

pub(crate) fn digest(graph: &EventGraph, ids: &[EventId]) -> Result<String, StoreError> {
    canonical_digest_of_values(
        &ids.iter()
            .map(|id| serde_json::json!({"event_id":id,"content_hash":graph.events[id].content_hash}))
            .collect::<Vec<_>>(),
    )
}

pub(crate) fn resolve(paths: &SessionPaths, id: &SessionId, event: &EventEnvelope) -> Result<Vec<u8>, StoreError> {
    let reference = event
        .payload_ref
        .as_ref()
        .ok_or_else(|| StoreError::invalid_request(StoreErrorCode::PayloadRefMissing, "blob reference missing"))?;
    let path = paths.blob_file(id, &reference.sha256)?;
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(StoreError::invalid_request(
            StoreErrorCode::MissingDependency,
            "original blob not published",
        )),
        Err(e) => Err(e.into()),
        Ok(_) => crate::blobs::read_file(&path),
    }
}
