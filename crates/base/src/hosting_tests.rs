use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn mode(path: &Path, bits: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).expect("set mode");
}

fn tenant(raw: &str) -> TenantId {
    TenantId::parse(raw).expect("valid tenant id")
}

fn policy(mode: HostingMode, tenant_id: Option<TenantId>) -> HostingPolicy {
    HostingPolicy::new(mode, tenant_id, &HostingSection::default()).expect("policy")
}

/// A home volume, a separate runtime directory and an operator home that share
/// one scratch root.
struct Boundary {
    root: tempfile::TempDir,
    home: PathBuf,
    runtime: PathBuf,
    operator: PathBuf,
}

impl Boundary {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("scratch root");
        let home = root.path().join("data/alice/home");
        let runtime = root.path().join("run/alice");
        let operator = root.path().join("operator/.horon");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&runtime).expect("runtime");
        std::fs::create_dir_all(&operator).expect("operator home");
        mode(&home, 0o700);
        mode(&runtime, 0o700);
        mode(&operator, 0o700);
        Self {
            root,
            home,
            runtime,
            operator,
        }
    }

    fn paths(&self) -> HostingPaths {
        HostingPaths {
            home: Some(self.home.clone()),
            runtime_dir: Some(self.runtime.clone()),
            operator_home: self.operator.clone(),
            gateway_enabled: false,
        }
    }

    fn root(&self) -> &Path {
        self.root.path()
    }
}

#[test]
fn an_absent_mode_selects_no_hosting_and_ignores_the_rest() {
    let policy = resolve(&HostingInputs::default()).expect("resolve");
    assert_eq!(policy.mode(), HostingMode::None);
    assert!(!policy.is_hosted());

    let stray_tenant = resolve(&HostingInputs {
        mode: None,
        tenant_id: Some("alice".to_string()),
        section: HostingSection::default(),
    })
    .expect("resolve");
    assert_eq!(stray_tenant.tenant_id(), None, "no mode means no tenant identity");
}

#[test]
fn the_environment_selects_each_hosted_mode() {
    let container = resolve(&HostingInputs {
        mode: Some("per-user-container".to_string()),
        tenant_id: Some("alice".to_string()),
        section: HostingSection::default(),
    })
    .expect("resolve");
    assert!(container.is_per_user_container());
    assert_eq!(container.tenant_id().map(TenantId::as_str), Some("alice"));

    let worker = resolve(&HostingInputs {
        mode: Some("shared-worker".to_string()),
        tenant_id: Some("alice".to_string()),
        section: HostingSection::default(),
    })
    .expect("resolve");
    assert!(worker.is_shared_worker());
    assert!(!worker.allows_shared_children());
}

#[test]
fn an_unknown_environment_value_names_the_allowed_values() {
    let error = resolve(&HostingInputs {
        mode: Some("container".to_string()),
        tenant_id: Some("alice".to_string()),
        section: HostingSection::default(),
    })
    .expect_err("unknown mode");
    let message = error.to_string();
    assert!(message.contains("unknown hosting mode 'container'"), "{message}");
    assert!(message.contains("per-user-container, shared-worker"), "{message}");
}

#[test]
fn a_hosted_mode_without_a_tenant_is_refused_at_resolution() {
    let error = resolve(&HostingInputs {
        mode: Some("per-user-container".to_string()),
        tenant_id: None,
        section: HostingSection::default(),
    })
    .expect_err("missing tenant");
    assert_eq!(
        error,
        HostingConfigError::MissingTenantId(HostingMode::PerUserContainer)
    );
    assert!(error.to_string().contains(TENANT_ID_ENV));
}

#[test]
fn tenant_ids_that_could_leave_the_volume_root_are_refused_at_resolution() {
    for raw in ["..", "../bob", "alice/bob", "/alice", "Alice", ""] {
        let error = resolve(&HostingInputs {
            mode: Some("per-user-container".to_string()),
            tenant_id: Some(raw.to_string()),
            section: HostingSection::default(),
        })
        .expect_err(raw);
        assert!(
            matches!(error, HostingConfigError::InvalidTenantId { .. }),
            "{raw:?} gave {error:?}"
        );
    }
}

#[test]
fn the_config_section_can_only_narrow_the_children_policy() {
    let section = HostingSection {
        allow_shared_children: Some(false),
        allowed_mcp_servers: None,
        mode: Some("shared-worker".to_string()),
    };
    let policy = resolve(&HostingInputs {
        mode: Some("per-user-container".to_string()),
        tenant_id: Some("alice".to_string()),
        section,
    })
    .expect("resolve");
    assert!(!policy.allows_shared_children());
    assert!(policy.is_per_user_container(), "the mode came from the environment");
}

#[test]
fn a_complete_container_boundary_is_accepted() {
    let boundary = Boundary::new();
    confirm_container_boundary(
        &policy(HostingMode::PerUserContainer, Some(tenant("alice"))),
        &boundary.paths(),
    )
    .expect("a complete boundary starts");
}

#[test]
fn the_operator_default_home_is_refused() {
    let boundary = Boundary::new();
    let mut paths = boundary.paths();
    paths.home = Some(boundary.operator.clone());
    let error = confirm_container_boundary(&policy(HostingMode::PerUserContainer, Some(tenant("alice"))), &paths)
        .expect_err("operator home");
    assert!(
        error.to_string().contains("must not be the operator's own home"),
        "{error}"
    );
}

