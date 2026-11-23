//! Starting an installed package executable on Windows: the Job Object that
//! owns the child's tree, and the activation environment the child starts with.
//!
//! Every scenario installs a real package into a temporary horon home. The
//! package's executable is this test binary, which is a native executable for
//! the host, so the installer validates it like any other package and the
//! launcher starts a real child process instead of a stand-in. That child is
//! told what to do by the launch environment the launcher sets, which this
//! harness process never holds: [`package_child_probe`] returns immediately in a
//! normal run and becomes the package process body when it is started. A
//! `descendant` scenario also starts a grandchild that keeps writing a
//! heartbeat, so a stop that only reached the leader is visible.
//!
//! This file is a separate integration target because it needs a native Windows
//! runner: the launcher uses a Job Object, and `.github/workflows/package-windows.yml`
//! runs it on `windows-latest` (x64) and `windows-11-arm` (arm64). It is
//! excluded from the default integration manifest for that reason; the reason
//! and the workflow live in `scripts/ci/test_target_allowlist.json`.
//!
//! The detached-daemon scenario is here because the package job must stay the
//! package child's: a daemon launched through the ordinary detached path keeps
//! running when the package child's job is terminated.
#![cfg(windows)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use horon_agent_base::packages::{
    InstallRequest, PackageProcess, PackageRecord, PackageScope, PackageStore, validate_package,
};
use horon_agent_platform::fs::{is_path_link, make_owner_writable};
use horon_agent_platform::process::package_child::{
    self, PACKAGE_ID_ENV, PACKAGE_KIND_ENV, PACKAGE_ROOT_ENV, PACKAGE_STATE_ENV, PackageChildSpec,
};
use horon_agent_platform::{is_process_running, set_permissions_executable, spawn_detached};
use tempfile::{TempDir, tempdir};

/// The test the launcher's filter selects inside the installed copy.
const PROBE: &str = "package_child_probe";
/// File the probe writes its mode from; the launcher's state directory holds it.
const MODE_MARKER: &str = "mode.txt";
/// File the probe writes last, once its markers and pipes are in place.
const READY_MARKER: &str = "ready.txt";
const ENV_MARKER: &str = "env.txt";
const ARGV_MARKER: &str = "argv.txt";
const CWD_MARKER: &str = "cwd.txt";
const HEARTBEAT_MARKER: &str = "descendant-heartbeat.txt";
/// The script the probe's descendant runs while the host still owns the tree.
const DESCENDANT_SCRIPT: &str = "descendant.cmd";
/// Tokens the probe writes to its redirected standard streams.
const STDOUT_TOKEN: &str = "HORON-PACKAGE-PROBE-STDOUT";
const STDERR_TOKEN: &str = "HORON-PACKAGE-PROBE-STDERR";
/// The variables a Windows package child inherits from this process.
const INHERITED_ENVIRONMENT: [&str; 9] = [
    "PATH",
    "PATHEXT",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "OS",
];

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
    let descendant = if mode.trim() == "descendant" {
        let script = state.join(DESCENDANT_SCRIPT);
        fs::write(
            &script,
            concat!(
                "@echo off\r\n",
                ":loop\r\n",
                ">>\"%~dp0descendant-heartbeat.txt\" echo .\r\n",
                "ping -n 2 127.0.0.1 >nul\r\n",
                "goto loop\r\n"
            ),
        )
        .expect("write the descendant script");
        // The descendant inherits this child's console and its job: whatever
        // owns this process owns the descendant too.
        Some(
            Command::new("cmd.exe")
                .args(["/D", "/C"])
                .arg(&script)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("start the descendant"),
        )
    } else {
        None
    };

    println!("{STDOUT_TOKEN}");
    eprintln!("{STDERR_TOKEN}");
    fs::write(state.join(READY_MARKER), b"ready").expect("write the ready marker");

    // The host owns the other end: it stops the child by cancelling it, and a
    // natural exit happens when the host closes stdin.
    let mut discarded = Vec::new();
    std::io::stdin()
        .read_to_end(&mut discarded)
        .expect("read standard input until the host closes it");
    if let Some(mut child) = descendant {
        let _ = child.try_wait();
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn host_target() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "aarch64-pc-windows-msvc",
        "x86_64" => "x86_64-pc-windows-msvc",
        arch => panic!("package_spawn_windows needs a Windows runner, got {arch}"),
    }
}

fn binary_name() -> &'static str {
    "bin/child.exe"
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

enum Fixture {
    Extension,
    Plugin,
}

fn fixture(kind: Fixture, id: &str) -> TempDir {
    match kind {
        Fixture::Extension => extension_fixture(id, "0.1.0"),
        Fixture::Plugin => plugin_fixture(id, "0.1.0"),
    }
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

    /// A horon home whose path contains a space, as a user's own home can.
    fn with_spaces() -> Self {
        let home = tempdir().expect("horon home");
        let spaced = home.path().join("horon home");
        fs::create_dir_all(&spaced).expect("create the spaced horon home");
        let store = PackageStore::for_horon_home(&spaced);
        Self { _home: home, store }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        thaw(self.store.root());
    }
}

