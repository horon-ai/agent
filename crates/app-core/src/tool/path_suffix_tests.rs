use super::*;
use std::time::Duration;

const BUDGET: Duration = Duration::from_secs(5);

#[test]
fn resolves_a_unique_path_suffix() {
    let temp = tempfile::tempdir().expect("tempdir");
    let nested = temp.path().join("crates/k/src");
    std::fs::create_dir_all(&nested).expect("mkdir");
    std::fs::write(nested.join("x.rs"), "fn main() {}\n").expect("write");

    let found = unique_suffix_match(temp.path(), "src/x.rs", BUDGET).expect("unique suffix");
    assert_eq!(found, nested.join("x.rs"));
    assert_eq!(relative_display(temp.path(), &found), "crates/k/src/x.rs");
}

#[test]
fn refuses_an_ambiguous_suffix() {
    let temp = tempfile::tempdir().expect("tempdir");
    for dir in ["a", "b"] {
        let nested = temp.path().join(dir).join("src");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::write(nested.join("x.rs"), "fn main() {}\n").expect("write");
    }

    assert_eq!(unique_suffix_match(temp.path(), "src/x.rs", BUDGET), None);
}

#[test]
fn matches_whole_components_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(temp.path().join("mysrc")).expect("mkdir");
    std::fs::write(temp.path().join("mysrc/x.rs"), "fn main() {}\n").expect("write");

    assert_eq!(unique_suffix_match(temp.path(), "src/x.rs", BUDGET), None);
    assert!(unique_suffix_match(temp.path(), "mysrc/x.rs", BUDGET).is_some());
}

#[test]
fn normalizes_dot_prefix_and_backslashes() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(temp.path().join("src")).expect("mkdir");
    std::fs::write(temp.path().join("src/x.rs"), "fn main() {}\n").expect("write");

    assert!(unique_suffix_match(temp.path(), "./src/x.rs", BUDGET).is_some());
    assert!(unique_suffix_match(temp.path(), "src\\x.rs", BUDGET).is_some());
    assert!(unique_suffix_match(temp.path(), "src/x.rs/", BUDGET).is_some());
}

#[test]
fn finds_hidden_files_but_skips_git() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(temp.path().join(".hidden")).expect("mkdir");
    std::fs::write(temp.path().join(".hidden/secret.rs"), "fn main() {}\n").expect("write");
    std::fs::create_dir_all(temp.path().join(".git/objects/ab")).expect("mkdir");
    std::fs::write(temp.path().join(".git/objects/ab/secret.rs"), "fn main() {}\n").expect("write");

    let found = unique_suffix_match(temp.path(), "secret.rs", BUDGET).expect("hidden file is searched");
    assert_eq!(found, temp.path().join(".hidden/secret.rs"));
}
