//! Tests for the volume lifecycle: export/import, the fence, the key contract,
//! the central metadata client and the deletion step machine.
//!
//! Every test uses its own temporary volume; nothing here reads or writes
//! `HORON_HOME` or the operator's real home.

use super::VolumeLayout;
use super::archive::{self, ImportReport};
use super::control_plane::{
    ControlPlane, DeletionRequest, ResponseBody, SnapshotDeletion, SnapshotQuery, SnapshotRecord, StepReport,
    VolumeDeletion, VolumeStateChange,
};
use super::delete::{self, JobState, STEPS};
use super::fence;
use super::key;
use super::manifest::{self, MANIFEST_FILE, METADATA_FILE, Manifest};
use crate::memory::store::{ListFilter, MemoryWiki, SearchQuery};
use horon_agent_memory_types::MemoryCategory;
use horon_agent_memory_types::page::PageKind;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A temporary volume root, removed on drop.
struct TempVolume {
    path: PathBuf,
}

impl TempVolume {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("horon-volume-{label}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create the temp volume");
        Self { path }
    }

    fn layout(&self) -> VolumeLayout {
        VolumeLayout::for_home(&self.path)
    }
}

impl Drop for TempVolume {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.path) {
            eprintln!("warning: cannot clean up {}: {error}", self.path.display());
        }
    }
}

fn page(
    id: &str,
    kind: PageKind,
    category: MemoryCategory,
    title: &str,
    body: &str,
    seconds: i64,
) -> horon_agent_memory_types::MemoryPage {
    let page_id = horon_agent_memory_types::page::PageId::parse(id).expect("valid page id");
    let now = horon_agent_time::Timestamp::from_timestamp_millis(seconds * 1_000).expect("timestamp in range");
    horon_agent_memory_types::MemoryPage::draft(page_id, kind, category, title, body, now)
}

/// Write one preference page into the store of `layout`.
fn seed(layout: &VolumeLayout, id: &str, title: &str, body: &str, seconds: i64) {
    let mut wiki = MemoryWiki::open(layout.store_root()).expect("open the wiki");
    let mut page = page(
        id,
        PageKind::Preference,
        MemoryCategory::Preference,
        title,
        body,
        seconds,
    );
    page.project = None;
    wiki.write_page(&page).expect("write the page");
}

fn search_ids(layout: &VolumeLayout, text: &str) -> Vec<String> {
    let wiki = MemoryWiki::open(layout.store_root()).expect("open the wiki");
    wiki.search(&SearchQuery {
        text: text.to_string(),
        limit: Some(10),
        ..SearchQuery::default()
    })
    .expect("search")
    .hits
    .into_iter()
    .map(|hit| hit.page.id)
    .collect()
}

/// Every entry path in the tar, for a failure message.
fn tar_paths(archive: &Path) -> Vec<String> {
    let file = std::fs::File::open(archive).expect("open the archive");
    let mut reader = tar::Archive::new(std::io::BufReader::new(file));
    reader
        .entries()
        .expect("entries")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().map(|path| path.display().to_string()).unwrap_or_default())
        .collect()
}

fn read_tar_entry(archive: &Path, name: &str) -> Option<Vec<u8>> {
    let file = std::fs::File::open(archive).expect("open the archive");
    let mut reader = tar::Archive::new(std::io::BufReader::new(file));
    for entry in reader.entries().expect("entries") {
        let mut entry = entry.expect("entry");
        let path = entry.path().expect("path").display().to_string();
        if path == name {
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut bytes).expect("read");
            return Some(bytes);
        }
    }
    None
}

/// Build a tar from arbitrary `(name, bytes)` rows, for the hostile cases.
fn build_tar(rows: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, bytes) in rows {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600);
        header.set_mtime(0);
        header.set_cksum();
        builder
            .append_data(&mut header, *name, *bytes)
            .expect("append the tar entry");
    }
    builder.into_inner().expect("finish the tar")
}

/// Build a one-entry tar whose name the `tar` crate's builder would refuse.
///
/// The builder rejects `..` and absolute names at write time, so a test that
/// wants the *importer* to be the thing that refuses needs a header written
/// directly. The name goes into the 100-byte `name` field, the size is zero and
/// the checksum is recomputed over the changed bytes.
fn build_raw_name_tar(name: &str) -> Vec<u8> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(0);
    header.set_mode(0o600);
    header.set_mtime(0);
    assert!(name.len() <= 100, "the raw name fits the header field");
    {
        let bytes = header.as_mut_bytes();
        bytes[..100].fill(0);
        bytes[..name.len()].copy_from_slice(name.as_bytes());
    }
    header.set_cksum();
    let mut out = Vec::new();
    out.extend_from_slice(header.as_bytes());
    // Two zero blocks end the archive.
    out.extend(std::iter::repeat_n(0u8, 1024));
    out
}

/// Build a tar with one hostile entry type.
fn build_typed_tar(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, kind, bytes) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(*kind);
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600);
        header.set_mtime(0);
        if *kind == tar::EntryType::Symlink {
            header.set_link_name("../escape").expect("set the link target");
        }
        header.set_cksum();
        builder
            .append_data(&mut header, *name, *bytes)
            .expect("append the tar entry");
    }
    builder.into_inner().expect("finish the tar")
}

fn write_archive(root: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = root.join(name);
    std::fs::write(&path, bytes).expect("write the archive");
    path
}

// ---------------------------------------------------------------------------
// Export and import
// ---------------------------------------------------------------------------

