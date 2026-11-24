//! Install, replace, disable, remove, verify and collect behaviour of the
//! package catalog.
//!
//! Every test uses its own temporary horon home, so nothing here reads or
//! writes the operator's real package area. The two process tests re-invoke
//! this test binary with `--exact` under a helper variable, which is the same
//! mechanism the durable-runtime writer test uses.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use horon_agent_base::packages::{
    InstallRequest, PackageKind, PackageRecord, PackageScope, PackageState, PackageStore, VerificationReport,
    validate_package,
};
use horon_agent_platform::fs::make_owner_writable;
use horon_agent_platform::set_permissions_executable;
use sha2::{Digest, Sha256};
use tempfile::{TempDir, tempdir};

const HELPER_ENV: &str = "PACKAGE_CATALOG_HELPER";
const STORE_ENV: &str = "PACKAGE_CATALOG_STORE";
const SOURCE_ENV: &str = "PACKAGE_CATALOG_SOURCE";
const OWNER_ENV: &str = "PACKAGE_CATALOG_OWNER";
const DIGEST_ENV: &str = "PACKAGE_CATALOG_DIGEST";
const READY_ENV: &str = "PACKAGE_CATALOG_READY";
const RELEASE_ENV: &str = "PACKAGE_CATALOG_RELEASE";
const CONTENTION_TEST: &str = "concurrent_processes_install_without_losing_updates";
const LEASE_TEST: &str = "a_live_reference_keeps_a_replaced_copy_until_the_holder_exits";

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
        (os, arch) => panic!("package_catalog needs a supported runner, got {os}/{arch}"),
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
///
/// These packages are installed, not started, so the bytes only need to be the
/// kind of file the validator requires. That keeps fixture setup independent of
/// this test binary's size.
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

fn write_extension(root: &Path, id: &str, version: &str) {
    fs::create_dir_all(root.join("bin")).expect("create the fixture binary directory");
    write_binary(&root.join(binary_name()), host_target());
    fs::write(
        root.join("extension.toml"),
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
}

fn extension_fixture(id: &str, version: &str) -> TempDir {
    let root = tempdir().expect("fixture directory");
    write_extension(root.path(), id, version);
    validate_package(root.path()).expect("the extension fixture validates");
    root
}

fn plugin_fixture(id: &str, version: &str) -> TempDir {
    let root = tempdir().expect("fixture directory");
    write_extension(&root.path().join("native"), id, version);
    fs::write(root.path().join("rule.md"), "A package rule").expect("write the fixture resource");
    fs::write(
        root.path().join("plugin.toml"),
        format!(
            r#"
schema_version = 1
id = "{id}"
version = "{version}"
name = "Plugin {id}"
extension = "native/extension.toml"
[resources]
rules = ["rule.md"]
"#
        ),
    )
    .expect("write plugin.toml");
    validate_package(root.path()).expect("the plugin fixture validates");
    root
}

/// A temporary horon home plus its package store.
struct Harness {
    home: TempDir,
    store: PackageStore,
}

impl Harness {
    fn new() -> Self {
        let home = tempdir().expect("horon home");
        let store = PackageStore::for_horon_home(home.path());
        Self { home, store }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        thaw(self.store.root());
    }
}

fn install(store: &PackageStore, source: &Path, scope: PackageScope, replace: bool) -> anyhow::Result<PackageRecord> {
    let request = InstallRequest::new(source, scope);
    store.install(if replace { request.replacing() } else { request })
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A content digest of every file under `root`, keyed by relative path.
///
/// Files are hashed in 64 KiB chunks, so a comparison never holds two copies of
/// a package tree in memory even when the fixture is a real agent binary.
fn tree_digest(root: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, directory: &Path, files: &mut BTreeMap<String, String>) {
        for entry in fs::read_dir(directory).expect("read the fixture tree") {
            let entry = entry.expect("read the fixture tree");
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, files);
            } else {
                let relative = path.strip_prefix(root).expect("fixture path under its root");
                let mut file = fs::File::open(&path).expect("open a fixture file");
                let mut hasher = Sha256::new();
                let mut buffer = [0u8; 64 * 1024];
                loop {
                    let read = file.read(&mut buffer).expect("read a fixture file");
                    if read == 0 {
                        break;
                    }
                    hasher.update(&buffer[..read]);
                }
                files.insert(relative.to_string_lossy().into_owned(), hex::encode(hasher.finalize()));
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files);
    files
}

/// Clear the read-only restriction a published copy carries, so a temporary
/// directory can clean itself up.
fn thaw(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.file_type().is_symlink() {
        return;
    }
    drop(make_owner_writable(path, metadata.permissions()));
    if metadata.is_dir()
        && let Ok(entries) = fs::read_dir(path)
    {
        for entry in entries.flatten() {
            thaw(&entry.path());
        }
    }
}

fn make_writable(path: &Path) {
    let metadata = fs::metadata(path).expect("inspect a published file");
    make_owner_writable(path, metadata.permissions()).expect("make a published file writable");
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("create the copied tree");
    for entry in fs::read_dir(source).expect("read the copied tree") {
        let entry = entry.expect("read the copied tree");
        let path = entry.path();
        let target = destination.join(entry.file_name());
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            fs::copy(&path, &target).expect("copy a published file");
        }
    }
}

