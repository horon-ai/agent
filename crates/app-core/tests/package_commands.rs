//! Package slash commands through the daemon's runtime-command code.
//!
//! Every case installs a real package into a private horon home and drives
//! [`horon_agent_app_core::server::package_commands`], which is exactly the pair
//! `RuntimeCommand::PackageCommands` and `RuntimeCommand::PackageCommand`
//! dispatch to. The listing and the run therefore read the session snapshot the
//! turn boundary captured, not a test copy of that decision.
//!
//! An executable command starts the installed `horon-package-probe`, this
//! target's sibling binary: it speaks the wire protocol on stdin/stdout and
//! appends what it received under the package's own state directory, so a case
//! can compare the host's refusal with what actually crossed the pipe. A
//! refused command leaves `starts.log` and `calls.log` absent or empty.
//!
//! The socket daemon is not started here: one `Server::run` installs
//! process-wide state (IRC, the package catalog publisher, a disk-cleanup
//! thread), which no per-case test can own. Calling the two functions the
//! daemon calls keeps the behaviour under test and keeps each case isolated.

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use horon_agent_app_core::agent::Agent;
use horon_agent_app_core::config;
use horon_agent_app_core::message::{Message, ToolDefinition};
use horon_agent_app_core::packages::{InstallRequest, PackageScope, PackageStore};
use horon_agent_app_core::provider::{EventStream, Provider};
use horon_agent_app_core::server::package_commands;
use horon_agent_app_core::tool::Registry;
use horon_agent_harness_api::{PackageCommandInfo, PackageCommandKind, PackageCommandList, PackageCommandOutcome};
use serde_json::json;
use tempfile::TempDir;

/// Each case changes process-wide environment variables, so they run one at a time.
static TEST_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The executable the fixtures install as the package binary.
const PROBE: &str = env!("CARGO_BIN_EXE_horon-package-probe");

/// The template fixture both kinds share: frontmatter plus placeholders.
const REVIEW_BODY: &str = "---\ndescription: Review a file\nargument-hint: <path>\n---\nReview $1 for $ARGUMENTS.";

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
        (os, arch) => panic!("package_commands needs a supported runner, got {os}/{arch}"),
    }
}

fn binary_name() -> &'static str {
    if std::env::consts::OS == "windows" {
        "bin/horon-package-probe.exe"
    } else {
        "bin/horon-package-probe"
    }
}

fn write_probe(root: &Path) {
    let destination = root.join(binary_name());
    fs::create_dir_all(destination.parent().expect("the fixture binary directory"))
        .expect("create the fixture binary directory");
    fs::copy(PROBE, &destination).expect("stage the probe as the package executable");
    horon_agent_platform::set_permissions_executable(&destination).expect("mark the probe runnable");
}

fn extension_manifest(id: &str, commands: &[&str]) -> String {
    let mut declarations = String::new();
    for &command in commands {
        declarations.push_str(&format!(
            "\n[[commands]]\nname = \"{command}\"\ndescription = \"Run the {command} probe command.\"\n"
        ));
    }
    format!(
        r#"
schema_version = 1
id = "{id}"
version = "1.0.0"
name = "Probe {id}"
protocol = "1.0"
{declarations}
[targets]
{target} = "{binary}"
"#,
        target = host_target(),
        binary = binary_name(),
    )
}

