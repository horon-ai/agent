//! `horon-agent install-release` driven through the real binary in an owned home.
//!
//! Each fixture builds the asset directory an installer would hand the command:
//! one tarred agent executable plus its `SHA256SUMS`. Verification, staging and
//! channel activation then run in the child process against a temporary
//! `HORON_AGENT_HOME`, so nothing here reads the machine's real installation.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const VERSION: &str = "1.2.3";
const NEXT_VERSION: &str = "1.2.4";
/// Small stand-in for the release executable: the update path moves bytes and
/// sets the executable bit, it does not inspect the binary format.
const AGENT_BYTES: &[u8] = b"#!/bin/sh\nprintf 'installed test agent\\n'\n";

struct Fixture {
    home: tempfile::TempDir,
    assets: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().expect("a temporary home"),
            assets: tempfile::tempdir().expect("a temporary assets directory"),
        }
    }

    fn install(&self, version: &str) -> Output {
        let assets = self.assets.path().to_string_lossy().into_owned();
        self.run(&["install-release", "--assets", &assets, "--agent-version", version])
    }

    fn installed_binary(&self, version: &str) -> PathBuf {
        self.home
            .path()
            .join("builds")
            .join("versions")
            .join(version)
            .join(binary_name())
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_horon-agent"))
            .current_dir(self.home.path())
            .env_clear()
            .env("HOME", self.home.path())
            .env("HORON_AGENT_HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env("XDG_CACHE_HOME", self.home.path().join("cache"))
            .env("HORON_AGENT_NON_INTERACTIVE", "1")
            .env("HORON_AGENT_NO_AUTO_UPDATE", "1")
            .env("NO_COLOR", "1")
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .args(args)
            .output()
            .expect("the CLI runs")
    }
}

/// The platform asset stem the release uses, e.g. `horon-agent-macos-aarch64`.
fn asset_stem() -> String {
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let extension = if cfg!(windows) { ".exe" } else { "" };
    format!("horon-agent-{platform}{extension}")
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "horon-agent.exe"
    } else {
        "horon-agent"
    }
}

/// Write a release asset directory: one archive of the agent executable plus
/// the checksum list that names it. An honest release records the archive's own
/// digest; a mismatched one records a digest for bytes the archive never held.
fn write_release(assets: &Path, binary: &[u8], honest: bool) -> String {
    let base = asset_stem();
    let archive_name = format!("{base}.tar.gz");
    let staging = tempfile::tempdir().expect("a staging directory");
    let staged = staging.path().join(&base);
    std::fs::write(&staged, binary).expect("the staged executable");
    let status = Command::new("tar")
        .arg("-czf")
        .arg(assets.join(&archive_name))
        .arg("-C")
        .arg(staging.path())
        .arg(&base)
        .status()
        .expect("tar runs");
    assert!(status.success(), "tar built the release archive");
    let archive_bytes = std::fs::read(assets.join(&archive_name)).expect("the release archive");
    let digest = if honest {
        sha256(&archive_bytes)
    } else {
        sha256(b"bytes the archive does not contain")
    };
    std::fs::write(assets.join("SHA256SUMS"), format!("{digest}  {archive_name}\n")).expect("SHA256SUMS");
    archive_name
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn file_sha256(path: &Path) -> String {
    sha256(&std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display())))
}

