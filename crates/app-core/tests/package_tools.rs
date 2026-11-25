//! Package tools through the shared dispatch path.
//!
//! Each case installs a real package into a private horon home and starts its
//! real child: the installed executable is this target's sibling binary
//! `horon-package-probe`, which speaks the wire on stdin/stdout and records what
//! it received under the package's own state directory. That record is how a
//! case compares the dispatcher's decision (a refusal, a hidden name, a plan-mode
//! block) with what actually crossed the pipe: nothing the host refused appears
//! in `calls.log`, and nothing at all appears when the decision came before a
//! start.
//!
//! Every case drives `Registry::execute`, the entry point a model's call takes,
//! after `Agent::enter_package_generation` captured the turn's packages. Plan
//! mode, the approval mode, the allow and block lists, the load tiers and the
//! `pre_tool` gate therefore run where they really run, not through a helper.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use horon_agent_app_core::agent::Agent;
use horon_agent_app_core::config;
use horon_agent_app_core::extensions::invoke;
use horon_agent_app_core::extensions::lifecycle::{PackageLifecycleRegistry, RevocationCause};
use horon_agent_app_core::message::{Message, ToolDefinition};
use horon_agent_app_core::packages::{InstallRequest, PackageScope, PackageStore};
use horon_agent_app_core::plan_mode::{self, PlanState};
use horon_agent_app_core::provider::{EventStream, Provider};
use horon_agent_app_core::secrets;
use horon_agent_app_core::tool::{Registry, Tool, ToolContext, ToolExecutionMode, ToolOutput};
use serde_json::{Value, json};
use tempfile::TempDir;

/// Each case changes process-wide environment variables, so they run one at a time.
static TEST_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The executable the fixtures install as the package binary.
const PROBE: &str = env!("CARGO_BIN_EXE_horon-package-probe");

/// How long a case waits for a child's own record before failing.
const CHILD_DEADLINE: Duration = Duration::from_secs(60);

fn host_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        (os, arch) => panic!("package_tools needs a supported runner, got {os}/{arch}"),
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

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn write_probe(root: &Path) {
    let destination = root.join(binary_name());
    fs::create_dir_all(destination.parent().expect("the fixture binary directory"))
        .expect("create the fixture binary directory");
    fs::copy(PROBE, &destination).expect("stage the probe as the package executable");
    horon_agent_platform::set_permissions_executable(&destination).expect("mark the probe runnable");
}

fn tool_declaration(name: &str) -> String {
    format!(
        "\n[[tools]]\nname = \"{name}\"\ndescription = \"Run the {name} probe tool.\"\n\
         input_schema = {{ type = \"object\", additionalProperties = true }}\n"
    )
}

fn command_declaration(name: &str) -> String {
    format!("\n[[commands]]\nname = \"{name}\"\ndescription = \"Run the {name} probe command.\"\n")
}

fn manifest(id: &str, tools: &[&str], commands: &[&str]) -> String {
    let tools: String = tools.iter().map(|tool| tool_declaration(tool)).collect();
    let commands: String = commands.iter().map(|command| command_declaration(command)).collect();
    format!(
        r#"
schema_version = 1
id = "{id}"
version = "1.0.0"
name = "Probe {id}"
protocol = "1.0"
{tools}{commands}
[targets]
{target} = "{binary}"
"#,
        target = host_target(),
        binary = binary_name(),
    )
}

fn extension_fixture(id: &str, tools: &[&str], commands: &[&str]) -> TempDir {
    let root = tempfile::tempdir().expect("fixture directory");
    write_probe(root.path());
    fs::write(root.path().join("extension.toml"), manifest(id, tools, commands)).expect("write extension.toml");
    root
}

/// The dispatcher refused the name before any child could start: either the
/// tool policy does not allow it or the registry has no such tool.
fn is_refusal(error: &dyn std::fmt::Display) -> bool {
    let text = error.to_string();
    text.contains("is not allowed") || text.contains("Unknown tool")
}