#[test]
fn an_exported_volume_imports_into_a_new_home_and_search_works() {
    let source = TempVolume::new("export-src");
    let layout = source.layout();
    seed(
        &layout,
        "mem_1",
        "아침 회의 선호",
        "아침 9시 이전 회의를 선호한다.",
        1_700_000_000,
    );
    seed(
        &layout,
        "mem_2",
        "빌드 잠금",
        "무거운 빌드는 build-lock으로 실행한다.",
        1_700_000_100,
    );

    let archive_path = source.path.join("export.tar");
    let report = archive::export(&layout, &archive_path).expect("export");
    assert_eq!(report.pages, 2);
    assert!(report.page_bytes > 0);
    assert!(report.store_revision >= 1, "the export records the revision it read");
    assert_eq!(
        std::fs::metadata(&archive_path).expect("archive exists").len(),
        report.archive_bytes
    );

    // The manifest is a table of contents for exactly the two pages, with the
    // encryption field the honest value.
    let manifest_bytes = read_tar_entry(&archive_path, MANIFEST_FILE).expect("manifest present");
    let manifest = Manifest::from_json(&manifest_bytes).expect("valid manifest");
    assert_eq!(manifest.version, manifest::FORMAT_VERSION);
    assert_eq!(manifest.pages, 2);
    assert_eq!(manifest.encryption, manifest::ENCRYPTION_NONE);
    assert_eq!(manifest.files.len(), 2);
    assert!(
        manifest.files.iter().all(|file| file.path.starts_with("wiki/")),
        "every page file is below the wiki prefix"
    );

    // Import into a brand new volume.
    let target = TempVolume::new("export-dst");
    let target_layout = target.layout();
    let imported: ImportReport = archive::import(&target_layout, &archive_path).expect("import");
    assert_eq!(imported.pages, 2);

    // Search works against the rebuilt index.
    assert_eq!(search_ids(&target_layout, "아침 회의"), vec!["mem_1"]);
    assert_eq!(search_ids(&target_layout, "빌드 잠금"), vec!["mem_2"]);
    let body = MemoryWiki::open(target_layout.store_root())
        .expect("open the imported wiki")
        .stored_body(&horon_agent_memory_types::page::PageId::parse("mem_2").expect("valid id"))
        .expect("read the body")
        .expect("the page is present");
    assert_eq!(body, "무거운 빌드는 build-lock으로 실행한다.");
}

#[test]
fn an_export_carries_one_consistent_revision_and_no_body_in_its_metadata() {
    let volume = TempVolume::new("export-meta");
    let layout = volume.layout();
    let marker = "volume-marker-архив-none";
    seed(&layout, "mem_1", "Person", marker, 1_700_000_000);
    let archive_path = volume.path.join("export.tar");
    archive::export(&layout, &archive_path).expect("export");

    let metadata = read_tar_entry(&archive_path, METADATA_FILE).expect("metadata present");
    let text = String::from_utf8(metadata).expect("metadata is UTF-8");
    assert!(!text.contains(marker), "metadata.jsonl must not carry the page body");
    let lines = manifest::parse_metadata(text.as_bytes()).expect("valid metadata");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].id, "mem_1");
    assert_eq!(lines[0].body_bytes, marker.len() as u64);
    assert_eq!(lines[0].kind, "preference");
}

#[test]
fn a_deleted_page_is_in_no_later_export() {
    let volume = TempVolume::new("export-deleted");
    let layout = volume.layout();
    seed(&layout, "mem_1", "Kept", "kept body", 1_700_000_000);
    seed(&layout, "mem_2", "Gone", "gone body", 1_700_000_100);
    let id = horon_agent_memory_types::page::PageId::parse("mem_2").expect("valid id");
    MemoryWiki::open(layout.store_root())
        .expect("open")
        .delete_page(&id)
        .expect("delete");

    let archive_path = volume.path.join("export.tar");
    let report = archive::export(&layout, &archive_path).expect("export");
    assert_eq!(report.pages, 1);
    let metadata = read_tar_entry(&archive_path, METADATA_FILE).expect("metadata");
    let text = String::from_utf8(metadata).expect("UTF-8");
    assert!(!text.contains("mem_2"), "a deleted page is not in the export");
}

#[test]
fn an_import_refuses_a_target_that_already_holds_anything() {
    let source = TempVolume::new("import-nonempty-src");
    let layout = source.layout();
    seed(&layout, "mem_1", "Page", "body", 1_700_000_000);
    let archive_path = source.path.join("export.tar");
    archive::export(&layout, &archive_path).expect("export");

    let target = TempVolume::new("import-nonempty-dst");
    let target_layout = target.layout();
    std::fs::create_dir_all(target_layout.store_root()).expect("create the store root");
    std::fs::write(target_layout.store_root().join("leftover.md"), b"x").expect("write a leftover");

    let error = archive::import(&target_layout, &archive_path).expect_err("a non-empty target is refused");
    assert!(
        error.to_string().contains("not empty"),
        "the refusal names the reason: {error}"
    );
}

#[test]
fn an_import_refuses_a_traversal_path() {
    let volume = TempVolume::new("import-traversal");
    let layout = volume.layout();
    let tar = build_raw_name_tar("../../../etc/passwd");
    let archive_path = write_archive(&volume.path, "evil.tar", &tar);
    let error = archive::import(&layout, &archive_path).expect_err("a traversal path is refused");
    assert!(
        error.to_string().contains("is not a page file"),
        "the refusal names the entry: {error}"
    );
}

#[test]
fn an_import_refuses_an_absolute_path() {
    let volume = TempVolume::new("import-absolute");
    let layout = volume.layout();
    let tar = build_raw_name_tar("/etc/passwd");
    let archive_path = write_archive(&volume.path, "evil.tar", &tar);
    let error = archive::import(&layout, &archive_path).expect_err("an absolute path is refused");
    assert!(error.to_string().contains("is not a page file"), "{error}");
}

#[test]
fn an_import_refuses_a_symlink_hardlink_or_device_entry() {
    for (label, kind) in [
        ("symlink", tar::EntryType::Symlink),
        ("hardlink", tar::EntryType::Link),
        ("device", tar::EntryType::Char),
        ("fifo", tar::EntryType::Fifo),
    ] {
        let volume = TempVolume::new(&format!("import-{label}"));
        let layout = volume.layout();
        let tar = build_typed_tar(&[("wiki/facts/x.md", kind, b"")]);
        let archive_path = write_archive(&volume.path, "evil.tar", &tar);
        let error = archive::import(&layout, &archive_path).expect_err("a non-regular entry is refused");
        assert!(
            error.to_string().contains("regular file"),
            "{label} is refused: {error}"
        );
    }
}

#[test]
fn an_import_refuses_a_page_whose_bytes_do_not_match_the_manifest() {
    let source = TempVolume::new("import-digest-src");
    let layout = source.layout();
    seed(&layout, "mem_1", "Page", "original body", 1_700_000_000);
    let archive_path = source.path.join("export.tar");
    archive::export(&layout, &archive_path).expect("export");

    // Rebuild the tar with the page replaced and the manifest untouched.
    let manifest_bytes = read_tar_entry(&archive_path, MANIFEST_FILE).expect("manifest");
    let metadata_bytes = read_tar_entry(&archive_path, METADATA_FILE).expect("metadata");
    let page_bytes = read_tar_entry(&archive_path, "wiki/preferences/mem_1.md")
        .unwrap_or_else(|| panic!("page path missing; tar holds {}", tar_paths(&archive_path).join(", ")));
    let mut tampered = String::from_utf8(page_bytes).expect("UTF-8");
    // Same length, so the size check passes and the digest check is what
    // refuses the file.
    // Same byte length as the original, so the size check cannot be what
    // refuses the file: the digest must.
    tampered = tampered.replace("original body", "tampered b0dy");
    let tar = build_tar(&[
        ("wiki/preferences/mem_1.md", tampered.as_bytes()),
        (METADATA_FILE, &metadata_bytes),
        (MANIFEST_FILE, &manifest_bytes),
    ]);
    let evil = write_archive(&source.path, "tampered.tar", &tar);

    let target = TempVolume::new("import-digest-dst");
    let error = archive::import(&target.layout(), &evil).expect_err("a tampered page is refused");
    assert!(
        error.to_string().contains("manifest digest"),
        "the refusal names the digest: {error}"
    );
}

