use super::*;

#[test]
fn extensions_select_the_server() {
    assert_eq!(
        server_for_path(Path::new("src/lib.rs")).map(|spec| spec.name),
        Some("rust-analyzer")
    );
    assert_eq!(
        server_for_path(Path::new("web/app.TSX")).map(|spec| spec.name),
        Some("typescript-language-server"),
        "the extension match is case-insensitive"
    );
    assert_eq!(
        server_for_path(Path::new("main.go")).map(|spec| spec.name),
        Some("gopls")
    );
    assert_eq!(
        server_for_path(Path::new("tool.py")).map(|spec| spec.name),
        Some("pyright")
    );
    assert_eq!(server_for_path(Path::new("notes.md")).map(|spec| spec.name), None);
    assert_eq!(server_for_path(Path::new("Makefile")).map(|spec| spec.name), None);
}

#[test]
fn servers_are_addressable_by_name() {
    assert_eq!(server_for_name("gopls").map(|spec| spec.program), Some("gopls"));
    assert!(server_for_name("no-such-server").is_none());
}

#[test]
fn the_workspace_root_is_the_nearest_marker_ancestor() {
    let dir = tempfile::tempdir().expect("temp dir");
    let crate_dir = dir.path().join("crates").join("k");
    let src = crate_dir.join("src");
    std::fs::create_dir_all(&src).expect("create src");
    std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").expect("write workspace manifest");
    std::fs::write(crate_dir.join("Cargo.toml"), "[package]\n").expect("write crate manifest");
    std::fs::write(src.join("lib.rs"), "fn a() {}\n").expect("write file");

    let spec = server_for_name("rust-analyzer").expect("rust-analyzer spec");
    let root = workspace_root(&src.join("lib.rs"), spec, Path::new("/fallback"));
    assert_eq!(root, crate_dir, "the nearest Cargo.toml wins");
}

#[test]
fn the_workspace_root_falls_back_to_the_working_directory() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("script.py");
    std::fs::write(&file, "print(1)\n").expect("write file");

    let spec = server_for_name("pyright").expect("pyright spec");
    let fallback = dir.path().join("fallback");
    std::fs::create_dir_all(&fallback).expect("create fallback");
    assert_eq!(workspace_root(&file, spec, &fallback), fallback);
}

#[test]
fn a_missing_executable_is_reported_as_not_installed() {
    let spec = ServerSpec {
        name: "absent",
        program: "horon-agent-absent-language-server",
        args: &[],
        extensions: &[],
        root_markers: &[],
    };
    assert!(!is_installed(&spec));
}

#[test]
fn an_executable_on_path_is_installed() {
    let dir = tempfile::tempdir().expect("temp dir");
    let program = dir.path().join("fake-language-server");
    std::fs::write(&program, "#!/bin/sh\nexit 0\n").expect("write program");
    horon_agent_platform::set_permissions_executable(&program).expect("make it executable");

    let spec = ServerSpec {
        name: "fake",
        program: "fake-language-server",
        args: &[],
        extensions: &[],
        root_markers: &[],
    };
    assert!(is_installed_for(&spec, dir.path()));
}

/// Execute bits are a Unix property; Windows decides executability from the
/// file extension, so the same file is installed there and this half of the
/// contract has nothing to assert.
#[cfg(unix)]
#[test]
fn a_file_without_execute_bits_is_not_installed() {
    let dir = tempfile::tempdir().expect("temp dir");
    let program = dir.path().join("fake-language-server");
    std::fs::write(&program, "#!/bin/sh\nexit 0\n").expect("write program");
    horon_agent_platform::fs::set_file_permissions_owner_only(&program).expect("strip execute bits");

    let spec = ServerSpec {
        name: "fake",
        program: "fake-language-server",
        args: &[],
        extensions: &[],
        root_markers: &[],
    };
    assert!(!is_installed_for(&spec, dir.path()));
}

fn is_installed_for(spec: &ServerSpec, dir: &Path) -> bool {
    horon_agent_platform::fs::is_executable(&dir.join(spec.program))
}