// ---------------------------------------------------------------------------
// Persistence and integrity
// ---------------------------------------------------------------------------

#[test]
fn installs_persist_and_verification_is_read_only() {
    let harness = Harness::new();
    let project = harness.home.path().join("project");
    fs::create_dir_all(&project).expect("create the project root");
    let extension = extension_fixture("hello", "1.2.3-beta.1");
    let plugin = plugin_fixture("greeter", "0.1.0");
    let executable = std::env::current_exe().expect("the native test binary path");
    for destination in [
        extension.path().join(binary_name()),
        plugin.path().join("native").join(binary_name()),
    ] {
        fs::copy(&executable, &destination).expect("copy the actual native executable");
        set_permissions_executable(&destination).expect("mark the native executable runnable");
    }

    let first = install(&harness.store, extension.path(), PackageScope::Global, false).expect("install the extension");
    let project_scope = PackageScope::project(&project).expect("canonicalize the project scope");
    let second = install(&harness.store, plugin.path(), project_scope.clone(), false).expect("install the plugin");

    assert_eq!(first.kind, PackageKind::Extension);
    assert_eq!(first.generation, 1);
    assert_eq!(first.state, PackageState::Installed);
    assert_eq!(
        first.source,
        extension.path().canonicalize().unwrap().to_string_lossy().into_owned()
    );
    assert!(first.scope.is_global());
    assert_eq!(first.files.len(), 2);
    assert!(first.files.windows(2).all(|pair| pair[0].path < pair[1].path));
    for file in &first.files {
        assert!(is_sha256(&file.sha256), "recorded hash is a SHA-256: {file:?}");
        let published = harness
            .store
            .store_dir()
            .join(first.digest.as_deref().unwrap())
            .join(&file.path);
        assert_eq!(fs::metadata(published).unwrap().len(), file.len);
    }
    assert_eq!(second.kind, PackageKind::Plugin);
    assert_eq!(second.generation, 2);
    assert_eq!(second.scope, project_scope);
    assert_eq!(
        second.scope.project_root(),
        Some(project.canonicalize().unwrap().as_path())
    );
    assert_eq!(second.files.len(), 4);
    assert_eq!(
        harness.store.inbox_dir(),
        harness.home.path().join("packages").join("inbox")
    );

    // A fresh store instance is a restart: nothing is read from process state.
    let reopened = PackageStore::for_horon_home(harness.home.path());
    let catalog = reopened.catalog().expect("read the reopened catalog");
    assert_eq!(catalog.generation(), 2);
    assert_eq!(catalog.len(), 2);
    assert_eq!(catalog.get("hello"), Some(&first));
    assert_eq!(catalog.get("greeter"), Some(&second));
    assert_eq!(catalog.active().count(), 2);

    let catalog_before = fs::read(harness.store.catalog_path()).expect("read installed.json");
    let tree_before = tree_digest(harness.store.root());
    let report = reopened.verify().expect("verify the published copies");
    let tree_after = tree_digest(harness.store.root());
    assert_eq!(
        report,
        VerificationReport {
            generation: 2,
            packages: 2
        }
    );
    assert_eq!(tree_before, tree_after, "verification writes nothing");
    assert_eq!(fs::read(harness.store.catalog_path()).unwrap(), catalog_before);

    let absent = PackageStore::new(harness.home.path().join("absent-packages"));
    assert_eq!(
        absent.verify().expect("verify an absent store"),
        VerificationReport {
            generation: 0,
            packages: 0
        }
    );
    assert!(!absent.root().exists(), "verification never creates the store");
}

