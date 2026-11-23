//! Turn-boundary package generations and revocation.
//!
//! Each case runs against a real package store in its own temporary horon home:
//! the fixtures are the same minimal native headers the catalog suite installs,
//! and nothing here reads or writes the operator's package area.
//!
//! The cases pin the behaviour the issue names, not an internal counter: one turn
//! keeps the generation it captured (by value, by pinned copy and by the store's
//! own collection report), a turn with no catalog change gets the same snapshot
//! arc back, and a disable or removal refuses a handle from before it while the
//! callbacks registered for that generation ran.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use horon_agent_app_core::extensions::lifecycle::{
    PackageLifecycleRegistry, RevocationCallback, RevocationCause, RevocationEvent, RevocationReport,
};
use horon_agent_app_core::extensions::package_snapshot::{PackageSession, PackageSnapshot};
use horon_agent_app_core::packages::{InstallRequest, PackageRecord, PackageScope, PackageState, PackageStore};
use horon_agent_platform::set_permissions_executable;
use tempfile::{TempDir, tempdir};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn host_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        (os, arch) => panic!("package_session needs a supported runner, got {os}/{arch}"),
    }
}

fn binary_name() -> &'static str {
    if std::env::consts::OS == "windows" {
        "bin/child.exe"
    } else {
        "bin/child"
    }
}

/// A minimal executable whose header satisfies the host's native-target check.
fn write_binary(path: &Path, target: &str) {
    let bytes = if target.ends_with("-darwin") {
        mach_o(target.starts_with("aarch64-"))
    } else if target.ends_with("-gnu") {
        elf(target.starts_with("aarch64-"))
    } else {
        pe(target.starts_with("aarch64-"))
    };
    fs::write(path, bytes).expect("write the fixture executable");
    set_permissions_executable(path).expect("mark the fixture executable");
}

fn mach_o(arm64: bool) -> Vec<u8> {
    let cpu: u32 = if arm64 { 0x0100_000c } else { 0x0100_0007 };
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0xfeed_facfu32.to_le_bytes());
    bytes.extend_from_slice(&cpu.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&24u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    // LC_BUILD_VERSION with platform 1 (macOS).
    bytes.extend_from_slice(&0x32u32.to_le_bytes());
    bytes.extend_from_slice(&24u32.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&[0u8; 32]);
    bytes
}

fn elf(arm64: bool) -> Vec<u8> {
    let machine: u16 = if arm64 { 183 } else { 62 };
    let loader: &[u8] = if arm64 {
        b"/lib/ld-linux-aarch64.so.1\0"
    } else {
        b"/lib64/ld-linux-x86-64.so.2\0"
    };
    let program_offset: usize = 64;
    let interpreter_offset: u64 = 120;
    let mut bytes = vec![0u8; interpreter_offset as usize];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[7] = 0;
    bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes[20..24].copy_from_slice(&[1, 0, 0, 0]);
    bytes[24..32].copy_from_slice(&0x0040_1000u64.to_le_bytes());
    bytes[32..40].copy_from_slice(&(program_offset as u64).to_le_bytes());
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
    bytes[program_offset..program_offset + 4].copy_from_slice(&3u32.to_le_bytes());
    bytes[program_offset + 8..program_offset + 16].copy_from_slice(&interpreter_offset.to_le_bytes());
    bytes[program_offset + 32..program_offset + 40].copy_from_slice(&(loader.len() as u64).to_le_bytes());
    bytes.extend_from_slice(loader);
    bytes
}

fn pe(arm64: bool) -> Vec<u8> {
    let machine: u16 = if arm64 { 0xaa64 } else { 0x8664 };
    let mut bytes = vec![0u8; 64];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
    bytes.extend_from_slice(b"PE\0\0");
    bytes.extend_from_slice(&machine.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&[0u8; 12]);
    bytes.extend_from_slice(&112u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&0x020bu16.to_le_bytes());
    bytes.extend_from_slice(&[0u8; 110]);
    bytes.extend_from_slice(&[0u8; 40]);
    bytes
}

