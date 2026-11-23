//! The cost of packages a user did not install, installed but never called, and
//! actually called.
//!
//! This target exists for the structural gate: it prints `STRUCTURAL_PERF
//! section=packages` lines that `scripts/perf/check_structural.py` bounds. Every
//! number is a counter the host already keeps, never a stopwatch, so the same
//! line means the same thing on every runner.
//!
//! Four situations, each measured on its own window with the counters reset:
//!
//! - `empty`: a home with no package area, driven through `enter_turn` twice
//!   without an agent. A session start and two turns must read no package file,
//!   scan no directory, write nothing into the area, and start no child.
//! - `turn-warm`: the first turn over an installed package, with the daemon's
//!   published generation attached. This is the floor the other two lines are
//!   read against: if the counters were off, this turn would report zeros too.
//! - `turn-unchanged`: a second turn at the same catalog generation. The
//!   session's own snapshot is reused, so nothing is read, rebuilt or started.
//! - `call`: one tool call through `Registry::execute` starts exactly one child,
//!   and a second call reuses it.
//!
//! The child is `horon-package-probe`, the target's sibling binary, installed
//! like any other package. Its own `starts.log` is the ground truth beside the
//! counter: the host may not believe it started a process the child never ran.
//!
//! Every measured window is a few milliseconds of work started immediately after
//! `reset()`: no sleep, no retry and no polling sits between the reset and the
//! snapshot. That matters because the daemon feed attached for the `turn-*` lines
//! runs a catalog watch task of its own; a window left open for seconds could
//! count a read this measurement did not cause.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use horon_agent_app_core::agent::Agent;
use horon_agent_app_core::config;
use horon_agent_app_core::extensions::package_snapshot::PackageSession;
use horon_agent_app_core::message::{Message, ToolDefinition};
use horon_agent_app_core::packages::{InstallRequest, PackageScope, PackageStore};
use horon_agent_app_core::provider::{EventStream, Provider};
use horon_agent_app_core::server::package_control;
use horon_agent_app_core::tool::{Registry, ToolContext, ToolExecutionMode, ToolOutput};
use horon_agent_perf_counters as counters;
use serde_json::{Value, json};
use tempfile::TempDir;

/// The executable the fixture installs as the package binary.
const PROBE: &str = env!("CARGO_BIN_EXE_horon-package-probe");

fn host_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        (os, arch) => panic!("package_cost needs a supported runner, got {os}/{arch}"),
    }
}

fn binary_name() -> &'static str {
    if std::env::consts::OS == "windows" {
        "bin/horon-package-probe.exe"
    } else {
        "bin/horon-package-probe"
    }
}

/// The registry name a declared tool gets, as the host documents it.
fn public_name(package_id: &str, tool: &str) -> String {
    format!("ext__{package_id}__{tool}")
}

