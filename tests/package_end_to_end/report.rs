//! The one line per check and the machine-readable report the compat workflow collects.
//!
//! Every check prints exactly one `PKG_E2E <kind> <step> ok` or
//! `PKG_E2E <kind> <step> FAIL <reason>` line, so a workflow log is readable
//! without parsing JSON, and `HORON_E2E_REPORT` additionally writes the
//! document `scripts/ci/package_compat.py` reads. Reasons stay off the JSON: a
//! reader that only needs pass/fail is not forced to tolerate free text.

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

/// The two package kinds, as the report names them.
pub const KINDS: [&str; 2] = ["extension", "plugin"];

/// Every step the report may mention, in matrix order.
///
/// `resource_call` belongs to the plugin: a plugin is the kind that bundles a
/// skill, rule, prompt, MCP server or command beside its executable. An
/// extension ships the executable only, so none of those resources exists to
/// call and the step is not part of its report.
pub const STEP_ORDER: [&str; 9] = [
    "install",
    "next_turn_tool_call",
    "resource_call",
    "cancel",
    "replace",
    "remove_blocks_new_calls",
    "descendants_reaped",
    "bad_target_refused",
    "integrity_refused",
];

const EXTENSION_STEPS: [&str; 8] = [
    "install",
    "next_turn_tool_call",
    "cancel",
    "replace",
    "remove_blocks_new_calls",
    "descendants_reaped",
    "bad_target_refused",
    "integrity_refused",
];

/// The steps one kind must report, in matrix order.
pub fn steps(kind: &str) -> &'static [&'static str] {
    match kind {
        "plugin" => &STEP_ORDER,
        _ => &EXTENSION_STEPS,
    }
}

/// Outcomes collected in print order.
pub struct Report {
    steps: Vec<(&'static str, &'static str, bool)>,
}

impl Report {
    pub fn new() -> Self {
        Self {
            steps: Vec::with_capacity(KINDS.len() * STEP_ORDER.len()),
        }
    }

    /// Print one line and remember the outcome.
    pub fn check(&mut self, kind: &'static str, step: &'static str, outcome: Result<(), String>) -> bool {
        // A step the kind does not have would be rejected when the report is
        // read, so it is a mistake in the test rather than a result.
        assert!(steps(kind).contains(&step), "{kind} has no {step} step");
        let ok = outcome.is_ok();
        match &outcome {
            Ok(()) => println!("PKG_E2E {kind} {step} ok"),
            // A reason with a newline would break the one-line contract the
            // workflow greps for; the pipe keeps it on the same line.
            Err(reason) => println!("PKG_E2E {kind} {step} FAIL {}", reason.replace('\n', " | ")),
        }
        self.steps.push((kind, step, ok));
        ok
    }

    /// `kind/step` pairs that failed, in print order.
    pub fn failures(&self) -> Vec<String> {
        self.steps
            .iter()
            .filter(|(_, _, ok)| !ok)
            .map(|(kind, step, _)| format!("{kind}/{step}"))
            .collect()
    }

    /// `kind/step` pairs the run never reached.
    pub fn missing(&self) -> Vec<String> {
        let mut missing = Vec::new();
        for kind in KINDS {
            for step in steps(kind) {
                if !self
                    .steps
                    .iter()
                    .any(|(seen_kind, seen_step, _)| *seen_kind == kind && *seen_step == *step)
                {
                    missing.push(format!("{kind}/{step}"));
                }
            }
        }
        missing
    }

    /// Write the JSON report, when the workflow asked for one.
    pub fn write(&self, path: Option<&Path>, host_triple: &str) {
        let Some(path) = path else {
            return;
        };
        let steps: Vec<Value> = self
            .steps
            .iter()
            .map(|(kind, step, ok)| json!({ "kind": kind, "step": step, "ok": ok }))
            .collect();
        let document = json!({
            "schema": 1,
            "host_triple": host_triple,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "cross_compiled": cross_compiled(),
            "steps": steps,
        });
        let mut text = serde_json::to_string_pretty(&document).expect("serialize the package report");
        text.push('\n');
        fs::write(path, text).unwrap_or_else(|error| panic!("write the package report {}: {error}", path.display()));
    }
}

/// The target triple this test binary was compiled for.
///
/// The compiler that built the workspace is the only authority on that: Cargo
/// does not hand `TARGET` to integration tests, so the triple is derived from the
/// compile-time OS/arch pair, spelled the way the release matrix spells it.
pub fn host_triple() -> &'static str {
    crate::stage::host_target()
}

/// Whether this binary is not native to the machine that ran it.
///
/// The report proves native execution only when the compiler that built the
/// workspace names the same host triple this test binary was compiled for. A
/// runner whose `rustc` cannot be run leaves the claim at `false`, because
/// nothing showed that the binary was built elsewhere.
pub fn cross_compiled() -> bool {
    match rustc_host() {
        Some(host) => host != crate::stage::host_target(),
        None => false,
    }
}

/// The `host:` line of `rustc -vV`, when the compiler can be asked.
fn rustc_host() -> Option<String> {
    let output = Command::new("rustc").arg("-vV").output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(|host| host.trim().to_owned())
        .filter(|host| !host.is_empty())
}