fn install(store: &PackageStore, source: &Path) -> PackageRecord {
    store
        .install(InstallRequest::new(source, PackageScope::Global))
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

/// One running package process with the directories its launch reported.
struct Started {
    process: Option<PackageProcess>,
    root: PathBuf,
    state: PathBuf,
    installed: PathBuf,
    _harness: Harness,
    _fixture: TempDir,
}

impl Started {
    fn process(&mut self) -> &mut PackageProcess {
        self.process.as_mut().expect("the child is still owned")
    }
}

/// Install `kind`, prepare its executable and start the probe in `mode`.
fn start_child(harness: Harness, kind: Fixture, id: &str, mode: &str) -> Started {
    let fixture = fixture(kind, id);
    install(&harness.store, fixture.path());
    let executable = harness
        .store
        .prepare_executable(harness.store.lease(&format!("{id}-session")).expect("take a lease"), id)
        .expect("prepare the installed executable");
    let root = executable.root().to_path_buf();
    let state = executable.state_dir().to_path_buf();
    let installed = executable.executable().to_path_buf();
    write_mode(&state, mode);
    let process = executable
        .start_with_arguments(&probe_arguments())
        .expect("start the package child");
    Started {
        process: Some(process),
        root,
        state,
        installed,
        _harness: harness,
        _fixture: fixture,
    }
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
        "USERPROFILE",
        "HOME",
        "TEMP",
        "TMP",
    ] {
        keys.insert(key.to_owned());
    }
    keys
}

/// One spelling of a directory path, for comparing the child's report.
///
/// Windows reports the directory a child was given, while `canonicalize`
/// returns the resolved long form with a `\\?\` prefix, so both sides take this
/// form before they are compared.
fn resolved(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Wait until the heartbeat file stops growing.
///
/// Its writer is a descendant the package left behind. The file is the
/// evidence: the pid alone would not tell a running process from a job member
/// that was terminated.
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
    if is_path_link(&metadata) {
        return;
    }
    let writable = metadata.permissions();
    if make_owner_writable(path, writable).is_err() {
        return;
    }
    if metadata.is_dir()
        && let Ok(entries) = fs::read_dir(path)
    {
        for entry in entries.flatten() {
            thaw(&entry.path());
        }
    }
}

// ---------------------------------------------------------------------------
// The Job Object that owns the child's tree
// ---------------------------------------------------------------------------