#[test]
fn a_symlinked_home_is_refused() {
    let boundary = Boundary::new();
    let real = boundary.root().join("elsewhere");
    std::fs::create_dir_all(&real).expect("real home");
    mode(&real, 0o700);
    let mut paths = boundary.paths();
    let link = boundary.root().join("linked-home");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    paths.home = Some(link);
    let error = confirm_container_boundary(&policy(HostingMode::PerUserContainer, Some(tenant("alice"))), &paths)
        .expect_err("symlink");
    assert!(error.to_string().contains("must not be a symbolic link"), "{error}");
}

#[test]
fn a_world_readable_home_names_the_mode_to_change() {
    let boundary = Boundary::new();
    mode(&boundary.home, 0o755);
    let error = confirm_container_boundary(
        &policy(HostingMode::PerUserContainer, Some(tenant("alice"))),
        &boundary.paths(),
    )
    .expect_err("world-readable home");
    let message = error.to_string();
    assert!(message.contains("owner-only (0700)"), "{message}");
    assert!(message.contains("0755"), "{message}");
}

#[test]
fn a_runtime_directory_inside_the_home_is_refused() {
    let boundary = Boundary::new();
    let mut paths = boundary.paths();
    let nested = boundary.home.join("run");
    std::fs::create_dir_all(&nested).expect("nested runtime");
    mode(&nested, 0o700);
    paths.runtime_dir = Some(nested);
    let error = confirm_container_boundary(&policy(HostingMode::PerUserContainer, Some(tenant("alice"))), &paths)
        .expect_err("runtime inside home");
    assert!(
        error.to_string().contains("must be separate from HORON_HOME"),
        "{error}"
    );
}

#[test]
fn a_missing_home_or_runtime_is_refused_by_name() {
    let boundary = Boundary::new();
    let policy = policy(HostingMode::PerUserContainer, Some(tenant("alice")));
    let mut no_home = boundary.paths();
    no_home.home = None;
    assert!(
        confirm_container_boundary(&policy, &no_home)
            .expect_err("missing home")
            .to_string()
            .contains("no HORON_HOME")
    );

    let mut no_runtime = boundary.paths();
    no_runtime.runtime_dir = None;
    assert!(
        confirm_container_boundary(&policy, &no_runtime)
            .expect_err("missing runtime")
            .to_string()
            .contains("no HORON_AGENT_RUNTIME_DIR")
    );
}

#[test]
fn a_relative_home_is_refused() {
    let boundary = Boundary::new();
    let mut paths = boundary.paths();
    paths.home = Some(PathBuf::from("relative/home"));
    let error = confirm_container_boundary(&policy(HostingMode::PerUserContainer, Some(tenant("alice"))), &paths)
        .expect_err("relative home");
    assert!(error.to_string().contains("must be an absolute path"), "{error}");
}

#[test]
fn an_enabled_gateway_is_refused_even_when_every_path_is_valid() {
    let boundary = Boundary::new();
    let mut paths = boundary.paths();
    paths.gateway_enabled = true;
    let error = confirm_container_boundary(&policy(HostingMode::PerUserContainer, Some(tenant("alice"))), &paths)
        .expect_err("gateway");
    assert!(error.to_string().contains("TCP gateway must be disabled"), "{error}");
}

#[test]
fn a_shared_worker_skips_the_container_boundary() {
    let paths = HostingPaths {
        home: None,
        runtime_dir: None,
        operator_home: PathBuf::from("/home/operator/.horon"),
        gateway_enabled: true,
    };
    confirm_container_boundary(&policy(HostingMode::SharedWorker, Some(tenant("alice"))), &paths)
        .expect("a shared worker starts and leaves the refusal to its own gate");
}

#[test]
fn a_runtime_directory_reaching_home_through_a_parent_symlink_is_refused_before_creation() {
    let boundary = Boundary::new();
    let real_parent = boundary.root().join("real-parent");
    let alias_parent = boundary.root().join("alias-parent");
    let home = real_parent.join("home");
    std::fs::create_dir_all(&home).expect("real home");
    mode(&home, 0o700);
    std::os::unix::fs::symlink(&real_parent, &alias_parent).expect("parent symlink");

    // Case 1: `home` passes through `alias_parent`, while `runtime_dir` names the
    // canonical target inside `home` and does not exist yet.
    let mut paths = boundary.paths();
    paths.home = Some(alias_parent.join("home"));
    let canonical_nested = std::fs::canonicalize(&home).expect("canonical home").join("run");
    paths.runtime_dir = Some(canonical_nested.clone());
    let error = confirm_container_boundary(&policy(HostingMode::PerUserContainer, Some(tenant("alice"))), &paths)
        .expect_err("canonical runtime inside symlinked-parent home must be refused");
    assert!(
        matches!(error, HostingBoundaryError::RuntimeInsideHome(_)),
        "unexpected error: {error}"
    );
    assert!(
        !canonical_nested.exists(),
        "confirm_container_boundary must not create runtime_dir inside home"
    );

    // Case 2: `home` is canonical and `runtime_dir` reaches inside it via `alias_parent`.
    let mut reverse = boundary.paths();
    reverse.home = Some(home.clone());
    reverse.runtime_dir = Some(alias_parent.join("home/nested-via-link"));
    let error = confirm_container_boundary(&policy(HostingMode::PerUserContainer, Some(tenant("alice"))), &reverse)
        .expect_err("symlinked-parent runtime inside home must be refused");
    assert!(
        matches!(error, HostingBoundaryError::RuntimeInsideHome(_)),
        "unexpected error: {error}"
    );
    assert!(
        !home.join("nested-via-link").exists(),
        "confirm_container_boundary must not create directories inside home"
    );
}

#[test]
fn tenant_runtime_digest_matches_sha256_prefix() {
    assert_eq!(tenant_runtime_digest(Path::new("/data/alice/home")), "f1d25fc17edb");
}