#[test]
fn ids_are_shared_across_kinds_and_scopes_and_replace_is_explicit() {
    let harness = Harness::new();
    let project = harness.home.path().join("project");
    fs::create_dir_all(&project).expect("create the project root");
    let project_scope = PackageScope::project(&project).expect("canonicalize the project scope");
    let extension = extension_fixture("shared", "1.0.0");
    let plugin = plugin_fixture("shared", "1.0.0");

    let first = install(&harness.store, extension.path(), PackageScope::Global, false).expect("install the extension");
    let error = install(&harness.store, plugin.path(), project_scope.clone(), false)
        .expect_err("a plugin may not reuse the extension's ID");
    let message = format!("{error:#}");
    assert!(message.contains("already installed"), "unexpected error: {message}");
    assert!(message.contains("replace"), "the error names the override: {message}");

    let replaced = install(&harness.store, plugin.path(), project_scope, true).expect("replace the ID with a plugin");
    assert_eq!(replaced.kind, PackageKind::Plugin);
    assert_eq!(replaced.generation, 2);
    assert_ne!(replaced.digest, first.digest);
    let old_copy = harness
        .store
        .store_dir()
        .join(first.digest.clone().expect("the replaced record names its copy"));
    assert!(old_copy.is_dir(), "the previous copy survives the publication");

    let report = harness.store.collect().expect("collect unreferenced copies");
    assert_eq!(report.collected, vec![first.digest.unwrap()]);
    assert!(report.retained.is_empty());
    assert!(!old_copy.exists());
    assert_eq!(harness.store.verify().expect("verify").packages, 1);
}

#[test]
fn disabled_and_removed_states_survive_a_reopen() {
    let harness = Harness::new();
    let extension = extension_fixture("hello", "1.0.0");
    let plugin = plugin_fixture("greeter", "1.0.0");
    install(&harness.store, extension.path(), PackageScope::Global, false).expect("install the extension");
    install(&harness.store, plugin.path(), PackageScope::Global, false).expect("install the plugin");

    assert_eq!(harness.store.disable("hello").expect("disable the extension"), 3);
    let disabled = PackageStore::for_horon_home(harness.home.path())
        .catalog()
        .expect("read the catalog")
        .get("hello")
        .cloned()
        .expect("the disabled package keeps its record");
    assert_eq!(disabled.state, PackageState::Disabled);
    assert!(disabled.digest.is_some() && disabled.executable.is_some());
    assert_eq!(harness.store.enable("hello").expect("enable the extension"), 4);

    assert_eq!(harness.store.remove("greeter").expect("remove the plugin"), 5);
    let removed = PackageStore::for_horon_home(harness.home.path())
        .catalog()
        .expect("read the catalog");
    let record = removed.get("greeter").expect("the removed package keeps a tombstone");
    assert_eq!(record.state, PackageState::Revoked);
    assert!(record.digest.is_none() && record.executable.is_none() && record.files.is_empty());
    assert_eq!(removed.active().count(), 1);
    let error = harness
        .store
        .disable("greeter")
        .expect_err("a removed package cannot change state");
    assert!(format!("{error:#}").contains("was removed"));

    let reinstall = plugin_fixture("greeter", "2.0.0");
    let error = install(&harness.store, reinstall.path(), PackageScope::Global, false)
        .expect_err("a removed ID needs an explicit replace");
    assert!(format!("{error:#}").contains("was removed"));
    let installed =
        install(&harness.store, reinstall.path(), PackageScope::Global, true).expect("replace the removed package");
    assert_eq!(installed.state, PackageState::Installed);
    assert_eq!(installed.generation, 6);
    assert_eq!(harness.store.verify().expect("verify").packages, 2);
}

