//! A real `horon-agent serve` daemon and its API bridge, in a private horon
//! home, for the package-management suites.
//!
//! Every child runs with `HOME`, `HORON_HOME`, the XDG directories and the
//! runtime directory pointed at one scratch root, and the inherited
//! `HORON_AGENT_SOCKET`/`HORON_AGENT_API_SOCKET` are removed. Nothing here reads
//! or writes the operator's real package area, sessions or daemon sockets.
//!
//! The fixture executables are minimal headers that satisfy the host's
//! native-target check and declare that target explicitly in `[targets]`. They
//! are only copied and hashed, never started.
#![allow(dead_code)]

use std::fs;
use std::future::Future;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use horon_agent::packages::PackageStore;
use horon_agent::protocol::{PackageControlOperation, PackageControlReceipt, PackageControlRequest, ServerEvent};
use horon_agent::server::Client as DaemonClient;
use horon_agent_platform::fs::make_owner_writable;
use horon_agent_platform::set_permissions_executable;
use serde_json::Value;

pub const BINARY: &str = env!("CARGO_BIN_EXE_horon-agent");
pub const STARTUP_DEADLINE: Duration = Duration::from_secs(120);
pub const SOURCE_DEADLINE: Duration = Duration::from_secs(60);

/// A daemon, an optional bridge, and the scratch home they own.
pub struct PackageDaemon {
    pub root: PathBuf,
    pub home: PathBuf,
    pub runtime: PathBuf,
    pub socket: PathBuf,
    pub api_socket: PathBuf,
    daemon: Option<Child>,
    bridge: Option<Child>,
}

impl PackageDaemon {
    /// A daemon-only instance over a fresh scratch home.
    pub fn start(label: &str) -> Self {
        let root = scratch_root(label);
        let home = root.join("home");
        let runtime = root.join("runtime");
        fs::create_dir_all(&home).expect("create the isolated horon home");
        fs::create_dir_all(&runtime).expect("create the isolated runtime directory");
        set_mode(&home, 0o700);
        set_mode(&runtime, 0o700);
        let mut daemon = Self {
            socket: runtime.join("horon-agent.sock"),
            api_socket: root.join("api.sock"),
            root,
            home,
            runtime,
            daemon: None,
            bridge: None,
        };
        daemon.spawn_daemon();
        daemon
    }

    /// Start the API bridge over this daemon's socket.
    pub fn with_bridge(mut self) -> Self {
        self.spawn_bridge();
        self
    }

    pub fn store(&self) -> PackageStore {
        PackageStore::for_horon_home(&self.home)
    }

    /// One `horon package` command with this instance's environment.
    pub fn cli(&self, args: &[&str]) -> Output {
        self.cli_command(args).output().expect("run the horon-agent CLI")
    }

    /// The same command, left running so a caller can start two at once.
    pub fn cli_child(&self, args: &[&str]) -> Child {
        self.cli_command(args).spawn().expect("spawn the horon-agent CLI")
    }

    /// One `horon package` command whose socket is looked up in `runtime`.
    pub fn cli_with_runtime(&self, runtime: &Path, args: &[&str]) -> Output {
        let mut command = self.cli_command(args);
        command.env("HORON_AGENT_RUNTIME_DIR", runtime);
        command.output().expect("run the horon-agent CLI")
    }

