//! Inbox drops: a complete package directory placed at
//! `$HORON_HOME/packages/inbox/<id>/` is installed exactly like a CLI install.
//!
//! [`PackageStore::sync_inbox`] visits the inbox in name order and hands every
//! usable entry to [`PackageStore::install`] with its directory name as the
//! expected ID. The dropped directory is left exactly where the operator put
//! it: the catalog records the ID it produced, so a later pass reports the
//! entry as already installed (or as a removed ID) instead of installing it
//! again. Nothing is deleted, so a source placed at the same path while a pass
//! runs is never lost. Entries that cannot be installed are reported per entry
//! and left in place. Nothing here starts a package executable.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use horon_agent_platform::fs::is_path_link;

use super::catalog::{InstallRequest, PackageRecord, PackageScope, PackageStore};
use super::manifest::is_package_id;

/// One entry of the inbox directory as a sync pass saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxEntry {
    /// Directory name under `inbox/`.
    pub name: String,
    pub outcome: InboxOutcome,
}

/// What one inbox entry did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboxOutcome {
    /// Published in the catalog. The inbox directory is left in place.
    Installed(PackageRecord),
    /// Left in the inbox because it cannot be installed as it is.
    Skipped(String),
}

/// The result of one inbox pass, in entry-name order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InboxReport {
    pub entries: Vec<InboxEntry>,
}

impl InboxReport {
    /// Records published by this pass, in entry-name order.
    pub fn installed(&self) -> impl Iterator<Item = &PackageRecord> + '_ {
        self.entries.iter().filter_map(|entry| match &entry.outcome {
            InboxOutcome::Installed(record) => Some(record),
            InboxOutcome::Skipped(_) => None,
        })
    }
}

impl PackageStore {
    /// Install every usable entry of `inbox/` into the global scope.
    ///
    /// Each candidate is installed through [`PackageStore::install`] with its
    /// directory name as the expected ID, so the validation, the published copy
    /// and the catalog update are the same ones a CLI install uses. A directory
    /// whose name is not a package ID, a link or a file, a package that does not
    /// validate, a package that declares another ID, and an ID the catalog
    /// already records (installed, disabled or removed) are skipped and
    /// reported without touching the existing record.
    ///
    /// The inbox directory itself must be a real directory: a link or a junction
    /// there would make the scan look somewhere the operator did not point it,
    /// so it is refused before anything is read.
    pub fn sync_inbox(&self) -> Result<InboxReport> {
        let inbox = self.inbox_dir();
        let root = match fs::symlink_metadata(&inbox) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(InboxReport::default()),
            Err(error) => return Err(error).with_context(|| format!("inspect {}", inbox.display())),
        };
        if is_path_link(&root) {
            bail!("the package inbox must not be a link: {}", inbox.display());
        }
        if !root.is_dir() {
            bail!("the package inbox is not a directory: {}", inbox.display());
        }
        horon_agent_perf_counters::record_package_dir_scan();
        let mut candidates = Vec::new();
        for entry in fs::read_dir(&inbox).with_context(|| format!("read {}", inbox.display()))? {
            let entry = entry.with_context(|| format!("read {}", inbox.display()))?;
            candidates.push((entry.file_name(), entry.path()));
        }
        candidates.sort();

        let mut report = InboxReport::default();
        for (file_name, path) in candidates {
            let name = file_name.to_string_lossy().into_owned();
            let outcome = self.sync_entry(file_name.to_str(), &path);
            report.entries.push(InboxEntry { name, outcome });
        }
        Ok(report)
    }

    fn sync_entry(&self, utf8_name: Option<&str>, path: &Path) -> InboxOutcome {
        let Some(name) = utf8_name else {
            return InboxOutcome::Skipped("entry name is not valid UTF-8".to_owned());
        };
        if !is_package_id(name) {
            return InboxOutcome::Skipped(
                "entry name is not a package ID; temporary and incomplete entries are ignored".to_owned(),
            );
        }
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) => return InboxOutcome::Skipped(format!("cannot inspect the entry: {error}")),
        };
        if is_path_link(&metadata) {
            return InboxOutcome::Skipped("links are not allowed in the inbox".to_owned());
        }
        if !metadata.is_dir() {
            return InboxOutcome::Skipped("entry is not a directory".to_owned());
        }
        // The expected ID is enforced inside `install`, on the same validation
        // that publishes the copy, so a directory cannot install a manifest
        // that declares another ID even if the bytes change in between. The
        // catalog's record of the ID, including a removed one, is what keeps a
        // later pass from installing the same directory twice.
        match self.install(InstallRequest::new(path, PackageScope::Global).expecting(name)) {
            Ok(record) => InboxOutcome::Installed(record),
            Err(error) => InboxOutcome::Skipped(format!("{error:#}")),
        }
    }
}
