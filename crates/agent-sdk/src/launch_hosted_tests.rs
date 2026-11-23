use super::*;
use std::os::unix::fs::PermissionsExt;

const MAX_TENANT_ID: &str = "alice";

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().expect("scratch root")
}

/// A short runtime base. The platform temporary directory on macOS is a long
/// per-user path, and a socket below it would exceed the Unix socket limit;
/// production containers choose `/run`, and this stands in for that choice.
fn short_runtime_base() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("hlrt")
        .tempdir_in("/tmp")
        .expect("short runtime base")
}

/// A fake runtime that records the environment it was started with and exits,
/// so a launch reports a startup failure after the environment is captured.
fn capture_script(path: &Path) -> PathBuf {
    let script = path.join("capture-env");
    fs::write(
        &script,
        "#!/bin/sh\nprintf '%s\\n' \"$HORON_HOME\" \"$HORON_AGENT_RUNTIME_DIR\" \
         \"$HORON_AGENT_HOSTING_MODE\" \"$HORON_AGENT_TENANT_ID\" \
         \"$HORON_AGENT_NO_AUTO_UPDATE\" > \"$CAPTURE_PATH\"\nexit 1\n",
    )
    .expect("write fake runtime");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).expect("make fake runtime executable");
    script
}

#[test]
fn paths_keep_sockets_short_and_out_of_the_home_volume() {
    let sandbox = scratch();
    let root = sandbox.path().join("data");
    let runtime_base = short_runtime_base();
    let paths = tenant_paths(&root, MAX_TENANT_ID, runtime_base.path()).expect("paths");

    assert_eq!(paths.instance, root.join("alice"));
    assert_eq!(paths.home, root.join("alice/home"));
    assert!(paths.runtime_dir.starts_with(&runtime_base));
    let runtime_name = paths
        .runtime_dir
        .file_name()
        .and_then(|name| name.to_str())
        .expect("runtime name");
    assert!(runtime_name.starts_with("horon-tenant-"), "{runtime_name}");
    assert_eq!(runtime_name.len(), "horon-tenant-".len() + 12);
    assert_eq!(paths.api_socket, paths.runtime_dir.join("horon-agent-api.sock"));
    assert!(
        !paths.runtime_dir.starts_with(&paths.home),
        "sockets must stay off the home volume"
    );
    assert!(
        paths.api_socket.as_os_str().len() <= 100,
        "{}",
        paths.api_socket.display()
    );
}

#[test]
fn tenant_ids_that_could_leave_the_root_are_refused() {
    let sandbox = scratch();
    let root = sandbox.path().join("data");
    let runtime_base = short_runtime_base();
    for tenant in [
        "",
        "..",
        "../alice",
        "alice/bob",
        "alice\\bob",
        "/alice",
        "Alice",
        ".alice",
    ] {
        let error = tenant_paths(&root, tenant, runtime_base.path()).expect_err(tenant);
        assert!(
            error.to_string().contains("invalid tenant id"),
            "{tenant:?} gave {error}"
        );
    }
}

#[test]
fn a_relative_volume_root_is_refused() {
    let sandbox = scratch();
    let error = tenant_paths(Path::new("data"), MAX_TENANT_ID, sandbox.path()).expect_err("relative root");
    assert!(error.to_string().contains("must be an absolute path"), "{error}");
}

#[test]
fn two_tenants_get_distinct_runtime_directories() {
    let sandbox = scratch();
    let root = sandbox.path().join("data");
    let runtime_base = short_runtime_base();
    let alice = tenant_paths(&root, "alice", runtime_base.path()).expect("alice");
    let bob = tenant_paths(&root, "bob", runtime_base.path()).expect("bob");
    let alice_again = tenant_paths(&root, "alice", runtime_base.path()).expect("alice again");
    assert_ne!(alice.runtime_dir, bob.runtime_dir);
    assert_ne!(alice.home, bob.home);
    assert_eq!(alice.runtime_dir, alice_again.runtime_dir, "derivation is stable");
}

#[test]
fn a_hosted_launch_fixes_the_boundary_and_ignores_caller_overrides() {
    let sandbox = scratch();
    let root = sandbox.path().join("data");
    let runtime_base = short_runtime_base();
    let operator = sandbox.path().join("operator");
    fs::create_dir_all(&operator).expect("operator home");
    fs::write(operator.join("auth.json"), "operator-secret").expect("operator credential");
    let capture = sandbox.path().join("captured.txt");
    let binary = capture_script(sandbox.path());
    let expected = tenant_paths(&root, MAX_TENANT_ID, runtime_base.path()).expect("paths");

    let mut options = HostedLaunchOptions::new(&root, MAX_TENANT_ID);
    options.binary = Some(binary);
    options.runtime_base = Some(runtime_base.path().to_path_buf());
    options.startup_timeout = Duration::from_secs(5);
    options
        .env
        .insert(OsString::from("CAPTURE_PATH"), capture.clone().into_os_string());
    options
        .env
        .insert(OsString::from("HORON_HOME"), operator.clone().into_os_string());
    options
        .env
        .insert(OsString::from(HOSTING_MODE_ENV), OsString::from("shared-worker"));
    options
        .env
        .insert(OsString::from(TENANT_ID_ENV), OsString::from("mallory"));

    let Err(error) = launch_hosted_instance(&options) else {
        panic!("the fake runtime exits, so the launch must fail");
    };
    assert!(
        error.to_string().contains("exited during startup"),
        "unexpected launch error: {error}"
    );

    let captured = fs::read_to_string(&capture).expect("captured environment");
    let lines: Vec<&str> = captured.lines().collect();
    assert_eq!(
        lines,
        vec![
            expected.home.display().to_string(),
            expected.runtime_dir.display().to_string(),
            "per-user-container".to_string(),
            "alice".to_string(),
            "1".to_string(),
        ],
        "the hosted launch owns the tenant environment"
    );
    assert!(
        !expected.home.join("auth.json").exists(),
        "a hosted home must not inherit the operator's logins"
    );
    assert!(
        !expected.home.join("config.toml").exists(),
        "a hosted home must not inherit the operator's mutable config"
    );
    assert!(operator.join("auth.json").exists(), "the operator home is untouched");
}