#[test]
fn an_import_refuses_an_archive_that_is_not_a_memory_export() {
    let volume = TempVolume::new("import-not-export");
    let layout = volume.layout();
    let tar = build_tar(&[("wiki/facts/x.md", b"x")]);
    let archive_path = write_archive(&volume.path, "notexport.tar", &tar);
    let error = archive::import(&layout, &archive_path).expect_err("a tar without a manifest is refused");
    assert!(error.to_string().contains("holds no manifest.json"), "{error}");
}

#[test]
fn an_import_refuses_an_encrypted_manifest_it_cannot_open() {
    let volume = TempVolume::new("import-encrypted");
    let layout = volume.layout();
    // A complete-shaped manifest whose encryption field names an algorithm this
    // build cannot open, with a metadata file so the manifest is what the
    // importer reads and refuses.
    let manifest = serde_json::json!({
        "version": manifest::FORMAT_VERSION,
        "exported_at": "2026-11-23T00:00:00Z",
        "store_revision": 1,
        "encryption": "aes-256-gcm",
        "pages": 0,
        "files": [],
        "total_bytes": 0,
    });
    let tar = build_tar(&[
        (METADATA_FILE, b""),
        (
            MANIFEST_FILE,
            serde_json::to_vec(&manifest).expect("manifest").as_slice(),
        ),
    ]);
    let archive_path = write_archive(&volume.path, "enc.tar", &tar);
    let error = archive::import(&layout, &archive_path).expect_err("an encrypted manifest is refused");
    assert!(
        error.to_string().contains("encrypted with"),
        "the refusal names the encryption: {error}"
    );
}

#[test]
fn an_encrypted_manifest_is_refused_before_any_page_is_written() {
    let manifest = serde_json::json!({
        "version": manifest::FORMAT_VERSION,
        "exported_at": "2026-11-23T00:00:00Z",
        "store_revision": 1,
        "encryption": "aes-256-gcm",
        "pages": 0,
        "files": [],
        "total_bytes": 0,
    });
    let parsed: Manifest = serde_json::from_value(manifest).expect("parse the manifest");
    let error = parsed.validate().expect_err("an encrypted manifest is refused");
    assert!(error.to_string().contains("encrypted with"), "{error}");
}

#[test]
fn a_manifest_lists_exactly_the_archive_files() {
    let volume = TempVolume::new("manifest-count");
    let layout = volume.layout();
    seed(&layout, "mem_1", "One", "first", 1_700_000_000);
    let archive_path = volume.path.join("export.tar");
    archive::export(&layout, &archive_path).expect("export");
    let manifest_bytes = read_tar_entry(&archive_path, MANIFEST_FILE).expect("manifest");
    let mut manifest = Manifest::from_json(&manifest_bytes).expect("valid manifest");
    manifest.pages = 2;
    let error = manifest.validate().expect_err("a pages/files mismatch is refused");
    assert!(error.to_string().contains("lists 1 file(s) for 2 page(s)"), "{error}");
}

// ---------------------------------------------------------------------------
// Quiesce and the write fence
// ---------------------------------------------------------------------------

#[test]
fn a_quiesced_store_refuses_writes_until_resume() {
    let volume = TempVolume::new("fence");
    let layout = volume.layout();
    seed(&layout, "mem_1", "Page", "body", 1_700_000_000);

    let report = fence::quiesce(&layout).expect("quiesce");
    assert!(!report.already_fenced);
    assert!(!report.fence.token.is_empty());
    assert_eq!(report.fence.page_files, 1);
    assert!(fence::is_fenced(&layout.store_root()).expect("read the fence"));

    // A write now refuses, and the refusal names the checkpoint.
    let mut wiki = MemoryWiki::open(layout.store_root()).expect("open");
    let mut next = page(
        "mem_2",
        PageKind::Fact,
        MemoryCategory::Fact,
        "Blocked",
        "blocked body",
        1_700_000_200,
    );
    next.project = None;
    let error = wiki.write_page(&next).expect_err("a fenced store refuses a write");
    assert!(
        error.to_string().contains("quiesced") && error.to_string().contains("volume resume"),
        "the refusal names the fence and the fix: {error}"
    );

    // A second quiesce is idempotent and reports the existing fence.
    let again = fence::quiesce(&layout).expect("second quiesce");
    assert!(again.already_fenced);

    // Resume lifts it and the write succeeds.
    assert!(fence::resume(&layout).expect("resume"));
    assert!(!fence::is_fenced(&layout.store_root()).expect("read the fence"));
    let mut wiki = MemoryWiki::open(layout.store_root()).expect("open");
    wiki.write_page(&next).expect("a resumed store accepts a write");
    assert!(!fence::resume(&layout).expect("second resume"));
}

#[test]
fn a_quiesce_checkpoints_the_write_ahead_log() {
    let volume = TempVolume::new("fence-wal");
    let layout = volume.layout();
    seed(&layout, "mem_1", "One", "first", 1_700_000_000);
    seed(&layout, "mem_2", "Two", "second", 1_700_000_100);

    let report = fence::quiesce(&layout).expect("quiesce");
    // The checkpoint ran on a real index carrying both writes and closed the
    // log: a `-wal` that still held committed rows would defeat the point.
    assert_eq!(report.fence.page_files, 2);
    assert!(
        report.fence.revision >= 2,
        "the quiesce read the store's own revision: {}",
        report.fence.revision
    );
    let wal = layout.store_root().join("index.sqlite-wal");
    let after = std::fs::metadata(&wal).map(|metadata| metadata.len()).unwrap_or(0);
    assert_eq!(after, 0, "the checkpoint truncated the write-ahead log");
}

