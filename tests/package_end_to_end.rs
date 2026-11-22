//! Both package kinds, installed and executed on the host they were built for.
//!
//! `.github/workflows/package-compat.yml` runs this target on one native runner
//! per supported target:
//!
//! ```text
//! cargo test --locked --test package_end_to_end -- --nocapture
//! ```
//!
//! with `HORON_E2E_EXAMPLE_BIN` naming the example executable built for that
//! host and `HORON_E2E_REPORT` naming the JSON report the workflow collects.
//! Without the variable the test returns early and prints one skip line, so the
//! default CI run builds nothing extra and stays cheap.
//!
//! The agent under test is the real `horon-agent` binary; the installed package
//! is the real example, staged in each of its two layouts; only the model is
//! faked, by a loopback `/v1/chat/completions` endpoint. Every check prints
//! exactly one `PKG_E2E <kind> <step> ok|FAIL <reason>` line.
//!
//! What each step proves:
//!
//! | Step | Kinds | Evidence |
//! | --- | --- | --- |
//! | `install` | both | `horon package install` publishes the copy and the catalog lists it |
//! | `next_turn_tool_call` | both | a turn whose fake model calls `ext__hello__greet` carries the child's answer back to the model |
//! | `resource_call` | plugin | the bundled executable command answers from a real child; the skill, rule, prompt, command and MCP server the plugin declares are loaded for the same generation, and its rule and skill reach the model |
//! | `cancel` | both | a `wait` call that never answers ends the run at its own `--max-time` deadline (below the host's 30 s reply deadline; status 124 on Unix, any non-zero on Windows, plus the literal) and leaves no child |
//! | `replace` | both | a second pending `wait` call stays pending across a `--replace` install of 0.2.0 (a turn keeps the generation it started with), ends at its own deadline, leaves no old child, and the next turn reaches the new copy |
//! | `remove_blocks_new_calls` | both | after `horon package remove` the tool is off the session's allow-list, so the run fails naming `ext__hello__greet` as not allowed, no child answers, and nothing is left running |
//! | `descendants_reaped` | both | after the runs, no process whose command line names this home's installed copies is left |
//! | `bad_target_refused` | both | an install whose declaration names no executable for this host is refused |
//! | `integrity_refused` | both | a declaration changed after install is reported by `verify`, and the same allow-list refusal keeps every turn away from the changed copy |
//!
//! `resource_call` is the plugin's step: a plugin is the kind that bundles
//! resources beside its executable, while an extension ships the executable
//! only, so the report of the extension kind stops at the eight steps above
//! without it.
//!
//! The one part a native runner cannot show from here is a `wait` call cancelled
//! by an operator keystroke: a one-shot run has no interactive cancel, so the
//! cancellation under test is the host's own deadline path
//! (`--max-time` → graceful shutdown), which is the same signal an interactive
//! cancel fires.

#[path = "package_end_to_end/host.rs"]
mod host;
#[path = "package_end_to_end/model.rs"]
mod model;
#[path = "package_end_to_end/procs.rs"]
mod procs;
#[path = "package_end_to_end/report.rs"]
mod report;
#[path = "package_end_to_end/stage.rs"]
mod stage;

use std::fs;
use std::path::Path;
use std::time::Duration;

use horon_agent::packages::{PackageRecord, PackageState, PackageStore, PackageTextResources};
use serde_json::Value;

use report::Report;
use stage::{Cli, Kind};

/// Deadline for a run whose package call never answers.
///
/// A call the child never answers is ended by the host's own reply deadline,
/// `WireLimits::reply_deadline` (30 s, `crates/app-core/src/extensions/wire.rs`;
/// the Limits table in `docs/runtime/PACKAGES.md`), and the turn then finishes
/// normally with an error tool result. So the run's `--max-time` must fire
/// first: 20 s leaves room for process startup and the model round and still
/// leaves 10 s before the wire gives up.
const PENDING_DEADLINE: &str = "20";