fn plugin_fixture(id: &str, tools: &[&str]) -> TempDir {
    let root = tempfile::tempdir().expect("fixture directory");
    fs::create_dir_all(root.path().join("native")).expect("create the bundled directory");
    write_probe(&root.path().join("native"));
    fs::write(root.path().join("native/extension.toml"), manifest(id, tools, &[])).expect("write the bundled manifest");
    fs::write(root.path().join("rule.md"), "A probe rule\n").expect("write the fixture resource");
    fs::write(
        root.path().join("plugin.toml"),
        format!(
            r#"
schema_version = 1
id = "{id}"
version = "1.0.0"
name = "Probe plugin {id}"
extension = "native/extension.toml"

[resources]
rules = ["rule.md"]
"#
        ),
    )
    .expect("write plugin.toml");
    root
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
            .prefix("horon-package-tools-home-")
            .tempdir()
            .expect("a private home");
        let runtime = tempfile::Builder::new()
            .prefix("horon-package-tools-runtime-")
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

async fn harness(config_body: &str) -> Harness {
    let env = IsolatedEnv::new(config_body);
    let work = env.home().join("work");
    fs::create_dir_all(&work).expect("create the working directory");
    let provider: Arc<dyn Provider> = Arc::new(StubProvider);
    let registry = Registry::new(Arc::clone(&provider)).await;
    let agent = Agent::new(provider, registry.clone());
    let session = agent.session_id().to_string();
    Harness {
        env,
        agent,
        registry,
        session,
        work,
    }
}

impl Harness {
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

    fn state(&self, package_id: &str) -> PathBuf {
        self.env.store().root().join("state").join(package_id)
    }

    /// Lines the child appended to `name`. An absent file is an empty record.
    fn record(&self, package_id: &str, name: &str) -> Vec<String> {
        fs::read_to_string(self.state(package_id).join(name))
            .map(|text| text.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// The definitions this session would actually send: the allow-list, the
    /// load tiers and the deferral flag all applied.
    async fn definitions(&self) -> Vec<ToolDefinition> {
        self.agent.tool_definitions_for_debug().await
    }

    async fn definition(&self, name: &str) -> Option<ToolDefinition> {
        self.definitions().await.into_iter().find(|def| def.name == name)
    }
}

fn wait_for_record(path: &Path, what: &str) -> String {
    let deadline = Instant::now() + CHILD_DEADLINE;
    loop {
        match fs::read_to_string(path) {
            Ok(text) if !text.trim().is_empty() => return text,
            other => {
                assert!(
                    Instant::now() < deadline,
                    "{what} never appeared at {}: {other:?}",
                    path.display()
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// Wait until the child's heartbeat stops growing, which is how a case proves
/// the supervised process tree was reaped.
fn wait_until_quiet(path: &Path) {
    let deadline = Instant::now() + CHILD_DEADLINE;
    let mut previous = fs::metadata(path).map(|metadata| metadata.len()).unwrap_or(0);
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let current = fs::metadata(path).map(|metadata| metadata.len()).unwrap_or(0);
        if current == previous && current > 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the package child kept writing to {}",
            path.display()
        );
        previous = current;
    }
}

#[cfg(unix)]
fn write_hook(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, body).expect("write the hook script");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("mark the hook executable");
}

// ---------------------------------------------------------------------------
// A call reaches the real child
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_package_tool_call_reaches_the_installed_extension_child() {
    let _env = TEST_ENV.lock().await;
    let mut h = harness("[tools]\nprofile = \"core\"\n").await;
    let fixture = extension_fixture("probe-ext", &["greet", "slow"], &["hello"]);
    h.install(fixture.path());
    h.enter().await;

    let name = public_name("probe-ext", "greet");
    let definition = h.definition(&name).await.expect("core lists the package tool");
    assert!(
        definition.defer_loading,
        "core exposes a package tool deferred, not in the cached prefix"
    );
    assert!(h.registry.tool_names().await.contains(&name));

    let output = h
        .call(&name, json!({"name": "민열"}))
        .await
        .expect("the call reaches the installed child");
    assert!(output.output.contains("\"tool\": \"greet\"") || output.output.contains("\"tool\":\"greet\""));
    assert!(
        output.output.contains("민열"),
        "the child echoes the arguments: {}",
        output.output
    );
    assert!(
        output.output.contains("\"pid\""),
        "the answer comes from a process: {}",
        output.output
    );

    let calls = h.record("probe-ext", "calls.log");
    assert_eq!(calls.len(), 1, "the child received exactly one call: {calls:?}");
    assert!(
        calls[0].starts_with("greet "),
        "the child saw the declared name: {calls:?}"
    );
    assert_eq!(h.record("probe-ext", "starts.log").len(), 1, "one child served it");

    // The command surface uses the same dispatch, and lands in the child's own
    // record rather than the tool record.
    let value = invoke::call_command(&h.session, "probe-ext", "hello", json!({"where": "test"}))
        .await
        .expect("the executable command dispatch reaches the child");
    assert_eq!(value["command"], "hello");
    assert_eq!(h.record("probe-ext", "commands.log").len(), 1);
    assert_eq!(h.record("probe-ext", "starts.log").len(), 1, "the child was reused");
}

#[tokio::test]
async fn a_plugin_tool_reaches_its_bundled_extension_and_an_undeclared_name_does_not() {
    let _env = TEST_ENV.lock().await;
    let mut h = harness("[tools]\nprofile = \"core\"\n").await;
    let plugin = plugin_fixture("probe-plugin", &["greet"]);
    h.install(plugin.path());
    // A second package with a command only: it declares no tool, so nothing of
    // its surface may appear under a tool name.
    let commands = extension_fixture("probe-commands", &[], &["hello"]);
    h.install(commands.path());
    h.enter().await;

    let name = public_name("probe-plugin", "greet");
    let output = h
        .call(&name, json!({"name": "plugin"}))
        .await
        .expect("a plugin's declared tool reaches its bundled extension");
    assert!(output.output.contains("\"tool\": \"greet\"") || output.output.contains("\"tool\":\"greet\""));
    assert_eq!(h.record("probe-plugin", "calls.log").len(), 1);
    assert!(
        h.record("probe-commands", "starts.log").is_empty(),
        "a package that declares no tool must never be started for a tool call"
    );
    assert!(
        fs::read_dir(h.state("probe-commands")).is_err() || h.record("probe-commands", "calls.log").is_empty(),
        "a tool-less package receives no tool call"
    );

    // A name nobody declared is not delivered either: the registry has no such
    // tool, so the dispatcher refuses before a child could start.
    let error = h
        .call(&public_name("probe-plugin", "missing"), json!({}))
        .await
        .expect_err("an undeclared tool name is refused");
    assert!(is_refusal(&error), "got {error}");
    assert_eq!(
        h.record("probe-plugin", "calls.log").len(),
        1,
        "nothing new crossed the pipe"
    );
}

// ---------------------------------------------------------------------------
// Exposure and policy
// ---------------------------------------------------------------------------

#[tokio::test]
async fn minimal_and_none_profiles_expose_no_package_tool() {
    let _env = TEST_ENV.lock().await;
    for profile in ["minimal", "none"] {
        let mut h = harness(&format!("[tools]\nprofile = \"{profile}\"\n")).await;
        let fixture = extension_fixture("probe-ext", &["greet"], &[]);
        h.install(fixture.path());
        h.enter().await;

        let name = public_name("probe-ext", "greet");
        assert!(
            h.definition(&name).await.is_none(),
            "{profile} must not list a package tool"
        );
        let error = h
            .call(&name, json!({}))
            .await
            .expect_err("a profile that names its tools refuses the call");
        assert!(is_refusal(&error), "{profile}: {error}");
        assert!(
            h.record("probe-ext", "starts.log").is_empty(),
            "{profile} must not start the package child"
        );
        drop(h);
    }
}

#[tokio::test]
async fn an_explicit_enabled_list_admits_only_the_names_it_gives() {
    let _env = TEST_ENV.lock().await;
    let mut h = harness(&format!(
        "[tools]\nprofile = \"core\"\nenabled = [\"read\", \"{}\"]\n",
        public_name("probe-ext", "greet")
    ))
    .await;
    let fixture = extension_fixture("probe-ext", &["greet", "slow"], &[]);
    h.install(fixture.path());
    h.enter().await;

    let listed = public_name("probe-ext", "greet");
    let definition = h
        .definition(&listed)
        .await
        .expect("a name the client listed is exposed");
    assert!(
        !definition.defer_loading,
        "an explicit list is exact, so its tools stay eager"
    );
    h.call(&listed, json!({"name": "listed"}))
        .await
        .expect("the listed tool runs");

    let unlisted = public_name("probe-ext", "slow");
    assert!(
        h.definition(&unlisted).await.is_none(),
        "a name the client did not list stays hidden"
    );
    assert!(
        h.call(&unlisted, json!({})).await.is_err(),
        "an unlisted package tool is not callable"
    );
    assert_eq!(
        h.record("probe-ext", "calls.log").len(),
        1,
        "only the listed call crossed"
    );
}

#[tokio::test]
async fn a_disabled_name_is_hidden_and_never_delivered() {
    let _env = TEST_ENV.lock().await;
    let mut h = harness(&format!(
        "[tools]\nprofile = \"core\"\ndisabled = [\"{}\"]\n",
        public_name("probe-ext", "greet")
    ))
    .await;
    let fixture = extension_fixture("probe-ext", &["greet"], &[]);
    h.install(fixture.path());
    h.enter().await;

    let name = public_name("probe-ext", "greet");
    assert!(h.definition(&name).await.is_none(), "[tools] disabled hides the name");
    let error = h.call(&name, json!({})).await.expect_err("a disabled name is refused");
    assert!(is_refusal(&error), "got {error}");
    assert!(h.record("probe-ext", "starts.log").is_empty(), "no child started");
}

#[tokio::test]
async fn plan_mode_refuses_the_process_effect_before_a_child_starts() {
    let _env = TEST_ENV.lock().await;
    let mut h = harness("[tools]\nprofile = \"core\"\n").await;
    let fixture = extension_fixture("probe-ext", &["greet"], &[]);
    h.install(fixture.path());
    h.enter().await;
    plan_mode::clear_state(&h.session);
    plan_mode::set_state(&h.session, PlanState::Planning);

    let error = h
        .call(&public_name("probe-ext", "greet"), json!({}))
        .await
        .expect_err("plan mode refuses a process effect");
    plan_mode::clear_state(&h.session);
    assert!(error.to_string().contains("plan mode"), "got {error}");
    assert!(h.record("probe-ext", "starts.log").is_empty(), "no child started");
}

#[tokio::test]
async fn approval_write_mode_asks_before_the_child_starts() {
    let _env = TEST_ENV.lock().await;
    let mut h = harness("[tools]\nprofile = \"core\"\napproval_mode = \"write\"\n").await;
    let fixture = extension_fixture("probe-ext", &["greet"], &[]);
    h.install(fixture.path());
    h.enter().await;

    let error = h
        .call(&public_name("probe-ext", "greet"), json!({"name": "approval"}))
        .await
        .expect_err("write mode asks before a call that runs a process");
    assert!(error.to_string().contains("needs approval"), "got {error}");
    assert!(
        h.record("probe-ext", "starts.log").is_empty(),
        "a refused call must not start the child"
    );
}

#[tokio::test]
async fn approval_off_runs_the_call_as_the_existing_contract_says() {
    let _env = TEST_ENV.lock().await;
    let mut h = harness("[tools]\nprofile = \"core\"\napproval_mode = \"off\"\n").await;
    let fixture = extension_fixture("probe-ext", &["greet"], &[]);
    h.install(fixture.path());
    h.enter().await;

    h.call(&public_name("probe-ext", "greet"), json!({"name": "off"}))
        .await
        .expect("off auto-approves an ordinary call");
    assert_eq!(h.record("probe-ext", "calls.log").len(), 1);
}

// ---------------------------------------------------------------------------
// Secrets
// ---------------------------------------------------------------------------

/// A GitHub-shaped fixture value: long enough for the masking rule, shorter than
/// the 36 characters the repository secret scan looks for.
const FIXTURE_CREDENTIAL: &str = "ghp_fixture0123456789abcdefghij";
const UNKNOWN_PLACEHOLDER: &str = "«SECRET:test:0123456789ab»";

#[tokio::test]
async fn a_known_secret_value_never_reaches_the_child() {
    let _env = TEST_ENV.lock().await;
    let mut h = harness("[tools]\nprofile = \"core\"\n").await;
    let fixture = extension_fixture("probe-ext", &["greet"], &[]);
    h.install(fixture.path());
    h.enter().await;
    // Teach the session vault this value, so a call carrying it is exactly the
    // leak the refusal exists for.
    secrets::session_vault_with(&h.session, |vault| secrets::obfuscate(FIXTURE_CREDENTIAL, vault, &[]))
        .expect("the session has a vault");

    let error = h
        .call(&public_name("probe-ext", "greet"), json!({"name": FIXTURE_CREDENTIAL}))
        .await
        .expect_err("a known secret value is refused");
    assert!(error.to_string().contains("secret"), "got {error}");
    assert!(h.record("probe-ext", "starts.log").is_empty(), "no child started");
}

#[tokio::test]
async fn an_unresolved_secret_placeholder_never_reaches_the_child() {
    let _env = TEST_ENV.lock().await;
    let mut h = harness("[tools]\nprofile = \"core\"\n").await;
    let fixture = extension_fixture("probe-ext", &["greet"], &[]);
    h.install(fixture.path());
    h.enter().await;

    let error = h
        .call(&public_name("probe-ext", "greet"), json!({"name": UNKNOWN_PLACEHOLDER}))
        .await
        .expect_err("an unresolved placeholder is refused");
    assert!(error.to_string().contains("placeholder"), "got {error}");
    assert!(h.record("probe-ext", "starts.log").is_empty(), "no child started");
}

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn the_pre_tool_gate_sees_the_exact_payload_the_child_receives() {
    let _env = TEST_ENV.lock().await;
    let snapshot = tempfile::tempdir().expect("hook directory");
    let captured = snapshot.path().join("captured.json");
    let hook = snapshot.path().join("capture.sh");
    write_hook(&hook, &format!("#!/bin/sh\ncat > '{}'\nexit 0\n", captured.display()));
    let mut h = harness(&format!(
        "[tools]\nprofile = \"core\"\n[hooks]\npre_tool = \"{}\"\n",
        hook.display()
    ))
    .await;
    let fixture = extension_fixture("probe-ext", &["greet"], &[]);
    h.install(fixture.path());
    h.enter().await;

    let input = json!({"name": "gate", "nested": {"n": 1, "flag": true}});
    h.call(&public_name("probe-ext", "greet"), input.clone())
        .await
        .expect("the allowed call runs");

    let reviewed = fs::read_to_string(&captured).expect("the gate saw the payload");
    let calls = h.record("probe-ext", "calls.log");
    assert_eq!(calls.len(), 1, "one call crossed: {calls:?}");
    let delivered = calls[0]
        .split_once(' ')
        .map(|(_, arguments)| arguments)
        .expect("the record holds the arguments");
    assert_eq!(
        reviewed.trim(),
        delivered.trim(),
        "the gate must review the exact bytes the child receives"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_blocking_pre_tool_gate_keeps_the_call_from_the_child() {
    let _env = TEST_ENV.lock().await;
    let snapshot = tempfile::tempdir().expect("hook directory");
    let hook = snapshot.path().join("block.sh");
    write_hook(&hook, "#!/bin/sh\necho 'blocked by the probe gate' >&2\nexit 2\n");
    let mut h = harness(&format!(
        "[tools]\nprofile = \"core\"\n[hooks]\npre_tool = \"{}\"\n",
        hook.display()
    ))
    .await;
    let fixture = extension_fixture("probe-ext", &["greet"], &[]);
    h.install(fixture.path());
    h.enter().await;

    let error = h
        .call(&public_name("probe-ext", "greet"), json!({"name": "blocked"}))
        .await
        .expect_err("the gate blocks the call");
    assert!(error.to_string().contains("pre_tool hook"), "got {error}");
    assert!(h.record("probe-ext", "starts.log").is_empty(), "no child started");
}

#[tokio::test]
async fn early_child_exit_includes_stderr_in_initialize_error() {
    let _env = TEST_ENV.lock().await;
    let mut h = harness("[tools]\nprofile = \"core\"\n").await;
    let fixture = extension_fixture("probe-stderr", &["greet"], &[]);

    h.install(fixture.path());
    h.enter().await;

    let error = h
        .call(&public_name("probe-stderr", "greet"), json!({}))
        .await
        .expect_err("exiting child must fail initialize");
    assert!(
        error.to_string().contains("boom on stderr"),
        "expected stderr snippet in error: {error}"
    );
}

// ---------------------------------------------------------------------------
// Collision and revocation
// ---------------------------------------------------------------------------

struct FixedTool {
    name: String,
}

#[async_trait]
impl Tool for FixedTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "A tool that was here first"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _input: Value, _ctx: ToolContext) -> anyhow::Result<ToolOutput> {
        Ok(ToolOutput::new("the existing tool answered"))
    }
}

#[tokio::test]
async fn an_existing_tool_name_wins_over_the_package_declaration() {
    let _env = TEST_ENV.lock().await;
    let mut h = harness("[tools]\nprofile = \"core\"\n").await;
    let fixture = extension_fixture("probe-ext", &["greet"], &[]);
    h.install(fixture.path());
    let name = public_name("probe-ext", "greet");
    h.registry
        .register(name.clone(), Arc::new(FixedTool { name: name.clone() }))
        .await;
    h.enter().await;

    let output = h.call(&name, json!({})).await.expect("the existing tool answers");
    assert_eq!(output.output, "the existing tool answered");
    assert!(
        h.record("probe-ext", "starts.log").is_empty(),
        "a colliding name never reaches the package"
    );
}

// The case waits on the child's own files with blocking sleeps while the call
// it spawned must keep running, so it needs a second worker thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revocation_mid_call_ends_the_request_and_reaps_the_child() {
    let _env = TEST_ENV.lock().await;
    let mut h = harness("[tools]\nprofile = \"core\"\n").await;
    // A package ID of its own: a revocation is recorded for the whole process,
    // so the other cases must not share it.
    let fixture = extension_fixture("probe-revoke", &["slow", "greet"], &[]);
    h.install(fixture.path());
    h.enter().await;
    let generation = h
        .agent
        .package_snapshot()
        .expect("the turn captured a generation")
        .generation();

    let registry = h.registry.clone();
    let context = h.context("revocation");
    let name = public_name("probe-revoke", "slow");
    let call = tokio::spawn(async move { registry.execute(&name, json!({"wait": true}), context).await });
    wait_for_record(&h.state("probe-revoke").join("calls.log"), "the slow call");
    wait_for_record(&h.state("probe-revoke").join("heartbeat"), "the parked child");

    PackageLifecycleRegistry::shared().revoke("probe-revoke", generation + 1, RevocationCause::Disabled);

    let outcome = tokio::time::timeout(Duration::from_secs(30), call)
        .await
        .expect("a revoked request must not wait for its reply deadline")
        .expect("the call task must not panic");
    let error = outcome.expect_err("the in-flight call fails when the child is revoked");
    assert!(error.to_string().contains("probe-revoke"), "got {error}");
    wait_until_quiet(&h.state("probe-revoke").join("heartbeat"));

    // A second call from a generation the revocation outran is refused at the
    // handle, before anything could start another child.
    let error = h
        .call(&public_name("probe-revoke", "greet"), json!({}))
        .await
        .expect_err("a stale handle is refused");
    assert!(error.to_string().contains("stale"), "got {error}");
    assert_eq!(
        h.record("probe-revoke", "starts.log").len(),
        1,
        "the revoked child was not replaced"
    );
}
