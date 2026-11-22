use std::fs;
use std::path::{Path, PathBuf};

use horon_agent_base::packages::{PackageKind, validate_package};
use tempfile::{TempDir, tempdir};

fn host_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        (os, arch) => panic!("package_manifest needs a supported runner, got {os}/{arch}"),
    }
}

fn binary_name() -> &'static str {
    if std::env::consts::OS == "windows" {
        "bin/child.exe"
    } else {
        "bin/child"
    }
}

fn extension(tail: &str, version: &str, artifact: &str, target: &str) -> String {
    format!(
        r#"
schema_version = 1
id = "hello"
version = "{version}"
name = "Hello"
protocol = "1.0"
{tail}
[targets]
{target} = "{artifact}"
"#
    )
}

fn fixture(nested: bool) -> (TempDir, PathBuf) {
    let root = tempdir().unwrap();
    let directory = if nested {
        root.path().join("native")
    } else {
        root.path().to_owned()
    };
    fs::create_dir_all(directory.join("bin")).unwrap();
    fs::copy(std::env::current_exe().unwrap(), directory.join(binary_name())).unwrap();
    fs::write(
        directory.join("extension.toml"),
        extension("", "1.2.3-beta.1+local.007", binary_name(), host_target()),
    )
    .unwrap();
    (root, directory)
}

fn plugin(root: &Path, resources: &str, version: &str) {
    fs::write(
        root.join("plugin.toml"),
        format!(
            r#"
schema_version = 1
id = "hello"
version = "{version}"
name = "Hello Plugin"
extension = "native/extension.toml"
[resources]
{resources}
"#
        ),
    )
    .unwrap();
}

fn rejected(root: &Path, reason: &str) {
    let message = format!("{:#}", validate_package(root).unwrap_err());
    assert!(message.contains(reason), "expected {reason:?}, got {message}");
}

#[test]
fn resolves_native_extension_without_launching_it() {
    let (root, _) = fixture(false);
    let validated = validate_package(root.path()).unwrap();
    assert_eq!(validated.kind, PackageKind::Extension);
    assert_eq!(validated.extension.id, "hello");
    assert_eq!(validated.executable, Path::new(binary_name()));
    assert_eq!(validated.files.len(), 2);
}

#[test]
fn resolves_plugin_binary_relative_to_its_extension_and_resources_to_root() {
    let (root, _) = fixture(true);
    fs::write(root.path().join("rule.md"), "A package rule").unwrap();
    plugin(root.path(), "rules = ['rule.md']", "1.2.3-beta.1+local.007");
    let validated = validate_package(root.path()).unwrap();
    assert_eq!(validated.kind, PackageKind::Plugin);
    assert_eq!(validated.display_name, "Hello Plugin");
    assert_eq!(validated.executable, Path::new("native").join(binary_name()));
    assert!(validated.files.contains(Path::new("rule.md")));
}

#[test]
fn selects_host_when_a_foreign_artifact_is_also_declared() {
    let (root, _) = fixture(false);
    // Synthetic foreign header. The host artifact is the genuine test binary.
    let mut file = [0u8; 256];
    file[..2].copy_from_slice(b"MZ");
    file[60] = 64;
    file[64..68].copy_from_slice(b"PE\0\0");
    let (target, machine): (&str, [u8; 2]) = if host_target().starts_with("x86_64-") {
        ("aarch64-pc-windows-msvc", [0x64, 0xaa])
    } else {
        ("x86_64-pc-windows-msvc", [0x64, 0x86])
    };
    file[68..70].copy_from_slice(&machine);
    file[70] = 1;
    file[84] = 112;
    file[86] = 2;
    file[88..90].copy_from_slice(&[0x0b, 0x02]);
    fs::write(root.path().join("foreign.exe"), file).unwrap();
    let text = extension("", "1.2.3", binary_name(), host_target());
    fs::write(
        root.path().join("extension.toml"),
        format!("{text}{target} = 'foreign.exe'\n"),
    )
    .unwrap();
    let validated = validate_package(root.path()).unwrap();
    assert_eq!(validated.executable, Path::new(binary_name()));
    assert!(validated.files.contains(Path::new("foreign.exe")));

    let wrong_machine = if machine == [0x64, 0xaa] {
        [0x64, 0x86]
    } else {
        [0x64, 0xaa]
    };
    file[68..70].copy_from_slice(&wrong_machine);
    fs::write(root.path().join("foreign.exe"), file).unwrap();
    rejected(root.path(), "PE CPU does not match declared target");
}

#[test]
fn rejects_missing_binary_and_ambiguous_root() {
    let (root, directory) = fixture(true);
    fs::write(root.path().join("rule.md"), "Rule").unwrap();
    plugin(root.path(), "rules = ['rule.md']", "1.2.3-beta.1+local.007");
    fs::remove_file(directory.join(binary_name())).unwrap();
    rejected(root.path(), "inspect");
    fs::write(root.path().join("extension.toml"), "").unwrap();
    rejected(root.path(), "exactly one");
}

#[test]
fn rejects_incompatible_versions_and_formats() {
    let (root, _) = fixture(false);
    let path = root.path().join("extension.toml");
    for bad_version in ["1.0", "01.2.3", "1.2.3-02", "1.2.3+", "1.2.3-a..b"] {
        fs::write(&path, extension("", bad_version, binary_name(), host_target())).unwrap();
        rejected(root.path(), "invalid SemVer");
    }
    let good = extension("", "1.2.3", binary_name(), host_target());
    fs::write(&path, good.replace("schema_version = 1", "schema_version = 2")).unwrap();
    rejected(root.path(), "unsupported package schema version");
    fs::write(&path, good.replace("protocol = \"1.0\"", "protocol = \"99.0\"")).unwrap();
    rejected(root.path(), "unsupported extension protocol");
    fs::write(&path, good).unwrap();
    fs::write(root.path().join(binary_name()), [0; 256]).unwrap();
    rejected(root.path(), "expected");
}

