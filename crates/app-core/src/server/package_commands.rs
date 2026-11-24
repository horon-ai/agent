//! Slash commands installed packages supply, listed and run for any client.
//!
//! Two kinds share one name space, `<package id>:<command>`. A template is a
//! Markdown resource the plugin declares: it expands into the user message a
//! person asked for. An executable command is declared in `extension.toml` and
//! answered by the package child; it passes the tool policy first, through
//! [`crate::tool::package_tool::command_gate`], so submitting a command can never run
//! a process a tool call would have had to ask about.
//!
//! Both the listing and the run read the session's current package snapshot
//! ([`crate::extensions::package_snapshot`]), which is the generation the turn
//! boundary captured. A command whose package the snapshot no longer lists is
//! refused before anything is read from disk or sent to a child, with a message
//! that says the entry is stale.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail, ensure};
use horon_agent_harness_api::{PackageCommandInfo, PackageCommandKind, PackageCommandList, PackageCommandOutcome};
use serde_json::{Value, json};

use crate::agent::Agent;
use crate::extensions::invoke;
use crate::extensions::package_snapshot;
use crate::packages::{PackageCommand, PackageTextResources};

/// Every package command this session currently provides.
///
/// The daemon's runtime-command dispatch calls this; integration tests drive
/// the same code with the agent a turn hands it.
pub async fn list(agent: &Agent) -> Result<(Value, String)> {
    let listing = generate(agent).await;
    let value = serde_json::to_value(&listing)?;
    let message = serde_json::to_string_pretty(&value)?;
    Ok((value, message))
}

/// Run the package command `args` names: `[package_id, command, …words]`.
///
/// The daemon's runtime-command dispatch calls this; integration tests drive
/// the same code with the agent a turn hands it.
pub async fn run(agent: &mut Agent, args: &[String]) -> Result<(Value, String)> {
    let [package_id, command, rest @ ..] = args else {
        bail!("A package command names a package and a command.");
    };
    let arguments = rest.join(" ");
    let session = package_snapshot::for_session(agent.session_id())
        .context("This session has no package generation, so it supplies no package commands.")?;
    let snapshot = session
        .current()
        .context("This session has no package generation, so it supplies no package commands.")?;
    let stale = |generation: u64| {
        anyhow::anyhow!(
            "The package command `{package_id}:{command}` is stale: package {package_id} is not part of \
             this session's package generation (generation {generation}). Run it again from the current list."
        )
    };
    let generation = snapshot.generation();
    let record = snapshot.get(package_id).ok_or_else(|| stale(generation))?.clone();
    // Revocation and a live disable outrank the captured generation: a command
    // from a package the catalog no longer offers must never reach a child.
    session
        .handle(package_id)
        .ok_or_else(|| stale(generation))?
        .ensure_live(&session)
        .await
        .map_err(|error| anyhow::anyhow!("The package command `{package_id}:{command}` is stale: {error:#}"))?;

    let store = session.store();
    let declarations = invoke::declarations_for(store, &record).await?;
    let outcome = if declarations
        .manifest
        .commands
        .iter()
        .any(|declared| declared.name.as_str() == command.as_str())
    {
        executable(agent, package_id, command, &arguments).await?
    } else {
        template(session.store(), &snapshot, package_id, command, &arguments)?
    };
    let message = match (&outcome.output, &outcome.text) {
        (Some(output), _) => output.clone(),
        (None, Some(text)) => text.clone(),
        (None, None) => String::new(),
    };
    Ok((serde_json::to_value(&outcome)?, message))
}

