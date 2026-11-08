//! The journal directory entry is flushed when, and only when, an open creates it.
//!
//! The first record confirms `runtime.jsonl` by flushing its own directory, but
//! a power loss before that directory's entry reaches its parent loses the
//! directory and the confirmed journal with it. These tests pin that every
//! level an open creates gets its parent flushed, that an existing directory
//! costs no flush, and that a failed flush is never reported as an open.

use super::*;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

fn counting_hook(seen: &Arc<Mutex<Vec<JournalIoPoint>>>) -> JournalHook {
    let seen = Arc::clone(seen);
    hook(move |point| {
        seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(point);
        Ok(())
    })
}

fn directory_syncs(seen: &Arc<Mutex<Vec<JournalIoPoint>>>) -> usize {
    seen.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .filter(|point| **point == JournalIoPoint::DirectoryEntrySync)
        .count()
}

fn open_observed(root: &Path, session: &str, seen: &Arc<Mutex<Vec<JournalIoPoint>>>) -> DurableRuntimeStore {
    let options = JournalOptions {
        limits: JournalLimits::default(),
        hooks: Some(counting_hook(seen)),
    };
    DurableRuntimeStore::open_in_with(root, session, options).expect("open journal")
}

#[test]
fn directory_entry_sync_flushes_every_level_the_open_created() {
    let temp = TempDir::new().expect("temp dir");
    // `runtime/` and `nested/` do not exist yet, and neither does the session
    // directory, so the open creates three levels and flushes three parents.
    let root = temp.path().join("runtime").join("nested");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let store = open_observed(&root, "ses_dir_new", &seen);

    assert!(store.directory().is_dir());
    assert_eq!(directory_syncs(&seen), 3);
}

#[test]
fn directory_entry_sync_skips_an_existing_directory() {
    let temp = TempDir::new().expect("temp dir");
    let first = Arc::new(Mutex::new(Vec::new()));
    let mut store = open_observed(temp.path(), "ses_dir_existing", &first);
    assert_eq!(directory_syncs(&first), 1, "only the new session directory is created");
    store
        .append(&new_id(), accepted("after open"))
        .expect("append into an existing directory");
    assert_eq!(
        directory_syncs(&first),
        1,
        "an append into an existing directory adds no entry flush"
    );
    drop(store);

    let second = Arc::new(Mutex::new(Vec::new()));
    let _reopened = open_observed(temp.path(), "ses_dir_existing", &second);
    assert_eq!(
        directory_syncs(&second),
        0,
        "reopening an existing journal directory flushes nothing"
    );
}

#[test]
fn directory_entry_sync_failure_fails_the_open() {
    let temp = TempDir::new().expect("temp dir");
    let options = JournalOptions {
        limits: JournalLimits::default(),
        hooks: Some(hook(|point| {
            if point == JournalIoPoint::DirectoryEntrySync {
                return Err(std::io::Error::other("injected directory sync failure"));
            }
            Ok(())
        })),
    };
    let Err(error) = DurableRuntimeStore::open_in_with(temp.path(), "ses_dir_fail", options) else {
        panic!("an unconfirmed directory entry must not be reported as an open journal");
    };
    assert_eq!(error.kind(), JournalErrorKind::Io);
}

fn accepted(content: &str) -> Payload {
    Payload::InputAccepted(InputAccepted {
        client_id: Some("client-1".to_string()),
        content: content.to_string(),
        images: Vec::new(),
        urgent: false,
        source: "user".to_string(),
    })
}