#[test]
fn failed_copy_and_invalid_input_leave_the_catalog_untouched() {
    let harness = Harness::new();
    let source = extension_fixture("stable", "1.0.0");
    let source_before = tree_digest(source.path());
    install(&harness.store, source.path(), PackageScope::Global, false).expect("install the valid package");
    let catalog_before = fs::read(harness.store.catalog_path()).expect("read installed.json");
    let stored_before = tree_digest(&harness.store.store_dir());

    let invalid = tempdir().expect("invalid fixture");
    write_extension(invalid.path(), "broken", "1.0.0");
    fs::write(invalid.path().join(binary_name()), b"not a native executable").expect("corrupt the fixture");
    let error = install(&harness.store, invalid.path(), PackageScope::Global, false)
        .expect_err("an invalid package is refused");
    assert!(
        format!("{error:#}").contains("child"),
        "the refusal names the file that failed validation: {error:#}"
    );

    // A bad copy offered as a replacement never becomes the visible generation.
    let broken_replace = tempdir().expect("invalid replacement fixture");
    write_extension(broken_replace.path(), "stable", "2.0.0");
    fs::write(broken_replace.path().join(binary_name()), b"not a native executable")
        .expect("corrupt the replacement fixture");
    install(&harness.store, broken_replace.path(), PackageScope::Global, true)
        .expect_err("an invalid replacement is refused");
    let after_replace = harness.store.catalog().expect("read the catalog");
    let stable = after_replace.get("stable").expect("the previous generation stays");
    assert_eq!(stable.version, "1.0.0");
    assert_eq!(after_replace.generation(), 1);
    assert_eq!(fs::read(harness.store.catalog_path()).unwrap(), catalog_before);

    // A store that cannot be created fails after validation, before any copy is
    // published, and must leave the previous catalog in place.
    let store_dir = harness.store.store_dir();
    let aside = harness.home.path().join("store-aside");
    fs::rename(&store_dir, &aside).expect("move the store aside");
    fs::write(&store_dir, b"not a directory").expect("block the store path");
    let second = extension_fixture("second", "1.0.0");
    let error = install(&harness.store, second.path(), PackageScope::Global, false)
        .expect_err("the blocked store refuses the install");
    let message = format!("{error:#}");
    assert!(message.contains("not a directory"), "unexpected error: {message}");
    fs::remove_file(&store_dir).expect("unblock the store path");
    fs::rename(&aside, &store_dir).expect("restore the store");

    assert_eq!(fs::read(harness.store.catalog_path()).unwrap(), catalog_before);
    assert_eq!(tree_digest(&harness.store.store_dir()), stored_before);
    assert_eq!(
        tree_digest(source.path()),
        source_before,
        "the caller's source is untouched"
    );
    assert_eq!(harness.store.verify().expect("verify").packages, 1);
}

#[test]
fn interrupted_publication_leftovers_are_collected_but_referenced_copies_are_not() {
    let harness = Harness::new();
    let source = extension_fixture("stable", "1.0.0");
    let record = install(&harness.store, source.path(), PackageScope::Global, false).expect("install the package");
    let digest = record.digest.clone().expect("the record names its copy");
    let store_dir = harness.store.store_dir();

    // A copy published just before a crash never reaches the catalog.
    let unrecorded = "f".repeat(64);
    copy_tree(&store_dir.join(&digest), &store_dir.join(&unrecorded));
    let staging = store_dir.join(".staging-4242-7");
    fs::create_dir_all(&staging).expect("create a staging leftover");
    fs::write(staging.join("partial"), b"partial").expect("write a staging leftover");
    let catalog_temp = harness.home.path().join("packages").join(".installed-crashed");
    fs::write(&catalog_temp, b"{}").expect("write a catalog leftover");

    let report = harness.store.collect().expect("collect leftovers");
    assert_eq!(report.removed_staging_directories, 1);
    assert_eq!(report.removed_catalog_temporaries, 1);
    assert_eq!(report.collected, vec![unrecorded]);
    assert!(report.retained.is_empty());
    assert!(!staging.exists());
    assert!(!catalog_temp.exists());
    assert!(store_dir.join(&digest).is_dir(), "a recorded copy is never collected");
    assert_eq!(harness.store.catalog().expect("read the catalog").generation(), 1);
    assert_eq!(harness.store.verify().expect("verify").packages, 1);

    // An entry the store did not write is reported instead of deleted.
    fs::create_dir_all(store_dir.join("notes")).expect("create an unknown entry");
    let error = harness.store.collect().expect_err("an unknown entry is refused");
    assert!(format!("{error:#}").contains("unexpected entry"));
    assert!(store_dir.join("notes").is_dir());
}

