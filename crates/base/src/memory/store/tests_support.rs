//! Shared helpers for the memory store tests.

use horon_agent_memory_types::MemoryCategory;
use horon_agent_memory_types::page::{MemoryPage, PageId, PageKind};
use horon_agent_time::Timestamp;
use std::path::{Path, PathBuf};

/// A unique directory under the system temp directory, removed on drop.
pub struct TempRoot {
    path: PathBuf,
}

impl TempRoot {
    /// Create a fresh root named after `label`.
    pub fn new(label: &str) -> Self {
        let nonce: u64 = rand::random();
        let path = std::env::temp_dir().join(format!("horon-wiki-{label}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create the temp root");
        Self { path }
    }

    /// The directory path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.path) {
            eprintln!("warning: cannot clean up {}: {error}", self.path.display());
        }
    }
}

/// An instant `seconds` after the Unix epoch.
pub fn at(seconds: i64) -> Timestamp {
    Timestamp::from_timestamp_millis(seconds * 1_000).expect("timestamp in range")
}

/// A page with the defaults of [`MemoryPage::draft`], for tests.
pub fn page(id: &str, kind: PageKind, category: MemoryCategory, title: &str, body: &str, seconds: i64) -> MemoryPage {
    MemoryPage::draft(
        PageId::parse(id).expect("valid page id"),
        kind,
        category,
        title,
        body,
        at(seconds),
    )
}

/// A preference page.
pub fn preference(id: &str, title: &str, body: &str, seconds: i64) -> MemoryPage {
    page(
        id,
        PageKind::Preference,
        MemoryCategory::Preference,
        title,
        body,
        seconds,
    )
}

/// A fact page.
pub fn fact(id: &str, title: &str, body: &str, seconds: i64) -> MemoryPage {
    page(id, PageKind::Fact, MemoryCategory::Fact, title, body, seconds)
}

/// A dated event page.
pub fn event(id: &str, title: &str, body: &str, seconds: i64) -> MemoryPage {
    page(id, PageKind::Event, MemoryCategory::Fact, title, body, seconds)
}
