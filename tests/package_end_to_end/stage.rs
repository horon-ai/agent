//! Staging the worked example in both layouts and driving the real CLI.
//!
//! The example's own layout test stages the same tree beside its build; this
//! module does it beside the binary the compat workflow built for the host, so
//! the install the CLI validates is the example an author would ship: the same
//! declarations, the same resources and a native executable for this machine.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// The layouts and resources the example ships.
const LAYOUTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/packages/example/layouts");

/// The declaration an extension ships at its root and a plugin bundles.
const DECLARATION: &str = "extension.toml";

/// The six targets the release matrix covers, in the order the declaration lists them.
const TARGETS: [&str; 6] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
    "aarch64-pc-windows-msvc",
    "x86_64-pc-windows-msvc",
];

/// Which layout one run installs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Extension,
    Plugin,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Extension => "extension",
            Kind::Plugin => "plugin",
        }
    }
}

/// The example binary the workflow built for the host, when it provided one.
pub fn example_binary() -> Option<PathBuf> {
    std::env::var_os("HORON_E2E_EXAMPLE_BIN")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

/// The host target triple as the package declaration spells it.
pub fn host_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        (os, arch) => panic!("the package end-to-end test needs a supported host, got {os}/{arch}"),
    }
}

fn host_binary_name() -> String {
    format!("horon-package-example{}", std::env::consts::EXE_SUFFIX)
}

fn foreign_target() -> &'static str {
    TARGETS
        .into_iter()
        .find(|target| *target != host_target())
        .expect("the release matrix has a target other than the host")
}

/// Stage one layout under `root` at `version` and return the package root.
///
/// The declaration keeps one `[targets]` row, for this host. The installer
/// opens and verifies every declared target's executable
/// (`crates/base/src/packages/manifest.rs`, `validate_package`), so a package
/// that lists the whole release matrix while shipping one binary is refused;
/// the documented rule is to trim the table to the targets a build ships
/// (`docs/runtime/PACKAGES.md`).
pub fn stage(root: &Path, kind: Kind, version: &str, example: &Path) -> PathBuf {
    let layouts = Path::new(LAYOUTS);
    fs::create_dir_all(root).expect("create the package source directory");
    let declaration = match kind {
        Kind::Extension => {
            let declaration = root.join(DECLARATION);
            write(
                &declaration,
                &host_only_targets(&versioned(&read(&layouts.join(DECLARATION)), version)),
            );
            declaration
        }
        Kind::Plugin => {
            write(
                &root.join("plugin.toml"),
                &versioned(&read(&layouts.join("plugin.toml")), version),
            );
            let bundled = root.join("extension");
            fs::create_dir_all(&bundled).expect("create the bundled directory");
            let declaration = bundled.join(DECLARATION);
            write(
                &declaration,
                &host_only_targets(&versioned(&read(&layouts.join(DECLARATION)), version)),
            );
            for resource in ["skills", "commands", "mcp", "rules", "prompts"] {
                copy_tree(
                    &layouts.join("resources").join(resource),
                    &root.join("resources").join(resource),
                );
            }
            declaration
        }
    };
    place_binary(declaration.parent().expect("the declaration has a directory"), example);
    root.to_path_buf()
}

/// Rewrite `[targets]` so it declares this host's executable and nothing else.
fn host_only_targets(text: &str) -> String {
    let host = host_target();
    let row = format!("{host} = \"bin/{host}/{}\"", host_binary_name());
    let mut out = String::with_capacity(text.len());
    let mut in_targets = false;
    let mut written = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "[targets]" {
            in_targets = true;
            out.push_str(trimmed);
            out.push('\n');
            continue;
        }
        if in_targets {
            if trimmed.starts_with('[') {
                // The next table starts here; close `[targets]` with its one row.
                out.push_str(&row);
                out.push('\n');
                written = true;
                in_targets = false;
                out.push_str(trimmed);
                out.push('\n');
            }
            // Every declared target other than this host's row is dropped.
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if in_targets && !written {
        out.push_str(&row);
        out.push('\n');
        written = true;
    }
    assert!(written, "the example declaration has a [targets] table");
    let rows = out
        .lines()
        .skip_while(|line| line.trim() != "[targets]")
        .skip(1)
        .take_while(|line| !line.trim().is_empty() && !line.trim().starts_with('['))
        .filter(|line| line.contains('='))
        .count();
    assert_eq!(rows, 1, "one executable target is staged");
    assert_eq!(out.matches(&row).count(), 1, "the staged target row is the host's");
    out
}

/// Stage an extension that declares only a target this host is not.
///
/// The executable is still the host's own, so the refusal can only come from the
/// declaration: the install must name the host target it is missing.
pub fn stage_foreign(root: &Path, example: &Path) -> PathBuf {
    let foreign = foreign_target();
    fs::create_dir_all(root).expect("create the foreign package directory");
    let name = host_binary_name();
    write(
        &root.join(DECLARATION),
        &format!(
            "schema_version = 1\nid = \"hello-foreign\"\nversion = \"0.1.0\"\n\
             name = \"Foreign package example\"\nprotocol = \"1.0\"\n\n\
             [targets]\n{foreign} = \"bin/{foreign}/{name}\"\n"
        ),
    );
    // The bytes are the host's own, at the path the declaration names, so the
    // only reason the install can refuse is the missing host target.
    let destination = root.join("bin").join(foreign).join(&name);
    fs::create_dir_all(destination.parent().expect("the artifact has a directory"))
        .expect("create the artifact directory");
    fs::copy(example, &destination).expect("stage the built example under the foreign target");
    horon_agent_platform::set_permissions_executable(&destination).expect("mark the foreign executable runnable");
    root.to_path_buf()
}

/// Copy the built example to the artifact path the declaration names.
fn place_binary(declaration_dir: &Path, example: &Path) {
    let destination = declaration_dir.join("bin").join(host_target()).join(host_binary_name());
    fs::create_dir_all(destination.parent().expect("the artifact has a directory"))
        .expect("create the artifact directory");
    fs::copy(example, &destination).expect("stage the built example as the package executable");
    horon_agent_platform::set_permissions_executable(&destination).expect("mark the package executable runnable");
}

fn versioned(text: &str, version: &str) -> String {
    let replaced = text.replace("version = \"0.1.0\"", &format!("version = \"{version}\""));
    assert_ne!(replaced, text, "the example declares the version this test rewrites");
    replaced
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create a package directory");
    }
    fs::write(path, text).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create a resource directory");
    for entry in fs::read_dir(from).expect("read a resource directory") {
        let entry = entry.expect("read a resource entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("the entry kind").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy a resource file");
        }
    }
}

