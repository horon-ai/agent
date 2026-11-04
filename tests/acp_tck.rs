//! The official acp-tck gates: `scripts/test/acp_tck.sh` runs the pinned suite against a spawned,
//! isolated daemon, writes its JSON report, and exits non-zero unless the verdict is
//! conformant; these tests assert the same gate on the report itself, once for the v1 suite
//! and once for the v2 suite (`--protocol-version 2`).
//!
//! Ignored by default: the suite is external, it is cloned on a first run, and a full run
//! takes minutes. Run them with
//! `cargo test --locked -p horon-agent --test acp_tck -- --ignored --nocapture`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn target_dir(manifest: &Path) -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => {
            let dir = PathBuf::from(dir);
            if dir.is_absolute() { dir } else { manifest.join(dir) }
        }
        None => manifest.join("target"),
    }
}

fn count(verdict: &serde_json::Value, tier: &str, status: &str) -> u64 {
    verdict["tier_counts"][tier][status].as_u64().unwrap_or(0)
}

/// Run `scripts/test/acp_tck.sh` for one protocol version and return its report path.
///
/// `protocol_version` is `None` for the v1 gate, which must keep running the suite without
/// naming a version at all: the script's default is part of what the v1 gate protects.
fn run_suite(manifest: &Path, protocol_version: Option<u8>) -> PathBuf {
    let script = manifest.join("scripts").join("test").join("acp_tck.sh");
    assert!(script.is_file(), "{} is missing", script.display());
    let report_name = match protocol_version {
        Some(version) => format!("acp-tck-v{version}-report.json"),
        None => "acp-tck-report.json".to_string(),
    };
    let report_path = target_dir(manifest).join(report_name);
    let _ = std::fs::remove_file(&report_path);

    // The required-pass list the final gate will enable lives in the environment for now; an
    // empty value leaves the gate as it was.
    let required_passes = std::env::var("ACP_TCK_REQUIRE_PASS").unwrap_or_default();

    let mut command = Command::new("bash");
    command
        .arg(&script)
        .arg("--report")
        .arg(&report_path)
        .arg("--agent")
        .arg(env!("CARGO_BIN_EXE_horon-agent"));
    if let Some(version) = protocol_version {
        command.arg("--protocol-version").arg(version.to_string());
    }
    if !required_passes.trim().is_empty() {
        command.arg("--require-pass").arg(required_passes.trim());
    }
    let output = command.output().expect("scripts/test/acp_tck.sh runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("{stdout}{stderr}");

    let text = std::fs::read_to_string(&report_path).unwrap_or_else(|error| {
        panic!(
            "no acp-tck report at {} ({error}); the script exited {:?}:\n{stdout}{stderr}",
            report_path.display(),
            output.status.code()
        )
    });
    let report: serde_json::Value = serde_json::from_str(&text).expect("the acp-tck report is JSON");
    assert_conformant_report(&report, &report_path, protocol_version.map_or(1, u64::from));
    assert!(
        output.status.success(),
        "scripts/test/acp_tck.sh exited {:?} although the report passes its gate",
        output.status.code()
    );
    report_path
}

fn assert_conformant_report(report: &serde_json::Value, report_path: &Path, version: u64) {
    let verdict = &report["verdict"];

    for tier in ["MANDATORY", "CAPABILITY", "ADVISORY", "INFORMATIONAL"] {
        println!(
            "{tier}: pass={} fail={} skipped={} not_tested={}",
            count(verdict, tier, "PASS"),
            count(verdict, tier, "FAIL"),
            count(verdict, tier, "SKIPPED"),
            count(verdict, tier, "NOT_TESTED"),
        );
    }
    for requirement in report["requirements"].as_array().into_iter().flatten() {
        let tier = requirement["tier"].as_str().unwrap_or_default();
        let status = requirement["status"].as_str().unwrap_or_default();
        if matches!(tier, "MANDATORY" | "CAPABILITY") && status != "PASS" {
            println!(
                "{} {tier} {status}: {}",
                requirement["id"].as_str().unwrap_or_default(),
                requirement["text"].as_str().unwrap_or_default(),
            );
        }
    }

    let verdict_hint = format!("the acp-tck report is at {}", report_path.display());
    assert_eq!(
        report["protocol_version"].as_u64(),
        Some(version),
        "the suite ran a different protocol version than the gate claims; {verdict_hint}"
    );
    assert_eq!(
        verdict["conformant"].as_bool(),
        Some(true),
        "the run is not conformant; {verdict_hint}"
    );
    assert_eq!(
        verdict["blocked_by_auth"].as_bool(),
        Some(false),
        "the run is blocked by auth; {verdict_hint}"
    );
    assert_eq!(
        verdict["blocked_by_version_mismatch"].as_bool(),
        Some(false),
        "the run is blocked by a protocol version mismatch; {verdict_hint}"
    );
    assert_eq!(
        count(verdict, "MANDATORY", "FAIL"),
        0,
        "a MANDATORY requirement failed; {verdict_hint}"
    );
    assert_eq!(
        count(verdict, "MANDATORY", "NOT_TESTED"),
        0,
        "a MANDATORY requirement was never exercised; {verdict_hint}"
    );
    assert_eq!(
        count(verdict, "CAPABILITY", "FAIL"),
        0,
        "a CAPABILITY requirement failed; {verdict_hint}"
    );
}

#[test]
#[ignore = "runs the pinned external acp-tck v1 suite; needs uv and a first-run clone"]
fn acp_tck_full_v1_run_is_conformant() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    run_suite(&manifest, None);
}

#[test]
#[ignore = "runs the pinned external acp-tck v2 suite; needs uv and a first-run clone"]
fn acp_tck_full_v2_run_is_conformant() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    run_suite(&manifest, Some(2));
}