#[test]
fn a_quiesce_refuses_a_volume_with_no_store() {
    let volume = TempVolume::new("fence-empty");
    let layout = volume.layout();
    let error = fence::quiesce(&layout).expect_err("an empty volume has nothing to quiesce");
    assert!(error.to_string().contains("does not exist yet"), "{error}");
}

// ---------------------------------------------------------------------------
// The key contract
// ---------------------------------------------------------------------------

#[test]
fn a_revoked_key_closes_the_volume() {
    let volume = TempVolume::new("key");
    let layout = volume.layout();
    seed(&layout, "mem_1", "Page", "body", 1_700_000_000);
    let record = key::create_key(&layout, "key-abc-123", "aes-256-gcm-envelope").expect("create the key");
    assert!(!record.is_revoked());
    assert!(key::ensure_open(&layout).expect("a live key opens").is_some());

    let revoked = key::revoke_key(&layout).expect("revoke").expect("a record exists");
    assert!(revoked.is_revoked());
    assert!(revoked.revoked_at.is_some());

    let error = key::ensure_open(&layout).expect_err("a revoked key fails closed");
    assert!(
        error.to_string().contains("revoked at") && error.to_string().contains("key-abc-123"),
        "the refusal names the key and the revocation: {error}"
    );

    // Revoking twice keeps the first instant, so a retried step is stable.
    let again = key::revoke_key(&layout)
        .expect("revoke again")
        .expect("a record exists");
    assert_eq!(again.revoked_at, revoked.revoked_at);
}

#[test]
fn a_key_record_carries_no_material_and_a_staged_copy_is_wiped() {
    let volume = TempVolume::new("key-stage");
    let layout = volume.layout();
    key::create_key(&layout, "key-1", "aes-256-gcm-envelope").expect("create the key");
    let text = std::fs::read_to_string(layout.key_path()).expect("read the key record");
    assert!(text.contains("key-1"));
    assert!(!text.contains("secret"), "the record names the key, not its bytes");

    // A deployment that staged material inside the volume is wiped.
    let staged = layout.key_material_dir();
    std::fs::create_dir_all(&staged).expect("create the staged directory");
    let material = staged.join("user.key");
    std::fs::write(&material, b"super-secret-material-bytes-here").expect("write the staged material");
    let wiped = key::wipe_staged_material(&layout).expect("wipe the staged material");
    assert_eq!(wiped, 1);
    assert!(!material.exists(), "the staged key file is gone");
    assert!(!staged.exists(), "the staged key directory is gone");
}

#[test]
fn key_material_is_redacted_in_debug_and_wiped_on_drop() {
    let material = key::KeyMaterial::new("key-1", b"very-secret-bytes".to_vec());
    let shown = format!("{material:?}");
    assert!(shown.contains("redacted"));
    assert!(!shown.contains("very-secret-bytes"));

    let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
    let target = captured.clone();
    let bytes = "material-for-consumption".to_string();
    key::KeyMaterial::new("key-1", bytes.clone().into_bytes()).consume(|bytes| {
        *target.lock().expect("lock") = Some(bytes.to_vec());
        bytes.len()
    });
    let seen = captured.lock().expect("lock").clone().expect("the callback ran");
    assert_eq!(seen.len(), bytes.len());
}

#[test]
fn a_volume_refuses_a_second_key_record() {
    let volume = TempVolume::new("key-dup");
    let layout = volume.layout();
    key::create_key(&layout, "key-1", "aes-256-gcm-envelope").expect("create the key");
    let error = key::create_key(&layout, "key-2", "aes-256-gcm-envelope").expect_err("a second key record is refused");
    assert!(error.to_string().contains("already holds key"), "{error}");
}

// ---------------------------------------------------------------------------
// The central metadata client: no memory content on the wire
// ---------------------------------------------------------------------------

/// A control plane that records every request it received, in order.
#[derive(Default)]
struct RecordingControlPlane {
    calls: RefCell<Vec<(String, String)>>,
    body_marker_hits: RefCell<Vec<String>>,
    snapshots: RefCell<Vec<SnapshotRecord>>,
    fail_on: RefCell<Option<String>>,
}

impl RecordingControlPlane {
    /// Plant a body marker; any request byte that contains it is recorded.
    fn with_marker(marker: &str) -> Self {
        let plane = Self::default();
        plane.body_marker_hits.borrow_mut().push(format!("__armed__{marker}"));
        plane
    }

    fn record(&self, operation: &str, body: &impl serde::Serialize) {
        let json = serde_json::to_string(body).expect("encode the request");
        let marker = self
            .body_marker_hits
            .borrow()
            .first()
            .filter(|armed| armed.starts_with("__armed__"))
            .map(|armed| armed.trim_start_matches("__armed__").to_string());
        if let Some(marker) = marker
            && json.contains(&marker)
        {
            self.body_marker_hits.borrow_mut().push(format!("{operation}:{json}"));
        }
        self.calls.borrow_mut().push((operation.to_string(), json));
    }

    fn fail(&self, operation: &str) {
        *self.fail_on.borrow_mut() = Some(operation.to_string());
    }

    fn maybe_fail(&self, operation: &str) -> anyhow::Result<()> {
        if self.fail_on.borrow().as_deref() == Some(operation) {
            return Err(anyhow::anyhow!("injected {operation} failure"));
        }
        Ok(())
    }

    fn marker_leak(&self) -> Vec<String> {
        self.body_marker_hits
            .borrow()
            .iter()
            .filter(|hit| !hit.starts_with("__armed__"))
            .cloned()
            .collect()
    }

    fn operations(&self) -> Vec<String> {
        self.calls
            .borrow()
            .iter()
            .map(|(operation, _)| operation.clone())
            .collect()
    }
}

impl ControlPlane for RecordingControlPlane {
    fn begin_deletion(&self, request: &DeletionRequest) -> anyhow::Result<String> {
        self.record("begin_deletion", request);
        self.maybe_fail("begin_deletion")?;
        Ok("job-1".to_string())
    }

    fn record_step(&self, report: &StepReport) -> anyhow::Result<()> {
        self.record("record_step", report);
        self.maybe_fail("record_step")
    }

    fn record_snapshot(&self, record: &SnapshotRecord) -> anyhow::Result<()> {
        self.record("record_snapshot", record);
        self.maybe_fail("record_snapshot")?;
        self.snapshots.borrow_mut().push(record.clone());
        Ok(())
    }

