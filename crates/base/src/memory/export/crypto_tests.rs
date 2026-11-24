//! Authenticated encryption (`AES-256-GCM`) unit tests for memory exports.

use super::VolumeLayout;
use super::archive::{self, ImportReport};
use super::key;
use super::manifest::{self, MANIFEST_FILE, METADATA_FILE, Manifest};
use crate::memory::store::{MemoryWiki, SearchQuery};
use horon_agent_memory_types::MemoryCategory;
use horon_agent_memory_types::page::PageKind;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

struct TempVolume {
    path: PathBuf,
}

impl TempVolume {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("horon-volume-aead-{label}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create the temp volume");
        Self { path }
    }

    fn layout(&self) -> VolumeLayout {
        VolumeLayout::for_home(&self.path)
    }
}

impl Drop for TempVolume {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.path));
    }
}

struct MemoryKeyMap {
    keys: HashMap<String, Vec<u8>>,
}

impl MemoryKeyMap {
    fn single(key_id: &str, bytes: [u8; 32]) -> Self {
        let mut keys = HashMap::new();
        keys.insert(key_id.to_string(), bytes.to_vec());
        Self { keys }
    }

    fn empty() -> Self {
        Self { keys: HashMap::new() }
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

fn seed(layout: &VolumeLayout, id: &str, title: &str, body: &str, seconds: i64) {
    let mut wiki = MemoryWiki::open(layout.store_root()).expect("open the wiki");
    let page_id = horon_agent_memory_types::page::PageId::parse(id).expect("valid page id");
    let now = horon_agent_time::Timestamp::from_timestamp_millis(seconds * 1_000).expect("timestamp");
    let mut page = horon_agent_memory_types::MemoryPage::draft(
        page_id,
        PageKind::Preference,
        MemoryCategory::Preference,
        title,
        body,
        now,
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

fn read_tar_entry(archive: &Path, name: &str) -> Option<Vec<u8>> {
    let file = std::fs::File::open(archive).expect("open archive");
    let mut reader = tar::Archive::new(std::io::BufReader::new(file));
    for entry in reader.entries().expect("entries") {
        let mut entry = entry.expect("entry");
        if entry.path().expect("path").display().to_string() == name {
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut bytes).expect("read entry");
            return Some(bytes);
        }
    }
    None
}

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
            .expect("append tar entry");
    }
    builder.into_inner().expect("finish tar")
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
    let imported: ImportReport = archive::import_with_key(&dst_layout, &archive_path, &provider).expect("keyed import");
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
    assert!(archive::import_with_key(&wrong_dst.layout(), &archive_path, &wrong).is_err());
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
    let tampered_path = source.path.join("tampered.tar");
    std::fs::write(&tampered_path, &tampered_tar).expect("write tampered");
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
    let tampered_meta_path = source.path.join("tampered-meta.tar");
    std::fs::write(&tampered_meta_path, &tampered_meta_tar).expect("write tampered meta");
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

#[test]
fn resolve_key_material_rejects_dual_sources_and_trims_key_id_consistently() {
    let source = TempVolume::new("crypto-trim-dual");
    let layout = source.layout();
    seed(&layout, "mem_trim", "Trim", "trimmed-key-secret", 1_700_000_000);
    let key_file = source.path.join("key.txt");
    std::fs::write(
        &key_file,
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    )
    .expect("write key");

    let dual = super::crypto::resolve_key_material("key-1", Some("PATH"), Some(&key_file))
        .expect_err("providing both env and file sources must fail");
    assert!(dual.to_string().contains("not both"), "{dual}");

    let material =
        super::crypto::resolve_key_material("  padded-key-id  ", None, Some(&key_file)).expect("resolve trimmed");
    assert_eq!(material.key_id(), "padded-key-id");
    let provider = material
        .consume(|bytes| super::crypto::StaticKeyProvider::new("  padded-key-id  ", bytes.to_vec()))
        .expect("provider");
    let archive_path = source.path.join("padded.tar");
    archive::export_with_key(&layout, &archive_path, &provider, " padded-key-id ").expect("export");

    let dst = TempVolume::new("crypto-trim-dst");
    let imported = archive::import_with_key(&dst.layout(), &archive_path, &provider).expect("import");
    assert_eq!(imported.pages, 1);
}