fn extension_fixture(id: &str, version: &str) -> TempDir {
    let root = tempdir().expect("fixture directory");
    fs::create_dir_all(root.path().join("bin")).expect("create the fixture binary directory");
    write_binary(&root.path().join(binary_name()), host_target());
    fs::write(
        root.path().join("extension.toml"),
        format!(
            r#"
schema_version = 1
id = "{id}"
version = "{version}"
name = "Hello {id}"
protocol = "1.0"
[targets]
{target} = "{binary}"
"#,
            target = host_target(),
            binary = binary_name(),
        ),
    )
    .expect("write extension.toml");
    root
}

fn install(store: &PackageStore, source: &Path, scope: PackageScope) -> PackageRecord {
    store
        .install(InstallRequest::new(source, scope))
        .expect("install the fixture package")
}

fn install_replacing(store: &PackageStore, source: &Path, scope: PackageScope) -> PackageRecord {
    store
        .install(InstallRequest::new(source, scope).replacing())
        .expect("replace the installed fixture package")
}

fn digest_of(record: &PackageRecord) -> String {
    record.digest.clone().expect("an installed record names its copy")
}

fn version_of(snapshot: &PackageSnapshot, package_id: &str) -> Option<String> {
    snapshot.get(package_id).map(|record| record.version.clone())
}

fn digest_in(snapshot: &PackageSnapshot, package_id: &str) -> Option<String> {
    snapshot.get(package_id).and_then(|record| record.digest.clone())
}

fn counting_callback(calls: Arc<AtomicUsize>) -> RevocationCallback {
    Arc::new(move |_event: &RevocationEvent| {
        calls.fetch_add(1, Ordering::SeqCst);
    })
}

fn states(package_id: &str, state: PackageState) -> BTreeMap<String, PackageState> {
    let mut map = BTreeMap::new();
    map.insert(package_id.to_owned(), state);
    map
}

// ---------------------------------------------------------------------------
// One turn, one generation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unchanged_catalog_reuses_the_same_turn_snapshot() {
    let home = tempdir().expect("horon home");
    let store = PackageStore::for_horon_home(home.path());
    let source = extension_fixture("reuse-alpha", "1.0.0");
    install(&store, source.path(), PackageScope::Global);
    let session = PackageSession::open(store.clone(), "reuse");

    let first = session.enter_turn(None).await.expect("first turn");
    let second = session.enter_turn(None).await.expect("second turn");

    assert!(
        Arc::ptr_eq(&first, &second),
        "an unchanged catalog must hand back the same snapshot"
    );
    assert_eq!(first.generation(), second.generation());
    assert_eq!(first.pinned_copies(), second.pinned_copies());
    assert!(first.get("reuse-alpha").is_some());
}

#[tokio::test]
async fn a_package_free_home_creates_no_package_area() {
    let home = tempdir().expect("horon home");
    let store = PackageStore::for_horon_home(home.path());
    let session = PackageSession::open(store.clone(), "idle");

    let snapshot = session.enter_turn(Some(home.path())).await.expect("an empty turn");

    assert!(snapshot.is_empty());
    assert_eq!(snapshot.generation(), 0);
    assert!(snapshot.pinned_copies().is_empty());
    assert!(
        !store.root().exists(),
        "a session with no installed package must not create the package area under {}",
        store.root().display()
    );
}