fn plugin_manifest(id: &str, resources: &[&str]) -> String {
    let listed = resources
        .iter()
        .map(|path| format!("\"{path}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"
schema_version = 1
id = "{id}"
version = "1.0.0"
name = "Probe plugin {id}"
extension = "native/extension.toml"

[resources]
commands = [{listed}]
"#
    )
}

/// A plugin whose bundled extension declares `commands` and whose resources
/// name `declared`, writing the body of every `templates` entry.
fn plugin_fixture(id: &str, commands: &[&str], declared: &[&str], templates: &[(&str, &str)]) -> TempDir {
    let root = tempfile::tempdir().expect("fixture directory");
    fs::create_dir_all(root.path().join("native")).expect("create the bundled directory");
    write_probe(&root.path().join("native"));
    fs::write(
        root.path().join("native/extension.toml"),
        extension_manifest(id, commands),
    )
    .expect("write the bundled manifest");
    for &(path, body) in templates {
        let destination = root.path().join(path);
        fs::create_dir_all(destination.parent().expect("the template directory"))
            .expect("create the template directory");
        fs::write(&destination, body).expect("write the template");
    }
    fs::write(root.path().join("plugin.toml"), plugin_manifest(id, declared)).expect("write plugin.toml");
    root
}

/// One package offering both kinds: an executable `greet` and a template
/// `review`.
fn mixed_fixture(id: &str) -> TempDir {
    plugin_fixture(
        id,
        &["greet"],
        &["commands/review.md"],
        &[("commands/review.md", REVIEW_BODY)],
    )
}

fn executable_info(package: &str, command: &str) -> PackageCommandInfo {
    PackageCommandInfo {
        name: format!("{package}:{command}"),
        package_id: package.to_owned(),
        command: command.to_owned(),
        kind: PackageCommandKind::Executable,
        description: format!("Run the {command} probe command."),
        argument_hint: None,
    }
}

fn template_info(package: &str, command: &str, description: &str, hint: Option<&str>) -> PackageCommandInfo {
    PackageCommandInfo {
        name: format!("{package}:{command}"),
        package_id: package.to_owned(),
        command: command.to_owned(),
        kind: PackageCommandKind::Template,
        description: description.to_owned(),
        argument_hint: hint.map(str::to_owned),
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct StubProvider;

#[async_trait]
impl Provider for StubProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        Err(anyhow::anyhow!("the stub provider never completes a request"))
    }

    fn name(&self) -> &str {
        "stub"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(StubProvider)
    }
}

/// Private `HORON_HOME`, runtime directory and `HOME` for one case, restored on drop.
struct IsolatedEnv {
    home: TempDir,
    _runtime: TempDir,
    previous: Vec<(&'static str, Option<OsString>)>,
}

impl IsolatedEnv {
    fn new(config_body: &str) -> Self {
        let home = tempfile::Builder::new()
            .prefix("pc-home-")
            .tempdir()
            .expect("a private home");
        let runtime = tempfile::Builder::new()
            .prefix("pc-runtime-")
            .tempdir()
            .expect("a private runtime directory");
        let previous = ["HORON_HOME", "HORON_AGENT_RUNTIME_DIR", "HOME"]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();
        horon_agent_app_core::env::set_var("HORON_HOME", home.path());
        horon_agent_app_core::env::set_var("HORON_AGENT_RUNTIME_DIR", runtime.path());
        horon_agent_app_core::env::set_var("HOME", home.path());
        fs::write(home.path().join("config.toml"), config_body).expect("write the private config");
        config::invalidate_config_cache();
        Self {
            home,
            _runtime: runtime,
            previous,
        }
    }

    fn store(&self) -> PackageStore {
        PackageStore::for_horon_home(self.home.path())
    }
}

impl Drop for IsolatedEnv {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..) {
            match value {
                Some(value) => horon_agent_app_core::env::set_var(key, value),
                None => horon_agent_app_core::env::remove_var(key),
            }
        }
        config::invalidate_config_cache();
    }
}

struct Harness {
    env: IsolatedEnv,
    agent: Agent,
}

async fn harness(config_body: &str) -> Harness {
    let env = IsolatedEnv::new(config_body);
    let provider: Arc<dyn Provider> = Arc::new(StubProvider);
    let registry = Registry::new(Arc::clone(&provider)).await;
    let agent = Agent::new(provider, registry);
    Harness { env, agent }
}

impl Harness {
    fn install(&self, source: &Path) {
        self.env
            .store()
            .install(InstallRequest::new(source, PackageScope::Global))
            .expect("install the probe package");
    }

    /// Capture this case's package generation, as the first request of a turn does.
    async fn enter(&mut self) {
        self.agent.enter_package_generation().await;
        assert!(
            self.agent.package_snapshot().is_some(),
            "the turn captured a package generation"
        );
    }