fn success(output: &Output) -> String {
    assert!(
        output.status.success(),
        "install-release failed with {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn installs_binary_from_assets_dir() {
    let fixture = Fixture::new();
    let archive_name = write_release(fixture.assets.path(), AGENT_BYTES, true);

    let stdout = success(&fixture.install(VERSION));
    assert!(
        stdout.contains("installed complete release"),
        "unexpected stdout: {stdout}"
    );

    let installed = fixture.installed_binary(VERSION);
    let expected = sha256(AGENT_BYTES);
    let actual = file_sha256(&installed);
    println!("asset {archive_name} installed at {}", installed.display());
    println!("expected sha256 {expected}");
    println!("installed sha256 {actual}");
    assert_eq!(actual, expected, "the installed executable matches the release bytes");

    // The current channel points at the version this command installed.
    let current = fixture.home.path().join("builds").join("current").join(binary_name());
    assert_eq!(
        std::fs::canonicalize(&current).expect("the current channel resolves"),
        std::fs::canonicalize(&installed).expect("the installed version resolves")
    );
}

#[test]
fn rejects_checksum_mismatch() {
    let fixture = Fixture::new();
    write_release(fixture.assets.path(), AGENT_BYTES, true);
    success(&fixture.install(VERSION));
    let installed = fixture.installed_binary(VERSION);
    let before = file_sha256(&installed);

    // The checksum list records bytes the archive does not contain.
    write_release(fixture.assets.path(), b"different agent bytes\n", false);
    let output = fixture.install(NEXT_VERSION);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("exit: {:?}", output.status.code());
    println!("stderr: {stderr}");
    assert!(!output.status.success(), "a mismatched archive must be refused");
    assert!(
        stderr.contains("Checksum mismatch"),
        "the failure names the checksum problem: {stderr}"
    );
    assert_eq!(
        file_sha256(&installed),
        before,
        "the previously installed version keeps its bytes"
    );
    assert!(
        !fixture.installed_binary(NEXT_VERSION).exists(),
        "a refused release installs no version"
    );
    assert!(
        output.status.code() != Some(101),
        "the refusal is an error, not a panic"
    );
}

#[test]
fn names_the_missing_asset_without_panicking() {
    let fixture = Fixture::new();

    // A directory path that does not exist at all is refused with the path named.
    let absent = fixture.home.path().join("no-such-assets");
    let absent_arg = absent.to_string_lossy().into_owned();
    let output = fixture.run(&["install-release", "--assets", &absent_arg, "--agent-version", VERSION]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("absent directory exit: {:?}", output.status.code());
    println!("absent directory stderr: {stderr}");
    assert!(!output.status.success(), "an absent assets directory is refused");
    assert!(
        stderr.contains("failed to install a release from the assets directory") && stderr.contains("no-such-assets"),
        "the failure names the assets directory: {stderr}"
    );
    assert!(!stderr.contains("panicked"), "the refusal is not a panic: {stderr}");

    let output = fixture.install(VERSION);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("empty assets exit: {:?}", output.status.code());
    println!("empty assets stderr: {stderr}");
    assert!(!output.status.success(), "an empty assets directory is refused");
    assert!(
        stderr.contains(&format!("release is missing required asset {}", asset_stem())),
        "the failure names the missing archive: {stderr}"
    );
    assert!(!stderr.contains("panicked"), "the refusal is not a panic: {stderr}");

    // A release without its checksum list is refused before anything is staged.
    write_release(fixture.assets.path(), AGENT_BYTES, true);
    std::fs::remove_file(fixture.assets.path().join("SHA256SUMS")).expect("remove SHA256SUMS");
    let output = fixture.install(VERSION);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("checksumless exit: {:?}", output.status.code());
    println!("checksumless stderr: {stderr}");
    assert!(!output.status.success(), "a release without checksums is refused");
    assert!(
        stderr.contains("release is missing required asset SHA256SUMS"),
        "the failure names the missing checksum list: {stderr}"
    );

    // A present but unparsable checksum list is refused by name as well.
    write_release(fixture.assets.path(), AGENT_BYTES, true);
    std::fs::write(fixture.assets.path().join("SHA256SUMS"), "not a checksum line\n").expect("SHA256SUMS");
    let output = fixture.install(VERSION);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("malformed sums exit: {:?}", output.status.code());
    println!("malformed sums stderr: {stderr}");
    assert!(!output.status.success(), "a malformed SHA256SUMS is refused");
    assert!(
        stderr.contains("Invalid SHA256SUMS line"),
        "the failure names the malformed list: {stderr}"
    );
    assert!(!stderr.contains("panicked"), "the refusal is not a panic: {stderr}");
    assert!(
        !fixture.installed_binary(VERSION).exists(),
        "nothing was installed before verification passed"
    );
}