/// One isolated horon home plus the workspace the CLI runs inside.
pub struct Cli {
    home: PathBuf,
    work: PathBuf,
}

impl Cli {
    /// Create the home and point its provider profile at the mock model.
    pub fn new(root: &Path, base_url: &str) -> Self {
        let home = root.join("home");
        let work = root.join("work");
        fs::create_dir_all(&home).expect("create the isolated horon home");
        fs::create_dir_all(&work).expect("create the working directory");
        write(
            &home.join("config.toml"),
            &format!(
                "[compaction]\nbackground = false\nmax_context_tokens = 20000\nrecent_tokens = 2048\n\n\
                 [provider]\ndefault_provider = \"qa\"\ndefault_model = \"qa-model\"\n\n\
                 [providers.qa]\ntype = \"openai-compatible\"\nbase_url = \"{base_url}\"\nauth = \"none\"\n\
                 default_model = \"qa-model\"\nmodels = [{{ id = \"qa-model\" }}]\n"
            ),
        );
        Self { home, work }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Run one CLI invocation to completion.
    pub fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("run the horon-agent CLI")
    }

    /// Start one CLI invocation and keep it, so a test can act while it runs.
    pub fn spawn(&self, args: &[&str]) -> Run {
        Run::start(self.command(args))
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_horon-agent"));
        command
            .args(args)
            .current_dir(&self.work)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            // Windows needs its own system variables to load a process image and
            // its DLLs; they are unset on Unix, so forwarding when present keeps
            // one code path. `env_clear` would otherwise strip them.
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("HORON_HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("config"))
            .env("XDG_CACHE_HOME", self.home.join("cache"))
            .env("XDG_RUNTIME_DIR", self.home.join("runtime"))
            .env("HORON_AGENT_RUNTIME_DIR", self.home.join("runtime"))
            .env("HORON_AGENT_NO_AUTO_UPDATE", "1")
            .env("HORON_AGENT_NON_INTERACTIVE", "1")
            .env("HORON_AGENT_MEMORY_ENABLED", "0")
            .env("HORON_AGENT_MEMORY_SIDECAR_ENABLED", "0")
            .env("HORON_AGENT_RUN_MCP", "0")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null());
        // `SystemRoot` and friends are what a Windows process image needs; they
        // do not exist on Unix, so forwarding them when present is one path.
        for key in ["SystemRoot", "SystemDrive", "windir", "ComSpec", "PATHEXT"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
    }
}

/// One running CLI invocation with both output streams drained.
pub struct Run {
    child: Child,
    stdout: JoinHandle<Vec<u8>>,
    stderr: JoinHandle<Vec<u8>>,
}

/// A finished invocation, with the collected output.
pub struct Finished {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    fn start(mut command: Command) -> Self {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn the horon-agent CLI");
        let stdout = child.stdout.take().expect("a stdout pipe");
        let stderr = child.stderr.take().expect("a stderr pipe");
        Self {
            child,
            stdout: drain(stdout),
            stderr: drain(stderr),
        }
    }

    /// Whether the invocation is still working.
    pub fn running(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(None) => true,
            Ok(Some(_)) => false,
            Err(error) => panic!("poll the horon-agent CLI: {error}"),
        }
    }

    /// Wait for the invocation, killing it at the deadline so a hung run cannot hang the test.
    pub fn finish(mut self, deadline: Duration) -> Finished {
        let end = Instant::now() + deadline;
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if Instant::now() >= end {
                        let _ = self.child.kill();
                        break self.child.wait().expect("reap the killed horon-agent CLI");
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                Err(error) => panic!("poll the horon-agent CLI: {error}"),
            }
        };
        Finished {
            status,
            stdout: text(&join(self.stdout)),
            stderr: text(&join(self.stderr)),
        }
    }
}

fn drain(mut reader: impl Read + Send + 'static) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = reader.read_to_end(&mut buffer);
        buffer
    })
}

fn join(handle: JoinHandle<Vec<u8>>) -> Vec<u8> {
    handle.join().unwrap_or_default()
}

pub fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A successful invocation's stdout, or the failure with both streams.
pub fn success(what: &str, output: &Output) -> Result<String, String> {
    if output.status.success() {
        return Ok(text(&output.stdout));
    }
    Err(format!(
        "{what} failed with {:?}: {} {}",
        output.status.code(),
        text(&output.stdout).trim(),
        text(&output.stderr).trim()
    ))
}

/// A refused invocation's stderr, or an error naming the unexpected success.
pub fn refused(what: &str, output: &Output) -> Result<String, String> {
    if output.status.success() {
        return Err(format!("{what} was accepted: {}", text(&output.stdout).trim()));
    }
    Ok(text(&output.stderr))
}
