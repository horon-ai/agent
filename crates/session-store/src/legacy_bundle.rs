//! Read-only legacy inventory and ownership guard.
use crate::legacy::{LegacySource, conflict, corrupt};
use crate::*;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

pub(crate) const INDEX: &str = "session-metadata-v1.sqlite3";

pub(crate) struct Bundle {
    pub files: BTreeMap<String, Vec<u8>>,
}

pub(crate) fn safe_path(path: &Path) -> Result<(), StoreError> {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(crate::blobs::unsafe_path(path));
        }
        prefix.push(component);
        match fs::symlink_metadata(&prefix) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(crate::blobs::unsafe_path(path));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn optional(path: &Path) -> Result<Option<Vec<u8>>, StoreError> {
    safe_path(path)?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        #[cfg(target_os = "macos")]
        options.custom_flags(0x100);
        #[cfg(target_os = "linux")]
        options.custom_flags(0x20000);
    }
    match options.open(path) {
        Ok(mut file) => {
            if !file.metadata()?.is_file() {
                return Err(crate::blobs::unsafe_path(path));
            }
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            Ok(Some(bytes))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn pid_guard(raw: &[u8]) -> Result<(), StoreError> {
    let pid = std::str::from_utf8(raw)
        .map_err(|e| corrupt(e.to_string()))?
        .trim()
        .parse::<u32>()
        .map_err(|e| corrupt(e.to_string()))?;
    if pid == 0 {
        return Err(corrupt("invalid legacy PID zero"));
    }
    // No unsafe FFI and no shell interpolation. ps exit 1 means no matching PID.
    let output = std::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output()?;
    match output.status.code() {
        Some(0) => Err(StoreError::busy("live legacy session owner")),
        Some(1) => Ok(()),
        _ => Err(StoreError::internal(StoreErrorCode::Io, "PID liveness query failed")),
    }
}

pub(crate) fn guard(source: &LegacySource) -> Result<(), StoreError> {
    for directory in ["active_pids", "streaming_pids"] {
        if let Some(raw) = optional(&source.home.join(directory).join(source.id.as_str()))? {
            pid_guard(&raw)?;
        }
    }
    if let Some(raw) = optional(&source.home.join(format!("session_leases/{}.json", source.id.as_str())))? {
        let lease: Lease = serde_json::from_slice(&raw).map_err(|e| corrupt(e.to_string()))?;
        if lease.session_id != source.id.as_str() {
            return Err(corrupt("lease session identity mismatch"));
        }
        if lease.away_host.is_some() {
            return Err(StoreError::busy("legacy lease belongs to another host"));
        }
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct Lease {
    session_id: String,
    epoch: u64,
    away_host: Option<String>,
}

impl Bundle {
    pub fn capture(source: &LegacySource) -> Result<Self, StoreError> {
        if source.runtime && !crate::legacy_runtime::index_stamps(&source.home)?.is_empty() {
            return Err(crate::legacy::migration_required());
        }
        guard(source)?;
        let id = source.id.as_str();
        let mut names = vec![
            format!("sessions/{id}.json"),
            format!("sessions/{id}.bak"),
            format!("sessions/{id}.journal.jsonl"),
            "sdk-archive.json".into(),
            INDEX.into(),
            format!("{INDEX}-wal"),
            format!("{INDEX}-shm"),
            format!("sessions/edit-stats/{id}.json"),
            format!("sessions/edit-stats/{id}.lock"),
            format!("session_leases/{id}.json"),
            format!("active_pids/{id}"),
            format!("streaming_pids/{id}"),
            format!("internal_pids/{id}"),
        ];
        for suffix in ["", "-goals", "-plan", "-gate-observations", "-review-state"] {
            names.push(format!("todos/{id}{suffix}.json"));
        }
        let sessions = source.home.join("sessions");
        safe_path(&sessions)?;
        let entries = match fs::read_dir(sessions) {
            Ok(entries) => Some(entries),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        for entry in entries.into_iter().flatten() {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| corrupt("non-UTF8 source filename"))?;
            if name.starts_with(&format!("{id}.")) {
                if !entry.file_type()?.is_file() {
                    return Err(crate::blobs::unsafe_path(&entry.path()));
                }
                names.push(format!("sessions/{name}"));
            }
        }
        names.sort();
        names.dedup();
        let mut files = BTreeMap::new();
        for name in names {
            if source.runtime && (name == INDEX || name == format!("{INDEX}-wal") || name == format!("{INDEX}-shm")) {
                continue;
            }
            if let Some(bytes) = optional(&source.home.join(&name))? {
                files.insert(name, bytes);
            }
        }
        guard(source)?;
        Ok(Self { files })
    }

    pub fn has_index(&self) -> bool {
        self.files.contains_key(INDEX)
            || self.files.contains_key(&format!("{INDEX}-wal"))
            || self.files.contains_key(&format!("{INDEX}-shm"))
    }

    pub fn hashes(&self) -> BTreeMap<String, String> {
        self.files.iter().map(|(p, b)| (p.clone(), sha256_hex(b))).collect()
    }

    pub fn verify(&self, source: &LegacySource) -> Result<(), StoreError> {
        if self.hashes() != Self::capture(source)?.hashes() {
            return Err(conflict());
        }
        Ok(())
    }

    pub fn guard_snapshot(&self, source: &LegacySource, raw: &serde_json::Value) -> Result<(), StoreError> {
        if let Some(pid) = raw.get("last_pid").filter(|v| !v.is_null()) {
            let pid = pid
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| corrupt("invalid snapshot PID"))?;
            pid_guard(pid.to_string().as_bytes())?;
        }
        if let Some(bytes) = self.files.get(&format!("session_leases/{}.json", source.id.as_str())) {
            let lease: Lease = serde_json::from_slice(bytes).map_err(|e| corrupt(e.to_string()))?;
            let epoch = raw.get("migration_epoch").and_then(|v| v.as_u64()).unwrap_or(0);
            if epoch < lease.epoch {
                return Err(StoreError::busy("legacy snapshot predates its lease"));
            }
        }
        Ok(())
    }

    pub fn copy_to(&self, root: &Path, disk: &mut crate::blobs::Disk) -> Result<(), StoreError> {
        use std::io::Write;
        for (name, bytes) in &self.files {
            let path = root.join(name);
            let parent = path.parent().ok_or_else(|| corrupt("source parent missing"))?;
            disk.directory(parent)?;
            let mut file = crate::blobs::new_file(&path)?;
            file.write_all(bytes)?;
            crate::blobs::sync_file(&file, disk.mode)?;
            disk.sync_dir(parent, IoPoint::DirectorySync)?;
        }
        Ok(())
    }
}

pub(crate) fn sync_tree(path: &Path, mode: SyncMode) -> Result<(), StoreError> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            sync_tree(&entry.path(), mode)?;
        } else if kind.is_file() {
            crate::blobs::sync_file(&File::open(entry.path())?, mode)?;
        } else {
            return Err(crate::blobs::unsafe_path(&entry.path()));
        }
    }
    crate::blobs::sync_directory(path, mode)
}

pub(crate) fn capture_index(home: &Path) -> Result<Bundle, StoreError> {
    safe_path(home)?;
    let mut files = BTreeMap::new();
    for suffix in ["", "-wal", "-shm"] {
        let name = format!("{INDEX}{suffix}");
        if let Some(bytes) = optional(&home.join(&name))? {
            files.insert(name, bytes);
        }
    }
    Ok(Bundle { files })
}

pub(crate) fn guard_home(home: &Path) -> Result<(), StoreError> {
    safe_path(home)?;
    for name in ["active_pids", "streaming_pids"] {
        let directory = home.join(name);
        safe_path(&directory)?;
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            let id = SessionId::parse(entry.file_name().to_string_lossy().as_ref())?;
            guard(&LegacySource::new(home, id)?)?;
        }
    }
    Ok(())
}