/// Build the listing from the session's current generation.
async fn generate(agent: &Agent) -> PackageCommandList {
    let Some(snapshot) = agent.package_snapshot() else {
        return PackageCommandList::default();
    };
    let Some(session) = package_snapshot::for_session(agent.session_id()) else {
        return PackageCommandList::default();
    };
    // Executable declarations are read first so a name the installer would have
    // refused stays the executable one: the tool-policy path is the stricter of
    // the two, and a tampered copy must not turn it into a template.
    let mut commands: BTreeMap<String, PackageCommandInfo> = BTreeMap::new();
    for record in snapshot.packages() {
        match invoke::declarations_for(session.store(), record).await {
            Ok(declarations) => {
                for declared in &declarations.manifest.commands {
                    commands.insert(
                        format!("{}:{}", record.id, declared.name),
                        PackageCommandInfo {
                            name: format!("{}:{}", record.id, declared.name),
                            package_id: record.id.clone(),
                            command: declared.name.clone(),
                            kind: PackageCommandKind::Executable,
                            description: declared.description.clone(),
                            argument_hint: None,
                        },
                    );
                }
            }
            Err(error) => crate::logging::warn(&format!(
                "package {} does not list its executable commands: {error:#}",
                record.id
            )),
        }
    }
    let resources = PackageTextResources::load(session.store(), snapshot.workspace(), snapshot.packages());
    for command in resources.commands() {
        commands
            .entry(command.name.clone())
            .or_insert_with(|| PackageCommandInfo {
                name: command.name.clone(),
                package_id: command.package_id.clone(),
                command: command.command.clone(),
                kind: PackageCommandKind::Template,
                description: command.description.clone(),
                argument_hint: command.argument_hint.clone(),
            });
    }
    for record in snapshot.packages() {
        if let Some(reason) = resources.rejection(&record.id) {
            crate::logging::warn(&format!(
                "package {} does not list its command resources: {reason}",
                record.id
            ));
        }
    }
    PackageCommandList {
        generation: snapshot.generation(),
        commands: commands.into_values().collect(),
    }
}

/// Run one declared executable command through the tool policy.
async fn executable(
    agent: &mut Agent,
    package_id: &str,
    command: &str,
    arguments: &str,
) -> Result<PackageCommandOutcome> {
    let name = crate::tool::package_tool::command_gate::policy_name(package_id, command);
    let context = crate::tool::ToolContext {
        session_id: agent.session_id().into(),
        message_id: crate::id::new_id("msg"),
        tool_call_id: crate::id::new_id("call"),
        working_dir: agent.working_dir().map(PathBuf::from),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    };
    // The shape a package documents for a command: the raw text after the name
    // and its whitespace-split words. Both are body text, never a file path the
    // host resolves.
    let input = json!({
        "text": arguments.trim(),
        "args": arguments.split_whitespace().collect::<Vec<_>>(),
    });
    let input = crate::tool::package_tool::command_gate::authorize(agent.session_id(), &name, input, &context).await?;
    let value = invoke::call_command(agent.session_id(), package_id, command, input).await?;
    Ok(PackageCommandOutcome {
        name: format!("{package_id}:{command}"),
        package_id: package_id.to_owned(),
        command: command.to_owned(),
        kind: PackageCommandKind::Executable,
        text: None,
        output: Some(invoke::render_value(&value)),
        data: Some(value),
    })
}

/// Expand one Markdown command into the user message it stands for.
fn template(
    store: &crate::packages::PackageStore,
    snapshot: &package_snapshot::PackageSnapshot,
    package_id: &str,
    command: &str,
    arguments: &str,
) -> Result<PackageCommandOutcome> {
    let resources = PackageTextResources::load(store, snapshot.workspace(), snapshot.packages());
    if let Some(reason) = resources.rejection(package_id) {
        bail!("The commands of package {package_id} are unavailable: {reason}");
    }
    let Some(declared) = resources.command(package_id, command) else {
        bail!("Package {package_id} does not declare the command `{command}`.");
    };
    Ok(PackageCommandOutcome {
        name: declared.name.clone(),
        package_id: package_id.to_owned(),
        command: command.to_owned(),
        kind: PackageCommandKind::Template,
        text: Some(expand(declared, arguments)?),
        output: None,
        data: None,
    })
}

/// The text one template command expands to.
fn expand(declared: &PackageCommand, arguments: &str) -> Result<String> {
    let text = crate::packages::expand(&declared.body, arguments);
    ensure!(
        !text.trim().is_empty(),
        "The package command `{}` expands to nothing.",
        declared.name
    );
    Ok(text)
}
