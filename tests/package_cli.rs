//! `horon package` driven through the real binary in a private horon home.
//!
//! Every case runs the released CLI surface with `HOME`, `HORON_HOME` and the
//! XDG directories pointed at a temporary directory, so nothing here reads or
//! writes the operator's real package area, provider configuration, sessions or
//! daemon sockets. The fixture executables are minimal headers that satisfy the
//! host's native-target check; they are only ever copied and hashed, never
//! started.
//!
//! The inbox cases call the store API directly because installing from the
//! inbox is a daemon-side pass (#643); they reuse the same fixtures.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use horon_agent::packages::{InboxOutcome, PackageKind, PackageScope, PackageState, PackageStore};
use horon_agent_platform::fs::make_owner_writable;
use horon_agent_platform::set_permissions_executable;
use tempfile::{TempDir, tempdir};

// ---------------------------------------------------------------------------
// Package fixtures
// ---------------------------------------------------------------------------

fn host_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        (os, arch) => panic!("package_cli needs a supported runner, got {os}/{arch}"),
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
fn host_bytes() -> Vec<u8> {
    let target = host_target();
    if target.ends_with("-darwin") {
        mach_o(target.starts_with("aarch64-"))
    } else if target.ends_with("-gnu") {
        elf(target.starts_with("aarch64-"))
    } else {
        pe(target.starts_with("aarch64-"))
    }
}

