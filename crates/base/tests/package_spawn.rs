//! Starting an installed package executable: the bytes that are allowed to run,
//! the activation environment, and the process tree the launch leaves behind.
//!
//! Every scenario installs a real package into a temporary horon home. The
//! package's executable is this test binary, which is a native executable for
//! the host, so the installer validates it like any other package and the
//! launcher starts a real child process instead of a stand-in. That child is
//! told what to do by the launch environment the launcher sets, which this
//! harness process never holds: [`package_child_probe`] returns immediately in
//! a normal run and becomes the package process body when it is started.
//!
//! Windows has its own target, `package_spawn_windows`, because its supervision
//! contract is a Job Object rather than a process group and it needs a native
//! Windows runner. These scenarios return early there instead of repeating that
//! target's coverage from another platform.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use horon_agent_base::packages::{
    InstallRequest, PackageKind, PackageProcess, PackageRecord, PackageScope, PackageState, PackageStore,
    validate_package,
};
use horon_agent_platform::fs::{make_owner_writable, symlink_file};
use horon_agent_platform::process::package_child::{
    PACKAGE_ID_ENV, PACKAGE_KIND_ENV, PACKAGE_ROOT_ENV, PACKAGE_STATE_ENV,
};
use horon_agent_platform::{is_process_running, set_permissions_executable};
use tempfile::{TempDir, tempdir};

/// The test the launcher's filter selects inside the installed copy.
const PROBE: &str = "package_child_probe";
const SECRET_PROBE_ENV: &str = "HORON_PACKAGE_SPAWN_SECRET_PROBE";
/// File the probe writes its mode from; the launcher's state directory holds it.
const MODE_MARKER: &str = "mode.txt";
/// File the probe writes last, once its markers and pipes are in place.
const READY_MARKER: &str = "ready.txt";
const ENV_MARKER: &str = "env.txt";
const ARGV_MARKER: &str = "argv.txt";
const CWD_MARKER: &str = "cwd.txt";
const HEARTBEAT_MARKER: &str = "descendant-heartbeat.txt";
/// Tokens the probe writes to its redirected standard streams.
const STDOUT_TOKEN: &str = "HORON-PACKAGE-PROBE-STDOUT";
const STDERR_TOKEN: &str = "HORON-PACKAGE-PROBE-STDERR";
/// The variables a package child inherits from this process.
const INHERITED_ENVIRONMENT: [&str; 5] = ["PATH", "LANG", "LC_ALL", "LC_CTYPE", "TZ"];

// ---------------------------------------------------------------------------
// The package process body
// ---------------------------------------------------------------------------

/// The package process body.
///
/// The launcher starts the installed copy of this binary and selects this
/// probe. It reports what the child received, starts a descendant when the
/// scenario needs one, and waits for the host to close stdin.
#[test]
#[ignore = "run only in the installed child process"]
fn package_child_probe() {
    let state = std::env::var_os(PACKAGE_STATE_ENV).expect("the launcher sets the package state directory");
    let state = PathBuf::from(state);

    let arguments: Vec<String> = std::env::args_os()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    fs::write(state.join(ARGV_MARKER), arguments.join("\n")).expect("write the received arguments");
    let mut environment = String::new();
    for (key, value) in std::env::vars_os() {
        environment.push_str(&key.to_string_lossy());
        environment.push('=');
        environment.push_str(&value.to_string_lossy());
        environment.push('\n');
    }
    fs::write(state.join(ENV_MARKER), environment).expect("write the received environment");
    let cwd = std::env::current_dir().expect("the working directory the launcher set");
    fs::write(state.join(CWD_MARKER), cwd.to_string_lossy().as_bytes()).expect("write the working directory");

    let mode = fs::read_to_string(state.join(MODE_MARKER)).expect("read the scenario mode");
    if mode.trim() == "descendant" {
        let heartbeat = state.join(HEARTBEAT_MARKER);
        Command::new("/bin/sh")
            .arg("-c")
            .arg("trap '' TERM; while :; do printf . >> \"$HORON_PACKAGE_PROBE_HEARTBEAT\"; sleep 0.05; done")
            .env("HORON_PACKAGE_PROBE_HEARTBEAT", &heartbeat)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the descendant");
    }

    println!("{STDOUT_TOKEN}");
    eprintln!("{STDERR_TOKEN}");
    fs::write(state.join(READY_MARKER), b"ready").expect("write the ready marker");

    // The host owns the other end: it stops the child by cancelling it, and a
    // natural exit happens when the host closes stdin.
    let mut discarded = Vec::new();
    std::io::stdin()
        .read_to_end(&mut discarded)
        .expect("read standard input until the host closes it");
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Whether these scenarios run on this platform.
///
/// Windows keeps the same contract through a Job Object and covers it in
/// `package_spawn_windows` on native runners, so the Unix scenarios stop here.
fn supervision_supported() -> bool {
    std::env::consts::OS != "windows"
}

fn host_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        (os, arch) => panic!("package_spawn needs a supported runner, got {os}/{arch}"),
    }
}