#[tokio::test]
async fn the_next_turn_and_a_new_session_observe_a_replaced_generation() {
    let home = tempdir().expect("horon home");
    let store = PackageStore::for_horon_home(home.path());
    let first_source = extension_fixture("replace-alpha", "1.0.0");
    let old_record = install(&store, first_source.path(), PackageScope::Global);
    let old_digest = digest_of(&old_record);
    let session = PackageSession::open(store.clone(), "first-turn");
    let first = session.enter_turn(None).await.expect("first turn");
    assert_eq!(digest_in(&first, "replace-alpha").as_deref(), Some(old_digest.as_str()));

    let second_source = extension_fixture("replace-alpha", "2.0.0");
    let new_record = install_replacing(&store, second_source.path(), PackageScope::Global);
    let new_digest = digest_of(&new_record);
    assert_ne!(old_digest, new_digest, "the replacement publishes distinct bytes");

    // The running turn keeps the generation it captured, and its pin is what
    // keeps the replaced copy on disk.
    assert_eq!(digest_in(&first, "replace-alpha").as_deref(), Some(old_digest.as_str()));
    assert!(first.pinned_copies().contains(&old_digest));
    let held = store.collect().expect("collect while the turn runs");
    assert!(
        held.retained.contains(&old_digest),
        "the running turn keeps its copy: {held:?}"
    );

    let next = session.enter_turn(None).await.expect("next turn");
    assert!(
        next.generation() > first.generation(),
        "the next turn observes the new generation"
    );
    assert_eq!(digest_in(&next, "replace-alpha").as_deref(), Some(new_digest.as_str()));

    let fresh = PackageSession::open(store.clone(), "new-session");
    let observed = fresh.enter_turn(None).await.expect("new session");
    assert_eq!(observed.generation(), next.generation());
    assert_eq!(digest_in(&observed, "replace-alpha"), digest_in(&next, "replace-alpha"));

    drop(first);
    drop(next);
    drop(observed);
    drop(session);
    drop(fresh);
    let released = store.collect().expect("collect after the last holder");
    assert!(
        released.collected.contains(&old_digest),
        "the replaced copy is reclaimed once nothing pins it: {released:?}"
    );
}

// ---------------------------------------------------------------------------
// Revocation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_disabled_package_stops_new_loads_and_runs_cancel_callbacks() {
    let home = tempdir().expect("horon home");
    let store = PackageStore::for_horon_home(home.path());
    let source = extension_fixture("disable-alpha", "1.0.0");
    install(&store, source.path(), PackageScope::Global);
    let session = PackageSession::open(store.clone(), "disable");
    let snapshot = session.enter_turn(None).await.expect("turn");
    let stale = session.handle("disable-alpha").expect("a handle from the running turn");

    let lifecycle = PackageLifecycleRegistry::shared();
    let calls = Arc::new(AtomicUsize::new(0));
    let _holder = lifecycle
        .register(
            "disable-alpha",
            snapshot.generation(),
            counting_callback(Arc::clone(&calls)),
        )
        .expect("register the live holder");

    let disabled_generation = store.disable("disable-alpha").expect("disable");
    let report: RevocationReport = lifecycle.apply_states(
        &states("disable-alpha", PackageState::Installed),
        &states("disable-alpha", PackageState::Disabled),
        disabled_generation,
    );
    assert_eq!(report, RevocationReport { fired: 1, failed: 0 });
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the live holder's callback ran");
    assert_eq!(lifecycle.holders("disable-alpha"), 0, "a fired holder is unregistered");
    assert_eq!(
        lifecycle.revocation("disable-alpha").map(|event| event.cause),
        Some(RevocationCause::Disabled)
    );

    let hidden = session.enter_turn(None).await.expect("turn after the disable");
    assert!(
        hidden.get("disable-alpha").is_none(),
        "a disabled package is not listed"
    );
    assert!(session.handle("disable-alpha").is_none());
    assert!(
        stale.ensure_live(&session).await.is_err(),
        "a request from before the disable is refused"
    );

    store.enable("disable-alpha").expect("enable");
    let visible = session.enter_turn(None).await.expect("turn after the enable");
    assert!(
        visible.get("disable-alpha").is_some(),
        "an enabled package is listed again"
    );
    let fresh = session.handle("disable-alpha").expect("a handle after the enable");
    fresh
        .ensure_live(&session)
        .await
        .expect("a handle from the enabled generation loads");
    assert!(
        stale.ensure_live(&session).await.is_err(),
        "the handle from before the disable stays stale after the enable"
    );
}

