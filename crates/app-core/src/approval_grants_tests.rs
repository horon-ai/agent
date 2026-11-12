use super::*;
use std::sync::Mutex;

/// The grant file is one shared path per agent home, so tests that write it must
/// not interleave. The lock is process-wide and only exists under `cfg(test)`.
static GRANT_TEST_LOCK: Mutex<()> = Mutex::new(());

struct Sandbox {
    root: tempfile::TempDir,
    home: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Sandbox {
    fn new() -> Self {
        let lock = GRANT_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = tempfile::tempdir().expect("workspace");
        let home = tempfile::tempdir().expect("home");
        crate::env::set_var("HORON_HOME", home.path());
        crate::config::invalidate_config_cache();
        Self {
            root,
            home,
            _lock: lock,
        }
    }

    fn workspace(&self) -> std::path::PathBuf {
        self.root.path().canonicalize().expect("canonical workspace")
    }

    fn write(&self, relative: &str) -> std::path::PathBuf {
        let path = self.root.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        std::fs::write(&path, b"x").expect("write");
        path
    }

    fn grant_file(&self) -> std::path::PathBuf {
        self.home.path().join("approvals.json")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        crate::env::remove_var("HORON_HOME");
        crate::config::invalidate_config_cache();
    }
}

fn request(action: &str, input: serde_json::Value, working_dir: &Path, capability: &str) -> PermissionRequest {
    PermissionRequest {
        id: "req-1".to_string(),
        action: action.to_string(),
        description: String::new(),
        rationale: String::new(),
        urgency: crate::safety::Urgency::Normal,
        wait: false,
        created_at: Utc::now(),
        context: Some(serde_json::json!({
            "working_dir": working_dir.display().to_string(),
            "input": input,
            "grant_capability": capability,
        })),
        run_id: None,
        expires_at: None,
        default_on_timeout: crate::safety::ApprovalDefault::Deny,
    }
}

#[test]
fn a_folder_grant_covers_a_second_file_in_that_folder() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.workspace();
    let first = sandbox.write("notes/first.md");
    let second = sandbox.write("notes/second.md");

    let grant = grant_from_request_at(
        &request("write", serde_json::json!({ "file_path": first }), &workspace, "write"),
        GrantScope::Folder,
        Utc::now(),
        7,
    )
    .expect("a single-path write request can be granted");
    assert_eq!(
        grant.path,
        first.canonicalize().expect("canonical first").parent().expect("folder"),
        "a folder grant stores the directory, not the file it was answered for"
    );
    assert_eq!(grant.workspace, workspace);

    let now = Utc::now();
    let second_target = canonical_target(&second).expect("canonical second");
    assert!(
        covered_by(&[grant], &workspace, &[second_target], GrantCapability::Write, now),
        "a folder grant must cover another file in that folder"
    );
}

#[test]
fn a_path_grant_does_not_cover_a_sibling() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.workspace();
    let first = sandbox.write("notes/first.md");
    let second = sandbox.write("notes/second.md");

    let grant = grant_from_request_at(
        &request("edit", serde_json::json!({ "file_path": first }), &workspace, "write"),
        GrantScope::Path,
        Utc::now(),
        7,
    )
    .expect("grant");

    let now = Utc::now();
    assert!(covered_by(
        &[grant.clone()],
        &workspace,
        &[canonical_target(&first).expect("first")],
        GrantCapability::Write,
        now
    ));
    assert!(
        !covered_by(
            &[grant],
            &workspace,
            &[canonical_target(&second).expect("second")],
            GrantCapability::Write,
            now
        ),
        "a path grant names one file only"
    );
}

#[test]
fn a_grant_does_not_cross_workspace_roots() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.workspace();
    let other_workspace = tempfile::tempdir().expect("other workspace");
    let file = sandbox.write("notes/first.md");

    let grant = grant_from_request_at(
        &request("write", serde_json::json!({ "file_path": file }), &workspace, "write"),
        GrantScope::Folder,
        Utc::now(),
        7,
    )
    .expect("grant");

    let other_root = other_workspace.path().canonicalize().expect("canonical other");
    assert!(
        !covered_by(
            &[grant],
            &other_root,
            &[canonical_target(&file).expect("target")],
            GrantCapability::Write,
            Utc::now()
        ),
        "the same absolute path must not be covered from another workspace"
    );
}

