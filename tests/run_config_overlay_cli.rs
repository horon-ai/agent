//! `horon run --config <file>`: per-run configuration overlays, driven through the real binary.

use std::path::Path;
use std::process::{Command, Output};

fn horon(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_horon-agent"))
        .current_dir(home)
        .env_clear()
        .env("HOME", home)
        .env("HORON_HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("HORON_AGENT_NON_INTERACTIVE", "1")
        .env("HORON_AGENT_NO_AUTO_UPDATE", "1")
        .env("HORON_AGENT_MEMORY_ENABLED", "0")
        .env("NO_COLOR", "1")
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .args(args)
        .output()
        .expect("run horon-agent")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn an_invalid_overlay_names_its_file_before_any_provider_work() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("broken-overlay.toml"), "[tools\nirc = true\n").unwrap();
    let result = horon(
        home.path(),
        &["run", "hi", "--no-update", "--config", "broken-overlay.toml"],
    );
    assert!(!result.status.success());
    assert!(stderr(&result).contains("broken-overlay.toml"), "{}", stderr(&result));
}

#[test]
fn an_overlay_is_applied_and_validated_for_that_run_only() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[tools]\nirc = false\n").unwrap();
    // `[prewalk] enabled` without `into` is invalid, so the run can only fail on it
    // if the overlay was merged over the file and checked.
    std::fs::write(home.path().join("overlay.toml"), "[prewalk]\nenabled = true\n").unwrap();

    let with = horon(home.path(), &["run", "hi", "--no-update", "--config", "overlay.toml"]);
    assert!(!with.status.success());
    assert!(stderr(&with).contains("[prewalk]"), "{}", stderr(&with));

    let without = horon(home.path(), &["run", "hi", "--no-update"]);
    assert!(!stderr(&without).contains("[prewalk]"), "{}", stderr(&without));
}