#[test]
fn extension_child_and_descendant_start_inside_the_job() {
    let mut started = start_child(Harness::new(), Fixture::Extension, "spawn-hello", "descendant");
    let state = started.state.clone();
    let root = started.root.clone();
    wait_for(&state.join(READY_MARKER), "the ready marker");
    let heartbeat = state.join(HEARTBEAT_MARKER);
    wait_for_growth(&heartbeat, 6);

    let stdout = started.process().take_stdout().expect("the child's standard output");
    let stderr = started.process().take_stderr().expect("the child's standard error");
    let stdin = started.process().take_stdin();
    assert!(stdin.is_some(), "the child must have a standard input pipe");
    assert!(
        started.process().try_wait().expect("inspect the child").is_none(),
        "the child must still be running while the marker is observed",
    );

    let arguments: Vec<String> = wait_for(&state.join(ARGV_MARKER), "the received arguments")
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(arguments.len(), 2 + probe_arguments().len());
    assert_eq!(arguments[0], started.installed.to_string_lossy());
    assert_eq!(arguments[1], root.to_string_lossy());
    let cwd = wait_for(&state.join(CWD_MARKER), "the working directory");
    assert_eq!(resolved(Path::new(&cwd)), resolved(&root));

    let environment = parse_environment(&wait_for(&state.join(ENV_MARKER), "the received environment"));
    let keys: BTreeSet<String> = environment.keys().cloned().collect();
    assert_eq!(
        keys,
        expected_environment_keys(),
        "the package child must see exactly the activation environment, so no host variable leaks",
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
        environment.get("USERPROFILE").map(String::as_str),
        Some(state.to_string_lossy().as_ref()),
        "the child's profile must be its own state directory",
    );
    assert_eq!(
        environment.get("HOME").map(String::as_str),
        Some(state.to_string_lossy().as_ref()),
        "the child's home must be its own state directory",
    );
    for key in ["TEMP", "TMP"] {
        assert_eq!(
            environment.get(key).map(String::as_str),
            Some(state.join("tmp").to_string_lossy().as_ref()),
            "{key} must point inside the package state directory",
        );
    }
    // Cancelling has to remove the leader and a descendant that is still
    // writing. Windows has no graceful request to ignore, so the job's
    // termination is the whole stop.
    drop(stdin);
    let leader = started.process().pid();
    let status = started.process().terminate().expect("stop the process tree");
    assert_eq!(
        started.process().terminate().expect("terminate is idempotent"),
        status,
        "a settled leader reports the same exit status and is not stopped again",
    );
    assert!(
        !is_process_running(leader),
        "the leader must be gone after cancellation"
    );
    wait_for_quiet(&heartbeat, "the cancelled descendant");

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
fn plugin_child_and_descendant_start_inside_the_job() {
    let mut started = start_child(Harness::new(), Fixture::Plugin, "spawn-plugin", "descendant");
    let state = started.state.clone();
    let root = started.root.clone();
    let bundled = Path::new("native").join(binary_name());
    assert!(
        started.installed.ends_with(&bundled),
        "the plugin runs the bundled extension executable: {}",
        started.installed.display(),
    );
    wait_for(&state.join(READY_MARKER), "the ready marker");
    let heartbeat = state.join(HEARTBEAT_MARKER);
    wait_for_growth(&heartbeat, 6);

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

    // The owner is the only lifetime handle: dropping it without a shutdown has
    // to stop the tree, because nothing else can.
    let leader = started.process().pid();
    assert!(
        is_process_running(leader),
        "the child must be running before the owner drops"
    );
    drop(started.process.take());
    assert!(!is_process_running(leader), "dropping the owner must stop the child");
    wait_for_quiet(&heartbeat, "the descendant of the dropped owner");
}

#[test]
fn a_package_home_with_spaces_starts_and_stops() {
    let mut started = start_child(Harness::with_spaces(), Fixture::Extension, "spawn-spaces", "plain");
    let state = started.state.clone();
    assert!(
        started.root.to_string_lossy().contains(' '),
        "the fixture must exercise a path with a space: {}",
        started.root.display(),
    );
    wait_for(&state.join(READY_MARKER), "the ready marker");

    let arguments: Vec<String> = wait_for(&state.join(ARGV_MARKER), "the received arguments")
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(arguments[0], started.installed.to_string_lossy());
    assert_eq!(arguments[1], started.root.to_string_lossy());
    let environment = parse_environment(&wait_for(&state.join(ENV_MARKER), "the received environment"));
    assert_eq!(
        environment.get(PACKAGE_ROOT_ENV).map(String::as_str),
        Some(started.root.to_string_lossy().as_ref()),
        "the spaced root must reach the child unchanged",
    );

    let leader = started.process().pid();
    started.process().terminate().expect("stop the spaced-path child");
    assert!(!is_process_running(leader), "the child must be gone after cancellation");
}

#[test]
fn a_program_that_is_not_an_image_is_refused_before_it_runs() {
    let fixture = tempdir().expect("fixture directory");
    let program = fixture.path().join("not an image.exe");
    fs::write(&program, b"this file is not a Windows image").expect("write the invalid program");
    let arguments = [fixture.path().as_os_str().to_owned()];
    let state_dir = fixture.path();

    let invalid = PackageChildSpec {
        program: &program,
        arguments: &arguments,
        root: state_dir,
        id: "broken-image",
        kind: "extension",
        state_dir,
        grace: Duration::from_millis(50),
    };
    let error = match package_child::spawn(&invalid) {
        Ok(_started) => panic!("a file that is not a Windows image must not start"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("not an image.exe"),
        "the refusal must name the executable that was refused: {error}",
    );

    // A directory is not a program either, and it is refused before any process
    // exists as well.
    let directory = PackageChildSpec {
        program: state_dir,
        arguments: &arguments,
        root: state_dir,
        id: "broken-directory",
        kind: "extension",
        state_dir,
        grace: Duration::from_millis(50),
    };
    assert!(
        package_child::spawn(&directory).is_err(),
        "a directory is not a startable package executable",
    );
}

#[test]
fn a_detached_daemon_is_not_joined_to_the_package_job() {
    let mut started = start_child(Harness::new(), Fixture::Extension, "spawn-detached", "plain");
    let state = started.state.clone();
    wait_for(&state.join(READY_MARKER), "the ready marker");

    // The daemon is started the ordinary detached way, from this host, while the
    // package child's job is alive. If the launcher ever put the host or an
    // ordinary detached child into that job, terminating the job below would end
    // this process too.
    let script = state.join("detached.cmd");
    fs::write(
        &script,
        "@echo off\r\nping -n 8 127.0.0.1 >nul\r\necho survived>\"%~dp0detached-survived.txt\"\r\n",
    )
    .expect("write the detached daemon script");
    let mut command = Command::new("cmd.exe");
    command
        .args(["/D", "/C"])
        .arg(&script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut daemon = spawn_detached(&mut command).expect("start a detached daemon");
    let daemon_id = daemon.id();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !is_process_running(daemon_id) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        is_process_running(daemon_id),
        "the detached daemon must be running first"
    );

    let leader = started.process().pid();
    started.process().terminate().expect("stop the package child's job");
    assert!(!is_process_running(leader), "the package child must be gone");
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        is_process_running(daemon_id),
        "a detached daemon must not be a member of the package Job Object",
    );
    assert!(
        !state.join("detached-survived.txt").exists(),
        "the detached daemon must still be running instead of having finished",
    );

    drop(daemon.kill());
    drop(daemon.wait());
}