#[test]
fn an_append_grant_does_not_authorise_a_write() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.workspace();
    let file = sandbox.write("log.txt");

    let grant = grant_from_request_at(
        &request("append", serde_json::json!({ "file_path": file }), &workspace, "append"),
        GrantScope::Path,
        Utc::now(),
        7,
    )
    .expect("grant");

    let target = canonical_target(&file).expect("target");
    let now = Utc::now();
    assert!(covered_by(
        std::slice::from_ref(&grant),
        &workspace,
        std::slice::from_ref(&target),
        GrantCapability::Append,
        now
    ));
    assert!(
        !covered_by(&[grant], &workspace, &[target], GrantCapability::Write, now),
        "an append answer must not authorise replacing the file"
    );
}

#[test]
fn a_write_grant_also_answers_an_append() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.workspace();
    let file = sandbox.write("log.txt");

    let grant = grant_from_request_at(
        &request("write", serde_json::json!({ "file_path": file }), &workspace, "write"),
        GrantScope::Path,
        Utc::now(),
        7,
    )
    .expect("grant");

    assert!(
        covered_by(
            &[grant],
            &workspace,
            &[canonical_target(&file).expect("target")],
            GrantCapability::Append,
            Utc::now()
        ),
        "replacing a file is a stronger answer than appending to it"
    );
}

#[test]
fn an_expired_grant_asks_again() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.workspace();
    let file = sandbox.write("notes/first.md");

    let then = Utc::now() - Duration::days(30);
    let grant = grant_from_request_at(
        &request("write", serde_json::json!({ "file_path": file }), &workspace, "write"),
        GrantScope::Path,
        then,
        7,
    )
    .expect("grant");
    assert!(grant.expires_at < Utc::now());

    assert!(!covered_by(
        &[grant],
        &workspace,
        &[canonical_target(&file).expect("target")],
        GrantCapability::Write,
        Utc::now()
    ));
}

#[test]
fn a_zero_ttl_stores_no_grant() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.workspace();
    let file = sandbox.write("notes/first.md");

    assert!(
        grant_from_request_at(
            &request("write", serde_json::json!({ "file_path": file }), &workspace, "write"),
            GrantScope::Path,
            Utc::now(),
            0
        )
        .is_none(),
        "a zero TTL turns grants off"
    );
}

#[test]
fn a_corrupt_grant_file_asks_again_instead_of_failing_open() {
    let sandbox = Sandbox::new();
    std::fs::write(sandbox.grant_file(), b"{ not json").expect("corrupt file");

    assert!(
        load_grants().is_empty(),
        "an unreadable grant file must yield no grants, so the next call asks"
    );
    assert!(
        sandbox.grant_file().exists(),
        "the user's file is left alone rather than deleted"
    );
}

#[test]
fn storing_a_grant_then_reading_it_back_finds_it() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.workspace();
    let file = sandbox.write("notes/first.md");
    let grant = grant_from_request_at(
        &request("write", serde_json::json!({ "file_path": file }), &workspace, "write"),
        GrantScope::Folder,
        Utc::now(),
        7,
    )
    .expect("grant");

    store(grant.clone()).expect("store");
    let read_back = load_grants();
    assert_eq!(read_back.len(), 1);
    assert_eq!(read_back[0].path, grant.path);
    assert_eq!(read_back[0].scope, GrantScope::Folder);

    // The second answer for the same place replaces the first instead of
    // growing the file, so answering twice cannot leave two conflicting rules.
    store(grant).expect("store again");
    assert_eq!(load_grants().len(), 1);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(sandbox.grant_file())
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "the grant file holds user answers and stays private");
    }
}