    fn list_snapshots(&self, query: &SnapshotQuery) -> anyhow::Result<Vec<SnapshotRecord>> {
        self.record("list_snapshots", query);
        self.maybe_fail("list_snapshots")?;
        Ok(self.snapshots.borrow().clone())
    }

    fn delete_snapshot(&self, deletion: &SnapshotDeletion) -> anyhow::Result<()> {
        self.record("delete_snapshot", deletion);
        self.maybe_fail("delete_snapshot")?;
        self.snapshots
            .borrow_mut()
            .retain(|snapshot| snapshot.provider_snapshot_id != deletion.provider_snapshot_id);
        Ok(())
    }

    fn delete_volume(&self, deletion: &VolumeDeletion) -> anyhow::Result<()> {
        self.record("delete_volume", deletion);
        self.maybe_fail("delete_volume")
    }

    fn mark_deleted(&self, change: &VolumeStateChange) -> anyhow::Result<()> {
        self.record("mark_deleted", change);
        self.maybe_fail("mark_deleted")
    }
}

#[test]
fn every_control_plane_request_excludes_the_page_body() {
    let volume = TempVolume::new("central-no-body");
    let layout = volume.layout();
    // A body marker that is unique and unmistakable.
    let marker = "ZZ-central-metadata-must-not-carry-this-body-42";
    seed(&layout, "mem_1", "Title", marker, 1_700_000_000);

    let plane = RecordingControlPlane::with_marker(marker);
    // The control plane holds one snapshot so the deletion exercises every call.
    plane.snapshots.borrow_mut().push(SnapshotRecord {
        provider_snapshot_id: "snap-1".to_string(),
        volume_id: "vol-1".to_string(),
        created_at: "2026-11-23T00:00:00Z".to_string(),
        expires_at: "2026-11-28T00:00:00Z".to_string(),
    });

    let report = delete::delete_user(&layout, &plane, "user-1").expect("delete_user runs");
    assert!(report.is_deleted(), "every step finished: {report:?}");
    assert_eq!(
        plane.marker_leak(),
        Vec::<String>::new(),
        "no request carried the page body"
    );

    // Every central operation was called, and each request is valid JSON of the
    // expected shape.
    let operations = plane.operations();
    for expected in [
        "begin_deletion",
        "record_step",
        "list_snapshots",
        "delete_snapshot",
        "delete_volume",
        "mark_deleted",
    ] {
        assert!(
            operations.iter().any(|operation| operation == expected),
            "the client made the {expected} call; it made {operations:?}"
        );
    }
    for (_, body) in plane.calls.borrow().iter() {
        let value: serde_json::Value = serde_json::from_str(body).expect("every request is JSON");
        let object = value.as_object().expect("every request is an object");
        for key in object.keys() {
            assert!(
                !matches!(
                    key.as_str(),
                    "body" | "content" | "text" | "title" | "tags" | "entities" | "path"
                ),
                "a request has no memory-content field, but carries `{key}`"
            );
        }
    }
}

#[test]
fn the_http_client_sends_typed_bodies_to_the_draft_paths() {
    use super::control_plane::Operation;
    // Every operation maps to a distinct internal path, so the mock server can
    // dispatch and a proxy log names what was called.
    let paths: Vec<&str> = [
        Operation::BeginDeletion,
        Operation::RecordStep,
        Operation::RecordSnapshot,
        Operation::ListSnapshots,
        Operation::DeleteSnapshot,
        Operation::DeleteVolume,
        Operation::MarkDeleted,
    ]
    .into_iter()
    .map(Operation::path)
    .collect();
    let mut unique = paths.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), paths.len(), "each operation has one path");
    assert!(
        paths.iter().all(|path| path.starts_with("/internal/v1/")),
        "the draft contract lives under /internal/v1/"
    );
}

#[test]
fn a_mock_control_plane_server_receives_a_deletion_and_carries_no_body() {
    // A loopback HTTP server, owned by the test, that records every request and
    // answers with the shapes the client expects.
    use std::io::{BufRead, Read as _, Write as _};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the mock server");
    let port = listener.local_addr().expect("address").port();
    let requests: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = requests.clone();
    // The server thread serves until the test process exits; the test does not
    // join it, because the loop only ends when the listener is dropped at exit.
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone the stream"));
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            let mut content_length = 0usize;
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                    break;
                }
                let lower = header.to_ascii_lowercase();
                if let Some(rest) = lower.strip_prefix("content-length:") {
                    content_length = rest.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            if content_length > 0 && reader.read_exact(&mut body).is_err() {
                continue;
            }
            let path = request_line.split_whitespace().nth(1).unwrap_or("").to_string();
            let text = String::from_utf8_lossy(&body).to_string();
            recorded.lock().expect("lock").push((path.clone(), text));
            let response: ResponseBody = if path.ends_with("/deletion-jobs") {
                ResponseBody {
                    job_id: Some("job-mock".to_string()),
                    snapshots: Vec::new(),
                }
            } else {
                ResponseBody::default()
            };
            let payload = serde_json::to_vec(&response).expect("encode the response");
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                payload.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&payload);
        }
    });

    let volume = TempVolume::new("mock-server");
    let layout = volume.layout();
    let marker = "ZZ-the-mock-server-must-not-see-this-body";
    seed(&layout, "mem_1", "Title", marker, 1_700_000_000);
    key::create_key(&layout, "key-1", "aes-256-gcm-envelope").expect("create the key");

    let token = super::control_plane::Secret::new("test-token-value");
    let plane = super::control_plane::HttpControlPlane::new(format!("http://127.0.0.1:{port}"), token)
        .expect("build the client");
    let report = delete::delete_user(&layout, &plane, "user-mock").expect("delete_user runs against the mock");
    assert!(report.is_deleted(), "the mock answers every step: {report:?}");

    let requests = requests.lock().expect("lock").clone();
    assert!(!requests.is_empty(), "the client reached the mock server");
    for (path, body) in &requests {
        assert!(!body.contains(marker), "request {path} carried the page body: {body}");
    }
    let paths: BTreeMap<&str, usize> = requests.iter().fold(BTreeMap::new(), |mut map, (path, _)| {
        *map.entry(path.as_str()).or_insert(0) += 1;
        map
    });
    assert!(
        paths.contains_key("/internal/v1/deletion-jobs"),
        "the deletion job was opened"
    );
    assert!(
        paths.contains_key("/internal/v1/user-volumes/state"),
        "the row was marked deleted"
    );
}