    fn cli_command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BINARY);
        command
            .args(["package"])
            .args(args)
            .current_dir(&self.root)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.root)
            .env("USERPROFILE", root_profile(&self.root))
            .env("HORON_HOME", &self.home)
            .env("HORON_AGENT_RUNTIME_DIR", &self.runtime)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("NO_COLOR", "1")
            .env("HORON_AGENT_NO_AUTO_UPDATE", "1")
            .env("HORON_AGENT_NON_INTERACTIVE", "1")
            .env("HORON_AGENT_MEMORY_ENABLED", "0")
            .env("HORON_AGENT_MEMORY_SIDECAR_ENABLED", "0")
            .env_remove("HORON_AGENT_SOCKET")
            .env_remove("HORON_AGENT_API_SOCKET")
            .stdin(Stdio::null());
        command
    }

    /// Apply one operation on the daemon directly, as a one-shot client.
    pub fn request(&self, operation: PackageControlOperation) -> PackageControlReceipt {
        let socket = self.socket.clone();
        let operation = operation.clone();
        block_on(async move {
            let mut client = DaemonClient::connect_with_path(socket.clone())
                .await
                .unwrap_or_else(|error| panic!("connect to {}: {error}", socket.display()));
            let reply = client
                .package_control(PackageControlRequest {
                    operation_id: format!("test-{}-{}", std::process::id(), operation_label(&operation)),
                    operation,
                })
                .await
                .unwrap_or_else(|error| panic!("apply a package operation: {error}"));
            match reply {
                ServerEvent::PackageControlResult { receipt, .. } => receipt,
                other => panic!("unexpected package reply: {other:?}"),
            }
        })
    }

    /// Stop the daemon and the bridge, leaving the home in place.
    pub fn stop(&mut self) {
        for child in [self.bridge.take(), self.daemon.take()].into_iter().flatten() {
            stop_child(child);
        }
    }

    /// Start the daemon again over the same home, as a restart would.
    pub fn restart(&mut self) {
        self.stop();
        self.spawn_daemon();
    }

    /// Restart the daemon and the bridge together.
    pub fn restart_with_bridge(&mut self) {
        self.stop();
        self.spawn_daemon();
        self.spawn_bridge();
    }

    /// An SDK client on this instance's API bridge.
    pub fn connect_sdk(&self) -> horon_agent_sdk::HoronAgentClient {
        horon_agent_sdk::HoronAgentClient::connect(horon_agent_sdk::ConnectOptions {
            socket_path: Some(self.api_socket.clone()),
            client_name: "package-daemon-qa/1.0".into(),
            request_timeout: Some(Duration::from_secs(120)),
            ensure_runtime: false,
            supports_user_response: false,
            supports_ask_requests: false,
            supports_permission_requests: false,
            a2ui: None,
        })
        .unwrap_or_else(|error| {
            panic!(
                "connect to the API bridge at {}: {error}\n{}",
                self.api_socket.display(),
                self.logs()
            )
        })
    }

    /// Poll the daemon's published generation until it reaches `at_least`.
    pub fn wait_for_published_generation(
        &self,
        client: &horon_agent_sdk::HoronAgentClient,
        at_least: u64,
    ) -> PackageControlReceipt {
        let deadline = Instant::now() + SOURCE_DEADLINE;
        let mut attempt = 0u32;
        loop {
            let receipt = client
                .package_control(PackageControlRequest {
                    operation_id: format!("status-{}-{attempt}", std::process::id()),
                    operation: PackageControlOperation::Status,
                })
                .unwrap_or_else(|error| panic!("read the daemon's published catalog: {error}"));
            if receipt.generation >= at_least {
                return receipt;
            }
            attempt = attempt.wrapping_add(1);
            if Instant::now() >= deadline {
                panic!(
                    "the daemon never published generation {at_least}; last observed {}\n{}",
                    receipt.generation,
                    self.logs()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The daemon's own log, plus the bridge's when one ran.
    pub fn logs(&self) -> String {
        let read = |name: &str| fs::read_to_string(self.runtime.join(name)).unwrap_or_default();
        format!(
            "--- daemon.log ---\n{}\n--- bridge.log ---\n{}",
            read("daemon.log"),
            read("bridge.log")
        )
    }

    /// Poll `probe` until it returns a value or the deadline passes.
    pub fn wait_for<T>(&self, what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + SOURCE_DEADLINE;
        loop {
            if let Some(value) = probe() {
                return value;
            }
            if Instant::now() >= deadline {
                panic!("{what} did not happen within {SOURCE_DEADLINE:?}\n{}", self.logs());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The catalog generation on disk, as a reader of the published file sees it.
    pub fn generation(&self) -> u64 {
        self.store().catalog().expect("read the package catalog").generation()
    }

    /// Catalog IDs with their state, read from disk.
    pub fn catalog_ids(&self) -> Vec<(String, String)> {
        let catalog = self.store().catalog().expect("read the package catalog");
        catalog
            .packages()
            .map(|record| (record.id.clone(), format!("{:?}", record.state)))
            .collect()
    }

    fn spawn_daemon(&mut self) {
        let log = fs::File::create(self.runtime.join("daemon.log")).expect("create the daemon log");
        let errors = log.try_clone().expect("clone the daemon log");
        let mut command = Command::new(BINARY);
        command
            .args(["serve", "--no-update"])
            .current_dir(&self.root)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.root)
            .env("USERPROFILE", root_profile(&self.root))
            .env("HORON_HOME", &self.home)
            .env("HORON_AGENT_RUNTIME_DIR", &self.runtime)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("NO_COLOR", "1")
            .env("HORON_AGENT_NO_AUTO_UPDATE", "1")
            .env("HORON_AGENT_NON_INTERACTIVE", "1")
            .env("HORON_AGENT_MEMORY_ENABLED", "0")
            .env("HORON_AGENT_MEMORY_SIDECAR_ENABLED", "0")
            .env_remove("HORON_AGENT_SOCKET")
            .env_remove("HORON_AGENT_API_SOCKET")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(errors));
        self.daemon = Some(command.spawn().expect("spawn horon-agent serve"));
        wait_for_socket(
            &self.socket,
            self.daemon.as_mut().expect("started daemon"),
            &self.runtime.join("daemon.log"),
        );
    }

    fn spawn_bridge(&mut self) {
        let log = fs::File::create(self.runtime.join("bridge.log")).expect("create the bridge log");
        let errors = log.try_clone().expect("clone the bridge log");
        let api = self.api_socket.to_str().expect("UTF-8 api socket path").to_string();
        let mut command = Command::new(BINARY);
        command
            .args(["api-bridge", "--no-update", "--api-socket", api.as_str()])
            .current_dir(&self.root)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.root)
            .env("USERPROFILE", root_profile(&self.root))
            .env("HORON_HOME", &self.home)
            .env("HORON_AGENT_RUNTIME_DIR", &self.runtime)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("NO_COLOR", "1")
            .env("HORON_AGENT_NO_AUTO_UPDATE", "1")
            .env("HORON_AGENT_NON_INTERACTIVE", "1")
            .env_remove("HORON_AGENT_SOCKET")
            .env_remove("HORON_AGENT_API_SOCKET")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(errors));
        self.bridge = Some(command.spawn().expect("spawn horon-agent api-bridge"));
        wait_for_socket(
            &self.api_socket,
            self.bridge.as_mut().expect("started bridge"),
            &self.runtime.join("bridge.log"),
        );
    }
}

impl Drop for PackageDaemon {
    fn drop(&mut self) {
        self.stop();
        thaw(&self.home.join("packages"));
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// One name under `inbox/`, installed by rename so it is complete when seen.
pub fn drop_into_inbox(daemon: &PackageDaemon, name: &str, write: impl FnOnce(&Path)) {
    let inbox = daemon.store().inbox_dir();
    fs::create_dir_all(&inbox).expect("create the inbox");
    let staged = inbox
        .parent()
        .expect("the inbox has a parent")
        .join(format!(".staging-{name}-{}", std::process::id()));
    if staged.exists() {
        fs::remove_dir_all(&staged).expect("clear a stale staging directory");
    }
    write(&staged);
    fs::rename(&staged, inbox.join(name)).expect("move the complete entry into the inbox");
}

/// The extension fixture: a manifest that declares the host target explicitly
/// and a minimal executable header for it.
pub fn write_extension(root: &Path, id: &str, version: &str) {
    fs::create_dir_all(root.join("bin")).expect("create the fixture binary directory");
    let binary = "bin/child";
    let path = root.join(binary);
    fs::write(&path, host_bytes()).expect("write the fixture executable");
    set_permissions_executable(&path).expect("mark the fixture executable");
    fs::write(
        root.join("extension.toml"),
        format!(
            "schema_version = 1\nid = \"{id}\"\nversion = \"{version}\"\nname = \"Hello {id}\"\n\
             protocol = \"1.0\"\n[targets]\n{} = \"{binary}\"\n",
            host_target()
        ),
    )
    .expect("write extension.toml");
}

/// A folder that cannot be installed: the manifest is not even TOML.
pub fn write_broken_extension(root: &Path) {
    fs::create_dir_all(root.join("bin")).expect("create the fixture binary directory");
    fs::write(root.join("bin/child"), host_bytes()).expect("write the fixture executable");
    fs::write(root.join("extension.toml"), "this is not a manifest\n").expect("write a broken manifest");
}

pub fn host_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        (os, arch) => panic!("the package daemon fixture needs a supported runner, got {os}/{arch}"),
    }
}

fn host_bytes() -> Vec<u8> {
    let target = host_target();
    if target.ends_with("-darwin") {
        mach_o(target.starts_with("aarch64-"))
    } else {
        elf(target.starts_with("aarch64-"))
    }
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

/// A frame written by a client that predates package management: the legacy
/// ping, answered on the same socket with the capability list.
pub fn legacy_ping(socket: &Path) -> Value {
    let stream = UnixStream::connect(socket).expect("connect to the daemon socket");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("set the read timeout");
    let mut writer = stream.try_clone().expect("clone the daemon socket");
    writer
        .write_all(b"{\"type\":\"ping\",\"id\":1}\n")
        .expect("write the legacy ping");
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read the pong");
    serde_json::from_str(&line).expect("parse the pong")
}

pub fn assert_success(output: &Output) -> String {
    assert!(
        output.status.success(),
        "command failed with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

pub fn path_text(path: &Path) -> String {
    path.to_str().expect("UTF-8 fixture path").to_string()
}

fn operation_label(operation: &PackageControlOperation) -> &'static str {
    match operation {
        PackageControlOperation::Install { .. } => "install",
        PackageControlOperation::Disable { .. } => "disable",
        PackageControlOperation::Enable { .. } => "enable",
        PackageControlOperation::Remove { .. } => "remove",
        PackageControlOperation::SyncInbox => "sync",
        PackageControlOperation::Status => "status",
    }
}

fn root_profile(root: &Path) -> PathBuf {
    root.join("profile")
}

fn scratch_root(label: &str) -> PathBuf {
    // macOS often supplies a long TMPDIR. Use a short, canonical base so the
    // daemon and bridge sockets fit in sockaddr_un and the store sees no links.
    let base = Path::new("/tmp")
        .canonicalize()
        .expect("resolve the short temporary directory");
    tempfile::Builder::new()
        .prefix(&format!("hp-{label}-"))
        .tempdir_in(base)
        .expect("create the private package test directory")
        .keep()
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("set the fixture directory mode");
}

fn wait_for_socket(path: &Path, child: &mut Child, log: &Path) {
    let deadline = Instant::now() + STARTUP_DEADLINE;
    loop {
        if let Some(status) = child.try_wait().expect("check socket owner") {
            let output = fs::read_to_string(log).unwrap_or_default();
            panic!(
                "process exited with {status} before socket {} opened\n{output}",
                path.display()
            );
        }
        if UnixStream::connect(path).is_ok() {
            return;
        }
        if Instant::now() >= deadline {
            let output = fs::read_to_string(log).unwrap_or_default();
            panic!(
                "socket {} never became connectable within {STARTUP_DEADLINE:?}\n{output}",
                path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn stop_child(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// A published copy is read-only; make the tree removable before cleanup.
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

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build a test runtime")
        .block_on(future)
}