#[test]
fn a_request_naming_two_paths_is_approved_once_and_not_granted() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.workspace();
    let request = request(
        "apply_patch",
        serde_json::json!({ "patch_text": "*** Begin Patch\n*** Update File: a.txt\n*** Update File: b.txt\n*** End Patch" }),
        &workspace,
        "write",
    );
    assert!(
        grant_from_request_at(&request, GrantScope::Folder, Utc::now(), 7).is_none(),
        "one answer must not decide a place the user was never shown"
    );
}

#[test]
fn a_request_without_an_effect_capability_is_not_granted() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.workspace();
    let mut request = request("read", serde_json::json!({ "file_path": "a.txt" }), &workspace, "write");
    request.context = Some(serde_json::json!({
        "working_dir": workspace.display().to_string(),
        "input": { "file_path": "a.txt" },
    }));
    assert!(
        grant_from_request_at(&request, GrantScope::Path, Utc::now(), 7).is_none(),
        "a read has no effect for a grant to cover"
    );
}

#[test]
fn a_file_that_does_not_exist_yet_is_granted_at_its_place() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.workspace();
    std::fs::create_dir_all(sandbox.root.path().join("notes")).expect("dir");
    let future = sandbox.root.path().join("notes/new-file.md");

    let resolved = canonical_target(&future).expect("the nearest ancestor exists");
    assert_eq!(
        resolved,
        workspace.join("notes/new-file.md"),
        "the missing name is appended to the canonical ancestor"
    );
}

#[test]
fn a_relative_path_is_never_granted() {
    assert!(
        canonical_target(Path::new("notes/first.md")).is_none(),
        "a grant names an absolute place, or it names nothing"
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_target_is_compared_after_resolution() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.workspace();
    let real = sandbox.write("real/inside.txt");
    let link = sandbox.root.path().join("link.txt");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");

    assert_eq!(
        canonical_target(&link).expect("canonical"),
        canonical_target(&real).expect("canonical"),
        "a symlink and its target are the same place"
    );
    assert_eq!(
        canonical_target(&link).expect("canonical"),
        workspace.join("real/inside.txt")
    );
}

#[test]
fn restricted_paths_match_the_raw_text_and_the_canonical_path() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.workspace();
    let secret = sandbox.write("secrets/env");

    let input = serde_json::json!({ "file_path": "secrets/env" });
    assert_eq!(
        restricted_matched(&["secrets/*".to_string()], "write", &input, Some(&workspace)).as_deref(),
        Some("secrets/*"),
        "a glob matches the text the model wrote"
    );

    let canonical = secret.canonicalize().expect("canonical");
    let pattern = format!("{}/**", workspace.display());
    assert_eq!(
        restricted_matched(&[pattern.clone()], "write", &input, Some(&workspace)).as_deref(),
        Some(pattern.as_str()),
        "a glob matches the resolved place too, so `..` cannot dodge it"
    );
    assert!(
        canonical.starts_with(&workspace),
        "the fixture must live under the workspace for the assertion above to mean anything"
    );
}

#[test]
fn a_bare_pattern_matches_the_file_name_in_any_folder() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.workspace();
    let input = serde_json::json!({ "file_path": "deep/nested/secrets.env" });

    assert_eq!(
        restricted_matched(&["secrets.env".to_string()], "write", &input, Some(&workspace)).as_deref(),
        Some("secrets.env"),
        "a pattern with no separator names the file wherever it is"
    );
}

#[test]
fn an_unparseable_restricted_pattern_restricts_every_path() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.workspace();
    let input = serde_json::json!({ "file_path": "notes/first.md" });

    assert_eq!(
        restricted_matched(&["[".to_string()], "write", &input, Some(&workspace)).as_deref(),
        Some("["),
        "a restriction that cannot be read must not mean `no restriction`"
    );
}

#[test]
fn a_tool_that_names_no_path_is_not_restricted() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.workspace();
    let input = serde_json::json!({ "command": "cat secrets.env" });

    assert_eq!(
        restricted_matched(&["secrets.env".to_string()], "bash", &input, Some(&workspace)),
        None,
        "bash names no path here; [[tools.bash_patterns]] is its equivalent"
    );
}