#[test]
fn a_control_plane_token_is_redacted_and_resolved_from_a_file() {
    let token = super::control_plane::Secret::new("do-not-print-me");
    assert_eq!(format!("{token:?}"), "Secret(<redacted>)");
    assert_eq!(token.authorization(), "Bearer do-not-print-me");

    let volume = TempVolume::new("token-file");
    let file = volume.path.join("token");
    std::fs::write(&file, "file-token-value\n").expect("write the token file");
    let resolved = super::control_plane::Secret::resolve(Some("HORON_TEST_ABSENT_TOKEN_VARIABLE"), Some(&file))
        .expect("the file token resolves");
    assert_eq!(resolved.authorization(), "Bearer file-token-value");

    let empty = volume.path.join("empty");
    std::fs::write(&empty, "\n").expect("write the empty token file");
    let error = super::control_plane::Secret::resolve(Some("HORON_TEST_ABSENT_TOKEN_VARIABLE"), Some(&empty))
        .expect_err("an empty token file is refused");
    assert!(error.to_string().contains("empty"), "{error}");
}

// ---------------------------------------------------------------------------
// delete_user: ordering, resumability and the early-deleted guard
// ---------------------------------------------------------------------------

#[test]
fn delete_user_finishes_every_step_before_reporting_deleted() {
    let volume = TempVolume::new("delete-order");
    let layout = volume.layout();
    seed(&layout, "mem_1", "Page", "body", 1_700_000_000);
    key::create_key(&layout, "key-1", "aes-256-gcm-envelope").expect("create the key");
    let plane = RecordingControlPlane::default();
    plane.snapshots.borrow_mut().push(SnapshotRecord {
        provider_snapshot_id: "snap-1".to_string(),
        volume_id: volume
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("vol")
            .to_string(),
        created_at: "2026-11-23T00:00:00Z".to_string(),
        expires_at: "2026-11-28T00:00:00Z".to_string(),
    });

    let report = delete::delete_user(&layout, &plane, "user-1").expect("delete_user");
    assert_eq!(report.state, JobState::Deleted);
    assert_eq!(report.steps_done, STEPS.to_vec());
    assert!(report.remaining.is_empty());
    assert!(report.failed_step.is_none());

    // The local record proves the deletion happened and holds every step.
    let job = delete::read_job(&layout).expect("read the job").expect("a job exists");
    assert_eq!(job.state(), JobState::Deleted);
    assert!(job.completed_at.is_some());
    assert_eq!(job.done(), STEPS.to_vec());

    // The store is fenced, the key is revoked and the pages are gone.
    assert!(fence::is_fenced(&layout.store_root()).expect("read the fence"));
    let record = key::read_key(&layout)
        .expect("read the key")
        .expect("a key record exists");
    assert!(record.is_revoked());
    assert!(!layout.store_root().join("wiki").exists(), "the page files are gone");

    // The central row reads deleted only after the provider calls.
    let operations = plane.operations();
    let mark = operations
        .iter()
        .position(|operation| operation == "mark_deleted")
        .expect("mark_deleted was called");
    let volume_delete = operations
        .iter()
        .position(|operation| operation == "delete_volume")
        .expect("delete_volume was called");
    let snapshot_delete = operations
        .iter()
        .position(|operation| operation == "delete_snapshot")
        .expect("delete_snapshot was called");
    assert!(snapshot_delete < volume_delete, "snapshots go before the volume");
    assert!(volume_delete < mark, "the row is marked deleted last");
}

#[test]
fn a_failed_step_stays_retryable_and_never_reports_deleted() {
    let volume = TempVolume::new("delete-retry");
    let layout = volume.layout();
    seed(&layout, "mem_1", "Page", "body", 1_700_000_000);
    key::create_key(&layout, "key-1", "aes-256-gcm-envelope").expect("create the key");

    let plane = RecordingControlPlane::default();
    plane.fail("delete_volume");
    let report = delete::delete_user(&layout, &plane, "user-1").expect("delete_user runs");
    assert_eq!(report.state, JobState::Deleting);
    assert_eq!(report.failed_step.as_deref(), Some("volume_delete"));
    assert!(!report.is_deleted());
    assert_eq!(report.remaining, vec!["volume_delete", "mark_deleted"]);
    assert!(
        report
            .failure
            .as_deref()
            .is_some_and(|failure| failure.contains("injected")),
        "the report names the failure: {report:?}"
    );

    // The record agrees, and the completed steps did not repeat on retry.
    let job = delete::read_job(&layout).expect("read the job").expect("a job exists");
    assert_eq!(job.state(), JobState::Deleting);
    assert!(job.completed_at.is_none());
    assert!(job.is_done("write_stop"));
    assert!(job.is_done("key_revoke"));
    assert!(!job.is_done("volume_delete"));

    // A retry with the failure cleared finishes, and does not repeat the
    // provider calls the earlier run already made.
    *plane.fail_on.borrow_mut() = None;
    plane.calls.borrow_mut().clear();
    let report = delete::delete_user(&layout, &plane, "user-1").expect("retry");
    assert!(report.is_deleted(), "the retry finished: {report:?}");
    let operations = plane.operations();
    assert!(
        !operations.iter().any(|operation| operation == "begin_deletion"),
        "the retry resumes the recorded job instead of opening a new one: {operations:?}"
    );
    assert!(
        operations.iter().any(|operation| operation == "delete_volume"),
        "the retry ran the failed step: {operations:?}"
    );
    assert!(
        !operations.iter().any(|operation| operation == "delete_snapshot"),
        "the retry skipped the already-finished snapshot step: {operations:?}"
    );
}

#[test]
fn a_failed_begin_deletion_leaves_no_job_and_no_deleted_row() {
    let volume = TempVolume::new("delete-begin-fail");
    let layout = volume.layout();
    key::create_key(&layout, "key-1", "aes-256-gcm-envelope").expect("create the key");
    let plane = RecordingControlPlane::default();
    plane.fail("begin_deletion");
    let error = delete::delete_user(&layout, &plane, "user-1").expect_err("the job cannot open");
    assert!(error.to_string().contains("injected begin_deletion failure"), "{error}");
    assert!(
        delete::read_job(&layout).expect("read the job").is_none(),
        "no job record exists before the control plane assigned one"
    );
    assert!(!plane.operations().iter().any(|operation| operation == "mark_deleted"));
}

#[test]
fn a_finished_job_is_not_run_again() {
    let volume = TempVolume::new("delete-idempotent");
    let layout = volume.layout();
    key::create_key(&layout, "key-1", "aes-256-gcm-envelope").expect("create the key");
    let plane = RecordingControlPlane::default();
    delete::delete_user(&layout, &plane, "user-1").expect("first run");

    plane.calls.borrow_mut().clear();
    let report = delete::delete_user(&layout, &plane, "user-1").expect("second run");
    assert!(report.is_deleted());
    assert!(
        plane.operations().is_empty(),
        "a finished deletion makes no provider call: {:?}",
        plane.operations()
    );
}