#[test]
fn rejects_path_escape_and_reserved_windows_paths_on_every_runner() {
    let (root, _) = fixture(false);
    for path in [
        "../bin/child",
        "/bin/child",
        "C:/child.exe",
        "bin//child",
        "bin/CON.exe",
    ] {
        fs::write(
            root.path().join("extension.toml"),
            extension("", "1.2.3", path, host_target()),
        )
        .unwrap();
        rejected(root.path(), "invalid package-relative path");
    }
}

#[test]
fn rejects_duplicate_command_and_oversized_tool_name() {
    let (root, directory) = fixture(true);
    plugin(root.path(), "commands = ['hello.md']", "1.2.3");
    fs::write(root.path().join("hello.md"), "template").unwrap();
    let manifest = directory.join("extension.toml");
    fs::write(
        &manifest,
        extension(
            "[[commands]]\nname = 'hello'\ndescription = 'Run'",
            "1.2.3",
            binary_name(),
            host_target(),
        ),
    )
    .unwrap();
    rejected(root.path(), "duplicate public package name: hello");
    let tools = format!(
        "[[tools]]\nname = '{}'\ndescription = 'Run'\ninput_schema = {{ type = 'object' }}",
        "a".repeat(53)
    );
    fs::write(&manifest, extension(&tools, "1.2.3", binary_name(), host_target())).unwrap();
    rejected(root.path(), "invalid public package name");
}

#[test]
fn refuses_package_paths_through_links() {
    let (root, directory) = fixture(false);
    let outside = tempdir().unwrap();
    let manifest = directory.join("extension.toml");
    if std::env::consts::OS == "windows" {
        // Directory junctions exercise reparse-point rejection without a symlink privilege.
        let junction = directory.join("redirect");
        fs::copy(directory.join(binary_name()), outside.path().join("child.exe")).unwrap();
        let output = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&junction)
            .arg(outside.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        fs::write(manifest, extension("", "1.2.3", "redirect/child.exe", host_target())).unwrap();
        let result = validate_package(root.path());
        fs::remove_dir(junction).unwrap();
        assert!(format!("{:#}", result.unwrap_err()).contains("links are not allowed"));
    } else {
        let source = outside.path().join("extension.toml");
        fs::copy(&manifest, &source).unwrap();
        fs::remove_file(&manifest).unwrap();
        horon_agent_platform::fs::symlink_file(&source, &manifest).unwrap();
        rejected(root.path(), "links are not allowed");
    }
}

#[test]
fn rejects_nonportable_path_spellings_before_installation() {
    let (root, _) = fixture(true);
    for path in ["Rules/first.md", "rules/second.md"] {
        let file = root.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, "Rule").unwrap();
    }
    plugin(
        root.path(),
        "rules = ['Rules/first.md', 'rules/second.md']",
        "1.2.3-beta.1+local.007",
    );
    rejected(root.path(), "case-insensitive package path collision");
    plugin(root.path(), "rules = ['règles.md']", "1.2.3-beta.1+local.007");
    rejected(root.path(), "invalid package-relative path");
}

#[test]
fn accepts_a_plugin_hook_file_that_subscribes_to_an_event() {
    let (root, _) = fixture(true);
    fs::write(root.path().join("hook.json"), r#"{"events": ["tool.after"]}"#).unwrap();
    plugin(root.path(), "hooks = ['hook.json']", "1.2.3-beta.1+local.007");
    let validated = validate_package(root.path()).unwrap();
    assert_eq!(validated.resources.hooks, ["hook.json"]);
}

#[test]
fn rejects_a_hook_file_that_repeats_a_declared_event() {
    let (root, _) = fixture(true);
    fs::write(
        root.path().join("native/extension.toml"),
        extension(
            r#"events = ["turn.start"]"#,
            "1.2.3-beta.1+local.007",
            binary_name(),
            host_target(),
        ),
    )
    .unwrap();
    fs::write(root.path().join("hook.json"), r#"{"events": ["turn.start"]}"#).unwrap();
    plugin(root.path(), "hooks = ['hook.json']", "1.2.3-beta.1+local.007");
    rejected(root.path(), "declared in extension.toml");
}

#[test]
fn rejects_two_hook_files_that_name_the_same_event() {
    let (root, _) = fixture(true);
    fs::write(root.path().join("first.json"), r#"{"events": ["turn.end"]}"#).unwrap();
    fs::write(root.path().join("second.json"), r#"{"events": ["turn.end"]}"#).unwrap();
    plugin(
        root.path(),
        "hooks = ['first.json', 'second.json']",
        "1.2.3-beta.1+local.007",
    );
    rejected(root.path(), "duplicate package event");
}

#[test]
fn rejects_a_hook_file_with_an_unknown_event() {
    let (root, _) = fixture(true);
    fs::write(root.path().join("hook.json"), r#"{"events": ["prompt.read"]}"#).unwrap();
    plugin(root.path(), "hooks = ['hook.json']", "1.2.3-beta.1+local.007");
    rejected(root.path(), "parse hook.json");
}
