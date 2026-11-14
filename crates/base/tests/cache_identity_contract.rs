//! Cache lineage identity: a root, its children and grandchildren keep distinct session
//! ids while sharing one lineage root that survives save and reload.
//!
//! `HORON_HOME` is process-global, so every case here takes `HOME_LOCK` and
//! installs its own home through `TempHome`. Without the lock, one case's
//! `remove_dir_all` deletes another case's home mid-save, which surfaces as
//! `store root is not usable: No such file or directory` (issue #287).

use horon_agent_base::session::Session;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Serializes the cases, because they all write the same process variable.
static HOME_LOCK: Mutex<()> = Mutex::new(());

/// One isolated `HORON_HOME`, installed for the life of the guard.
struct TempHome {
    path: std::path::PathBuf,
    _lock: MutexGuard<'static, ()>,
}

impl TempHome {
    fn new() -> Self {
        // A panicking case leaves the lock poisoned; the next case still needs
        // its own home, and the state it observes is only the temp path.
        let lock = HOME_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let path = std::env::temp_dir().join(format!(
            "cache-identity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("the clock to be past the epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("the temp home");
        unsafe { std::env::set_var("HORON_HOME", &path) };
        Self { path, _lock: lock }
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn root_child_and_grandchild_share_one_lineage_root() {
    let _home = TempHome::new();
    let mut root = Session::create(None, Some("root".into()));
    let root_id = root.cache_root();
    assert!(uuid_like(&root_id), "{root_id}");

    let mut child = Session::create(Some(root.id.clone()), Some("child".into()));
    child.inherit_cache_root(&root);
    let child_root = child.cache_root();
    assert_eq!(child_root, root_id);
    assert_ne!(child.id, root.id);

    let mut grandchild = Session::create(Some(child.id.clone()), Some("grandchild".into()));
    grandchild.inherit_cache_root(&child);
    assert_eq!(grandchild.cache_root(), root_id);
    assert_ne!(grandchild.id, child.id);
    assert_ne!(grandchild.id, root.id);
}

#[test]
fn lineage_root_survives_save_and_reload() {
    let _home = TempHome::new();
    let mut root = Session::create(None, Some("persisted".into()));
    let expected = root.cache_root();
    root.save().unwrap();
    let loaded = Session::load(&root.id).unwrap();
    assert_eq!(loaded.cache_root_id.as_deref(), Some(expected.as_str()));

    let mut child = Session::create(Some(root.id.clone()), Some("persisted child".into()));
    child.inherit_cache_root(&root);
    child.save().unwrap();
    let loaded_child = Session::load(&child.id).unwrap();
    assert_eq!(loaded_child.cache_root_id.as_deref(), Some(expected.as_str()));
    assert_ne!(loaded_child.id, loaded.id);
}

#[test]
fn session_without_a_stored_root_derives_one_from_its_own_id() {
    let _home = TempHome::new();
    let mut session = Session::create(None, Some("legacy".into()));
    session.cache_root_id = None;
    let first = session.cache_root();
    assert!(uuid_like(&first), "{first}");
    // Stable: a second call returns the remembered value, not a new one.
    assert_eq!(session.cache_root(), first);
    assert_eq!(session.cache_root_id.as_deref(), Some(first.as_str()));
}

#[test]
fn unreadable_parent_never_shares_a_root_with_the_parent() {
    let _home = TempHome::new();
    let mut orphan = Session::create(Some("missing-parent-session".into()), Some("orphan".into()));
    let orphan_root = orphan.cache_root();
    assert!(
        uuid_like(&orphan_root),
        "an orphan gets a root of its own: {orphan_root}"
    );
    assert_ne!(orphan_root, "missing-parent-session");
}

fn uuid_like(value: &str) -> bool {
    value.len() == 36
        && value.chars().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}