#[test]
fn a_deletion_job_for_another_user_is_refused() {
    let volume = TempVolume::new("delete-wrong-user");
    let layout = volume.layout();
    key::create_key(&layout, "key-1", "aes-256-gcm-envelope").expect("create the key");
    let plane = RecordingControlPlane::default();
    plane.fail("mark_deleted");
    delete::delete_user(&layout, &plane, "user-1").expect("the first run records a job");
    *plane.fail_on.borrow_mut() = None;

    let error = delete::delete_user(&layout, &plane, "user-2").expect_err("another user is refused");
    assert!(error.to_string().contains("belongs to user `user-1`"), "{error}");
}

#[test]
fn the_volume_deletion_asks_for_the_machine_first() {
    let volume = TempVolume::new("delete-machine-first");
    let layout = volume.layout();
    key::create_key(&layout, "key-1", "aes-256-gcm-envelope").expect("create the key");
    let plane = RecordingControlPlane::default();
    delete::delete_user(&layout, &plane, "user-1").expect("delete_user");

    let delete_volume = plane
        .calls
        .borrow()
        .iter()
        .find(|(operation, _)| operation == "delete_volume")
        .map(|(_, body)| body.clone())
        .expect("the volume delete was recorded");
    let value: serde_json::Value = serde_json::from_str(&delete_volume).expect("JSON");
    assert_eq!(value["destroy_machine_first"], serde_json::json!(true));
}

// ---------------------------------------------------------------------------
// Partial page deletion leaves no trace
// ---------------------------------------------------------------------------

#[test]
fn a_partially_deleted_page_is_absent_from_reindex_search_and_export() {
    let volume = TempVolume::new("partial-delete");
    let layout = volume.layout();
    // A token unique to the page being deleted, so a match can only come from
    // that page and its absence from a query proves the deletion reached the
    // index, not just the file.
    seed(&layout, "mem_1", "Kept", "kept body", 1_700_000_000);
    seed(
        &layout,
        "mem_2",
        "Gone",
        "zebracorn memory of the deleted page",
        1_700_000_100,
    );
    seed(&layout, "mem_3", "Also kept", "also kept body", 1_700_000_200);

    // Delete one page and then rebuild the index from the markdown.
    let id = horon_agent_memory_types::page::PageId::parse("mem_2").expect("valid id");
    MemoryWiki::open(layout.store_root())
        .expect("open")
        .delete_page(&id)
        .expect("delete");
    MemoryWiki::open(layout.store_root())
        .expect("open")
        .reindex()
        .expect("rebuild the index");

    // Reindex: the page is not a row.
    let wiki = MemoryWiki::open(layout.store_root()).expect("open");
    let ids: Vec<String> = wiki
        .list(&ListFilter {
            include_inactive: true,
            ..ListFilter::default()
        })
        .expect("list")
        .into_iter()
        .map(|row| row.id)
        .collect();
    let mut ids = ids;
    ids.sort();
    assert_eq!(ids, vec!["mem_1", "mem_3"], "only the kept pages remain");
    assert!(wiki.page(&id).expect("read").is_none());

    // Search: no hit for the deleted page's unique token.
    assert!(
        search_ids(&layout, "zebracorn").is_empty(),
        "the deleted page's token is not searchable"
    );
    assert_eq!(search_ids(&layout, "kept body").len(), 2);

    // Export: the deleted page is in no metadata line or page file.
    let archive_path = volume.path.join("export.tar");
    let report = archive::export(&layout, &archive_path).expect("export");
    assert_eq!(report.pages, 2);
    let metadata = read_tar_entry(&archive_path, METADATA_FILE).expect("metadata");
    let text = String::from_utf8(metadata).expect("UTF-8");
    assert!(!text.contains("mem_2"), "the deleted page has no metadata line");
    assert!(
        !text.contains("zebracorn"),
        "the deleted body is not in the export metadata"
    );
    assert!(
        read_tar_entry(&archive_path, "wiki/preferences/mem_2.md").is_none(),
        "the deleted page file is not in the tar"
    );
}

// ---------------------------------------------------------------------------
// The volume layout
// ---------------------------------------------------------------------------

#[test]
fn the_layout_keeps_lifecycle_state_outside_the_store() {
    let layout = VolumeLayout::for_home("/srv/tenant");
    assert_eq!(layout.home(), Path::new("/srv/tenant"));
    assert_eq!(layout.store_root(), Path::new("/srv/tenant/memory"));
    assert_eq!(layout.state_dir(), Path::new("/srv/tenant/volume-state"));
    assert_eq!(
        layout.deletion_job_path(),
        Path::new("/srv/tenant/volume-state/deletion-job.json")
    );
    assert_eq!(layout.fence_path(), Path::new("/srv/tenant/memory/.write-fence.json"));
    assert_eq!(layout.key_path(), Path::new("/srv/tenant/memory/key.json"));
    assert!(
        !layout.deletion_job_path().starts_with(layout.store_root()),
        "the deletion job must survive a wipe of the store"
    );
}

// ---------------------------------------------------------------------------
// Authenticated encryption of exported archives
// ---------------------------------------------------------------------------

struct MemoryKeyMap {
    keys: std::collections::HashMap<String, Vec<u8>>,
}

impl MemoryKeyMap {
    fn single(key_id: &str, bytes: [u8; 32]) -> Self {
        let mut keys = std::collections::HashMap::new();
        keys.insert(key_id.to_string(), bytes.to_vec());
        Self { keys }
    }

    fn empty() -> Self {
        Self {
            keys: std::collections::HashMap::new(),
        }
    }
}

impl key::KeyProvider for MemoryKeyMap {
    fn key_material(&self, key_id: &str) -> anyhow::Result<Option<key::KeyMaterial>> {
        Ok(self
            .keys
            .get(key_id)
            .map(|bytes| key::KeyMaterial::new(key_id, bytes.clone())))
    }
}

