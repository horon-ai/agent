//! The bearer credential store: issue, rotate, revoke and verification.
//!
//! Every assertion here is about the contract the bridge depends on: the file
//! holds only a digest, a credential is printed once, revoking stops it, and a
//! file that is not owner-only refuses instead of authenticating anybody.
use super::*;
use sha2::{Digest, Sha256};

/// A throwaway root with its own `harness-api/` directory.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after the epoch")
            .as_nanos()
            % 1_000_000_000;
        let root = std::env::temp_dir().join(format!("horon-api-cred-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&root).expect("scratch root");
        Self { root }
    }

    fn store(&self) -> CredentialStore {
        CredentialStore::at(self.root.join(CREDENTIALS_DIRECTORY).join(CREDENTIALS_FILE))
    }

    fn credential_directory(&self) -> PathBuf {
        self.root.join(CREDENTIALS_DIRECTORY)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn read_store_text(store: &CredentialStore) -> String {
    std::fs::read_to_string(store.path()).expect("the store file exists after issue")
}

#[test]
fn issuing_returns_the_token_once_and_stores_only_its_digest() {
    let scratch = Scratch::new("issue");
    let store = scratch.store();
    let issued = store.issue(Some("laptop")).expect("issue");
    assert_eq!(issued.token.len(), 64);
    assert!(issued.token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(issued.token.bytes().all(|byte| !byte.is_ascii_uppercase()));

    let text = read_store_text(&store);
    assert!(
        !text.contains(&issued.token),
        "the plaintext token must never reach the file"
    );
    assert!(text.contains("sha256:"), "{text}");
    assert!(text.contains(&format!(
        "sha256:{}",
        hex::encode(Sha256::digest(issued.token.as_bytes()))
    )));

    let records = store.records().expect("records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, issued.record.id);
    assert_eq!(records[0].label, "laptop");
    assert_eq!(records[0].rotated_at, None);

    // The same token is not available twice: a second issue makes a new one.
    let second = store.issue(Some("desktop")).expect("issue another");
    assert_ne!(second.token, issued.token);
    assert_eq!(store.records().expect("records").len(), 2);
}

#[test]
fn only_the_issued_token_authenticates() {
    let scratch = Scratch::new("authenticate");
    let store = scratch.store();
    let issued = store.issue(None).expect("issue");

    let authenticated = store
        .authenticate(&issued.token)
        .expect("authenticate")
        .expect("the issued token authenticates");
    assert_eq!(authenticated.id, issued.record.id);
    assert_eq!(authenticated.label, DEFAULT_LABEL);

    let zeros = "0".repeat(64);
    assert_eq!(store.authenticate(&zeros).expect("authenticate"), None);
    assert_eq!(store.authenticate("not-a-token").expect("authenticate"), None);
    assert_eq!(store.authenticate("").expect("authenticate"), None);
    // A token of the right shape but the wrong value never matches.
    let mut wrong = issued.token.clone();
    wrong.replace_range(0..1, if issued.token.starts_with('0') { "1" } else { "0" });
    assert_eq!(wrong.len(), 64);
    assert_eq!(store.authenticate(&wrong).expect("authenticate"), None);
}

#[test]
fn revoking_removes_the_credential_so_its_token_stops_authenticating() {
    let scratch = Scratch::new("revoke");
    let store = scratch.store();
    let kept = store.issue(Some("kept")).expect("issue kept");
    let doomed = store.issue(Some("doomed")).expect("issue doomed");
    assert!(store.authenticate(&doomed.token).expect("authenticate").is_some());

    let removed = store.revoke("doomed").expect("revoke by label");
    assert_eq!(removed.id, doomed.record.id);
    assert_eq!(store.authenticate(&doomed.token).expect("authenticate"), None);
    // The other credential is untouched.
    assert!(store.authenticate(&kept.token).expect("authenticate").is_some());
    assert_eq!(store.records().expect("records").len(), 1);
}

#[test]
fn rotating_replaces_the_hash_and_keeps_the_identity() {
    let scratch = Scratch::new("rotate");
    let store = scratch.store();
    let issued = store.issue(Some("laptop")).expect("issue");

    let rotated = store.rotate(&issued.record.id).expect("rotate");
    assert_eq!(rotated.record.id, issued.record.id);
    assert_eq!(rotated.record.label, "laptop");
    assert_eq!(rotated.record.created_at, issued.record.created_at);
    assert!(rotated.record.rotated_at.is_some());
    assert_ne!(rotated.token, issued.token);

    assert_eq!(store.authenticate(&issued.token).expect("authenticate"), None);
    assert!(store.authenticate(&rotated.token).expect("authenticate").is_some());
    let text = read_store_text(&store);
    assert!(!text.contains(&issued.token));
    assert!(!text.contains(&rotated.token));
}

#[test]
fn a_label_shared_by_two_credentials_is_ambiguous_and_names_both_ids() {
    let scratch = Scratch::new("ambiguous");
    let store = scratch.store();
    let first = store.issue(Some("shared")).expect("issue first");
    let second = store.issue(Some("shared")).expect("issue second");

    let error = store.revoke("shared").expect_err("an ambiguous label is refused");
    let text = error.to_string();
    assert!(text.contains(&first.record.id), "{text}");
    assert!(text.contains(&second.record.id), "{text}");
    assert!(text.contains("name one by id"), "{text}");
    // Naming the id is unambiguous.
    store.revoke(&first.record.id).expect("revoke by id");
    assert_eq!(store.records().expect("records").len(), 1);
}

#[test]
fn an_unknown_selector_points_at_the_list_command() {
    let scratch = Scratch::new("unknown");
    let store = scratch.store();
    let error = store.revoke("nobody").expect_err("unknown selector");
    assert!(error.to_string().contains("api-credentials list"), "{error}");
}

#[test]
fn an_empty_store_demands_the_issue_command() {
    let scratch = Scratch::new("empty");
    let store = scratch.store();
    assert!(!store.has_any().expect("has_any"));
    let error = store.require_credential().expect_err("an empty store is refused");
    assert!(error.to_string().contains("api-credentials issue"), "{error}");
    store.issue(None).expect("issue");
    store
        .require_credential()
        .expect("a stored credential satisfies the requirement");
}

#[test]
fn a_label_must_be_short_trimmed_and_printable() {
    let scratch = Scratch::new("label");
    let store = scratch.store();
    assert!(store.issue(Some("  ")).is_err());
    assert!(store.issue(Some(&"x".repeat(MAX_LABEL_CHARS + 1))).is_err());
    assert!(store.issue(Some("line\nbreak")).is_err());
    let issued = store.issue(Some("  padded  ")).expect("a padded label is trimmed");
    assert_eq!(issued.record.label, "padded");
}

#[test]
fn the_store_file_and_directory_are_owner_only() {
    let scratch = Scratch::new("modes");
    let store = scratch.store();
    store.issue(None).expect("issue");
    let file = std::fs::symlink_metadata(store.path()).expect("file metadata");
    let directory = std::fs::symlink_metadata(scratch.credential_directory()).expect("directory metadata");
    // `mode_bits` is `None` where the platform keeps access in an ACL instead of
    // a mode, so the assertion follows the platform rather than a cfg line.
    if let Some(bits) = horon_agent_platform::fs::mode_bits(&file) {
        assert_eq!(bits & 0o777, 0o600, "the credential file must be owner-only");
    }
    if let Some(bits) = horon_agent_platform::fs::mode_bits(&directory) {
        assert_eq!(bits & 0o777, 0o700, "the credential directory must be owner-only");
    }
}

#[cfg(unix)]
#[test]
fn a_symlinked_store_is_refused() {
    let scratch = Scratch::new("symlink");
    let store = scratch.store();
    std::fs::create_dir_all(scratch.credential_directory()).expect("directory");
    let elsewhere = scratch.root.join("elsewhere.json");
    std::fs::write(&elsewhere, "{\"version\":1,\"credentials\":[]}").expect("decoy file");
    std::os::unix::fs::symlink(&elsewhere, store.path()).expect("symlink the store path");
    let error = store.records().expect_err("a symlinked store is refused");
    assert!(error.to_string().contains("regular file"), "{error}");
    assert!(store.authenticate(&"a".repeat(64)).is_err());
}

#[cfg(unix)]
#[test]
fn a_loose_permission_store_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new("loose");
    let store = scratch.store();
    store.issue(None).expect("issue");
    std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o644)).expect("loosen the mode");
    let error = store.records().expect_err("a world-readable store is refused");
    let text = error.to_string();
    assert!(text.contains("0644"), "{text}");
    assert!(text.contains("not owner-only"), "{text}");
    assert!(store.authenticate(&"b".repeat(64)).is_err());
}