#[tokio::test]
async fn a_removed_package_refuses_a_stale_handle_even_after_it_is_installed_again() {
    let home = tempdir().expect("horon home");
    let store = PackageStore::for_horon_home(home.path());
    let first_source = extension_fixture("remove-alpha", "1.0.0");
    install(&store, first_source.path(), PackageScope::Global);
    let session = PackageSession::open(store.clone(), "remove");
    session.enter_turn(None).await.expect("turn");
    let stale = session.handle("remove-alpha").expect("a handle from the running turn");

    let lifecycle = PackageLifecycleRegistry::shared();
    let removed_generation = store.remove("remove-alpha").expect("remove");
    let report = lifecycle.apply_states(
        &states("remove-alpha", PackageState::Installed),
        &states("remove-alpha", PackageState::Revoked),
        removed_generation,
    );
    assert_eq!(
        report,
        RevocationReport { fired: 0, failed: 0 },
        "no holder was registered"
    );

    assert!(
        stale.ensure_live(&session).await.is_err(),
        "a request obtained before the removal must be refused"
    );
    let empty = session.enter_turn(None).await.expect("turn after the removal");
    assert!(empty.get("remove-alpha").is_none());
    assert!(session.handle("remove-alpha").is_none());

    let second_source = extension_fixture("remove-alpha", "2.0.0");
    install_replacing(&store, second_source.path(), PackageScope::Global);
    let visible = session.enter_turn(None).await.expect("turn after the reinstall");
    assert_eq!(version_of(&visible, "remove-alpha").as_deref(), Some("2.0.0"));
    let fresh = session.handle("remove-alpha").expect("a handle after the reinstall");
    fresh
        .ensure_live(&session)
        .await
        .expect("a handle from the reinstalled generation loads");
    assert!(
        stale.ensure_live(&session).await.is_err(),
        "the request from before the removal stays refused"
    );
}

#[test]
fn a_holder_cannot_register_for_a_generation_the_package_already_left() {
    let lifecycle = PackageLifecycleRegistry::shared();
    let package_id = "register-alpha";
    let calls = Arc::new(AtomicUsize::new(0));
    lifecycle.revoke(package_id, 11, RevocationCause::Removed);
    let stale = lifecycle.register(package_id, 10, counting_callback(Arc::clone(&calls)));
    assert!(stale.is_err(), "a holder from before the revocation cannot register");
    let holder = lifecycle
        .register(package_id, 12, counting_callback(Arc::clone(&calls)))
        .expect("a holder from after the revocation may register");
    assert_eq!(lifecycle.holders(package_id), 1);
    drop(holder);
    assert_eq!(lifecycle.holders(package_id), 0, "dropping the holder unregisters it");
    lifecycle.revoke(package_id, 13, RevocationCause::Disabled);
    assert_eq!(calls.load(Ordering::SeqCst), 0, "a dropped holder is never called");
}

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_project_scoped_package_is_refused_outside_its_canonical_workspace() {
    let home = tempdir().expect("horon home");
    let store = PackageStore::for_horon_home(home.path());
    let workspace = tempdir().expect("project workspace");
    let other = tempdir().expect("other workspace");
    let scoped_source = extension_fixture("scope-alpha", "1.0.0");
    install(
        &store,
        scoped_source.path(),
        PackageScope::project(workspace.path()).expect("a canonical project scope"),
    );
    let global_source = extension_fixture("scope-global", "1.0.0");
    install(&store, global_source.path(), PackageScope::Global);
    fs::create_dir_all(workspace.path().join("nested")).expect("create a nested directory");
    let canonical = workspace.path().canonicalize().expect("canonical workspace");

    let inside = PackageSession::open(store.clone(), "inside");
    let spelling = workspace.path().join("nested").join("..");
    let listed = inside
        .enter_turn(Some(&spelling))
        .await
        .expect("turn inside the workspace");
    assert_eq!(listed.workspace(), Some(canonical.as_path()));
    assert!(
        listed.get("scope-alpha").is_some(),
        "two spellings of one workspace select the same scope"
    );
    assert!(listed.get("scope-global").is_some());
    inside
        .handle("scope-alpha")
        .expect("a scoped handle")
        .ensure_live(&inside)
        .await
        .expect("the project package loads inside its workspace");

    let outside = PackageSession::open(store.clone(), "outside");
    let elsewhere = outside
        .enter_turn(Some(other.path()))
        .await
        .expect("turn outside the workspace");
    assert!(
        elsewhere.get("scope-alpha").is_none(),
        "a project package is not listed outside its root"
    );
    assert!(outside.handle("scope-alpha").is_none());
    assert!(
        elsewhere.get("scope-global").is_some(),
        "a global package loads anywhere"
    );
}