#[test]
fn encrypted_export_and_import_round_trip_seals_pages_and_metadata() {
    let source = TempVolume::new("enc-src");
    let layout = source.layout();
    seed(
        &layout,
        "mem_e1",
        "아침 회의",
        "아침 9시 이전 회의를 선호한다.",
        1_700_000_000,
    );
    seed(
        &layout,
        "mem_e2",
        "비밀 토큰 메모",
        "confidential-body-marker-42",
        1_700_000_100,
    );

    let key_bytes = [0x5Au8; 32];
    let provider = MemoryKeyMap::single("key-enc-1", key_bytes);
    let archive_path = source.path.join("encrypted.tar");
    let report = archive::export_with_key(&layout, &archive_path, &provider, "key-enc-1").expect("encrypted export");
    assert_eq!(report.pages, 2);
    assert!(report.page_bytes > 0);

    let manifest_bytes = read_tar_entry(&archive_path, MANIFEST_FILE).expect("manifest");
    let manifest = Manifest::from_json(&manifest_bytes).expect("manifest parses");
    assert_eq!(manifest.encryption, manifest::ENCRYPTION_AES_256_GCM);
    assert_eq!(manifest.key_id.as_deref(), Some("key-enc-1"));

    // Neither page bodies nor metadata titles appear in plaintext in the tar.
    let sealed_meta = read_tar_entry(&archive_path, METADATA_FILE).expect("metadata");
    assert!(
        !String::from_utf8_lossy(&sealed_meta).contains("비밀 토큰 메모"),
        "sealed metadata must not expose page titles"
    );
    let sealed_page = read_tar_entry(&archive_path, "wiki/preferences/mem_e2.md").expect("page entry");
    assert!(
        !String::from_utf8_lossy(&sealed_page).contains("confidential-body-marker-42"),
        "sealed page must not expose plaintext body"
    );

    // Unkeyed import refuses cleanly.
    let no_key_dst = TempVolume::new("enc-nokey");
    let err = archive::import(&no_key_dst.layout(), &archive_path).expect_err("unkeyed import fails");
    assert!(err.to_string().contains("encrypted with"), "{err}");
    assert!(!no_key_dst.layout().store_root().exists());

    // Keyed import restores pages and rebuilds the index.
    let dst = TempVolume::new("enc-dst");
    let dst_layout = dst.layout();
    let imported = archive::import_with_key(&dst_layout, &archive_path, &provider).expect("keyed import");
    assert_eq!(imported.pages, 2);
    assert_eq!(search_ids(&dst_layout, "confidential-body-marker-42"), vec!["mem_e2"]);
    let body = MemoryWiki::open(dst_layout.store_root())
        .expect("open")
        .stored_body(&horon_agent_memory_types::page::PageId::parse("mem_e2").expect("id"))
        .expect("read")
        .expect("present");
    assert_eq!(body, "confidential-body-marker-42");
}

#[test]
fn encrypted_import_rejects_wrong_revoked_or_tampered_archives_without_writing() {
    let source = TempVolume::new("enc-fail-src");
    let layout = source.layout();
    seed(&layout, "mem_f1", "Secret", "top-secret-body", 1_700_000_000);
    let provider = MemoryKeyMap::single("key-enc-2", [0x11u8; 32]);
    let archive_path = source.path.join("encrypted.tar");
    archive::export_with_key(&layout, &archive_path, &provider, "key-enc-2").expect("export");

    // Wrong key fails authentication before writing any page.
    let wrong_dst = TempVolume::new("enc-wrong");
    let wrong = MemoryKeyMap::single("key-enc-2", [0x22u8; 32]);
    let err = archive::import_with_key(&wrong_dst.layout(), &archive_path, &wrong).expect_err("wrong key rejected");
    assert!(!err.to_string().is_empty());
    assert!(!wrong_dst.layout().store_root().join("wiki").exists());

    // Missing/revoked key fails closed.
    let missing_dst = TempVolume::new("enc-missing");
    let empty = MemoryKeyMap::empty();
    let err = archive::import_with_key(&missing_dst.layout(), &archive_path, &empty).expect_err("missing key rejected");
    assert!(err.to_string().contains("key-enc-2"), "{err}");
    assert!(!missing_dst.layout().store_root().join("wiki").exists());

    // Tampered encrypted page bytes fail digest/AEAD verification.
    let mut sealed_page = read_tar_entry(&archive_path, "wiki/preferences/mem_f1.md").expect("page");
    let last = sealed_page.len() - 1;
    sealed_page[last] ^= 0xFF;
    let metadata = read_tar_entry(&archive_path, METADATA_FILE).expect("meta");
    let manifest = read_tar_entry(&archive_path, MANIFEST_FILE).expect("manifest");
    let tampered_tar = build_tar(&[
        ("wiki/preferences/mem_f1.md", sealed_page.as_slice()),
        (METADATA_FILE, metadata.as_slice()),
        (MANIFEST_FILE, manifest.as_slice()),
    ]);
    let tampered_path = write_archive(&source.path, "tampered.tar", &tampered_tar);
    let tampered_dst = TempVolume::new("enc-tampered");
    assert!(archive::import_with_key(&tampered_dst.layout(), &tampered_path, &provider).is_err());
    assert!(!tampered_dst.layout().store_root().join("wiki").exists());

    // Tampered sealed metadata fails GCM tag check before writing any page.
    let valid_page = read_tar_entry(&archive_path, "wiki/preferences/mem_f1.md").expect("page");
    let mut tampered_meta = metadata.clone();
    let meta_last = tampered_meta.len() - 1;
    tampered_meta[meta_last] ^= 0xFF;
    let tampered_meta_tar = build_tar(&[
        ("wiki/preferences/mem_f1.md", valid_page.as_slice()),
        (METADATA_FILE, tampered_meta.as_slice()),
        (MANIFEST_FILE, manifest.as_slice()),
    ]);
    let tampered_meta_path = write_archive(&source.path, "tampered-meta.tar", &tampered_meta_tar);
    let tampered_meta_dst = TempVolume::new("enc-tampered-meta");
    let err = archive::import_with_key(&tampered_meta_dst.layout(), &tampered_meta_path, &provider)
        .expect_err("tampered metadata rejected");
    assert!(err.to_string().contains("authentication failed"), "{err}");
    assert!(!tampered_meta_dst.layout().store_root().join("wiki").exists());

    // Revoked local key record prevents exporting under that key_id.
    key::create_key(&layout, "key-enc-2", "aes-256-gcm").expect("create key record");
    key::revoke_key(&layout).expect("revoke key");
    let revoked_archive = source.path.join("revoked-export.tar");
    let err = archive::export_with_key(&layout, &revoked_archive, &provider, "key-enc-2")
        .expect_err("revoked key export refused");
    assert!(err.to_string().contains("revoked"), "{err}");
}
