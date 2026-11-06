//! Staged original publication and local completion receipts.
use crate::blobs::{Disk, checked_file, new_file, read_file};
use crate::legacy::*;
use crate::legacy_bundle::{Bundle, guard, safe_path, sync_tree};
use crate::*;
use std::fs;
use std::io::Write;

const MANIFEST: &str = "legacy-source-manifest.json";
const RECEIPT: &str = "legacy-completed.sha256";

fn hit(hook: &mut MigrationHook, point: MigrationPoint) -> Result<(), StoreError> {
    hook(point).map_err(Into::into)
}

pub(crate) fn migrate(
    source: &LegacySource,
    destination: &SessionPaths,
    mut hook: MigrationHook,
) -> Result<MigrationReport, StoreError> {
    let index_stamps = crate::legacy_runtime::index_stamps(&source.home)?;
    let bundle = Bundle::capture(source)?;
    hit(&mut hook, MigrationPoint::SourceCaptured)?;
    bundle.verify(source)?;
    safe_path(destination.root())?;
    let mut disk = Disk::new(Box::new(|_| Ok(())));
    disk.directory(destination.root())?;
    let lock_path = destination.root().join(format!(".legacy-{}.lock", source.id.as_str()));
    let lock = match new_file(&lock_path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => checked_file(&lock_path, true)?,
        Err(e) => return Err(e.into()),
    };
    crate::writer::lock_exclusive(&lock)?;
    disk.sync_file(&lock, IoPoint::LockFileSync)?;
    disk.sync_dir(destination.root(), IoPoint::DirectorySync)?;
    let target = destination.session_dir(&source.id)?;
    let previous = if target.exists() {
        let writer_lock = checked_file(&target.join(".writer.lock"), true)?;
        crate::writer::lock_exclusive(&writer_lock)?;
        let manifest = load_manifest(destination, &source.id)?;
        if manifest.source_files != bundle.hashes() {
            return Err(conflict());
        }
        validate(destination, &manifest)?;
        bundle.verify(source)?;
        if manifest.display_recovery_version == 1 {
            finish(destination, &manifest, &mut hook)?;
            crate::legacy_completion::after_session(destination, &manifest)?;
            return Ok(MigrationReport {
                outcome: MigrationOutcome::AlreadyPublished,
                receipt: manifest,
            });
        }
        Some(manifest)
    } else {
        None
    };
    #[cfg(not(feature = "legacy-session-index-import"))]
    if bundle.has_index() {
        return Err(migration_required());
    }
    let mut state = crate::legacy_replay::replay(source, &bundle)?;
    let stage_root = destination
        .root()
        .join(format!(".legacy-stage-{}", uuid::Uuid::new_v4()));
    disk.directory(&stage_root)?;
    let staged = SessionPaths::new(stage_root.join("sessions"));
    disk.directory(staged.root())?;
    let stage = staged.session_dir(&source.id)?;
    disk.directory(&stage)?;
    bundle.copy_to(&stage.join("legacy-originals"), &mut disk)?;
    hit(&mut hook, MigrationPoint::SourceCopied)?;
    bundle.verify(source)?;
    let snapshot_working_dir = state.metadata.working_dir.clone();
    let snapshot_save_label = state.metadata.save_label.clone();
    let index_session_ids = crate::legacy_index::import(&bundle, &stage, &mut state)?;
    let (logical_source_digest, mut events) = crate::legacy_events::build(&source.id, &mut state)?;
    let mut recovered = state.metadata.clone();
    recovered.working_dir = recovered.working_dir.or(snapshot_working_dir);
    if recovered.saved {
        recovered.save_label = recovered.save_label.or(snapshot_save_label);
    }
    if recovered != state.metadata {
        // Preserve every existing record/ID/hash. This deterministic child makes
        // the effective legacy display metadata portable without host receipts.
        let payload = serde_json::json!(MetadataChangedPayload {
            metadata: recovered,
            resume_key: None,
        });
        let recovery_digest = sha256_hex(canonical_json(&payload)?.as_bytes());
        let ordinal = u64::try_from(events.len()).map_err(|e| corrupt(e.to_string()))?;
        events.push(
            EventDraft::new(
                source.id.clone(),
                legacy_event_id(
                    source.id.as_str(),
                    &format!("display-recovery-v1/{logical_source_digest}/{recovery_digest}"),
                    ordinal,
                )?,
                events.last().map(|event| event.envelope().event_id.clone()),
                legacy_origin_id(source.id.as_str(), &logical_source_digest)?,
                pick_legacy_timestamp(None, Some(state.metadata.created_at)),
                EventKind::new(KnownEventKind::MetadataChanged),
                payload,
            )
            .seal()?,
        );
    }
    let manifest = MigrationReceipt {
        version: 1,
        session_id: source.id.clone(),
        source_files: bundle.hashes(),
        source_digest: sha256_hex(
            canonical_json(&serde_json::to_value(bundle.hashes()).map_err(|e| corrupt(e.to_string()))?)?.as_bytes(),
        ),
        logical_source_digest: logical_source_digest.clone(),
        origin_id: legacy_origin_id(source.id.as_str(), &logical_source_digest)?,
        events: events
            .iter()
            .map(|e| (e.envelope().event_id.clone(), e.envelope().content_hash.clone()))
            .collect(),
        index_imported: index_session_ids.is_some(),
        display_recovery_version: 1,
        index_session_ids,
        index_stamps,
        diagnostics: state.diagnostics,
    };
    let mut writer = BlockingWriter::open(staged.clone(), source.id.clone())?;
    for event in &events {
        writer.append(event)?;
    }
    hit(&mut hook, MigrationPoint::EventsStaged)?;
    let bytes = canonical_json(&serde_json::to_value(&manifest).map_err(|e| corrupt(e.to_string()))?)?;
    let mut file = new_file(&stage.join(MANIFEST))?;
    file.write_all(bytes.as_bytes())?;
    hit(&mut hook, MigrationPoint::ManifestSync)?;
    crate::blobs::sync_file(&file, SyncMode::Full)?;
    if source.runtime {
        crate::legacy_runtime::mark_first_file(&staged, &source.id)?;
    }
    validate(&staged, &manifest)?;
    hit(&mut hook, MigrationPoint::OriginalsSync)?;
    sync_tree(&stage)?;
    hit(&mut hook, MigrationPoint::StageDirectorySync)?;
    crate::blobs::sync_directory(&stage, SyncMode::Full)?;
    crate::blobs::sync_directory(staged.root(), SyncMode::Full)?;
    bundle.verify(source)?;
    if manifest.index_stamps != crate::legacy_runtime::index_stamps(&source.home)? {
        return Err(conflict());
    }
    if let Some(previous) = previous {
        if !manifest.events.starts_with(&previous.events) {
            return Err(conflict());
        }
        let mut existing = BlockingWriter::open(destination.clone(), source.id.clone())?;
        validate(destination, &previous)?;
        if manifest.events.len() != previous.events.len() {
            let graph = EventGraph::open(destination, &source.id)?;
            // Accept the exact old prefix, or the exact extended prefix after an
            // interrupted upgrade. Never choose between local descendants/heads.
            if graph.heads().len() != 1
                || graph.ordered_ids().iter().any(|id| {
                    !manifest.events.iter().any(|(expected, hash)| {
                        expected == id && graph.event(id).is_some_and(|event| &event.content_hash == hash)
                    })
                })
            {
                return Err(StoreError::invalid_request(
                    StoreErrorCode::SourceConflict,
                    "offline display recovery has local descendants or divergent heads; originals preserved",
                ));
            }
            for event in events.iter().skip(previous.events.len()) {
                existing.append(event)?;
            }
        }
        hit(&mut hook, MigrationPoint::UpgradeAppended)?;
        bundle.verify(source)?;
        crate::cache_io::publish(
            &mut disk,
            &target.join(MANIFEST),
            bytes.as_bytes(),
            &ReadProbe::default(),
        )?;
        hit(&mut hook, MigrationPoint::UpgradeManifestPublished)?;
        finish(destination, &manifest, &mut hook)?;
        crate::legacy_completion::after_session(destination, &manifest)?;
        return Ok(MigrationReport {
            outcome: MigrationOutcome::Published,
            receipt: manifest,
        });
    }
    // Mark the real destination catalog dirty before its originals appear.
    let dirty = destination.root().join(".catalog.dirty");
    disk.directory(&dirty)?;
    let dirty_file = match new_file(&dirty.join(source.id.as_str())) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => checked_file(&dirty.join(source.id.as_str()), true)?,
        Err(e) => return Err(e.into()),
    };
    disk.sync_file(&dirty_file, IoPoint::DirtyFileSync)?;
    disk.sync_dir(&dirty, IoPoint::DirtyDirectorySync)?;
    hit(&mut hook, MigrationPoint::BeforePublish)?;
    guard(source)?;
    bundle.verify(source)?;
    if target.exists() {
        return Err(conflict());
    }
    fs::rename(&stage, &target)?;
    hit(&mut hook, MigrationPoint::Renamed)?;
    finish(destination, &manifest, &mut hook)?;
    crate::legacy_completion::after_session(destination, &manifest)?;
    drop(writer);
    Ok(MigrationReport {
        outcome: MigrationOutcome::Published,
        receipt: manifest,
    })
}