#[test]
fn a_reference_owner_that_names_a_windows_device_is_refused() {
    let harness = Harness::new();
    let error = match harness.store.lease("com1") {
        Ok(_lease) => panic!("COM1 is a reserved device name, so it cannot own a reference"),
        Err(error) => error,
    };
    let message = format!("{error:#}");
    assert!(
        message.contains("reserved Windows device name"),
        "the refusal must name the rule: {message}"
    );
    // Only the numbered families are reserved, and only for the digits 1-9.
    harness.store.lease("com0").expect("com0 is not a device name");
}

#[test]
fn verification_reports_a_changed_copy_and_leaves_it_in_place() {
    let harness = Harness::new();
    let source = extension_fixture("hello", "1.0.0");
    let record = install(&harness.store, source.path(), PackageScope::Global, false).expect("install the package");
    let directory = harness
        .store
        .store_dir()
        .join(record.digest.as_deref().expect("the record names its copy"));
    let executable = directory.join(record.executable.as_deref().expect("the record names its executable"));
    let original = fs::read(&executable).expect("read the published executable");
    assert!(
        fs::OpenOptions::new().write(true).open(&executable).is_err(),
        "a published copy refuses writes"
    );

    make_writable(&executable);
    fs::write(&executable, b"tampered").expect("corrupt the published copy");
    let catalog_before = fs::read(harness.store.catalog_path()).expect("read installed.json");
    let error = harness.store.verify().expect_err("a changed copy fails verification");
    let message = format!("{error:#}");
    assert!(message.contains("stored package hello"), "unexpected error: {message}");
    assert!(
        message.contains("sha256"),
        "the report names the hash disagreement: {message}"
    );
    assert_eq!(
        fs::read(&executable).unwrap(),
        b"tampered",
        "verification repairs nothing"
    );
    assert_eq!(fs::read(harness.store.catalog_path()).unwrap(), catalog_before);

    fs::write(&executable, &original).expect("restore the published executable");
    harness.store.verify().expect("the restored copy verifies");
    let manifest = directory.join("extension.toml");
    make_writable(&directory);
    make_writable(&manifest);
    fs::remove_file(&manifest).expect("remove a published file");
    let error = harness.store.verify().expect_err("a missing file fails verification");
    assert!(format!("{error:#}").contains("missing extension.toml"));
}

// ---------------------------------------------------------------------------
// Cross-process behaviour
// ---------------------------------------------------------------------------

#[test]
fn concurrent_processes_install_without_losing_updates() {
    if std::env::var(HELPER_ENV).as_deref() == Ok("install") {
        let store = PackageStore::new(std::env::var(STORE_ENV).expect("helper store"));
        let source = PathBuf::from(std::env::var(SOURCE_ENV).expect("helper source"));
        store
            .install(InstallRequest::new(source, PackageScope::Global))
            .expect("the helper installs its package");
        return;
    }

    let harness = Harness::new();
    let fixtures = [
        extension_fixture("alpha", "1.0.0"),
        extension_fixture("beta", "1.0.0"),
        extension_fixture("gamma", "1.0.0"),
    ];
    let mut children = fixtures
        .iter()
        .map(|fixture| {
            let mut command = helper_command("install", CONTENTION_TEST, &harness);
            command.env(SOURCE_ENV, fixture.path());
            command.spawn().expect("spawn a helper installer")
        })
        .collect::<Vec<Child>>();
    for child in &mut children {
        let status = child.wait().expect("wait for a helper installer");
        assert!(status.success(), "a concurrent install failed: {status}");
    }

    let catalog = harness.store.catalog().expect("read the catalog");
    assert_eq!(catalog.generation(), 3, "every install was recorded");
    assert_eq!(catalog.len(), 3);
    let mut generations = Vec::new();
    for id in ["alpha", "beta", "gamma"] {
        let record = catalog.get(id).expect("the install is recorded");
        assert_eq!(record.state, PackageState::Installed);
        generations.push(record.generation);
    }
    generations.sort_unstable();
    assert_eq!(generations, [1, 2, 3], "every install kept its own generation");
    assert_eq!(
        harness.store.verify().expect("verify"),
        VerificationReport {
            generation: 3,
            packages: 3
        }
    );
    // A fresh mutation takes the catalog lock, so no process left it held.
    assert_eq!(harness.store.disable("alpha").expect("the catalog lock is free"), 4);
}