// ---------------------------------------------------------------------------
// Reclamation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn removal_keeps_a_copy_until_the_last_holder_and_keeps_the_install_source() {
    let home = tempdir().expect("horon home");
    let store = PackageStore::for_horon_home(home.path());
    let source = extension_fixture("reclaim-alpha", "1.0.0");
    let record = install(&store, source.path(), PackageScope::Global);
    let digest = digest_of(&record);
    let session = PackageSession::open(store.clone(), "reclaim");
    let snapshot = session.enter_turn(None).await.expect("turn");
    assert!(snapshot.pinned_copies().contains(&digest));

    store.remove("reclaim-alpha").expect("remove");
    let held = store.collect().expect("collect while the turn runs");
    assert!(
        held.retained.contains(&digest),
        "a live turn keeps its copy after the removal: {held:?}"
    );
    assert!(store.store_dir().join(&digest).is_dir());
    assert!(
        source.path().join("extension.toml").is_file(),
        "the folder the user installed from is not a managed copy"
    );
    assert!(source.path().join(binary_name()).is_file());

    drop(snapshot);
    drop(session);
    let released = store.collect().expect("collect after the last holder");
    assert!(
        released.collected.contains(&digest),
        "the removed copy is reclaimed once released: {released:?}"
    );
    assert!(!store.store_dir().join(&digest).exists());
    assert!(
        source.path().join("extension.toml").is_file(),
        "the install source still exists"
    );
    assert!(source.path().join(binary_name()).is_file());
}

#[tokio::test]
async fn another_sessions_pin_keeps_a_replaced_copy_until_it_releases() {
    let home = tempdir().expect("horon home");
    let store = PackageStore::for_horon_home(home.path());
    let first_source = extension_fixture("shared-alpha", "1.0.0");
    let old_record = install(&store, first_source.path(), PackageScope::Global);
    let old_digest = digest_of(&old_record);
    let first_session = PackageSession::open(store.clone(), "owner-one");
    let second_session = PackageSession::open(store.clone(), "owner-two");
    let first = first_session.enter_turn(None).await.expect("first owner");
    let second = second_session.enter_turn(None).await.expect("second owner");
    assert!(first.pinned_copies().contains(&old_digest));
    assert!(second.pinned_copies().contains(&old_digest));

    let second_source = extension_fixture("shared-alpha", "2.0.0");
    install_replacing(&store, second_source.path(), PackageScope::Global);
    drop(first);
    drop(first_session);
    let held = store.collect().expect("collect with one owner left");
    assert!(
        held.retained.contains(&old_digest),
        "the other owner's pin keeps the replaced copy: {held:?}"
    );

    drop(second);
    drop(second_session);
    let released = store.collect().expect("collect after both owners");
    assert!(
        released.collected.contains(&old_digest),
        "the replaced copy is reclaimed after the last owner: {released:?}"
    );
}

#[tokio::test]
async fn a_restored_session_observes_the_current_generation() {
    let home = tempdir().expect("horon home");
    let store = PackageStore::for_horon_home(home.path());
    let first_source = extension_fixture("restore-alpha", "1.0.0");
    install(&store, first_source.path(), PackageScope::Global);
    let session = PackageSession::open(store.clone(), "session:restore-alpha");
    let before = session.enter_turn(None).await.expect("turn before the restart");
    assert_eq!(version_of(&before, "restore-alpha").as_deref(), Some("1.0.0"));
    drop(before);
    drop(session);

    let second_source = extension_fixture("restore-alpha", "2.0.0");
    install_replacing(&store, second_source.path(), PackageScope::Global);
    // A restored session starts from the store, not from anything the previous
    // process cached, so its first turn is the current generation.
    let restored = PackageSession::open(store.clone(), "session:restore-alpha");
    let after = restored.enter_turn(None).await.expect("first turn after the restore");
    assert_eq!(
        after.generation(),
        store.catalog().expect("read the catalog").generation(),
        "a restored session observes the current generation"
    );
    assert_eq!(version_of(&after, "restore-alpha").as_deref(), Some("2.0.0"));
    restored
        .handle("restore-alpha")
        .expect("a handle after the restore")
        .ensure_live(&restored)
        .await
        .expect("the restored session may load the current generation");
}