fn load_manifest(paths: &SessionPaths, id: &SessionId) -> Result<MigrationReceipt, StoreError> {
    let path = paths.session_dir(id)?.join(MANIFEST);
    let manifest: MigrationReceipt = serde_json::from_slice(&read_file(&path)?).map_err(|e| corrupt(e.to_string()))?;
    if manifest.version != 1 || manifest.session_id != *id {
        return Err(corrupt("migration receipt identity/version mismatch"));
    }
    let digest = sha256_hex(
        canonical_json(&serde_json::to_value(&manifest.source_files).map_err(|e| corrupt(e.to_string()))?)?.as_bytes(),
    );
    if digest != manifest.source_digest
        || legacy_origin_id(id.as_str(), &manifest.logical_source_digest)? != manifest.origin_id
    {
        return Err(corrupt("migration receipt digest mismatch"));
    }
    Ok(manifest)
}

fn validate(paths: &SessionPaths, manifest: &MigrationReceipt) -> Result<(), StoreError> {
    let graph = EventGraph::open(paths, &manifest.session_id)?;
    if manifest.events.is_empty() {
        return Err(corrupt("empty migration receipt"));
    }
    for (id, hash) in &manifest.events {
        let event = graph.event(id).ok_or_else(|| corrupt("migration event missing"))?;
        if &event.content_hash != hash || event.origin_id != manifest.origin_id {
            return Err(corrupt("migration event differs from receipt"));
        }
    }
    let head = manifest
        .events
        .last()
        .ok_or_else(|| corrupt("missing migration head"))?;
    graph.replay(Some(&head.0))?;
    let originals = paths.session_dir(&manifest.session_id)?.join("legacy-originals");
    for (name, hash) in &manifest.source_files {
        let relative = std::path::Path::new(name);
        if relative.is_absolute()
            || relative
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(corrupt("unsafe migration manifest path"));
        }
        safe_path(&originals.join(relative))?;
        if sha256_hex(&read_file(&originals.join(relative))?) != *hash {
            return Err(corrupt("preserved legacy original differs from receipt"));
        }
    }
    Ok(())
}