/// How long the test waits for a package child to appear during that call.
///
/// The wait plus the replacement install that follows must finish inside the
/// run's own 20 s deadline, and the whole child-start phase stays under the
/// host's 30 s reply deadline: 8 s plus a second or two of install is well
/// inside both, and even a slow Windows start fits.
const CHILD_DEADLINE: Duration = Duration::from_secs(8);

/// How long a run may take after it started, so a hung CLI cannot hang the test.
const RUN_DEADLINE: Duration = Duration::from_secs(120);

/// How long a reaped process may take to leave the process table.
const REAP_DEADLINE: Duration = Duration::from_secs(30);

#[test]
fn package_kinds_execute_end_to_end() {
    let Some(example) = stage::example_binary() else {
        println!("PKG_E2E skipped: HORON_E2E_EXAMPLE_BIN is not set");
        return;
    };
    assert!(
        example.is_file(),
        "HORON_E2E_EXAMPLE_BIN does not name a file: {}",
        example.display()
    );

    let model = model::Model::start();
    let mut report = Report::new();
    // One isolated horon home per kind: a plugin install must not land on a
    // record the extension pass left behind (an install over an existing id is
    // refused without `--replace`, and the ids are the same example id).
    for kind in [Kind::Extension, Kind::Plugin] {
        let scratch = Scratch::new();
        let cli = Cli::new(scratch.path(), model.base_url());
        run_kind(scratch.path(), &example, &cli, &model, kind, &mut report);
    }

    let destination = std::env::var_os("HORON_E2E_REPORT").map(std::path::PathBuf::from);
    report.write(destination.as_deref(), report::host_triple());

    let missing = report.missing();
    assert!(
        missing.is_empty(),
        "the package end-to-end report is incomplete: {}",
        missing.join(", ")
    );
    let failures = report.failures();
    assert!(failures.is_empty(), "failed package checks: {}", failures.join(", "));
}

