use anyhow::{Context, Result, ensure};
use horon_agent_session_store as store;
use std::io::Write;

pub(super) fn origin(paths: &store::SessionPaths) -> Result<store::OriginId> {
    let path = paths.root().join(".origin-id");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => {
            let id = store::OriginId::new_v4();
            file.write_all(id.as_str().as_bytes())?;
            file.sync_all()?;
            crate::platform::sync_directory(paths.root())?;
            Ok(id)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure!(
                !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
                "unsafe origin path"
            );
            Ok(store::OriginId::parse(std::fs::read_to_string(path)?)?)
        }
        Err(error) => Err(error.into()),
    }
}

pub(super) fn validate_legacy_source(paths: &store::SessionPaths, id: &store::SessionId) -> Result<()> {
    let home = paths.root().parent().context("legacy home missing")?;
    let sources = match store::legacy::runtime_sources(paths, id, store::ReadProbe::default()) {
        Err(error) if error.code == store::StoreErrorCode::MigrationIncomplete => {
            store::legacy::LegacySource::new(home, id.clone())?.migrate_for_runtime(paths)?;
            store::legacy::runtime_sources(paths, id, store::ReadProbe::default())?
        }
        result => result?,
    };
    let Some(receipt) = sources else {
        return Ok(());
    };
    for (relative, expected) in &receipt.source_files {
        ensure!(
            std::path::Path::new(relative)
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_))),
            "unsafe legacy source receipt"
        );
        let name = std::path::Path::new(relative);
        let filename = name.file_name().and_then(|name| name.to_str()).unwrap_or("");
        let transcript = name.parent() == Some(std::path::Path::new("sessions"))
            && filename.starts_with(&format!("{}.", id.as_str()));
        // Match the inventory roles, not arbitrary JSON: unknown receipt paths
        // still fail closed. Authorization never opens DB bytes or archived copies.
        let index = [
            "session-metadata-v1.sqlite3",
            "session-metadata-v1.sqlite3-wal",
            "session-metadata-v1.sqlite3-shm",
        ]
        .contains(&relative.as_str());
        let mutable = relative == "sdk-archive.json"
            || ["", "-goals", "-plan", "-gate-observations", "-review-state"]
                .iter()
                .any(|suffix| relative == &format!("todos/{}{suffix}.json", id.as_str()))
            || [".json", ".lock"]
                .iter()
                .any(|suffix| relative == &format!("sessions/edit-stats/{}{suffix}", id.as_str()))
            || relative == &format!("session_leases/{}.json", id.as_str())
            || ["active_pids", "streaming_pids", "internal_pids"]
                .iter()
                .any(|directory| relative == &format!("{directory}/{}", id.as_str()));
        ensure!(
            transcript || index || mutable,
            "SourceConflict: unknown legacy source role {relative}"
        );
        if index || mutable {
            continue;
        }
        let bytes = std::fs::read(home.join(relative))
            .with_context(|| format!("SourceConflict: legacy source {relative} disappeared"))?;
        ensure!(
            store::sha256_hex(&bytes) == *expected,
            "SourceConflict: legacy source {relative} changed"
        );
    }
    for suffix in [".journal.jsonl", ".bak"] {
        let relative = format!("sessions/{}{suffix}", id.as_str());
        ensure!(
            receipt.source_files.contains_key(&relative) || !home.join(&relative).exists(),
            "SourceConflict: legacy source bundle gained {relative}"
        );
    }
    Ok(())
}
