//! Tests for the spawn-tree read rule.

use super::{ensure_visible, ensure_visible_from};
use crate::session::Session;

/// A sandboxed `HORON_HOME`, so a test never touches the real session store.
struct Sandbox {
    _dir: tempfile::TempDir,
    _guard: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
}

impl Sandbox {
    fn new() -> Self {
        let guard = crate::storage::lock_test_env();
        let dir = tempfile::tempdir().expect("sandbox home");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", dir.path());
        Self {
            _dir: dir,
            _guard: guard,
            previous,
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(previous) => crate::env::set_var("HORON_HOME", previous),
            None => crate::env::remove_var("HORON_HOME"),
        }
    }
}

fn saved_session(parent: Option<String>) -> Session {
    let mut session = Session::create(parent, None);
    session.save().expect("a session saves");
    session
}

#[test]
fn a_session_reads_itself_descendants_and_ancestors() {
    let _sandbox = Sandbox::new();
    let root = saved_session(None);
    let child = saved_session(Some(root.id.clone()));
    let grandchild = saved_session(Some(child.id.clone()));
    let sibling = saved_session(Some(root.id.clone()));

    ensure_visible(&root.id, &grandchild).expect("a descendant is visible");
    ensure_visible(&grandchild.id, &root).expect("an ancestor is visible");
    ensure_visible(&child.id, &child).expect("itself is visible");
    ensure_visible(&child.id, &sibling).expect("a sibling is visible");
}

#[test]
fn an_unrelated_tree_is_refused() {
    let _sandbox = Sandbox::new();
    let mine = saved_session(None);
    let theirs = saved_session(None);

    let error = ensure_visible(&mine.id, &theirs).expect_err("an unrelated tree is refused");
    assert!(
        error.to_string().contains("outside this session's spawn tree"),
        "the refusal names the rule: {error}"
    );
}

#[test]
fn an_in_memory_caller_uses_the_same_rule() {
    let _sandbox = Sandbox::new();
    let root = saved_session(None);
    let child = saved_session(Some(root.id.clone()));
    let unrelated = saved_session(None);

    ensure_visible_from(&root, &child).expect("a child is visible to its parent");
    let error = ensure_visible_from(&root, &unrelated).expect_err("an unrelated tree is refused");
    assert!(error.to_string().contains("outside this session's spawn tree"));
}

#[test]
fn an_unsaved_caller_is_still_an_ancestor() {
    let _sandbox = Sandbox::new();
    let caller = Session::create(None, None);
    let child = saved_session(Some(caller.id.clone()));

    ensure_visible_from(&caller, &child).expect("an unsaved caller still names its spawn tree");
}

#[test]
fn a_missing_ancestor_chain_is_an_error() {
    let _sandbox = Sandbox::new();
    let caller = saved_session(None);
    let orphan = Session::create(Some("session_missing_parent".to_string()), None);

    let error = ensure_visible(&caller.id, &orphan).expect_err("an unreadable chain is refused");
    assert!(
        error.to_string().contains("session_missing_parent"),
        "the error names the unreadable ancestor: {error}"
    );
}