fn finish(paths: &SessionPaths, manifest: &MigrationReceipt, hook: &mut MigrationHook) -> Result<(), StoreError> {
    let target = paths.session_dir(&manifest.session_id)?;
    hit(hook, MigrationPoint::ParentSync)?;
    crate::blobs::sync_directory(paths.root(), SyncMode::Full)?;
    hit(hook, MigrationPoint::ParentSynced)?;
    let digest = sha256_hex(&read_file(&target.join(MANIFEST))?);
    crate::legacy_runtime::publish_runtime_sources(paths, manifest, &digest)?;
    hit(hook, MigrationPoint::RuntimeSourcesPublished)?;
    let path = target.join(RECEIPT);
    let current = match checked_file(&path, false) {
        Ok(_) => Some(read_file(&path)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if current.as_deref() != Some(digest.as_bytes()) {
        hit(hook, MigrationPoint::ReceiptSync)?;
        crate::cache_io::publish(
            &mut Disk::new(Box::new(|_| Ok(()))),
            &path,
            digest.as_bytes(),
            &ReadProbe::default(),
        )?;
    }
    hit(hook, MigrationPoint::ReceiptDirectorySync)?;
    crate::blobs::sync_directory(&target, SyncMode::Full)?;
    let pending = target.join(".legacy-first-file.json");
    if pending.try_exists()? {
        checked_file(&pending, false)?;
        fs::remove_file(pending)?;
        crate::blobs::sync_directory(&target, SyncMode::Full)?;
    }
    Ok(())
}

pub(crate) fn receipt(paths: &SessionPaths, id: &SessionId) -> Result<MigrationReceipt, StoreError> {
    let path = paths.session_dir(id)?.join(RECEIPT);
    if !path.try_exists()? {
        return Err(StoreError::invalid_request(
            StoreErrorCode::MigrationIncomplete,
            "migration completion receipt absent",
        ));
    }
    let manifest = load_manifest(paths, id)?;
    if read_file(&path)? != sha256_hex(&read_file(&paths.session_dir(id)?.join(MANIFEST))?).as_bytes() {
        return Err(corrupt("completion receipt mismatch"));
    }
    validate(paths, &manifest)?;
    // A surviving marker is re-synced before it can authorize runtime activation.
    crate::blobs::sync_directory(paths.root(), SyncMode::Full)?;
    crate::blobs::sync_file(&checked_file(&path, false)?, SyncMode::Full)?;
    crate::blobs::sync_directory(&paths.session_dir(id)?, SyncMode::Full)?;
    Ok(manifest)
}
