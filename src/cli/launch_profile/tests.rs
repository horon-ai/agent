use super::*;

#[test]
fn two_profiles_never_share_a_home_or_endpoint_directory() {
    let base = tempfile::tempdir().unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let work = resolve("work", base.path(), runtime.path()).unwrap();
    let personal = resolve("personal", base.path(), runtime.path()).unwrap();
    assert_eq!(work.home, base.path().join("profiles").join("work"));
    assert_ne!(work.home, personal.home);
    assert_ne!(work.runtime_dir, personal.runtime_dir);
    assert!(work.runtime_dir.starts_with(runtime.path()));
}

#[test]
fn names_that_could_leave_the_profile_root_are_refused() {
    for name in [
        "",
        "../work",
        "work/home",
        "Work",
        "-work",
        "work-",
        "wörk",
        &"a".repeat(33),
    ] {
        assert!(validate_name(name).is_err(), "{name:?}");
    }
}

#[test]
fn the_description_names_the_profile_and_a_safe_alias() {
    let base = tempfile::tempdir().unwrap();
    let profile = resolve("work", base.path(), base.path()).unwrap();
    let text = describe(&profile);
    assert!(text.starts_with("profile: work\n"));
    assert!(text.ends_with("alias horon-work='horon --profile work'"));
}