/// A one-tool extension whose executable is this target's sibling probe.
fn extension_fixture(id: &str, tool: &str) -> TempDir {
    let root = tempfile::tempdir().expect("fixture directory");
    let binary = root.path().join(binary_name());
    fs::create_dir_all(binary.parent().expect("the fixture binary directory"))
        .expect("create the fixture binary directory");
    fs::copy(PROBE, &binary).expect("stage the probe as the package executable");
    horon_agent_platform::set_permissions_executable(&binary).expect("mark the probe runnable");
    fs::write(
        root.path().join("extension.toml"),
        format!(
            r#"
schema_version = 1
id = "{id}"
version = "1.0.0"
name = "Cost probe {id}"
protocol = "1.0"

[[tools]]
name = "{tool}"
description = "Run the {tool} probe tool."
input_schema = {{ type = "object", additionalProperties = true }}

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

/// Private `HORON_HOME`, runtime directory and `HOME` for this process, restored
/// on drop.
struct IsolatedEnv {
    home: TempDir,
    _runtime: TempDir,
    previous: Vec<(&'static str, Option<OsString>)>,
}

impl IsolatedEnv {
    fn new() -> Self {
        let home = tempfile::Builder::new()
            .prefix("horon-package-cost-home-")
            .tempdir()
            .expect("a private home");
        let runtime = tempfile::Builder::new()
            .prefix("horon-package-cost-runtime-")
            .tempdir()
            .expect("a private runtime directory");
        let previous = ["HORON_HOME", "HORON_AGENT_RUNTIME_DIR", "HOME"]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();
        horon_agent_app_core::env::set_var("HORON_HOME", home.path());
        horon_agent_app_core::env::set_var("HORON_AGENT_RUNTIME_DIR", runtime.path());
        horon_agent_app_core::env::set_var("HOME", home.path());
        fs::write(home.path().join("config.toml"), "[tools]\nprofile = \"core\"\n").expect("write the private config");
        config::invalidate_config_cache();
        Self {
            home,
            _runtime: runtime,
            previous,
        }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn store(&self) -> PackageStore {
        PackageStore::for_horon_home(self.home())
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
    registry: Registry,
    session: String,
    work: PathBuf,
}

impl Harness {
    async fn new() -> Self {
        let env = IsolatedEnv::new();
        let work = env.home().join("work");
        fs::create_dir_all(&work).expect("create the working directory");
        let provider: Arc<dyn Provider> = Arc::new(StubProvider);
        let registry = Registry::new(Arc::clone(&provider)).await;
        let agent = Agent::new(provider, registry.clone());
        let session = agent.session_id().to_string();
        Self {
            env,
            agent,
            registry,
            session,
            work,
        }
    }

    fn install(&self, source: &Path) {
        self.env
            .store()
            .install(InstallRequest::new(source, PackageScope::Global))
            .expect("install the probe package");
    }

    async fn enter(&mut self) {
        self.agent.enter_package_generation().await;
    }

    fn context(&self, call: &str) -> ToolContext {
        ToolContext {
            session_id: self.session.clone(),
            message_id: format!("{call}-message"),
            tool_call_id: format!("{call}-call"),
            working_dir: Some(self.work.clone()),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
        }
    }

    async fn call(&self, name: &str, input: Value) -> anyhow::Result<ToolOutput> {
        self.registry.execute(name, input, self.context(name)).await
    }

    /// Lines the child appended to `name`. An absent file is an empty record.
    fn record(&self, package_id: &str, name: &str) -> Vec<String> {
        let path = self.env.store().root().join("state").join(package_id).join(name);
        fs::read_to_string(path)
            .map(|text| text.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }
}

/// One measured window, printed in the shape the structural gate parses.
fn print_line(source: &str, fields: &[(&str, u64)]) {
    let rendered: Vec<String> = fields.iter().map(|(name, value)| format!("{name}={value}")).collect();
    std::println!(
        "STRUCTURAL_PERF section=packages source={source} {}",
        rendered.join(" ")
    );
}

#[tokio::test]
async fn structural_package_costs_are_measured() {
    // ---- a home with no package area -------------------------------------
    //
    // No daemon and no environment: a bare session over a store whose root does
    // not exist. This is the path every user who never installed a package pays,
    // so its cost is zero rather than "small".
    let idle_home = tempfile::tempdir().expect("an idle horon home");
    let idle_store = PackageStore::for_horon_home(idle_home.path());
    let idle_session = PackageSession::open(idle_store.clone(), "cost-empty");
    counters::reset();
    let first = idle_session.enter_turn(None).await.expect("the first idle turn");
    let second = idle_session.enter_turn(None).await.expect("the second idle turn");
    let empty = counters::snapshot();
    assert_eq!(first.len(), 0, "an absent area lists no package");
    assert_eq!(second.len(), 0);
    assert!(
        !idle_store.root().exists(),
        "a turn with no installed package must not create {}",
        idle_store.root().display()
    );

    // ---- one installed package, never called ----------------------------
    let mut harness = Harness::new().await;
    let fixture = extension_fixture("cost-probe", "greet");
    harness.install(fixture.path());
    // The daemon publishes the catalog generation and attaches its feed to this
    // process, exactly as `serve` does before the first session; every turn below
    // then reads that generation instead of the catalog file.
    let mut feed = package_control::subscribe()
        .await
        .expect("the daemon publishes the package catalog");
    let published = feed.borrow_and_update().generation();
    assert!(
        published >= 1,
        "the installed package is published: generation {published}"
    );

    counters::reset();
    harness.enter().await;
    let warm = counters::snapshot();
    let name = public_name("cost-probe", "greet");
    assert!(
        harness.registry.tool_names().await.contains(&name),
        "the first turn exposes the installed package's tool"
    );

    // ---- the same generation a second time ------------------------------
    counters::reset();
    harness.enter().await;
    let unchanged = counters::snapshot();
    assert!(
        harness.record("cost-probe", "starts.log").is_empty(),
        "two turns over an installed package must not start its child"
    );

    // ---- one real call --------------------------------------------------
    counters::reset();
    let output = harness
        .call(&name, json!({"name": "cost"}))
        .await
        .expect("the call reaches the installed child");
    assert!(
        output.output.contains("cost"),
        "the child echoes the argument: {}",
        output.output
    );
    let call = counters::snapshot();

    // ---- and a second one, which reuses the child -----------------------
    counters::reset();
    harness
        .call(&name, json!({"name": "again"}))
        .await
        .expect("the second call reaches the same child");
    let repeat = counters::snapshot();

    let starts = harness.record("cost-probe", "starts.log");
    assert_eq!(starts.len(), 1, "two calls share one child process: {starts:?}");

    assert_eq!(empty.package_area_reads, 0, "no package file is opened");
    assert_eq!(empty.package_dir_scans, 0, "no package directory is scanned");
    assert_eq!(empty.package_area_writes, 0, "nothing is written into the area");
    assert_eq!(empty.package_snapshot_builds, 0, "no snapshot is built");
    assert_eq!(empty.package_child_starts, 0, "no child is started");
    assert!(
        warm.package_area_reads >= 1,
        "the first turn reads the catalog and its declaration"
    );
    assert_eq!(warm.package_snapshot_builds, 1, "the first turn builds one snapshot");
    assert!(warm.package_area_writes >= 1, "the first turn takes a lease");
    assert_eq!(warm.package_child_starts, 0);
    assert_eq!(
        unchanged.package_area_reads, 0,
        "an unchanged generation reads no package file"
    );
    assert_eq!(unchanged.package_dir_scans, 0);
    assert_eq!(
        unchanged.package_area_writes, 0,
        "an unchanged generation grows no reference"
    );
    assert_eq!(
        unchanged.package_snapshot_builds, 0,
        "an unchanged generation reuses the snapshot"
    );
    assert_eq!(unchanged.package_child_starts, 0);
    assert_eq!(call.package_child_starts, 1, "one call starts exactly one child");
    // Three opens, and none of them the declaration, which the turn already
    // cached: the catalog re-check in `ensure_live`, then the launch's own
    // verification, which reads the installed executable's header and content.
    assert_eq!(
        call.package_area_reads, 3,
        "a call re-checks the catalog and verifies the executable"
    );
    assert_eq!(repeat.package_child_starts, 0, "a second call reuses the live child");

    print_line(
        "empty",
        &[
            ("turns", 2),
            ("packages", 0),
            ("area_reads", empty.package_area_reads),
            ("dir_scans", empty.package_dir_scans),
            ("area_writes", empty.package_area_writes),
            ("snapshot_builds", empty.package_snapshot_builds),
            ("child_starts", empty.package_child_starts),
        ],
    );
    print_line(
        "turn-warm",
        &[
            ("turns", 1),
            ("packages", 1),
            ("area_reads", warm.package_area_reads),
            ("dir_scans", warm.package_dir_scans),
            ("area_writes", warm.package_area_writes),
            ("snapshot_builds", warm.package_snapshot_builds),
            ("child_starts", warm.package_child_starts),
        ],
    );
    print_line(
        "turn-unchanged",
        &[
            ("turns", 1),
            ("packages", 1),
            ("area_reads", unchanged.package_area_reads),
            ("dir_scans", unchanged.package_dir_scans),
            ("area_writes", unchanged.package_area_writes),
            ("snapshot_builds", unchanged.package_snapshot_builds),
            ("child_starts", unchanged.package_child_starts),
        ],
    );
    print_line(
        "call",
        &[
            ("calls", 1),
            ("area_reads", call.package_area_reads),
            ("child_starts", call.package_child_starts),
        ],
    );
    print_line(
        "call-repeat",
        &[("calls", 1), ("child_starts", repeat.package_child_starts)],
    );
}