#[test]
fn delete_removes_only_that_tenant() {
    let sandbox = scratch();
    let root = sandbox.path().join("data");
    let runtime_base = short_runtime_base();
    for tenant in ["alice", "bob"] {
        let home = root.join(tenant).join("home");
        fs::create_dir_all(&home).expect("tenant home");
        fs::write(home.join("session.json"), tenant).expect("tenant state");
    }

    delete_hosted_instance(&root, "alice", runtime_base.path()).expect("delete alice");
    assert!(!root.join("alice").exists(), "alice's instance directory is gone");
    assert_eq!(
        fs::read_to_string(root.join("bob/home/session.json")).expect("bob state"),
        "bob"
    );
    assert!(root.join("bob").exists(), "bob's instance directory is untouched");

    delete_hosted_instance(&root, "alice", runtime_base.path()).expect("deleting an absent tenant is a no-op");
    assert!(root.join("bob").exists());
}

#[test]
fn delete_refuses_a_symlinked_instance() {
    let sandbox = scratch();
    let root = sandbox.path().join("data");
    let runtime_base = short_runtime_base();
    fs::create_dir_all(&root).expect("volume root");
    let outside = sandbox.path().join("outside");
    fs::create_dir_all(&outside).expect("outside directory");
    fs::write(outside.join("keep.txt"), "keep").expect("outside file");
    std::os::unix::fs::symlink(&outside, root.join("alice")).expect("symlink");

    let error = delete_hosted_instance(&root, "alice", runtime_base.path()).expect_err("symlink");
    assert!(error.to_string().contains("symbolic link"), "{error}");
    assert_eq!(
        fs::read_to_string(outside.join("keep.txt")).expect("outside file survives"),
        "keep"
    );
}

#[test]
fn stop_is_idempotent_when_nothing_runs() {
    let sandbox = scratch();
    let root = sandbox.path().join("data");
    let runtime_base = short_runtime_base();
    stop_hosted_instance(&root, "alice", runtime_base.path()).expect("nothing to stop");
}

#[test]
fn sdk_and_instance_script_derive_identical_tenant_paths() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/hosting/instance.sh");
    let sandbox = scratch();
    let runtime_base = short_runtime_base();

    for (label, use_trailing_slash, tenant) in [("plain", false, "alice"), ("slash", true, "bob-42")] {
        let root = sandbox.path().join(label);
        let root_arg = if use_trailing_slash {
            format!("{}/", root.display())
        } else {
            root.display().to_string()
        };
        let runtime_arg = if use_trailing_slash {
            format!("{}/", runtime_base.path().display())
        } else {
            runtime_base.path().display().to_string()
        };
        let sdk_paths = tenant_paths(Path::new(&root_arg), tenant, Path::new(&runtime_arg)).expect("sdk paths");
        let output = std::process::Command::new(&script)
            .args(["create", tenant, "--root", &root_arg, "--runtime-dir", &runtime_arg])
            .output()
            .expect("run instance.sh create");
        assert!(
            output.status.success(),
            "instance.sh create failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).expect("utf-8 output");
        let reported_home = stdout
            .lines()
            .find_map(|line| line.trim_start().strip_prefix("home: "))
            .expect("home line");
        let reported_runtime = stdout
            .lines()
            .find_map(|line| line.trim_start().strip_prefix("runtime: "))
            .expect("runtime line");
        assert_eq!(Path::new(reported_home), sdk_paths.home);
        assert_eq!(Path::new(reported_runtime), sdk_paths.runtime_dir);
    }
}

#[test]
fn hosted_launch_strips_gateway_and_api_bind_overrides() {
    let sandbox = scratch();
    let root = sandbox.path().join("data");
    let runtime_base = short_runtime_base();
    let capture = sandbox.path().join("captured-env.txt");
    let script = sandbox.path().join("capture-reserved");
    fs::write(
        &script,
        "#!/bin/sh\nprintf '%s|%s|%s\\n' \"$HORON_AGENT_GATEWAY_ENABLED\" \"${HORON_AGENT_API_BIND:-unset}\" \"${HORON_AGENT_CONFIG:-unset}\" > \"$CAPTURE_PATH\"\nexit 1\n",
    )
    .expect("write capture");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).expect("chmod");

    let mut options = HostedLaunchOptions::new(&root, MAX_TENANT_ID);
    options.binary = Some(script);
    options.runtime_base = Some(runtime_base.path().to_path_buf());
    options.startup_timeout = Duration::from_secs(5);
    options
        .env
        .insert(OsString::from("CAPTURE_PATH"), capture.clone().into_os_string());
    options
        .env
        .insert(OsString::from("HORON_AGENT_GATEWAY_ENABLED"), OsString::from("1"));
    options
        .env
        .insert(OsString::from("HORON_AGENT_API_BIND"), OsString::from("0.0.0.0:7946"));
    options.env.insert(
        OsString::from("HORON_AGENT_CONFIG"),
        OsString::from("/etc/override.toml"),
    );

    let Err(_) = launch_hosted_instance(&options) else {
        panic!("capture script exits non-zero");
    };
    let captured = fs::read_to_string(&capture).expect("captured");
    assert_eq!(captured.trim(), "0|unset|unset");
}
