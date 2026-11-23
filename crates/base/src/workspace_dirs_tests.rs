use super::*;
use crate::message::{ContentBlock, Role};
use crate::session::Session;

fn session() -> Session {
    Session::create_with_id("workspace_dirs_test".to_string(), None, None)
}

fn tempdir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("horon-workspace-dirs-")
        .tempdir()
        .expect("temp dir");
    let path = std::fs::canonicalize(dir.path()).expect("canonical temp dir");
    (dir, path)
}

#[test]
fn aliases_of_one_directory_collapse_to_the_canonical_entry() {
    let (_temp, root) = tempdir();
    let library = root.join("library");
    std::fs::create_dir(&library).expect("library dir");
    let canonical = std::fs::canonicalize(&library).expect("canonical library");

    let mut session = session();
    assert!(
        session
            .add_additional_dir(library.to_str().expect("utf-8 path"))
            .expect("add")
    );
    assert_eq!(
        session.additional_dirs(),
        vec![canonical.to_string_lossy().into_owned()]
    );

    // A trailing separator, a `.` component, a relative path and a symlink are
    // all the same directory; none of them adds a second entry.
    let with_slash = format!("{}/", canonical.to_string_lossy());
    assert!(!session.add_additional_dir(&with_slash).expect("add alias"));
    let with_dot = format!("{}/.", canonical.to_string_lossy());
    assert!(!session.add_additional_dir(&with_dot).expect("add dot alias"));
    #[cfg(unix)]
    {
        let link = root.join("library-link");
        std::os::unix::fs::symlink(&canonical, &link).expect("symlink");
        assert!(
            !session
                .add_additional_dir(link.to_str().expect("utf-8 path"))
                .expect("add symlink")
        );
    }
    assert_eq!(
        session.additional_dirs(),
        vec![canonical.to_string_lossy().into_owned()]
    );
}

#[test]
fn several_roots_keep_their_addition_order() {
    let (_temp, root) = tempdir();
    let first = root.join("first");
    let second = root.join("second");
    std::fs::create_dir(&first).expect("first dir");
    std::fs::create_dir(&second).expect("second dir");

    let mut session = session();
    assert!(
        session
            .add_additional_dir(first.to_str().expect("utf-8"))
            .expect("add first")
    );
    assert!(
        session
            .add_additional_dir(second.to_str().expect("utf-8"))
            .expect("add second")
    );
    assert_eq!(
        session.additional_dirs(),
        vec![
            std::fs::canonicalize(&first)
                .expect("canonical first")
                .to_string_lossy()
                .into_owned(),
            std::fs::canonicalize(&second)
                .expect("canonical second")
                .to_string_lossy()
                .into_owned(),
        ]
    );
}

#[test]
fn a_missing_path_or_a_file_is_rejected_and_the_list_is_unchanged() {
    let (_temp, root) = tempdir();
    let file = root.join("notes.txt");
    std::fs::write(&file, b"hello").expect("file");

    let mut session = session();
    let missing = root.join("missing");
    assert_eq!(
        session.add_additional_dir(missing.to_str().expect("utf-8")),
        Err(DirError::NotFound(missing.to_string_lossy().into_owned()))
    );
    assert_eq!(
        session.add_additional_dir(file.to_str().expect("utf-8")),
        Err(DirError::NotADirectory(file.to_string_lossy().into_owned()))
    );
    assert_eq!(session.add_additional_dir("  "), Err(DirError::Empty));
    assert!(session.additional_dirs().is_empty());
}

#[test]
fn removal_changes_only_the_list_and_leaves_the_directory_alone() {
    let (_temp, root) = tempdir();
    let library = root.join("library");
    let tests = root.join("tests");
    std::fs::create_dir(&library).expect("library dir");
    std::fs::create_dir(&tests).expect("tests dir");

    let mut session = session();
    session
        .add_additional_dir(library.to_str().expect("utf-8"))
        .expect("add library");
    session
        .add_additional_dir(tests.to_str().expect("utf-8"))
        .expect("add tests");

    // Removing by the canonical path removes the entry under its other
    // spelling too, and the directory still holds whatever it held.
    std::fs::write(library.join("book.txt"), b"pages").expect("write file");
    assert!(session.remove_additional_dir(library.to_str().expect("utf-8")));
    assert_eq!(
        session.additional_dirs(),
        vec![
            std::fs::canonicalize(&tests)
                .expect("canonical tests")
                .to_string_lossy()
                .into_owned()
        ]
    );
    assert!(library.is_dir());
    assert_eq!(std::fs::read(library.join("book.txt")).expect("read file"), b"pages");

    // A second removal of the same root changes nothing.
    assert!(!session.remove_additional_dir(library.to_str().expect("utf-8")));
}