fn binary_name() -> &'static str {
    if std::env::consts::OS == "windows" {
        "bin/child.exe"
    } else {
        "bin/child"
    }
}

/// Write an `extension.toml` and stage this test binary as its executable.
fn write_extension(root: &Path, id: &str, version: &str) {
    let binary = binary_name();
    let destination = root.join(binary);
    fs::create_dir_all(destination.parent().expect("the fixture binary directory"))
        .expect("create the fixture binary directory");
    fs::copy(
        std::env::current_exe().expect("the native test binary path"),
        &destination,
    )
    .expect("stage the native test binary as the package executable");
    set_permissions_executable(&destination).expect("mark the package executable runnable");
    fs::write(
        root.join("extension.toml"),
        format!(
            r#"
schema_version = 1
id = "{id}"
version = "{version}"
name = "Spawn {id}"
protocol = "1.0"
[targets]
{target} = "{binary}"
"#,
            target = host_target(),
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
    _home: TempDir,
    store: PackageStore,
}

impl Harness {
    fn new() -> Self {
        let home = tempdir().expect("horon home");
        let store = PackageStore::for_horon_home(home.path());
        Self { _home: home, store }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        thaw(self.store.root());
    }
}

fn install(store: &PackageStore, source: &Path, replace: bool) -> PackageRecord {
    let request = InstallRequest::new(source, PackageScope::Global);
    store
        .install(if replace { request.replacing() } else { request })
        .expect("install the package fixture")
}

fn write_mode(state: &Path, mode: &str) {
    fs::write(state.join(MODE_MARKER), mode).expect("write the scenario mode");
}

/// The arguments that make the installed copy run only the probe.
fn probe_arguments() -> Vec<OsString> {
    vec![
        OsString::from("--exact"),
        OsString::from("--ignored"),
        OsString::from(PROBE),
        OsString::from("--nocapture"),
        OsString::from("--test-threads=1"),
    ]
}

// ---------------------------------------------------------------------------
// Observation helpers
// ---------------------------------------------------------------------------

fn wait_for(path: &Path, what: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match fs::read_to_string(path) {
            Ok(contents) => return contents,
            Err(error) => {
                assert!(
                    Instant::now() < deadline,
                    "{what} was not written at {}: {error}",
                    path.display(),
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

fn parse_environment(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| {
            line.split_once('=')
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
        })
        .collect()
}

fn expected_environment_keys() -> BTreeSet<String> {
    let mut keys: BTreeSet<String> = INHERITED_ENVIRONMENT
        .into_iter()
        .filter(|key| std::env::var_os(key).is_some())
        .map(str::to_owned)
        .collect();
    for key in [
        PACKAGE_ID_ENV,
        PACKAGE_KIND_ENV,
        PACKAGE_ROOT_ENV,
        PACKAGE_STATE_ENV,
        "HOME",
        "TMPDIR",
    ] {
        keys.insert(key.to_owned());
    }
    keys
}

/// Wait until the heartbeat file stops growing.
///
/// Its writer is a descendant the package left behind. Its pid says nothing once
/// it is a zombie that PID 1 has not reaped, so the file is the evidence that it
/// no longer runs.
fn wait_for_quiet(path: &Path, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut previous = fs::metadata(path).expect("the heartbeat file").len();
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let current = fs::metadata(path).expect("the heartbeat file").len();
        if current == previous {
            return;
        }
        assert!(Instant::now() < deadline, "{what} kept writing to {}", path.display(),);
        previous = current;
    }
}

/// Poll the process owner for the leader's exit status.
fn wait_for_status(process: &mut PackageProcess) -> ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = process.try_wait().expect("inspect the package child") {
            return status;
        }
        assert!(Instant::now() < deadline, "the package child did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_growth(path: &Path, minimum: u64) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let len = fs::metadata(path).map(|metadata| metadata.len()).unwrap_or(0);
        if len >= minimum {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{} never reached {minimum} bytes",
            path.display(),
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Clear the read-only restriction a published copy carries, so the temporary
/// home can remove itself.
fn thaw(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if horon_agent_platform::fs::is_path_link(&metadata) {
        return;
    }
    let writable = metadata.permissions();
    if make_owner_writable(path, writable).is_err() {
        return;
    }
    if metadata.is_dir() {
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                thaw(&entry.path());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The activation contract
// ---------------------------------------------------------------------------

#[test]
fn extension_child_starts_with_the_activation_environment() {
    if !supervision_supported() {
        return;
    }
    if std::env::var_os(SECRET_PROBE_ENV).is_none() {
        let output = Command::new(std::env::current_exe().expect("the native test executable"))
            .args([
                "--exact",
                "extension_child_starts_with_the_activation_environment",
                "--nocapture",
            ])
            .env(SECRET_PROBE_ENV, "1")
            .env("OPENAI_API_KEY", "test-provider-key")
            .env("HORON_TEST_SECRET", "test-secret")
            .output()
            .expect("start a parent process with test secrets");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let harness = Harness::new();
    let fixture = extension_fixture("spawn-hello", "0.1.0");
    let record = install(&harness.store, fixture.path(), false);
    assert_eq!(record.state, PackageState::Installed);
    assert_eq!(record.kind, PackageKind::Extension);

    let executable = harness
        .store
        .prepare_executable(
            harness.store.lease("spawn-session").expect("take a lease"),
            "spawn-hello",
        )
        .expect("prepare the installed executable");
    assert_eq!(executable.id(), "spawn-hello");
    assert_eq!(executable.generation(), record.generation);
    assert_eq!(executable.record().digest, record.digest);
    assert_eq!(executable.executable(), executable.root().join(binary_name()).as_path(),);
    assert!(executable.root().is_absolute() && executable.state_dir().is_absolute());
    assert_eq!(executable.arguments(), vec![executable.root().as_os_str().to_owned()],);
    let catalog = harness.store.catalog().expect("read the catalog");
    assert_eq!(catalog.get("spawn-hello").expect("the record").digest, record.digest,);

    let root = executable.root().to_path_buf();
    let state = executable.state_dir().to_path_buf();
    let installed = executable.executable().to_path_buf();
    write_mode(&state, "plain");
    assert_eq!(std::env::var("OPENAI_API_KEY").as_deref(), Ok("test-provider-key"));
    assert_eq!(std::env::var("HORON_TEST_SECRET").as_deref(), Ok("test-secret"));

    let mut process = executable
        .start_with_arguments(&probe_arguments())
        .expect("start the package child");
    wait_for(&state.join(READY_MARKER), "the ready marker");
    let stdout = process.take_stdout().expect("the child's standard output");
    let stderr = process.take_stderr().expect("the child's standard error");
    let stdin = process.take_stdin();
    assert!(stdin.is_some(), "the child must have a standard input pipe");
    assert!(
        process.try_wait().expect("inspect the child").is_none(),
        "the child must still be running while the marker is observed",
    );

    let arguments: Vec<String> = wait_for(&state.join(ARGV_MARKER), "the received arguments")
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(arguments.len(), 2 + probe_arguments().len());
    assert_eq!(arguments[0], installed.to_string_lossy());
    assert_eq!(arguments[1], root.to_string_lossy());
    // The launcher passes the configured copy path; the kernel reports the
    // resolved working directory, which differs where the temporary directory
    // is a link (`/var` to `/private/var` on macOS).
    let expected_cwd = fs::canonicalize(&root).expect("resolve the package root");
    let cwd = wait_for(&state.join(CWD_MARKER), "the working directory");
    assert_eq!(cwd, expected_cwd.to_string_lossy());

    let environment = parse_environment(&wait_for(&state.join(ENV_MARKER), "the received environment"));
    let keys: BTreeSet<String> = environment
        .keys()
        // CoreFoundation may set this key after exec on macOS. The launcher
        // does not pass it through from the parent.
        .filter(|key| std::env::consts::OS != "macos" || key.as_str() != "__CF_USER_TEXT_ENCODING")
        .cloned()
        .collect();
    assert_eq!(
        keys,
        expected_environment_keys(),
        "the package child must see exactly the activation environment",
    );
    assert_eq!(environment.get(PACKAGE_ID_ENV).map(String::as_str), Some("spawn-hello"),);
    assert_eq!(environment.get(PACKAGE_KIND_ENV).map(String::as_str), Some("extension"),);
    assert_eq!(
        environment.get(PACKAGE_ROOT_ENV).map(String::as_str),
        Some(root.to_string_lossy().as_ref()),
    );
    assert_eq!(
        environment.get(PACKAGE_STATE_ENV).map(String::as_str),
        Some(state.to_string_lossy().as_ref()),
    );
    assert_eq!(
        environment.get("HOME").map(String::as_str),
        Some(state.to_string_lossy().as_ref()),
        "the child's home must be its own state directory",
    );
    assert_eq!(
        environment.get("TMPDIR").map(String::as_str),
        Some(state.join("tmp").to_string_lossy().as_ref()),
    );
    assert!(!environment.contains_key("OPENAI_API_KEY"), "a provider key leaked",);
    assert!(!environment.contains_key("HORON_TEST_SECRET"), "a test secret leaked",);

    drop(stdin);
    process.terminate().expect("stop the package child");
    let mut out = String::new();
    std::io::BufReader::new(stdout)
        .read_to_string(&mut out)
        .expect("read the child's standard output");
    assert!(
        out.contains(STDOUT_TOKEN),
        "standard output was not the child's pipe: {out}",
    );
    let mut err = String::new();
    std::io::BufReader::new(stderr)
        .read_to_string(&mut err)
        .expect("read the child's standard error");
    assert!(
        err.contains(STDERR_TOKEN),
        "standard error was not the child's pipe: {err}",
    );
}

#[test]
fn plugin_child_starts_from_its_bundled_root() {
    if !supervision_supported() {
        return;
    }
    let harness = Harness::new();
    let fixture = plugin_fixture("spawn-plugin", "0.1.0");
    let record = install(&harness.store, fixture.path(), false);
    assert_eq!(record.kind, PackageKind::Plugin);
    let executable = harness
        .store
        .prepare_executable(
            harness.store.lease("spawn-plugin-session").expect("take a lease"),
            "spawn-plugin",
        )
        .expect("prepare the installed executable");
    let root = executable.root().to_path_buf();
    let state = executable.state_dir().to_path_buf();
    let bundled = Path::new("native").join(binary_name());
    assert!(
        executable.executable().ends_with(&bundled),
        "the plugin runs the bundled extension executable: {}",
        executable.executable().display(),
    );
    write_mode(&state, "plain");
    let mut process = executable
        .start_with_arguments(&probe_arguments())
        .expect("start the plugin child");
    wait_for(&state.join(READY_MARKER), "the ready marker");

    let environment = parse_environment(&wait_for(&state.join(ENV_MARKER), "the received environment"));
    assert_eq!(environment.get(PACKAGE_KIND_ENV).map(String::as_str), Some("plugin"),);
    assert_eq!(
        environment.get(PACKAGE_ROOT_ENV).map(String::as_str),
        Some(root.to_string_lossy().as_ref()),
    );
    let arguments: Vec<String> = wait_for(&state.join(ARGV_MARKER), "the received arguments")
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(arguments[1], root.to_string_lossy());
    assert_eq!(
        wait_for(&state.join(CWD_MARKER), "the working directory"),
        fs::canonicalize(&root)
            .expect("resolve the package root")
            .to_string_lossy(),
    );
    process.terminate().expect("stop the plugin child");
}

// ---------------------------------------------------------------------------
// The bytes that are allowed to run
// ---------------------------------------------------------------------------

#[test]
fn a_changed_executable_is_refused_before_start() {
    if !supervision_supported() {
        return;
    }
    let harness = Harness::new();
    let fixture = extension_fixture("spawn-changed", "0.1.0");
    install(&harness.store, fixture.path(), false);
    let executable = harness
        .store
        .prepare_executable(
            harness.store.lease("spawn-changed-session").expect("take a lease"),
            "spawn-changed",
        )
        .expect("prepare the installed executable");

    let path = executable.executable().to_path_buf();
    let metadata = fs::metadata(&path).expect("inspect the installed executable");
    make_owner_writable(&path, metadata.permissions()).expect("make the installed executable writable");
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open the installed executable");
    file.write_all(b"changed-after-install")
        .expect("change the installed executable");
    drop(file);

    let error = match executable.start() {
        Ok(_started) => panic!("bytes that changed after install must be refused"),
        Err(error) => error,
    };
    assert!(
        format!("{error:#}").contains("recorded"),
        "the refusal must name the recorded bytes: {error:#}",
    );

    assert!(
        harness
            .store
            .prepare_executable(
                harness.store.lease("spawn-changed-second").expect("take a lease"),
                "spawn-changed",
            )
            .is_err(),
        "preparing the changed copy must be refused too",
    );
}

#[test]
fn a_swapped_executable_path_is_refused() {
    if !supervision_supported() {
        return;
    }
    let harness = Harness::new();
    let fixture = extension_fixture("spawn-swapped", "0.1.0");
    install(&harness.store, fixture.path(), false);
    let executable = harness
        .store
        .prepare_executable(
            harness.store.lease("spawn-swapped-session").expect("take a lease"),
            "spawn-swapped",
        )
        .expect("prepare the installed executable");

    let path = executable.executable().to_path_buf();
    let decoy = executable.state_dir().join("decoy");
    fs::copy(std::env::current_exe().expect("the native test binary path"), &decoy).expect("stage a decoy executable");
    fs::remove_file(&path).expect("remove the installed executable");
    symlink_file(&decoy, &path).expect("swap the installed executable for a link");

    let error = match executable.start() {
        Ok(_started) => panic!("a link in place of the executable must be refused"),
        Err(error) => error,
    };
    assert!(
        format!("{error:#}").contains("links are not allowed"),
        "the refusal must name the link: {error:#}",
    );

    assert!(
        harness
            .store
            .prepare_executable(
                harness.store.lease("spawn-swapped-second").expect("take a lease"),
                "spawn-swapped",
            )
            .is_err(),
        "preparing a path that became a link must be refused",
    );
}

#[test]
fn a_missing_executable_reports_the_parent_error() {
    if !supervision_supported() {
        return;
    }
    let harness = Harness::new();
    let fixture = extension_fixture("spawn-missing", "0.1.0");
    install(&harness.store, fixture.path(), false);
    let executable = harness
        .store
        .prepare_executable(
            harness.store.lease("spawn-missing-session").expect("take a lease"),
            "spawn-missing",
        )
        .expect("prepare the installed executable");
    fs::remove_file(executable.executable()).expect("remove the installed executable");

    let error = match executable.start() {
        Ok(_started) => panic!("a missing executable must be refused"),
        Err(error) => error,
    };
    assert!(
        format!("{error:#}").contains("executable"),
        "the refusal must name the executable: {error:#}",
    );
}

#[test]
fn a_lease_keeps_a_replaced_copy_startable() {
    if !supervision_supported() {
        return;
    }
    let harness = Harness::new();
    let first = extension_fixture("spawn-replace", "0.1.0");
    let installed = install(&harness.store, first.path(), false);
    let first_digest = installed.digest.clone().expect("the first copy digest");
    let executable = harness
        .store
        .prepare_executable(
            harness.store.lease("spawn-replace-session").expect("take a lease"),
            "spawn-replace",
        )
        .expect("prepare the installed executable");
    assert_eq!(executable.record().digest.as_deref(), Some(first_digest.as_str()),);
    let root = executable.root().to_path_buf();
    let state = executable.state_dir().to_path_buf();

    let second = extension_fixture("spawn-replace", "0.2.0");
    let replaced = install(&harness.store, second.path(), true);
    assert_ne!(replaced.digest.as_deref(), Some(first_digest.as_str()));

    write_mode(&state, "plain");
    let mut process = executable
        .start_with_arguments(&probe_arguments())
        .expect("the pinned copy still starts after a replacement");
    assert_eq!(process.executable().root(), root.as_path());
    wait_for(&state.join(READY_MARKER), "the ready marker");
    let collection = harness.store.collect().expect("collect unpinned copies");
    assert!(
        collection.retained.contains(&first_digest),
        "a lease-pinned copy must be retained: {collection:?}",
    );

    process.terminate().expect("stop the pinned child");
    drop(process);
    let collection = harness.store.collect().expect("collect the released copy");
    assert!(
        collection.collected.contains(&first_digest),
        "the released copy must be collected: {collection:?}",
    );
}

// ---------------------------------------------------------------------------
// Lifetime: cancellation, exit and reaping
// ---------------------------------------------------------------------------

#[test]
fn cancelling_stops_the_child_and_a_surviving_descendant() {
    if !supervision_supported() {
        return;
    }
    let harness = Harness::new();
    let fixture = extension_fixture("spawn-cancel", "0.1.0");
    install(&harness.store, fixture.path(), false);
    let executable = harness
        .store
        .prepare_executable(
            harness.store.lease("spawn-cancel-session").expect("take a lease"),
            "spawn-cancel",
        )
        .expect("prepare the installed executable");
    let state = executable.state_dir().to_path_buf();
    write_mode(&state, "descendant");
    let mut process = executable
        .start_with_arguments(&probe_arguments())
        .expect("start the package child");
    wait_for(&state.join(READY_MARKER), "the ready marker");
    let heartbeat = state.join(HEARTBEAT_MARKER);
    wait_for_growth(&heartbeat, 2);

    let leader = process.pid();
    assert!(
        is_process_running(leader),
        "the child must be running before cancellation",
    );
    let status = process.terminate().expect("stop the process tree");
    assert!(!is_process_running(leader), "the leader must be reaped by terminate",);
    // The descendant ignores the graceful request, so only the hard kill of the
    // group after the leader exited can have reached it. Its pid proves nothing
    // once it is a zombie PID 1 has not reaped, so the heartbeat it writes is the
    // evidence that it no longer runs.
    wait_for_quiet(&heartbeat, "the cancelled descendant");
    assert_eq!(
        process.terminate().expect("terminate is idempotent"),
        status,
        "a reaped leader reports the same exit status and is not signalled again",
    );
}

#[test]
fn dropping_the_owner_stops_a_running_child() {
    if !supervision_supported() {
        return;
    }
    let harness = Harness::new();
    let fixture = extension_fixture("spawn-drop", "0.1.0");
    install(&harness.store, fixture.path(), false);
    let executable = harness
        .store
        .prepare_executable(
            harness.store.lease("spawn-drop-session").expect("take a lease"),
            "spawn-drop",
        )
        .expect("prepare the installed executable");
    let state = executable.state_dir().to_path_buf();
    write_mode(&state, "plain");
    let process = executable
        .start_with_arguments(&probe_arguments())
        .expect("start the package child");
    wait_for(&state.join(READY_MARKER), "the ready marker");
    let leader = process.pid();
    assert!(
        is_process_running(leader),
        "the child must be running before the owner drops",
    );

    // The owner is the only lifetime handle: dropping it without a shutdown has
    // to stop the tree, because nothing else can.
    drop(process);
    assert!(!is_process_running(leader), "dropping the owner must reap the child",);
}

#[test]
fn a_natural_exit_sweeps_a_surviving_descendant_and_reaps() {
    if !supervision_supported() {
        return;
    }
    let harness = Harness::new();
    let fixture = extension_fixture("spawn-exit", "0.1.0");
    install(&harness.store, fixture.path(), false);
    let executable = harness
        .store
        .prepare_executable(
            harness.store.lease("spawn-exit-session").expect("take a lease"),
            "spawn-exit",
        )
        .expect("prepare the installed executable");
    let state = executable.state_dir().to_path_buf();
    write_mode(&state, "descendant");
    let mut process = executable
        .start_with_arguments(&probe_arguments())
        .expect("start the package child");
    wait_for(&state.join(READY_MARKER), "the ready marker");
    let heartbeat = state.join(HEARTBEAT_MARKER);
    wait_for_growth(&heartbeat, 2);
    let leader = process.pid();

    // Closing the host's end of stdin is how a package is told to finish. The
    // leader exits on its own, and `wait` must not answer before the descendant
    // that ignored the graceful request is gone.
    drop(process.take_stdin());
    let status = process.wait().expect("wait for the natural exit");
    assert!(status.success(), "the package child exited with {status}");
    assert!(!is_process_running(leader), "the leader must be reaped");
    wait_for_quiet(&heartbeat, "the descendant of the natural exit");
    assert_eq!(process.try_wait().expect("inspect the reaped child"), Some(status),);
    assert_eq!(
        process.terminate().expect("terminate after reaping"),
        status,
        "a reaped leader is not signalled again",
    );
}

#[test]
fn try_wait_sweeps_before_reporting_an_exit() {
    if !supervision_supported() {
        return;
    }
    let harness = Harness::new();
    let fixture = extension_fixture("spawn-poll", "0.1.0");
    install(&harness.store, fixture.path(), false);
    let executable = harness
        .store
        .prepare_executable(
            harness.store.lease("spawn-poll-session").expect("take a lease"),
            "spawn-poll",
        )
        .expect("prepare the installed executable");
    let state = executable.state_dir().to_path_buf();
    write_mode(&state, "descendant");
    let mut process = executable
        .start_with_arguments(&probe_arguments())
        .expect("start the package child");
    wait_for(&state.join(READY_MARKER), "the ready marker");
    let heartbeat = state.join(HEARTBEAT_MARKER);
    wait_for_growth(&heartbeat, 2);
    let leader = process.pid();

    drop(process.take_stdin());
    let status = wait_for_status(&mut process);
    assert!(status.success(), "the package child exited with {status}");
    assert!(!is_process_running(leader), "the leader must be reaped");
    // `try_wait` answers `Some` only after the group was swept, so the descendant
    // that ignored the graceful request has stopped writing by now.
    wait_for_quiet(&heartbeat, "the descendant of the polled exit");
    assert_eq!(process.try_wait().expect("inspect the reaped child"), Some(status),);
}