#[test]
fn a_live_reference_keeps_a_replaced_copy_until_the_holder_exits() {
    if std::env::var(HELPER_ENV).as_deref() == Ok("lease") {
        let store = PackageStore::new(std::env::var(STORE_ENV).expect("helper store"));
        let owner = std::env::var(OWNER_ENV).expect("helper owner");
        let digest = std::env::var(DIGEST_ENV).expect("helper digest");
        let ready = PathBuf::from(std::env::var(READY_ENV).expect("helper ready file"));
        let release = PathBuf::from(std::env::var(RELEASE_ENV).expect("helper release file"));
        let mut lease = store.lease(&owner).expect("take the reference");
        lease.pin([digest]).expect("pin the published copy");
        fs::write(&ready, b"pinned").expect("announce the reference");
        let deadline = Instant::now() + Duration::from_secs(60);
        while !release.exists() {
            assert!(Instant::now() < deadline, "the holder was never released");
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(lease);
        return;
    }

    let harness = Harness::new();
    let first_source = extension_fixture("leased", "1.0.0");
    let first =
        install(&harness.store, first_source.path(), PackageScope::Global, false).expect("install the first version");
    let first_digest = first.digest.clone().expect("the first record names its copy");
    let mut reader = harness
        .store
        .lease("reader-unit-test")
        .expect("take a reader reference");
    let view = harness.store.snapshot(&mut reader).expect("pin the catalog");
    assert_eq!(view.generation(), 1);
    assert_eq!(reader.pinned(), vec![first_digest.clone()]);
    drop(reader);
    assert!(
        !harness.store.root().join("refs").join("reader-unit-test").exists(),
        "a released reference leaves no directory behind"
    );
    let ready = harness.home.path().join("lease-ready");
    let release = harness.home.path().join("lease-release");
    let mut command = helper_command("lease", LEASE_TEST, &harness);
    command
        .env(OWNER_ENV, "session-unit-test")
        .env(DIGEST_ENV, &first_digest)
        .env(READY_ENV, ready.to_string_lossy().as_ref())
        .env(RELEASE_ENV, release.to_string_lossy().as_ref());
    let mut holder = command.spawn().expect("spawn the reference holder");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready.exists() {
        if let Some(status) = holder.try_wait().expect("probe the reference holder") {
            panic!("the reference holder exited before pinning: {status}");
        }
        assert!(Instant::now() < deadline, "the reference holder never pinned");
        std::thread::sleep(Duration::from_millis(10));
    }

    let second_source = extension_fixture("leased", "2.0.0");
    let second = install(&harness.store, second_source.path(), PackageScope::Global, true)
        .expect("replace the package under a live reference");
    let second_digest = second.digest.clone().expect("the second record names its copy");
    assert_ne!(first_digest, second_digest);

    let report = harness.store.collect().expect("collect under a live reference");
    assert_eq!(report.retained, vec![first_digest.clone()]);
    assert!(report.collected.is_empty());
    assert!(
        harness.store.store_dir().join(&first_digest).is_dir(),
        "the pinned copy stays on disk"
    );

    // A killed holder releases the operating-system lock without unwinding.
    holder.kill().expect("stop the reference holder");
    holder.wait().expect("reap the reference holder");
    let report = harness.store.collect().expect("collect after the holder exits");
    assert_eq!(report.removed_stale_references, 1, "the dead reference is reclaimed");
    assert_eq!(report.collected, vec![first_digest.clone()]);
    assert!(!harness.store.store_dir().join(&first_digest).exists());
    assert!(harness.store.store_dir().join(&second_digest).is_dir());
    assert_eq!(harness.store.verify().expect("verify").packages, 1);
    // A fresh mutation takes the catalog lock, so no process left it held.
    assert_eq!(harness.store.disable("leased").expect("the catalog lock is free"), 3);
}

/// A spawned copy of this test binary, already in its helper role.
fn helper_command(role: &str, test: &str, harness: &Harness) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("the test binary path"));
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(HELPER_ENV, role)
        .env(STORE_ENV, harness.store.root())
        .env("HORON_HOME", harness.home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    command
}