/// One package kind, from install to a refused integrity change.
fn run_kind(scratch: &Path, example: &Path, cli: &Cli, model: &model::Model, kind: Kind, report: &mut Report) {
    let label = kind.label();
    let store = PackageStore::for_horon_home(cli.home());
    // Every installed copy of this home lives below this path and nothing else
    // does, so it identifies a package child exactly.
    let fragment = path_text(&store.store_dir());
    let sources = scratch.join("sources").join(label);

    let first = stage::stage(&sources.join("v1"), kind, "0.1.0", example);
    let installed = install(cli, &first, false).and_then(|stdout| {
        require(
            &stdout,
            &format!("Installed hello 0.1.0 [{label}, installed, global]"),
            "unexpected install report",
        )?;
        expect_record(cli, "hello", "0.1.0")
    });
    if !report.check(label, "install", installed) {
        for step in report::steps(label).iter().skip(1) {
            report.check(label, *step, Err("the install did not complete".to_owned()));
        }
        return;
    }

    // A turn whose model calls the package tool: the child's answer has to come
    // back to the model, which is what the next request's tool message shows.
    let greet = format!("PKG-E2E-{label}-greet");
    model.script(&greet, "ext__hello__greet", r#"{"name":"Ada"}"#);
    let next_turn = run_turn(cli, &greet).and_then(|()| {
        let tool = tool_result(model, &greet)?;
        require(&tool, "hello, Ada", "the model did not receive the child's answer")
    });
    report.check(label, "next_turn_tool_call", next_turn);

    // The plugin's bundled resources, exercised while the copy is installed:
    // its executable command answers from a real child, its skill, rule, prompt,
    // command and MCP server are loaded for the same generation, and its rule
    // and skill reach the model that turn already sent.
    if kind == Kind::Plugin {
        let resources = plugin_resources(cli, model, &greet);
        report.check(label, "resource_call", resources);
    }

    // (a) Cancellation: a call that never answers. The run's own deadline has to
    // fire before the host's reply deadline ends the call with an error result,
    // and the child the call held has to be reaped when the run stops.
    let wait = format!("PKG-E2E-{label}-wait");
    model.script(&wait, "ext__hello__wait", "{}");
    let pending = pending_run(cli, &wait, PENDING_DEADLINE);
    let saw_child = !procs::wait_for(&fragment, CHILD_DEADLINE).is_empty();
    let cancelled = pending.finish(RUN_DEADLINE);
    let cancel = (|| -> Result<(), String> {
        if !saw_child {
            return Err(format!(
                "no package child appeared within {CHILD_DEADLINE:?}; run output: {} {}",
                cancelled.stdout.trim(),
                cancelled.stderr.trim()
            ));
        }
        // A deadline exit is 124 on Unix; Windows has no such convention, so any
        // non-zero status plus the literal below is the evidence there.
        let status_ok = if std::env::consts::OS == "windows" {
            !cancelled.status.success()
        } else {
            cancelled.status.code() == Some(124)
        };
        if !status_ok {
            return Err(format!(
                "the run that waited on a package call ended with {:?}, not its deadline status: {} {}",
                cancelled.status.code(),
                cancelled.stdout.trim(),
                cancelled.stderr.trim()
            ));
        }
        require(
            &cancelled.stderr,
            "run exceeded its --max-time limit",
            "the pending call did not end on the run's deadline",
        )?;
        procs::wait_until_gone(&fragment, REAP_DEADLINE)
    })();
    report.check(label, "cancel", cancel);

    // (b) Replacement: a second pending call, with the new copy published while
    // it waits. A turn keeps the generation it started with, so the replacement
    // does not end the call: the run is still working right after the install
    // and stops at its own deadline. The next turn then reads the new copy.
    let second = stage::stage(&sources.join("v2"), kind, "0.2.0", example);
    let waiting = format!("PKG-E2E-{label}-replaced-call");
    model.script(&waiting, "ext__hello__wait", "{}");
    let mut pending = pending_run(cli, &waiting, PENDING_DEADLINE);
    let replace_child = !procs::wait_for(&fragment, CHILD_DEADLINE).is_empty();
    let replace_install = if replace_child {
        install(cli, &second, true)
    } else {
        Err("the wait call never reached an installed child".to_owned())
    };
    let still_running = pending.running();
    let after_replace = pending.finish(RUN_DEADLINE);
    let replaced = replace_install
        .and_then(|stdout| {
            require(&stdout, "Installed hello 0.2.0", "unexpected replace report")?;
            if !still_running {
                return Err("the run had already ended when the replacement finished".to_owned());
            }
            Ok(())
        })
        .and_then(|()| {
            require(
                &after_replace.stderr,
                "run exceeded its --max-time limit",
                "the call kept from the old generation did not end on the run's deadline",
            )
        })
        .and_then(|()| procs::wait_until_gone(&fragment, REAP_DEADLINE))
        .and_then(|()| {
            let marker = format!("PKG-E2E-{label}-replaced");
            model.script(&marker, "ext__hello__greet", r#"{"name":"Ada"}"#);
            run_turn(cli, &marker)?;
            expect_record(cli, "hello", "0.2.0")?;
            let tool = tool_result(model, &marker)?;
            require(&tool, "hello, Ada", "the replaced copy did not answer the next turn")
        });
    report.check(label, "replace", replaced);

    let removed = stage::success("package remove", &cli.run(&["package", "remove", "hello"]))
        .and_then(|stdout| require(&stdout, "Removed hello", "unexpected remove report"))
        .and_then(|()| {
            let marker = format!("PKG-E2E-{label}-removed");
            model.script(&marker, "ext__hello__greet", r#"{"name":"Ada"}"#);
            expect_refused_turn(cli, model, &marker, &fragment)
        });
    report.check(label, "remove_blocks_new_calls", removed);

    // A liveness probe that never saw a child cannot prove one was reaped, so
    // the reaping step depends on the same observation the cancel step used.
    let reaped = if saw_child {
        procs::wait_until_gone(&fragment, REAP_DEADLINE)
    } else {
        Err("the liveness probe never observed a package child".to_owned())
    };
    report.check(label, "descendants_reaped", reaped);

    let foreign = stage::stage_foreign(&sources.join("foreign"), example);
    let installed_foreign = cli.run(&["package", "install", &path_text(&foreign)]);
    let bad_target =
        stage::refused("package install of a foreign-target package", &installed_foreign).and_then(|stderr| {
            require(
                &stderr,
                "no executable for",
                "the refusal does not name a missing target",
            )?;
            require(
                &stderr,
                stage::host_target(),
                "the refusal does not name the host target",
            )
        });
    report.check(label, "bad_target_refused", bad_target);

    // The declaration is what the host compares against the installation
    // record, so changing it after install is the integrity error a start
    // refuses; the binary bytes are covered by `package verify`'s file hashes.
    let integrity = install(cli, &first, true)
        .and_then(|stdout| require(&stdout, "Installed hello 0.1.0", "unexpected reinstall report"))
        .and_then(|()| tamper_declaration(cli))
        .and_then(|()| {
            let verify = cli.run(&["package", "verify"]);
            let stderr = stage::refused("package verify", &verify)?;
            require(
                &stderr,
                "stored package hello",
                "verify did not report the changed copy",
            )
        })
        .and_then(|()| {
            let marker = format!("PKG-E2E-{label}-tampered");
            model.script(&marker, "ext__hello__greet", r#"{"name":"Ada"}"#);
            expect_refused_turn(cli, model, &marker, &fragment)
        });
    report.check(label, "integrity_refused", integrity);
}

/// The resources a plugin contributes to the session that lists it.
fn plugin_resources(cli: &Cli, model: &model::Model, marker: &str) -> Result<(), String> {
    // Its bundled extension declares one executable command; the host's own
    // dispatch starts the installed child to answer it.
    let value = host::call_command(cli.home(), "hello", "hello")?;
    if value["command"] != "hello" {
        return Err(format!("the executable command answered {value}"));
    }
    if value["arguments"]["text"] != "qa" {
        return Err(format!("the executable command saw the wrong arguments: {value}"));
    }

    let store = PackageStore::for_horon_home(cli.home());
    let catalog = store.catalog().map_err(|error| format!("read the catalog: {error}"))?;
    let record = catalog
        .get("hello")
        .ok_or_else(|| "the catalog has no hello record".to_owned())?
        .clone();

    let resources = PackageTextResources::load(&store, None, std::iter::once(&record));
    if let Some(reason) = resources.rejection("hello") {
        return Err(format!("the plugin's text resources were rejected: {reason}"));
    }
    let skill = resources.skills().next().ok_or("the plugin loaded no skill")?;
    // A package skill's public name is namespaced, and that is the name the
    // model is told about.
    if skill.name != "hello--greeting" || !skill.description.contains("Greet one person by name") {
        return Err(format!("unexpected skill: {} ({})", skill.name, skill.description));
    }
    if !resources
        .rules()
        .any(|rule| rule.text.contains("- One sentence per greeting."))
    {
        return Err("the plugin loaded no rule with the example's first sentence".to_owned());
    }
    let prompt = resources
        .prompt("hello--greeting")
        .ok_or("the plugin loaded no prompt named hello--greeting")?;
    if !prompt.contains("# Greeting prompt") || !prompt.contains("Draft one greeting") {
        return Err(format!("unexpected prompt text: {prompt}"));
    }
    let command = resources
        .command("hello", "greeting")
        .ok_or("the plugin loaded no command named hello:greeting")?;
    if command.name != "hello:greeting" || !command.body.contains("Write one greeting for $ARGUMENTS") {
        return Err(format!("unexpected command: {} ({})", command.name, command.body));
    }
    let servers = store
        .mcp_servers(&record)
        .map_err(|error| format!("read the plugin's MCP servers: {error}"))?;
    if !servers
        .iter()
        .any(|server| server.public_name == "plugin_hello_example-clock")
    {
        return Err(format!(
            "the plugin registered no example-clock MCP server: {:?}",
            servers
                .iter()
                .map(|server| server.public_name.as_str())
                .collect::<Vec<_>>()
        ));
    }

    // A rule and a skill reach a session only through the request the model
    // reads, so the request itself is the evidence that they arrived.
    let body = request_with(model, marker)?;
    require(
        &body,
        "## Package hello (resources/rules/greeting.md)",
        "the rule did not reach the model",
    )?;
    require(
        &body,
        "- One sentence per greeting.",
        "the rule text did not reach the model",
    )?;
    require(
        &body,
        "`/hello--greeting ` - Greet one person by name in the package example's wording.",
        "the skill did not reach the model",
    )
}

/// Change the installed declaration so it no longer matches its record.
fn tamper_declaration(cli: &Cli) -> Result<(), String> {
    let store = PackageStore::for_horon_home(cli.home());
    let catalog = store.catalog().map_err(|error| format!("read the catalog: {error}"))?;
    let record = catalog
        .get("hello")
        .ok_or_else(|| "the catalog has no hello record".to_owned())?;
    let digest = record
        .digest
        .clone()
        .ok_or_else(|| "the record names no published copy".to_owned())?;
    let declared = record
        .files
        .iter()
        .find(|file| file.path.ends_with("extension.toml"))
        .ok_or_else(|| "the record lists no declaration file".to_owned())?;
    let installed = store.store_dir().join(digest).join(&declared.path);
    let metadata = fs::metadata(&installed).map_err(|error| format!("inspect {}: {error}", installed.display()))?;
    horon_agent_platform::fs::make_owner_writable(&installed, metadata.permissions())
        .map_err(|error| format!("make {} writable: {error}", installed.display()))?;
    let mut bytes = fs::read(&installed).map_err(|error| format!("read {}: {error}", installed.display()))?;
    bytes.extend_from_slice(b"\n# changed after install by the package end-to-end test\n");
    fs::write(&installed, bytes).map_err(|error| format!("write {}: {error}", installed.display()))
}

/// Start a run whose fake model asks for the `wait` call that never returns.
fn pending_run(cli: &Cli, marker: &str, max_time: &str) -> stage::Run {
    let message = format!("{marker} wait for the package call");
    cli.spawn(&[
        "run",
        message.as_str(),
        "--no-session",
        "--no-update",
        "--max-time",
        max_time,
    ])
}

/// Run one CLI invocation whose fake model answers with the scripted tool call.
fn run_turn(cli: &Cli, marker: &str) -> Result<(), String> {
    let message = format!("{marker} run the package turn");
    let finished = cli
        .spawn(&["run", message.as_str(), "--no-session", "--no-update"])
        .finish(RUN_DEADLINE);
    if !finished.status.success() {
        return Err(format!(
            "the run failed with {:?}: {} {}",
            finished.status.code(),
            finished.stdout.trim(),
            finished.stderr.trim()
        ));
    }
    require(
        &finished.stdout,
        model::FINAL_TEXT,
        "the turn did not reach the closing round after the tool call",
    )
}

/// A turn whose package call the session refuses before any child starts.
///
/// `horon-agent run` captures the turn's packages and adds only the names it
/// exposed to the session's allow-list, so a package that is removed or whose
/// declaration no longer matches leaves the tool off that list: the call is
/// refused by the session, the run fails, and no model round or child follows.
fn expect_refused_turn(cli: &Cli, model: &model::Model, marker: &str, fragment: &str) -> Result<(), String> {
    // The mock keeps every request of this kind, including the earlier turns
    // that legitimately carried the child's answer, so only the requests this
    // run makes can show whether a child answered it.
    let before = model.bodies().len();
    let message = format!("{marker} run the package turn");
    let finished = cli
        .spawn(&["run", message.as_str(), "--no-session", "--no-update"])
        .finish(RUN_DEADLINE);
    if finished.status.success() {
        return Err(format!(
            "the run accepted a call the session should refuse: {}",
            finished.stdout.trim()
        ));
    }
    require(
        &finished.stderr,
        "ext__hello__greet",
        "the refusal does not name the package tool",
    )?;
    // The core profile keeps the name off the session's allow-list, and a name
    // the registry lost answers as unknown; both are refusals before any child,
    // and `crates/app-core/tests/package_tools.rs` accepts the same pair.
    if !finished.stderr.contains("is not allowed") && !finished.stderr.contains("Unknown tool") {
        return Err(format!(
            "the run failed without refusing the tool by name: {}",
            finished.stderr.trim()
        ));
    }
    if let Some(answered) = model
        .bodies()
        .iter()
        .skip(before)
        .find(|raw| raw.contains("hello, Ada"))
    {
        return Err(format!("a child answered the refused call: {}", answered.trim()));
    }
    let pids = procs::pids_running(fragment);
    if !pids.is_empty() {
        return Err(format!("the refused call left processes running: {pids:?}"));
    }
    Ok(())
}

/// Install one staged package root, optionally replacing the record it finds.
fn install(cli: &Cli, source: &Path, replace: bool) -> Result<String, String> {
    let source = path_text(source);
    let mut args: Vec<&str> = vec!["package", "install", &source];
    if replace {
        args.push("--replace");
    }
    stage::success("package install", &cli.run(&args))
}

/// The catalog record of `id`, provided it is installed at `version`.
fn expect_record(cli: &Cli, id: &str, version: &str) -> Result<(), String> {
    let store = PackageStore::for_horon_home(cli.home());
    let catalog = store.catalog().map_err(|error| format!("read the catalog: {error}"))?;
    let record: &PackageRecord = catalog
        .get(id)
        .ok_or_else(|| format!("the catalog has no {id} record"))?;
    if record.state != PackageState::Installed {
        return Err(format!("{id} is {:?}, not installed", record.state));
    }
    if record.version != version {
        return Err(format!("{id} is version {}, expected {version}", record.version));
    }
    Ok(())
}

/// The tool results the model read in the request that carried `marker`.
fn tool_result(model: &model::Model, marker: &str) -> Result<String, String> {
    model
        .bodies()
        .iter()
        .rev()
        .filter(|raw| raw.contains(marker))
        .filter_map(|raw| serde_json::from_str::<Value>(raw).ok())
        .map(|body| tool_texts(&body))
        .find(|texts| !texts.is_empty())
        .map(|texts| texts.join("\n"))
        .ok_or_else(|| format!("no model request carried a tool result for {marker}"))
}

/// The first request that carried `marker`, exactly as the provider sent it.
fn request_with(model: &model::Model, marker: &str) -> Result<String, String> {
    model
        .bodies()
        .into_iter()
        .find(|raw| raw.contains(marker))
        .ok_or_else(|| format!("no model request carried {marker}"))
}

fn tool_texts(body: &Value) -> Vec<String> {
    let Some(messages) = body["messages"].as_array() else {
        return Vec::new();
    };
    messages
        .iter()
        .filter(|message| message["role"] == "tool")
        .filter_map(|message| match &message["content"] {
            Value::String(text) => Some(text.clone()),
            Value::Array(parts) => {
                let text: String = parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("");
                (!text.is_empty()).then_some(text)
            }
            _ => None,
        })
        .collect()
}

fn require(haystack: &str, needle: &str, reason: &str) -> Result<(), String> {
    if haystack.contains(needle) {
        Ok(())
    } else {
        Err(format!("{reason}: {needle} is missing from {}", haystack.trim()))
    }
}

fn path_text(path: &Path) -> String {
    path.to_str()
        .unwrap_or_else(|| panic!("{} is not valid UTF-8", path.display()))
        .to_owned()
}

/// A scratch directory under the OS temporary directory, removed on drop.
struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Self {
        let base = std::env::temp_dir()
            .canonicalize()
            .expect("the OS temporary directory must resolve");
        let dir = tempfile::Builder::new()
            .prefix("pkge2e-")
            .tempdir_in(base)
            .expect("a scratch directory");
        Self { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // An installed copy is read-only, which Windows cannot delete until it
        // is thawed; everything below the scratch root is this test's own.
        thaw(self.dir.path());
    }
}

fn thaw(path: &Path) {
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let child = entry.path();
        if child.is_dir() {
            thaw(&child);
        } else if let Ok(metadata) = fs::metadata(&child) {
            let _ = horon_agent_platform::fs::make_owner_writable(&child, metadata.permissions());
        }
    }
}
