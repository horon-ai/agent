//! Cache lineage identity: a root, its children and grandchildren keep distinct session
//! ids while sharing one lineage root that survives save and reload.

use horon_agent_base::session::Session;

fn temp_home() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cache-identity-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn root_child_and_grandchild_share_one_lineage_root() {
    let home = temp_home();
    unsafe { std::env::set_var("HORON_HOME", &home) };
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

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn lineage_root_survives_save_and_reload() {
    let home = temp_home();
    unsafe { std::env::set_var("HORON_HOME", &home) };
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

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn session_without_a_stored_root_derives_one_from_its_own_id() {
    let home = temp_home();
    unsafe { std::env::set_var("HORON_HOME", &home) };
    let mut session = Session::create(None, Some("legacy".into()));
    session.cache_root_id = None;
    let first = session.cache_root();
    assert!(uuid_like(&first), "{first}");
    // Stable: a second call returns the remembered value, not a new one.
    assert_eq!(session.cache_root(), first);
    assert_eq!(session.cache_root_id.as_deref(), Some(first.as_str()));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn unreadable_parent_never_shares_a_root_with_the_parent() {
    let home = temp_home();
    unsafe { std::env::set_var("HORON_HOME", &home) };
    let mut orphan = Session::create(Some("missing-parent-session".into()), Some("orphan".into()));
    orphan.cache_root_id = None;
    let orphan_root = orphan.cache_root();
    assert!(uuid_like(&orphan_root), "{orphan_root}");
    // A different session with a different missing parent gets a different root.
    let mut other = Session::create(Some("another-missing-parent".into()), Some("other".into()));
    other.cache_root_id = None;
    assert_ne!(other.cache_root(), orphan_root);
    let _ = std::fs::remove_dir_all(&home);
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