#[test]
fn a_removed_root_that_no_longer_exists_still_leaves_the_list() {
    let (_temp, root) = tempdir();
    let gone = root.join("gone");
    std::fs::create_dir(&gone).expect("gone dir");
    let stored = std::fs::canonicalize(&gone).expect("canonical gone");
    std::fs::remove_dir(&gone).expect("remove dir");

    let mut session = session();
    session.set_additional_dirs(vec![stored.to_string_lossy().into_owned()]);
    // The exact stored spelling stays removable after the directory is gone.
    assert!(session.remove_additional_dir(&stored.to_string_lossy()));
    assert!(session.additional_dirs().is_empty());
}

#[test]
fn a_list_the_session_context_already_carries_gets_a_reminder_after_a_change() {
    let (_temp, root) = tempdir();
    let library = root.join("library");
    std::fs::create_dir(&library).expect("library dir");
    let canonical = std::fs::canonicalize(&library).expect("canonical library");

    let mut session = session();
    session.working_dir = Some(root.to_string_lossy().into_owned());
    session
        .add_additional_dir(library.to_str().expect("utf-8"))
        .expect("add");
    assert!(session.ensure_initial_session_context_message());
    let context = session.active_context()[0].content_preview();
    assert!(context.contains("Additional directories:"), "{context}");
    assert!(context.contains(&canonical.to_string_lossy().into_owned()), "{context}");

    // The first real message freezes the context, so a later removal reaches
    // the model through the next prompt's one-time reminder instead.
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "start".to_string(),
            cache_control: None,
        }],
    );
    assert!(session.remove_additional_dir(library.to_str().expect("utf-8")));
    assert_eq!(
        session.take_pending_additional_dirs_reminder().as_deref(),
        Some("Additional directories: (none)")
    );
}

#[test]
fn canonical_roots_deduplicates_aliases_and_preserves_first_seen_order() {
    let (_temp, root) = tempdir();
    let first = root.join("first");
    let second = root.join("second");
    let third = root.join("third");
    std::fs::create_dir(&first).expect("first dir");
    std::fs::create_dir(&second).expect("second dir");
    std::fs::create_dir(&third).expect("third dir");

    let raw = vec![
        first.to_string_lossy().into_owned(),
        second.to_string_lossy().into_owned(),
        format!("{}/.", first.display()),
        format!("{}/../second", first.display()),
        format!("{}/", second.display()),
        third.to_string_lossy().into_owned(),
    ];
    let resolved = canonical_roots(&raw).expect("canonical roots");
    assert_eq!(
        resolved,
        vec![
            first.to_string_lossy().into_owned(),
            second.to_string_lossy().into_owned(),
            third.to_string_lossy().into_owned(),
        ]
    );
}

#[test]
fn a_removed_root_with_parent_components_still_leaves_the_list() {
    let (_temp, root) = tempdir();
    let a = root.join("a");
    let b = a.join("b");
    let c = a.join("c");
    std::fs::create_dir_all(&b).expect("create a/b");
    std::fs::create_dir_all(&c).expect("create a/c");
    let stored = std::fs::canonicalize(&c)
        .expect("canonical a/c")
        .to_string_lossy()
        .into_owned();
    std::fs::remove_dir(&c).expect("remove a/c");

    let mut session = session();
    session.set_additional_dirs(vec![stored]);
    let parent_alias = format!("{}/../c", b.display());
    assert!(session.remove_additional_dir(&parent_alias));
    assert!(session.additional_dirs().is_empty());
}