fn write_binary(path: &Path) {
    fs::write(path, host_bytes()).expect("write the fixture executable");
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

fn write_extension_root(root: &Path, id: &str, version: &str) {
    fs::create_dir_all(root.join("bin")).expect("create the fixture binary directory");
    write_binary(&root.join(binary_name()));
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

fn write_plugin_root(root: &Path, id: &str, version: &str) {
    write_extension_root(&root.join("native"), id, version);
    fs::write(root.join("rule.md"), "A package rule").expect("write the fixture resource");
    fs::write(
        root.join("plugin.toml"),
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

fn link_directory(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("link the inbox entry");
    #[cfg(windows)]
    {
        let output = Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .expect("run mklink");
        assert!(
            output.status.success(),
            "mklink failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn canonical_text(path: &Path) -> String {
    path.canonicalize()
        .expect("the fixture path resolves")
        .to_string_lossy()
        .into_owned()
}

// ---------------------------------------------------------------------------
// Sandbox
// ---------------------------------------------------------------------------

/// A temporary horon home plus a temporary workspace the CLI runs inside.
struct Sandbox {
    home: TempDir,
    workspace: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            home: tempdir().expect("a temporary horon home"),
            workspace: tempdir().expect("a temporary workspace"),
        }
    }

    fn store(&self) -> PackageStore {
        PackageStore::for_horon_home(self.home.path())
    }

    fn extension_source(&self, directory: &str, id: &str, version: &str) -> PathBuf {
        let root = self.workspace.path().join(directory);
        write_extension_root(&root, id, version);
        root
    }

    fn plugin_source(&self, directory: &str, id: &str, version: &str) -> PathBuf {
        let root = self.workspace.path().join(directory);
        write_plugin_root(&root, id, version);
        root
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_in(self.workspace.path(), args)
    }

    fn run_in(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_horon-agent"))
            .current_dir(cwd)
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("HORON_HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env("XDG_CACHE_HOME", self.home.path().join("cache"))
            .env("XDG_RUNTIME_DIR", self.home.path().join("runtime"))
            .env("HORON_AGENT_NO_AUTO_UPDATE", "1")
            .env("HORON_AGENT_NON_INTERACTIVE", "1")
            .env("NO_COLOR", "1")
            .args(args)
            .output()
            .expect("run the horon-agent CLI")
    }

    fn home_entries(&self) -> Vec<String> {
        let mut names = Vec::new();
        for entry in fs::read_dir(self.home.path()).expect("read the temporary horon home") {
            let entry = entry.expect("read the temporary horon home");
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        names
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        thaw(&self.home.path().join("packages"));
    }
}

fn assert_success(output: &Output) -> String {
    assert!(
        output.status.success(),
        "command failed with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        text(&output.stdout),
        text(&output.stderr)
    );
    text(&output.stdout)
}

fn assert_failure(output: &Output) -> String {
    assert!(
        !output.status.success(),
        "command unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    text(&output.stderr)
}

// ---------------------------------------------------------------------------
// CLI: install, list, verify
// ---------------------------------------------------------------------------

#[test]
fn installs_lists_and_verifies_both_kinds_without_provider_config() {
    let sandbox = Sandbox::new();
    let project = sandbox.workspace.path().join("project");
    fs::create_dir_all(&project).expect("create the project root");
    let extension = sandbox.extension_source("hello-src", "hello", "1.2.3");
    let plugin = sandbox.plugin_source("widget-src", "widget", "0.1.0");
    let native_executable = std::env::current_exe().expect("the native test executable path");
    for destination in [extension.join(binary_name()), plugin.join("native").join(binary_name())] {
        fs::copy(&native_executable, &destination).expect("copy the native executable into the package");
    }

    let output = sandbox.run(&[
        "package",
        "install",
        &path_text(&extension),
        "--project",
        &path_text(&project),
    ]);
    let stdout = assert_success(&output);
    assert_eq!(
        stdout.trim(),
        format!(
            "Installed hello 1.2.3 [extension, installed, project {}] from {}",
            canonical_text(&project),
            canonical_text(&extension)
        )
    );

    let output = sandbox.run(&["package", "install", &path_text(&plugin)]);
    let stdout = assert_success(&output);
    assert_eq!(
        stdout.trim(),
        format!(
            "Installed widget 0.1.0 [plugin, installed, global] from {}",
            canonical_text(&plugin)
        )
    );

    let output = sandbox.run(&["package", "list"]);
    let stdout = assert_success(&output);
    assert!(
        stdout.contains("Installed packages (catalog generation 2):"),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "hello 1.2.3 [extension, installed, project {}] from {}",
            canonical_text(&project),
            canonical_text(&extension)
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "widget 0.1.0 [plugin, installed, global] from {}",
            canonical_text(&plugin)
        )),
        "{stdout}"
    );

    let output = sandbox.run(&["package", "list", "--kind", "plugin"]);
    let stdout = assert_success(&output);
    assert!(stdout.contains("widget 0.1.0"), "{stdout}");
    assert!(!stdout.contains("hello"), "{stdout}");

    let output = sandbox.run(&["package", "verify"]);
    assert_eq!(
        assert_success(&output).trim(),
        "Verified 2 packages at catalog generation 2."
    );

    let catalog = sandbox.store().catalog().expect("read the catalog");
    assert_eq!(catalog.generation(), 2);
    let hello = catalog.get("hello").expect("the extension record");
    assert_eq!(hello.kind, PackageKind::Extension);
    assert_eq!(hello.version, "1.2.3");
    assert_eq!(hello.state, PackageState::Installed);
    assert_eq!(hello.scope, PackageScope::project(&project).expect("a project scope"));
    assert_eq!(hello.source, canonical_text(&extension));
    assert_eq!(hello.files.len(), 2);
    let widget = catalog.get("widget").expect("the plugin record");
    assert_eq!(widget.kind, PackageKind::Plugin);
    assert_eq!(widget.scope, PackageScope::Global);
    assert_eq!(widget.files.len(), 4);

    // A package command is a standalone filesystem operation: it leaves no
    // config, log, session or runtime state behind, so no provider profile,
    // update check or daemon was touched.
    assert_eq!(sandbox.home_entries(), vec!["packages"]);
    assert!(!sandbox.home.path().join("runtime").exists());
}

#[test]
fn replace_is_explicit_and_keeps_one_record() {
    let sandbox = Sandbox::new();
    let first = sandbox.extension_source("hello-v1", "hello", "1.0.0");
    assert_success(&sandbox.run(&["package", "install", &path_text(&first)]));

    let second = sandbox.extension_source("hello-v2", "hello", "2.0.0");
    let stderr = assert_failure(&sandbox.run(&["package", "install", &path_text(&second)]));
    assert!(stderr.contains("package hello is already installed"), "{stderr}");
    assert!(stderr.contains("pass replace"), "{stderr}");

    let catalog = sandbox.store().catalog().expect("read the catalog");
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog.generation(), 1);
    let hello = catalog.get("hello").expect("the extension record");
    assert_eq!(hello.version, "1.0.0");
    assert_eq!(hello.source, canonical_text(&first));

    let output = sandbox.run(&["package", "install", &path_text(&second), "--replace"]);
    let stdout = assert_success(&output);
    assert!(stdout.contains("Installed hello 2.0.0"), "{stdout}");

    let catalog = sandbox.store().catalog().expect("read the catalog");
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog.generation(), 2);
    let hello = catalog.get("hello").expect("the extension record");
    assert_eq!(hello.version, "2.0.0");
    assert_eq!(hello.source, canonical_text(&second));
    assert_eq!(hello.state, PackageState::Installed);
}

#[test]
fn disable_and_remove_change_state_and_keep_the_record() {
    let sandbox = Sandbox::new();
    let source = sandbox.extension_source("hello-src", "hello", "1.0.0");
    assert_success(&sandbox.run(&["package", "install", &path_text(&source)]));

    let output = sandbox.run(&["package", "disable", "hello"]);
    assert_eq!(assert_success(&output).trim(), "Disabled hello (catalog generation 2).");
    let record = sandbox.store().catalog().expect("read the catalog");
    assert_eq!(record.get("hello").expect("the record").state, PackageState::Disabled);

    // Disabling twice is a no-op that keeps the generation.
    let output = sandbox.run(&["package", "disable", "hello"]);
    assert_eq!(assert_success(&output).trim(), "Disabled hello (catalog generation 2).");

    let output = sandbox.run(&["package", "list"]);
    assert!(assert_success(&output).contains("[extension, disabled, global]"));

    let output = sandbox.run(&["package", "remove", "hello"]);
    assert_eq!(assert_success(&output).trim(), "Removed hello (catalog generation 3).");
    let catalog = sandbox.store().catalog().expect("read the catalog");
    let record = catalog.get("hello").expect("the revoked record");
    assert_eq!(record.state, PackageState::Revoked);
    assert!(record.digest.is_none());
    assert!(record.files.is_empty());
    assert_eq!(record.source, canonical_text(&source));

    let output = sandbox.run(&["package", "list"]);
    let stdout = assert_success(&output);
    assert!(stdout.contains("[extension, removed, global]"), "{stdout}");

    let stderr = assert_failure(&sandbox.run(&["package", "remove", "hello"]));
    assert!(stderr.contains("already removed"), "{stderr}");

    let output = sandbox.run(&["package", "verify"]);
    assert_eq!(
        assert_success(&output).trim(),
        "Verified 0 packages at catalog generation 3."
    );

    let stderr = assert_failure(&sandbox.run(&["package", "install", &path_text(&source)]));
    assert!(stderr.contains("package hello was removed"), "{stderr}");
    assert_success(&sandbox.run(&["package", "install", &path_text(&source), "--replace"]));
    let catalog = sandbox.store().catalog().expect("read the catalog");
    assert_eq!(catalog.get("hello").expect("the record").state, PackageState::Installed);
    assert_eq!(catalog.generation(), 4);
}

#[test]
fn project_scope_requires_an_explicit_project_root() {
    let sandbox = Sandbox::new();
    let project = sandbox.workspace.path().join("project");
    fs::create_dir_all(&project).expect("create the project root");
    let hello = sandbox.extension_source("hello-src", "hello", "1.0.0");

    // Running inside a project never installs into it: the default scope is the
    // user, and `--project` is the only project-scope permission.
    let output = sandbox.run_in(&project, &["package", "install", &path_text(&hello)]);
    assert_success(&output);
    let catalog = sandbox.store().catalog().expect("read the catalog");
    assert_eq!(catalog.get("hello").expect("the record").scope, PackageScope::Global);

    let widget = sandbox.plugin_source("widget-src", "widget", "0.1.0");
    let spelled = project.join(".");
    let output = sandbox.run(&[
        "package",
        "install",
        &path_text(&widget),
        "--project",
        &path_text(&spelled),
    ]);
    assert_success(&output);
    let catalog = sandbox.store().catalog().expect("read the catalog");
    assert_eq!(
        catalog.get("widget").expect("the record").scope,
        PackageScope::project(&project).expect("a project scope")
    );

    let other = sandbox.extension_source("other-src", "other", "1.0.0");
    let missing = sandbox.workspace.path().join("missing-project");
    let stderr = assert_failure(&sandbox.run(&[
        "package",
        "install",
        &path_text(&other),
        "--project",
        &path_text(&missing),
    ]));
    assert!(stderr.contains("resolve the project root"), "{stderr}");

    let file = sandbox.workspace.path().join("project-file");
    fs::write(&file, "not a directory").expect("write the project file");
    let stderr =
        assert_failure(&sandbox.run(&["package", "install", &path_text(&other), "--project", &path_text(&file)]));
    assert!(stderr.contains("project root is not a directory"), "{stderr}");
    let catalog = sandbox.store().catalog().expect("read the catalog");
    assert!(catalog.get("other").is_none());
}

#[test]
fn verify_reports_a_changed_copy_without_rewriting_it() {
    let sandbox = Sandbox::new();
    let source = sandbox.extension_source("hello-src", "hello", "1.0.0");
    assert_success(&sandbox.run(&["package", "install", &path_text(&source)]));

    let catalog = sandbox.store().catalog().expect("read the catalog");
    let record = catalog.get("hello").expect("the record");
    let digest = record.digest.as_deref().expect("a published copy");
    let executable = record.executable.as_deref().expect("an executable");
    let published = sandbox.store().store_dir().join(digest).join(executable);
    let metadata = fs::metadata(&published).expect("inspect the published executable");
    make_owner_writable(&published, metadata.permissions()).expect("thaw the published executable");
    fs::write(&published, b"not the published copy").expect("damage the published copy");

    let stderr = assert_failure(&sandbox.run(&["package", "verify"]));
    assert!(stderr.contains("stored package hello"), "{stderr}");
    assert!(published.exists(), "verify must leave the damaged copy in place");
}

#[test]
fn list_and_verify_never_install_from_the_inbox() {
    let sandbox = Sandbox::new();
    let store = sandbox.store();
    write_extension_root(&store.inbox_dir().join("hello"), "hello", "1.0.0");

    let output = sandbox.run(&["package", "list"]);
    assert_eq!(assert_success(&output).trim(), "No packages installed.");

    let output = sandbox.run(&["package", "verify"]);
    assert_eq!(
        assert_success(&output).trim(),
        "Verified 0 packages at catalog generation 0."
    );

    assert!(store.inbox_dir().join("hello").is_dir(), "the inbox entry stays put");
    assert!(
        store.catalog().expect("read the catalog").is_empty(),
        "a read-only command must not install an inbox entry"
    );
}

// ---------------------------------------------------------------------------
// Inbox
// ---------------------------------------------------------------------------

fn skipped(report: &horon_agent::packages::InboxReport, name: &str) -> String {
    let entry = report
        .entries
        .iter()
        .find(|entry| entry.name == name)
        .unwrap_or_else(|| panic!("no inbox entry named {name}: {report:?}"));
    match &entry.outcome {
        InboxOutcome::Skipped(reason) => reason.clone(),
        other => panic!("inbox entry {name} was not skipped: {other:?}"),
    }
}

#[test]
fn inbox_installs_complete_entries_without_running_them() {
    let sandbox = Sandbox::new();
    let store = sandbox.store();
    let inbox = store.inbox_dir();
    write_extension_root(&inbox.join("hello"), "hello", "1.0.0");
    write_plugin_root(&inbox.join("widget"), "widget", "0.1.0");

    let report = store.sync_inbox().expect("sync the inbox");
    assert_eq!(report.entries.len(), 2, "{report:?}");
    assert_eq!(report.installed().count(), 2, "{report:?}");

    let catalog = store.catalog().expect("read the catalog");
    assert_eq!(catalog.generation(), 2);
    let hello = catalog.get("hello").expect("the extension record");
    assert_eq!(hello.kind, PackageKind::Extension);
    assert_eq!(hello.scope, PackageScope::Global);
    assert_eq!(hello.source, canonical_text(&inbox.join("hello")));
    assert_eq!(
        catalog.get("widget").expect("the plugin record").kind,
        PackageKind::Plugin
    );

    // The copy holds the fixture bytes exactly: the executable was treated as
    // data, never started. The fixture is a bare header, so starting it would
    // fail the way any other malformed image does.
    let digest = hello.digest.as_deref().expect("a published copy");
    let executable = hello.executable.as_deref().expect("an executable");
    let published = store.store_dir().join(digest).join(executable);
    assert_eq!(
        fs::read(&published).expect("read the published executable"),
        host_bytes()
    );

    // The dropped source is never deleted; the catalog's record of its ID is
    // what stops a second pass from installing it again.
    assert!(inbox.join("hello").join("extension.toml").is_file());
    let again = store.sync_inbox().expect("sync the same inbox again");
    assert_eq!(again.installed().count(), 0, "{again:?}");
    assert!(skipped(&again, "hello").contains("already installed"));
    assert!(skipped(&again, "widget").contains("already installed"));
    assert_eq!(store.catalog().expect("read the catalog").generation(), 2);
}

#[test]
fn inbox_diagnoses_entries_it_cannot_install() {
    let sandbox = Sandbox::new();
    let store = sandbox.store();
    let inbox = store.inbox_dir();
    write_extension_root(&inbox.join(".staging-hello"), "hello", "1.0.0");
    write_extension_root(&inbox.join("hello.tmp"), "hello", "1.0.0");
    write_extension_root(&inbox.join("other"), "hello", "1.0.0");
    fs::create_dir_all(inbox.join("partial")).expect("create an incomplete entry");
    fs::write(inbox.join("plainfile"), "not a package").expect("write a file entry");
    let target = sandbox.workspace.path().join("hello-src");
    write_extension_root(&target, "hello", "1.0.0");
    link_directory(&target, &inbox.join("linked"));

    let report = store.sync_inbox().expect("sync the inbox");
    assert_eq!(report.entries.len(), 6, "{report:?}");
    assert_eq!(report.installed().count(), 0, "{report:?}");
    assert!(skipped(&report, ".staging-hello").contains("not a package ID"));
    assert!(skipped(&report, "hello.tmp").contains("not a package ID"));
    assert!(skipped(&report, "other").contains("declares ID hello, expected other"));
    assert!(skipped(&report, "partial").contains("exactly one of extension.toml and plugin.toml"));
    assert!(skipped(&report, "plainfile").contains("not a directory"));
    assert!(skipped(&report, "linked").contains("links are not allowed"));
    assert!(store.catalog().expect("read the catalog").is_empty());

    // Every rejected entry is still there for an operator to fix or delete.
    for name in [".staging-hello", "hello.tmp", "other", "partial", "plainfile", "linked"] {
        assert!(
            fs::symlink_metadata(inbox.join(name)).is_ok(),
            "{name} must stay in the inbox"
        );
    }
}

#[test]
fn inbox_skips_installed_and_removed_ids() {
    let sandbox = Sandbox::new();
    let store = sandbox.store();
    let installed = sandbox.extension_source("dup-src", "dup", "1.0.0");
    let removed = sandbox.extension_source("gone-src", "gone", "1.0.0");
    assert_success(&sandbox.run(&["package", "install", &path_text(&installed)]));
    assert_success(&sandbox.run(&["package", "install", &path_text(&removed)]));
    assert_success(&sandbox.run(&["package", "remove", "gone"]));

    write_extension_root(&store.inbox_dir().join("dup"), "dup", "9.9.9");
    write_extension_root(&store.inbox_dir().join("gone"), "gone", "9.9.9");
    let report = store.sync_inbox().expect("sync the inbox");
    assert_eq!(report.installed().count(), 0, "{report:?}");
    assert!(skipped(&report, "dup").contains("already installed"));
    assert!(skipped(&report, "gone").contains("was removed"));

    let catalog = store.catalog().expect("read the catalog");
    let dup = catalog.get("dup").expect("the installed record");
    assert_eq!(dup.version, "1.0.0");
    assert_eq!(dup.source, canonical_text(&installed));
    let gone = catalog.get("gone").expect("the revoked record");
    assert_eq!(gone.state, PackageState::Revoked);
    assert!(gone.digest.is_none());
}

#[test]
fn a_removed_id_is_not_reclaimed_from_the_original_source() {
    let sandbox = Sandbox::new();
    let store = sandbox.store();
    let source = sandbox.extension_source("hello-src", "hello", "1.0.0");
    assert_success(&sandbox.run(&["package", "install", &path_text(&source)]));
    assert_success(&sandbox.run(&["package", "remove", "hello"]));

    // Removing a package never deletes its source. Placing that same source in
    // the inbox is not consent to install it again.
    assert!(source.join("extension.toml").is_file(), "the source stays on disk");
    copy_tree(&source, &store.inbox_dir().join("hello"));
    let report = store.sync_inbox().expect("sync the inbox");
    assert_eq!(report.installed().count(), 0, "{report:?}");
    assert!(skipped(&report, "hello").contains("was removed"));
    assert!(store.inbox_dir().join("hello").is_dir());

    let catalog = store.catalog().expect("read the catalog");
    let record = catalog.get("hello").expect("the revoked record");
    assert_eq!(record.state, PackageState::Revoked);
    assert_eq!(catalog.generation(), 2);
}

#[test]
fn inbox_refuses_a_linked_or_file_root() {
    let sandbox = Sandbox::new();
    let store = sandbox.store();
    fs::create_dir_all(store.root()).expect("create the package area");
    let target = sandbox.workspace.path().join("inbox-target");
    write_extension_root(&target, "hello", "1.0.0");
    link_directory(&target, &store.inbox_dir());
    let error = store.sync_inbox().expect_err("a linked inbox root is refused");
    assert!(format!("{error:#}").contains("must not be a link"), "{error:#}");
    assert!(store.catalog().expect("read the catalog").is_empty());

    if fs::remove_dir(store.inbox_dir()).is_err() {
        fs::remove_file(store.inbox_dir()).expect("remove the linked inbox root");
    }
    fs::write(store.inbox_dir(), "not a directory").expect("write a file at the inbox path");
    let error = store.sync_inbox().expect_err("a file at the inbox path is refused");
    assert!(format!("{error:#}").contains("is not a directory"), "{error:#}");
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
            fs::copy(&path, &target).expect("copy a source file");
        }
    }
}