    fn generation(&self) -> u64 {
        self.agent
            .package_snapshot()
            .expect("the turn captured a package generation")
            .generation()
    }

    async fn list(&self) -> PackageCommandList {
        let (value, _) = package_commands::list(&self.agent)
            .await
            .expect("the session builds its package command listing");
        serde_json::from_value(value).expect("the listing is the typed reply")
    }

    async fn run(
        &mut self,
        package: &str,
        command: &str,
        words: &[&str],
    ) -> anyhow::Result<(PackageCommandOutcome, String)> {
        let mut args = Vec::with_capacity(words.len() + 2);
        args.push(package.to_owned());
        args.push(command.to_owned());
        args.extend(words.iter().copied().map(str::to_owned));
        let (value, message) = package_commands::run(&mut self.agent, &args).await?;
        let outcome = serde_json::from_value(value).expect("the outcome is the typed reply");
        Ok((outcome, message))
    }

    fn state(&self, package_id: &str) -> std::path::PathBuf {
        self.env.store().root().join("state").join(package_id)
    }

    /// Lines the child appended to `name`. An absent file is an empty record.
    fn record(&self, package_id: &str, name: &str) -> Vec<String> {
        fs::read_to_string(self.state(package_id).join(name))
            .map(|text| text.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Both kinds from one generation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn one_generation_lists_and_runs_both_command_kinds() {
    let _env = TEST_ENV.lock().await;
    let mut h = harness("[tools]\nprofile = \"core\"\n").await;
    let fixture = mixed_fixture("probe-both");
    h.install(fixture.path());
    h.enter().await;
    let generation = h.generation();

    let listing = h.list().await;
    assert_eq!(
        listing.generation, generation,
        "the listing names the turn's generation"
    );
    assert_eq!(
        listing.commands,
        vec![
            executable_info("probe-both", "greet"),
            template_info("probe-both", "review", "Review a file", Some("<path>")),
        ],
        "one package lists its executable and its template together, sorted by name"
    );

    // The template expands to the message a client submits, and never starts a
    // process.
    let (outcome, message) = h
        .run("probe-both", "review", &["src/lib.rs", "tests"])
        .await
        .expect("the template command expands");
    assert_eq!(
        outcome,
        PackageCommandOutcome {
            name: "probe-both:review".to_owned(),
            package_id: "probe-both".to_owned(),
            command: "review".to_owned(),
            kind: PackageCommandKind::Template,
            text: Some("Review src/lib.rs for src/lib.rs tests.".to_owned()),
            output: None,
            data: None,
        }
    );
    assert_eq!(message, "Review src/lib.rs for src/lib.rs tests.");
    assert!(
        h.record("probe-both", "starts.log").is_empty(),
        "a template command never starts the package child"
    );

    // The executable command reaches the installed child through the tool
    // policy, and the child records exactly the payload the daemon builds.
    let (outcome, message) = h
        .run("probe-both", "greet", &["alpha", "beta"])
        .await
        .expect("the executable command reaches the child");
    assert_eq!(outcome.name, "probe-both:greet");
    assert_eq!(outcome.package_id, "probe-both");
    assert_eq!(outcome.kind, PackageCommandKind::Executable);
    assert_eq!(outcome.text, None, "an executable command has no template text");
    let data = outcome.data.expect("the child's value comes back unrendered");
    assert_eq!(data["command"], "greet");
    assert_eq!(data["arguments"]["text"], "alpha beta");
    assert_eq!(data["arguments"]["args"], json!(["alpha", "beta"]));
    let output = outcome.output.expect("the child's answer is rendered");
    assert!(output.contains("\"command\": \"greet\""), "got {output}");
    assert_eq!(message, output, "the run's message is the rendered answer");
    assert_eq!(
        h.record("probe-both", "commands.log"),
        vec!["greet {\"args\":[\"alpha\",\"beta\"],\"text\":\"alpha beta\"}".to_owned()],
        "the child received the raw text and the split words"
    );
    assert_eq!(
        h.record("probe-both", "starts.log"),
        vec!["probe-both".to_owned()],
        "one child served it"
    );

    assert_eq!(h.generation(), generation, "both runs stayed on the turn's generation");
}

// ---------------------------------------------------------------------------
// Install-time refusals
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_executable_and_a_template_cannot_share_one_name() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("[tools]\nprofile = \"core\"\n");
    // The bundled extension declares the executable `greet`; the plugin declares
    // a template whose stem is also `greet`.
    let fixture = plugin_fixture(
        "probe-dup",
        &["greet"],
        &["commands/greet.md"],
        &[("commands/greet.md", "Say hello.")],
    );

    let error = env
        .store()
        .install(InstallRequest::new(fixture.path(), PackageScope::Global))
        .expect_err("a name with two kinds behind it is refused");
    assert_eq!(error.to_string(), "duplicate public package name: greet");
    assert!(
        env.store().catalog().expect("read the catalog").is_empty(),
        "a refused install publishes nothing"
    );
}

#[tokio::test]
async fn a_template_path_outside_the_package_root_is_refused() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("[tools]\nprofile = \"core\"\n");

    let relative = plugin_fixture("probe-escape", &[], &["../escape.md"], &[]);
    let error = env
        .store()
        .install(InstallRequest::new(relative.path(), PackageScope::Global))
        .expect_err("a parent traversal is refused");
    assert_eq!(error.to_string(), "invalid package-relative path: ../escape.md");

    let absolute = plugin_fixture("probe-escape-abs", &[], &["/tmp/escape.md"], &[]);
    let error = env
        .store()
        .install(InstallRequest::new(absolute.path(), PackageScope::Global))
        .expect_err("an absolute path is refused");
    assert_eq!(error.to_string(), "invalid package-relative path: /tmp/escape.md");
}

// ---------------------------------------------------------------------------
// Runtime refusals
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_entry_from_a_removed_package_is_refused_before_a_child_starts() {
    let _env = TEST_ENV.lock().await;
    let mut h = harness("[tools]\nprofile = \"core\"\n").await;
    let fixture = mixed_fixture("probe-stale");
    h.install(fixture.path());
    h.enter().await;
    // A client saw this name in the listing above; the operator removes the
    // package before the client invokes it.
    let listing = h.list().await;
    assert_eq!(listing.commands.len(), 2, "the stale entry came from a real listing");
    h.env
        .store()
        .remove("probe-stale")
        .expect("remove the installed package");

    let error = h
        .run("probe-stale", "greet", &[])
        .await
        .expect_err("a removed package's entry is refused");
    assert_eq!(
        error.to_string(),
        "The package command `probe-stale:greet` is stale: package probe-stale is removed; only an installed package loads"
    );
    assert!(
        h.record("probe-stale", "starts.log").is_empty(),
        "a stale entry never starts the child"
    );
    assert!(h.record("probe-stale", "calls.log").is_empty());
    assert!(h.record("probe-stale", "commands.log").is_empty());
}

#[tokio::test]
async fn write_mode_stops_an_executable_command_before_the_child_starts() {
    let _env = TEST_ENV.lock().await;
    let mut h = harness("[tools]\nprofile = \"core\"\napproval_mode = \"write\"\n").await;
    let fixture = mixed_fixture("probe-approval");
    h.install(fixture.path());
    h.enter().await;

    let error = h
        .run("probe-approval", "greet", &["alpha"])
        .await
        .expect_err("write mode asks before a command that runs a process");
    assert!(error.to_string().contains("needs approval"), "got {error}");
    assert!(
        h.record("probe-approval", "starts.log").is_empty(),
        "a denied command must not start the child"
    );

    // The template of the same package still expands: it runs no process, so
    // the approval mode does not gate it.
    let (outcome, _) = h
        .run("probe-approval", "review", &["notes.md"])
        .await
        .expect("a template command needs no process approval");
    assert_eq!(
        outcome.text.as_deref(),
        Some("Review notes.md for notes.md."),
        "write mode does not stop a template"
    );
    assert!(h.record("probe-approval", "starts.log").is_empty());
}
